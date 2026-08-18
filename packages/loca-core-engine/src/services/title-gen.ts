// Title generation for sessions.
//
// After a session's first turn completes (user message + first assistant
// reply), we ask the configured LLM for a short title and PATCH it onto
// the session. The client picks it up via the `session.updated` WS frame
// with op="title_changed". The user can also regenerate the title on demand
// from the session bar, which routes here through `regenerateTitle`.
//
// The automatic path (`maybeGenerateTitle`) is fire-and-forget: failures are
// logged but never surface. While a title is being generated we broadcast a
// `session.updated` frame with op="title_generating" so the client can show a
// spinner; the matching `generating: false` is always sent when we finish.
//
// The prompt is the user-overridable `prompts.titleGenerationPrompt` setting
// (default in @loca/shared/domain/prompts.ts). It is a template with
// `{firstMessage}` and `{recentMessages}` placeholders: we fill the first with
// the opening message and the second with the latest messages that fit the
// model's context window.

import type OpenAI from "openai";
import { DEFAULT_TITLE_GENERATION_PROMPT, errMessage } from "@loca/shared";
import { sessionsRepo } from "./sessions-store.ts";
import { loadEffective, loadEffectiveResolved } from "./core-settings.ts";
import { type LlmRequest, streamChatCompletion } from "./llm-provider.ts";
import { resolveContextSize, resolveEndpoint } from "./endpoint-resolver.ts";
import { thinkingBudget } from "./thinking-budget.ts";
import { frameBus } from "../frame-bus.ts";
import { getLogger } from "../platform/log.ts";

const log = getLogger("title-gen");

// Hard cap to keep titles from blowing up the UI / DB. The LLM is asked
// for ~5 words but adversarial / runaway models can ignore that.
const MAX_TITLE_CHARS = 80;

// Cap on the raw streamed output we accumulate before sanitizing. A
// non-compliant endpoint can ignore `maxTokens` and stream unboundedly;
// `sanitize()` runs regexes (the `<think>` strip) over this string, so an
// unbounded accumulation would let it do super-linear work on the event loop.
// Generous enough for any legitimate title plus a stray `<think>` block.
const MAX_TITLE_RAW_CHARS = 4096;

// Title generation runs outside the LLM scheduler (and its watchdog), so it
// gets its own deadline: a wedged endpoint must not keep a streaming
// connection (and this fire-and-forget task) alive forever.
const TITLE_GEN_TIMEOUT_MS = 30_000;

// Output allowance reserved for the title itself, on top of the thinking
// budget, when deciding how many recent messages can fit the context window.
const TITLE_OUTPUT_TOKENS = 32;

// Slack left under the context window so a slightly-off character estimate
// can't tip the request into an overflow error.
const TITLE_SAFETY_MARGIN_TOKENS = 64;

/** Run automatically after a session's first turn. No-op unless the session
 *  has no title yet. */
export function maybeGenerateTitle(sessionId: string, ownerClientId: string): Promise<void> {
  return generateTitle(sessionId, ownerClientId, { force: false });
}

/** Run on explicit user request (the session bar's regenerate button).
 *  Replaces any existing title. */
export function regenerateTitle(sessionId: string, ownerClientId: string): Promise<void> {
  return generateTitle(sessionId, ownerClientId, { force: true });
}

