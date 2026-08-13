// LLM provider abstraction.
//
// All chat traffic flows client -> core -> upstream (per plan §9). This
// module owns the upstream call: it builds an OpenAI-SDK-compatible client
// pointed at either the local llama-server or an external endpoint, runs
// a streaming chat completion, and yields raw OpenAI delta chunks.
//
// The scheduler (llm-scheduler.ts) wraps the call to enforce concurrency.
// services/chat.ts wraps the scheduler to orchestrate the turn lifecycle
// (tool-call hops, persistence, frame emission).

import OpenAI from "openai";

// The SDK's exposed fetch contract. Re-declared here (rather than
// imported from the SDK's `./internal/...`) so we don't depend on
// internal paths. Kept synchronized with @openai/Fetch.
type SdkFetch = (input: string | URL | Request, init?: RequestInit) => Promise<Response>;

export type LlmRoute = "default" | "secondary";

export interface LlmEndpointConfig {
  baseUrl: string;
  apiKey: string;
  model: string;
  // Optional reasoning hint (provider-specific encoding handled below).
  reasoning?: "off" | "on";
  // OpenAI-style sampling (sent to both local and external endpoints).
  temperature?: number;
  topP?: number;
  maxTokens?: number;
  // llama.cpp-only samplers (sent in the extra body for local; OpenAI-style
  // endpoints reject them) and the per-turn thinking budget (local only).
  topK?: number;
  minP?: number;
  repeatPenalty?: number;
  // DRY (Don't Repeat Yourself) sampler multiplier; > 0 discourages the model
  // from repeating whole phrases, which curbs the semantic loops small
  // reasoning models fall into. 0/undefined omits it. llama.cpp-only.
  dryMultiplier?: number;
  // OpenAI-style presence penalty; sent to both providers when non-zero.
  presencePenalty?: number;
  // llama.cpp sampler-chain order (canonical names). Empty/undefined = the
  // server's default chain. llama.cpp-only.
  samplers?: string[];
  reasoningBudget?: number;
  // External-only: the OpenAI-style `reasoning_effort` level. Local endpoints
  // use `reasoningBudget` instead.
  reasoningEffort?: "minimal" | "low" | "medium" | "high";
  // Test seam: when present, the OpenAI SDK uses this fetch instead of the
  // global. Production callers leave this undefined. Typed to the SDK's
  // `Fetch` shape (single-signature `(input, init?) => Promise<Response>`)
  // rather than `typeof fetch` so the overload-vs-single-signature
  // mismatch doesn't force a cast at the construction site.
  fetch?: SdkFetch;
}

export interface LlmRequest {
  endpoint: LlmEndpointConfig;
  // OpenAI-SDK chat-completion message shape (caller has already converted
  // domain messages into this format).
  messages: OpenAI.Chat.Completions.ChatCompletionMessageParam[];
  // Optional tool list (OpenAI function-tool format).
  tools?: OpenAI.Chat.Completions.ChatCompletionTool[];
  // Per-call sampling overrides; merged on top of endpoint defaults.
  overrides?: {
    temperature?: number;
    topP?: number;
    maxTokens?: number;
    // Per-call thinking budget, in tokens. 0 turns thinking off; N>0 enables
    // it and caps the `<think>` block at N tokens. When set, it overrides the
    // endpoint's reasoning mode (used by single-shot utility calls, each of
    // which has its own budget setting). Leave undefined for the chat path,
    // which uses the endpoint reasoning mode + the server's boot budget.
    reasoningBudget?: number;
  };
  signal?: AbortSignal;
}

export interface LlmDelta {
  contentDelta?: string;
  reasoningDelta?: string;
  toolCalls?: Array<{
    index: number;
    id?: string;
    name?: string;
    argumentsDelta?: string;
  }>;
  finishReason?: string;
  usage?: { prompt: number; completion: number; total: number };
}

// Builds an OpenAI-SDK client pointed at the configured endpoint.
export function buildClient(endpoint: LlmEndpointConfig): OpenAI {
  return new OpenAI({
    baseURL: endpoint.baseUrl,
    apiKey: endpoint.apiKey || "sk-local",
    // Keep the SDK's default timeout (10 minutes). It only bounds the time to
    // response headers, not the streamed body, so long completions are fine.
    // Never pass `timeout: 0`: the SDK treats it as a literal 0ms deadline and
    // aborts every request instantly with "Request timed out".
    maxRetries: 0,
    // The SDK's `Fetch` type ultimately resolves through DOM's RequestInit;
    // Deno's RequestInit body differs at the ReadableStream<.read(view)>
    // level (DOM allows ArrayBufferView<ArrayBufferLike>, Deno is
    // stricter). They're runtime-compatible (the SDK only constructs
    // requests via Request/string/URL, never passes a ReadableStream body)
    // but TS can't see that. The cast is the boundary marker.
    // deno-lint-ignore no-explicit-any
    ...(endpoint.fetch ? { fetch: endpoint.fetch as any } : {}),
  });
}

