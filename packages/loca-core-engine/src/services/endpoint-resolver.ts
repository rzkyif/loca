// Resolves an LlmEndpointConfig from the current core settings.
//
// All settings keys read here are defined in the shared schema
// (`@loca/shared/src/domain/settings/groups/{llm,dualModel}.ts`):
//   - llm.provider              : "local" | "external"
//   - llm.host, llm.port        : local llama-server bind addr (default 127.0.0.1:7701)
//   - llm.external.baseUrl/apiKey/model
//   - llm.external.contextSize
//   - llm.contextSize           : local context window
//   - llm.reasoning             : "off" | "on"
//   - llm.reasoningEffort       : "low" | "medium" | "high" (external only)
//   - llm.temperature, llm.topP, llm.presencePenalty : sampling, sent to both
//   - llm.topK, llm.minP, llm.repeatPenalty, llm.dryMultiplier, llm.samplers, llm.reasoningBudget : local only
//   - dualModel.external.baseUrl/apiKey/model        (route="secondary")
//   - dualModel.external.contextSize
//
// External API keys are pulled from the encrypted secrets vault first; a
// plain-text value placed directly in settings.json is honored as a
// lower-precedence fallback (e.g. headless/config-file setups). The client
// routes keys to the vault and core redacts them from GET /settings, so the
// vault is the authoritative source.

import type { LlmEndpointConfig } from "./llm-provider.ts";
import { DEFAULT_SAMPLING } from "@loca/shared";
import { resolveExternalApiKey } from "./external-endpoint.ts";
import { numSetting, strSetting } from "./settings-access.ts";
import { host } from "../platform/runtime.ts";

export type LlmRoute = "default" | "secondary";

export async function resolveEndpoint(
  settings: Record<string, unknown>,
  route: LlmRoute = "default",
): Promise<LlmEndpointConfig> {
  if (route === "secondary") {
    const baseUrl = strSetting(settings, "dualModel.external.baseUrl", "");
    const apiKey = await resolveExternalApiKey(settings, "dualModel.external.apiKey", baseUrl);
    return {
      baseUrl,
      apiKey,
      model: strSetting(settings, "dualModel.external.model", ""),
      // The secondary route is a fast classifier/responder for simple turns:
      // thinking is forced off and sampling overrides are dropped by design.
      reasoning: "off",
    };
  }
  const provider = strSetting(settings, "llm.provider", "local") as "local" | "external";
  // Default mirrors the shared schema (settings.json is sparse): thinking is off
  // unless the user turns it on. A legacy "auto" reads as "on" (the two are
  // equivalent for thinking; "auto" was dropped from the toggle).
  const reasoning = strSetting(settings, "llm.reasoning", "off") === "off" ? "off" : "on";
  // OpenAI-style samplers apply to both providers; read with the schema defaults
  // (sparse store) so a value is always sent.
  const temperature = numSetting(settings, "llm.temperature", DEFAULT_SAMPLING.temperature);
  const topP = numSetting(settings, "llm.topP", DEFAULT_SAMPLING.topP);
  // presence_penalty is provider-agnostic (0 = off, its default); dry_multiplier
  // is llama.cpp-only and read in the local branch below.
  const presencePenalty = numSetting(settings, "llm.presencePenalty", 0) || undefined;
  if (provider === "external") {
    const baseUrl = strSetting(settings, "llm.external.baseUrl", "");
    const apiKey = await resolveExternalApiKey(settings, "llm.external.apiKey", baseUrl);
    // OpenAI-style endpoints take an effort level rather than a token budget.
    const reasoningEffort = strSetting(settings, "llm.reasoningEffort", "high") as
      | "minimal"
      | "low"
      | "medium"
      | "high";
    return {
      baseUrl,
      apiKey,
      model: strSetting(settings, "llm.external.model", ""),
      reasoning,
      reasoningEffort,
      temperature,
      topP,
      presencePenalty,
    };
  }
  const llmHost = strSetting(settings, "llm.host", "127.0.0.1");
  const port = strSetting(
    settings,
    "llm.port",
    String(host().localEndpoints?.llmDefaultPort() ?? 7701),
  );
  return {
    baseUrl: `http://${llmHost}:${port}/v1`,
    apiKey: "sk-local",
    model: "default",
    // The core spawned this one, so it is what the scheduler's semaphore is
    // sized against. True regardless of `llm.host`: a LAN bind address does not
    // stop it being the process we run with a single slot.
    isLocalProvider: true,
    reasoning,
    temperature,
    topP,
    // llama.cpp-only samplers, plus the per-turn thinking budget (0/unset =
    // unrestricted, so omit it). Presets write all of these on model select.
    topK: numSetting(settings, "llm.topK", DEFAULT_SAMPLING.topK),
    minP: numSetting(settings, "llm.minP", DEFAULT_SAMPLING.minP),
    repeatPenalty: numSetting(settings, "llm.repeatPenalty", DEFAULT_SAMPLING.repeatPenalty),
    // DRY is on by default (small local models loop without it); 0 turns it off.
    dryMultiplier:
      numSetting(settings, "llm.dryMultiplier", DEFAULT_SAMPLING.dryMultiplier ?? 0) || undefined,
    presencePenalty,
    // Optional sampler-chain order, `;`-joined by presets from a catalog model.
    // Empty = the server's default chain.
    samplers: splitSamplers(strSetting(settings, "llm.samplers", "")),
    reasoningBudget: numSetting(settings, "llm.reasoningBudget", 0) || undefined,
  };
}

/** Parse the `;`-joined `llm.samplers` value into a sampler-name list, or
 *  undefined when unset (so the request omits it and the server uses its
 *  default chain). */
function splitSamplers(value: string): string[] | undefined {
  const names = value
    .split(";")
    .map((s) => s.trim())
    .filter(Boolean);
  return names.length > 0 ? names : undefined;
}

/** Context window size for the active provider on the given route, in
 *  tokens. Used by token-budget math (tool filter, session usage). Returns
 *  the user-configured value or a sensible default. */
export function resolveContextSize(
  settings: Record<string, unknown>,
  route: LlmRoute = "default",
): number {
  if (route === "secondary") {
    return numSetting(settings, "dualModel.external.contextSize", 128000);
  }
  const provider = strSetting(settings, "llm.provider", "local");
  if (provider === "external") {
    return numSetting(settings, "llm.external.contextSize", 128000);
  }
  return numSetting(settings, "llm.contextSize", 4096);
}