async function generateTitle(
  sessionId: string,
  ownerClientId: string,
  opts: { force: boolean },
): Promise<void> {
  let signalledStart = false;
  try {
    let session;
    try {
      session = await sessionsRepo().getOrThrow(ownerClientId, sessionId);
    } catch {
      return;
    }
    // Temporary sessions never get a generated title: they show a fixed
    // "Temporary Session" label in the bar, so a title would be wasted work
    // (and a wasted LLM call).
    if (session.temporary) return;
    // The automatic path only titles untitled sessions; the manual path
    // always regenerates.
    if (!opts.force && session.title && session.title.trim() !== "") return;

    // Tell the client we're working now that we've committed to a run. The
    // `finally` below always sends the matching `generating: false`, so an
    // optimistic spinner can never get stuck.
    broadcastGenerating(ownerClientId, sessionId, true);
    signalledStart = true;

    const sessionMessages = await sessionsRepo().listMessages(sessionId);
    const firstUser = sessionMessages.find((m) => m.role === "user");
    const firstAssistant = sessionMessages.find((m) => m.role === "assistant");
    if (!firstUser || !firstAssistant) return;

    // Honor the owner's per-client title prompt + thinking budget (client-on-core);
    // the endpoint/context-size reads below are core-global and unaffected.
    const settings = await loadEffectiveResolved(ownerClientId);
    const template = strSetting(
      settings,
      "prompts.titleGenerationPrompt",
      DEFAULT_TITLE_GENERATION_PROMPT,
    );
    // The SPARSE record for the resolvers: their `llm.port` fallback is the
    // channel-adjusted `llmPort()`, and the schema's default is the bare "7701",
    // so a resolved record sends dev and latest at the stable channel's
    // llama-server. See D-078.
    const sparse = await loadEffective(ownerClientId);
    const endpoint = await resolveEndpoint(sparse);
    // Title Thinking Budget defaults to 128; 0 turns thinking off for this
    // call. maxTokens carries the budget on top of the title's own allowance so
    // thinking can't crowd it out.
    const budget = thinkingBudget(settings, "prompts.titleGenerationThinkingBudget");
    const contextSize = resolveContextSize(sparse, "default");

    const { firstMessage, recentMessages } = selectTitleContext(
      sessionMessages,
      template,
      contextSize,
      TITLE_OUTPUT_TOKENS + budget,
    );
    const systemPrompt = fillTemplate(template, firstMessage, recentMessages);

    const llmMessages: OpenAI.Chat.Completions.ChatCompletionMessageParam[] = [
      { role: "system", content: systemPrompt },
      { role: "user", content: "Title:" },
    ];
    const req: LlmRequest = {
      endpoint,
      messages: llmMessages,
      overrides: {
        temperature: 0.3,
        maxTokens: TITLE_OUTPUT_TOKENS + budget,
        reasoningBudget: budget,
      },
      signal: AbortSignal.timeout(TITLE_GEN_TIMEOUT_MS),
    };

    let title = "";
    for await (const delta of streamChatCompletion(req)) {
      if (delta.contentDelta) title += delta.contentDelta;
      // Stop accumulating once we have far more than any real title needs;
      // breaking the loop also returns the async generator so the upstream
      // stream is torn down.
      if (title.length > MAX_TITLE_RAW_CHARS) break;
    }
    title = sanitize(title);
    if (!title) return;

    await sessionsRepo().patchTitle(ownerClientId, sessionId, title);
    frameBus().broadcastToClient(ownerClientId, {
      kind: "session.updated",
      sessionId,
      op: "title_changed",
      payload: { title },
    });
  } catch (err) {
    log.warn(`title gen failed for ${sessionId}: ${errMessage(err)}`);
  } finally {
    if (signalledStart) broadcastGenerating(ownerClientId, sessionId, false);
  }
}

function broadcastGenerating(ownerClientId: string, sessionId: string, generating: boolean): void {
  frameBus().broadcastToClient(ownerClientId, {
    kind: "session.updated",
    sessionId,
    op: "title_generating",
    payload: { generating },
  });
}

// Exported for testing. Picks the opening message plus the newest messages
// that fit the title prompt within the model's context window.
export function __selectTitleContextForTesting(
  messages: { role: string; content?: unknown }[],
  template: string,
  contextSize: number,
  outputAllowance: number,
): { firstMessage: string; recentMessages: string } {
  return selectTitleContext(messages, template, contextSize, outputAllowance);
}

