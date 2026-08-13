// Single-shot LLM utility endpoints. These are non-streaming wrappers used
// by the client for short post-processing tasks (STT autocorrect, STT
// merge) so that no LLM traffic ever bypasses core. They use the same
// provider resolution + secrets vault as the streaming chat path.
//
// Routes:
//   POST /api/v1/llm/autocorrect  { text }                  → { text }
//   POST /api/v1/llm/merge        { existing, next }        → { text }
//
// Prompts are user-overridable via core settings:
//   prompts.autocorrectPrompt        (default: DEFAULT_AUTOCORRECT_PROMPT)
//   prompts.mergeTranscriptionPrompt (default: DEFAULT_MERGE_TRANSCRIPTION_PROMPT)

import { Hono, type MiddlewareHandler } from "hono";
import { DEFAULT_AUTOCORRECT_PROMPT, DEFAULT_MERGE_TRANSCRIPTION_PROMPT } from "@loca/shared";
import { loadCoreSettings } from "../../services/core-settings.ts";
import { resolveEndpoint } from "../../services/endpoint-resolver.ts";
import { singleShot } from "../../services/single-shot.ts";
import { strSetting } from "../../services/settings-access.ts";
import { thinkingBudget } from "../../services/thinking-budget.ts";
import { AppError } from "../../platform/errors.ts";
import { readJson } from "../body.ts";
import { getLogger } from "../../platform/log.ts";

const log = getLogger("llm-utility");

export function llmRoutes(authed: MiddlewareHandler): Hono {
  const r = new Hono();
  r.use("*", authed);

  r.post("/autocorrect", async (c) => {
    const body = (await readJson(c)) as { text?: unknown };
    if (typeof body.text !== "string" || body.text.length === 0) {
      throw new AppError("validation_error", "body must be { text: string }");
    }
    const settings = await loadCoreSettings();
    const systemPrompt =
      strSetting(settings, "prompts.autocorrectPrompt", "") || DEFAULT_AUTOCORRECT_PROMPT;
    const endpoint = await resolveEndpoint(settings, "default");
    // Autocorrect sits on the dictation hot path, where thinking adds seconds
    // of latency for little gain, so the Cleanup Thinking Budget defaults to 0
    // (off). A positive budget opts in and caps the `<think>` block.
    const budget = thinkingBudget(settings, "prompts.autocorrectThinkingBudget");
    const startedAt = Date.now();
    log.info(`autocorrect starting (in ${body.text.length} chars, model ${endpoint.model})`);
    const text = await singleShot({
      systemPrompt,
      userMessage: body.text,
      endpoint,
      overrides: { temperature: 0.2, reasoningBudget: budget },
    });
    log.info(
      `autocorrect done in ${
        Date.now() - startedAt
      }ms (in ${body.text.length} chars, out ${text.length} chars)`,
    );
    return c.json({ text });
  });

  r.post("/merge", async (c) => {
    const body = (await readJson(c)) as { existing?: unknown; next?: unknown };
    if (
      typeof body.existing !== "string" ||
      typeof body.next !== "string" ||
      body.next.length === 0
    ) {
      throw new AppError("validation_error", "body must be { existing: string, next: string }");
    }
    const settings = await loadCoreSettings();
    const systemPrompt =
      strSetting(settings, "prompts.mergeTranscriptionPrompt", "") ||
      DEFAULT_MERGE_TRANSCRIPTION_PROMPT;
    const endpoint = await resolveEndpoint(settings, "default");
    // Same hot path as autocorrect: thinking off by default via the Merge
    // Thinking Budget setting (0), opt-in with a positive budget.
    const budget = thinkingBudget(settings, "prompts.mergeTranscriptionThinkingBudget");
    const userMessage = `<existing>\n${body.existing}\n</existing>\n<new>\n${body.next}\n</new>`;
    const startedAt = Date.now();
    log.info(`merge starting (in ${userMessage.length} chars, model ${endpoint.model})`);
    const text = await singleShot({
      systemPrompt,
      userMessage,
      endpoint,
      overrides: { temperature: 0.2, reasoningBudget: budget },
    });
    log.info(
      `merge done in ${
        Date.now() - startedAt
      }ms (in ${userMessage.length} chars, out ${text.length} chars)`,
    );
    return c.json({ text });
  });

  return r;
}
