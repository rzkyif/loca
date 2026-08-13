// Non-streaming wrapper over streamChatCompletion. Returns the concatenated
// assistant content. Used by utility endpoints (autocorrect, merge,
// title-gen) that don't need delta-by-delta streaming.
//
// We don't go through llm-scheduler here on purpose. These calls are
// short, parallel-safe, and shouldn't compete with chat turns for the
// `--parallel N` slots on llama-server. If that turns out to be wrong
// for any specific caller, it can switch to the scheduler explicitly.

import type OpenAI from "openai";
import { type LlmRequest, streamChatCompletion } from "./llm-provider.ts";

export interface SingleShotOptions {
  systemPrompt: string;
  userMessage: string;
  endpoint: LlmRequest["endpoint"];
  overrides?: LlmRequest["overrides"];
  signal?: AbortSignal;
}

export async function singleShot(opts: SingleShotOptions): Promise<string> {
  const messages: OpenAI.Chat.Completions.ChatCompletionMessageParam[] = [
    { role: "system", content: opts.systemPrompt },
    { role: "user", content: opts.userMessage },
  ];
  let text = "";
  for await (const delta of streamChatCompletion({
    endpoint: opts.endpoint,
    messages,
    overrides: opts.overrides,
    signal: opts.signal,
  })) {
    if (delta.contentDelta) text += delta.contentDelta;
  }
  return text.trim();
}