function selectTitleContext(
  messages: { role: string; content?: unknown }[],
  template: string,
  contextSize: number,
  outputAllowance: number,
): { firstMessage: string; recentMessages: string } {
  // Title prompting only cares about the conversational turns; tool calls,
  // reasoning traces, and empty messages carry no topic signal.
  const turns: { role: string; text: string }[] = [];
  for (const m of messages) {
    if (m.role !== "user" && m.role !== "assistant") continue;
    const text = extractText(m);
    if (text) turns.push({ role: m.role, text });
  }
  if (turns.length === 0) return { firstMessage: "", recentMessages: "" };

  const firstMessage = turns[0].text;

  // Budget the recent transcript against the context window, leaving room for
  // the template's fixed text, the title output, the thinking budget, and a
  // safety margin. The opening message is always included.
  const overhead = estimateTokens(
    template.replaceAll("{firstMessage}", "").replaceAll("{recentMessages}", ""),
  );
  let budget =
    contextSize -
    overhead -
    outputAllowance -
    TITLE_SAFETY_MARGIN_TOKENS -
    estimateTokens(firstMessage);

  // Walk newest -> oldest, skipping the opening turn (shown separately), and
  // keep what fits. Reverse so the transcript reads chronologically.
  const picked: string[] = [];
  for (let i = turns.length - 1; i >= 1; i--) {
    const line = `${roleLabel(turns[i].role)}: ${turns[i].text}`;
    const cost = estimateTokens(line) + 1; // +1 for the joining newline
    if (budget - cost < 0) break;
    budget -= cost;
    picked.push(line);
  }
  picked.reverse();

  return { firstMessage, recentMessages: picked.join("\n") };
}

function fillTemplate(template: string, firstMessage: string, recentMessages: string): string {
  return template
    .replaceAll("{firstMessage}", firstMessage)
    .replaceAll("{recentMessages}", recentMessages || "(none)");
}

// Rough char-based token estimate (~4 chars/token). The codebase has no
// tokenizer; this matches the character-budget convention used elsewhere and
// only needs to be in the right ballpark for context fitting.
function estimateTokens(s: string): number {
  return Math.ceil(s.length / 4);
}

function roleLabel(role: string): string {
  return role === "assistant" ? "Assistant" : "User";
}

// Exported for testing. `sanitize` is purely lexical: strips
// `<think>`/`Title:`/quotes/punctuation, collapses whitespace, caps length.
export function __sanitizeForTesting(raw: string): string {
  return sanitize(raw);
}

function sanitize(raw: string): string {
  let s = raw;
  // Strip any <think>...</think> blocks the model may have emitted (some
  // reasoning models can't be cleanly told to skip them).
  s = s.replace(/<think[\s\S]*?<\/think>/gi, "");
  // Title-only output: drop everything after the first line.
  s = s.split("\n")[0] ?? "";
  s = s.trim();
  // Strip an optional "Title:" prefix the model often re-emits.
  s = s.replace(/^title\s*[:-]\s*/i, "");
  // Strip surrounding quotes and trailing punctuation the model often adds
  // despite the instruction.
  s = s.replace(/^["'`]+|["'`]+$/g, "");
  s = s.replace(/[.!?]+$/g, "").trim();
  // Collapse whitespace.
  s = s.replace(/\s+/g, " ");
  if (s.length > MAX_TITLE_CHARS) s = s.slice(0, MAX_TITLE_CHARS).trimEnd();
  return s;
}

function extractText(m: { content?: unknown }): string {
  // Domain messages may use either a string content or an array of parts.
  // For title prompting we just need a representative text snippet.
  const c = m.content;
  if (typeof c === "string") return c;
  if (Array.isArray(c)) {
    return c
      .map((part) => {
        if (typeof part === "string") return part;
        if (part && typeof part === "object" && "text" in part) {
          return String((part as { text: unknown }).text ?? "");
        }
        return "";
      })
      .join("\n")
      .trim();
  }
  return "";
}

function strSetting(s: Record<string, unknown>, key: string, def: string): string {
  const v = s[key];
  return typeof v === "string" && v.length > 0 ? v : def;
}