// Runs a streaming chat completion and yields normalized deltas.
export async function* streamChatCompletion(req: LlmRequest): AsyncIterable<LlmDelta> {
  const client = buildClient(req.endpoint);

  const temperature = req.overrides?.temperature ?? req.endpoint.temperature;
  const topP = req.overrides?.topP ?? req.endpoint.topP;
  const maxTokens = req.overrides?.maxTokens ?? req.endpoint.maxTokens;

  const extra: Record<string, unknown> = {};
  const isLocal = isLocalEndpoint(req.endpoint.baseUrl);
  const perCallBudget = req.overrides?.reasoningBudget;
  if (perCallBudget !== undefined) {
    // A per-call thinking budget overrides the endpoint reasoning mode. 0
    // turns thinking off; N>0 enables it and caps the `<think>` block. llama.cpp
    // honors `reasoning_budget` per request (overriding the server boot budget).
    const think = perCallBudget > 0;
    if (isLocal) {
      extra.chat_template_kwargs = { enable_thinking: think };
      if (think) setLocalThinkingBudget(extra, perCallBudget);
    } else if (think) {
      // OpenAI-compatible servers take no token budget; ask for thinking only.
      extra.reasoning_effort = "high";
    }
  } else if (req.endpoint.reasoning) {
    if (isLocal) {
      // llama.cpp accepts `chat_template_kwargs.enable_thinking` (per its server
      // docs). Thinking-by-default models (e.g. Qwen3) keep thinking unless the
      // template is told `false`, so "off" must send the flag, not omit it.
      const think = req.endpoint.reasoning === "on";
      extra.chat_template_kwargs = { enable_thinking: think };
      // Carry the per-turn budget here so a capped `<think>` applies on the chat
      // path without a server restart. Sent per request (each client keeps its
      // own budget), never pinned at boot.
      if (think && req.endpoint.reasoningBudget) {
        setLocalThinkingBudget(extra, req.endpoint.reasoningBudget);
      }
    } else if (req.endpoint.reasoning === "on") {
      // OpenAI-compatible servers take no token budget; ask for an effort level.
      // "off" sends nothing: non-reasoning models reject the parameter.
      extra.reasoning_effort = req.endpoint.reasoningEffort ?? "high";
    }
  }
  // Local-only samplers llama.cpp accepts in the extra body (OpenAI-style
  // endpoints reject them). There is no per-call override channel, so single-shot
  // utility calls receive these too; they pin a low temperature, where top_k /
  // min_p are near-inert and a mild repeat penalty is harmless.
  if (isLocal) {
    if (req.endpoint.topK !== undefined) extra.top_k = req.endpoint.topK;
    if (req.endpoint.minP !== undefined) extra.min_p = req.endpoint.minP;
    if (req.endpoint.repeatPenalty !== undefined) {
      extra.repeat_penalty = req.endpoint.repeatPenalty;
    }
    // DRY curbs verbatim phrase loops the token-level repeat penalty misses; the
    // base/length/window default on the server, so only the multiplier is sent.
    if (req.endpoint.dryMultiplier) extra.dry_multiplier = req.endpoint.dryMultiplier;
    // Sampler-chain order (catalog models set a loop-resistant one); omitted when
    // unset so the server keeps its default chain.
    if (req.endpoint.samplers?.length) extra.samplers = req.endpoint.samplers;
  }
  // presence_penalty is OpenAI-standard, so it goes to both providers. Omitted
  // when 0 (its no-op) to avoid tripping minimal external servers.
  if (req.endpoint.presencePenalty) extra.presence_penalty = req.endpoint.presencePenalty;

  const stream = await client.chat.completions.create(
    {
      model: req.endpoint.model,
      messages: req.messages,
      tools: req.tools,
      stream: true,
      stream_options: { include_usage: true },
      ...(temperature !== undefined ? { temperature } : {}),
      ...(topP !== undefined ? { top_p: topP } : {}),
      ...(maxTokens !== undefined ? { max_tokens: maxTokens } : {}),
      ...extra,
    },
    { signal: req.signal },
  );

  for await (const chunk of stream) {
    const choice = chunk.choices[0];
    const delta: LlmDelta = {};
    if (choice?.delta?.content) delta.contentDelta = choice.delta.content;
    // Reasoning can arrive as either `reasoning_content` (llama.cpp /
    // DeepSeek style) or a flat `reasoning` string (some OpenAI-compatible
    // gateways). Accept either, preferring `reasoning_content` when both
    // appear in the same delta.
    const rawDelta = choice?.delta as
      | { reasoning_content?: string; reasoning?: string }
      | undefined;
    const reasoning = rawDelta?.reasoning_content ?? rawDelta?.reasoning;
    if (reasoning) delta.reasoningDelta = reasoning;
    const toolCalls = choice?.delta?.tool_calls;
    if (toolCalls) {
      delta.toolCalls = toolCalls.map((tc) => ({
        index: tc.index,
        id: tc.id,
        name: tc.function?.name,
        argumentsDelta: tc.function?.arguments,
      }));
    }
    if (choice?.finish_reason) delta.finishReason = choice.finish_reason;
    if (chunk.usage) {
      delta.usage = {
        prompt: chunk.usage.prompt_tokens ?? 0,
        completion: chunk.usage.completion_tokens ?? 0,
        total: chunk.usage.total_tokens ?? 0,
      };
    }
    yield delta;
  }
}

function isLocalEndpoint(baseUrl: string): boolean {
  return (
    baseUrl.includes("127.0.0.1") || baseUrl.includes("localhost") || baseUrl.includes("0.0.0.0")
  );
}

// Encodes a per-request thinking-token cap for llama-server's OpenAI-compatible
// endpoint. The budget arrives as `thinking_budget_tokens` (mapped internally to
// the `reasoning_budget_tokens` sampler); `reasoning_control` must be true for
// the server to create the on-demand budget sampler that forcibly ends the
// `<think>` block at the cap. Both are per-request body fields, so no boot flag
// and every client can run a different budget. (llama.cpp does NOT read a
// top-level `reasoning_budget`, so that name is silently dropped.)
function setLocalThinkingBudget(extra: Record<string, unknown>, budget: number): void {
  extra.thinking_budget_tokens = budget;
  extra.reasoning_control = true;
}
