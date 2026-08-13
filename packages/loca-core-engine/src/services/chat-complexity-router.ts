// Dual-model routing: a single-shot LLM classifier that labels the user's
// request "simple" or "complex" and routes complex requests to the configured
// secondary endpoint. Ambiguous or empty replies fall back to the default model.

import type OpenAI from "openai";
import { DEFAULT_COMPLEXITY_DETECTION_PROMPT } from "@loca/shared";
import { strSetting } from "./settings-access.ts";
import { streamChatCompletion } from "./llm-provider.ts";
import { resolveEndpoint } from "./endpoint-resolver.ts";
import { thinkingBudget } from "./thinking-budget.ts";

export async function classifyComplexity(
  settings: Record<string, unknown>,
  userMessage: string,
  signal?: AbortSignal,
): Promise<"default" | "secondary"> {
  const systemPrompt = strSetting(
    settings,
    "prompts.complexityDetectionPrompt",
    DEFAULT_COMPLEXITY_DETECTION_PROMPT,
  );
  const endpoint = await resolveEndpoint(settings, "default");
  // The classifier must emit one word ("simple"/"complex"), so thinking is off
  // by default (Complexity Thinking Budget = 0); a positive budget opts in and
  // is added on top of the one-word answer allowance.
  const budget = thinkingBudget(settings, "prompts.complexityDetectionThinkingBudget");
  const messages: OpenAI.Chat.Completions.ChatCompletionMessageParam[] = [
    { role: "system", content: systemPrompt },
    { role: "user", content: userMessage },
  ];
  let response = "";
  for await (const delta of streamChatCompletion({
    endpoint,
    messages,
    overrides: {
      temperature: 0,
      maxTokens: 16 + budget,
      reasoningBudget: budget,
    },
    signal,
  })) {
    if (delta.contentDelta) response += delta.contentDelta;
  }
  const text = response.toLowerCase();
  if (text.includes("complex") && !text.includes("simple")) return "secondary";
  return "default";
}
