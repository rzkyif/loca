// endpoint resolver: pure transform over a settings record. Secret
// lookups are mocked through the secrets module (we don't need to stand
// up the encrypted vault for this).

import { assertEquals } from "@std/assert";
import { DEFAULT_SAMPLING } from "@loca/shared";
import { attachHost } from "@loca/core-engine";
import { denoHost } from "../host/deno-host.ts";
import { resolveContextSize, resolveEndpoint } from "@loca/core-engine/services/endpoint-resolver";

// Pure-transform tests: the only side effect is resolveExternalApiKey's vault
// lookup, which must resolve to "no key". Point the engine host at an isolated
// empty temp root so getSecret reads a non-existent vault (undefined) rather than
// the developer's real one, without standing up the full test harness.
Deno.env.set("LOCA_CORE_HOME", Deno.makeTempDirSync({ prefix: "loca-endpoint-test-" }));
attachHost(denoHost());

Deno.test("resolveEndpoint: defaults to local llama-server when provider is unset", async () => {
  const cfg = await resolveEndpoint({});
  assertEquals(cfg.baseUrl, "http://127.0.0.1:7701/v1");
  assertEquals(cfg.apiKey, "sk-local");
  assertEquals(cfg.model, "default");
  // Thinking is off unless the user turns it on.
  assertEquals(cfg.reasoning, "off");
  // Sampling is always populated from the schema defaults (sparse store).
  assertEquals(cfg.temperature, DEFAULT_SAMPLING.temperature);
  assertEquals(cfg.topP, DEFAULT_SAMPLING.topP);
  assertEquals(cfg.topK, DEFAULT_SAMPLING.topK);
  assertEquals(cfg.minP, DEFAULT_SAMPLING.minP);
  assertEquals(cfg.repeatPenalty, DEFAULT_SAMPLING.repeatPenalty);
  // 0/unset budget = unrestricted, so it is omitted.
  assertEquals(cfg.reasoningBudget, undefined);
});

Deno.test("resolveEndpoint: local honors host/port, custom sampling, legacy 'auto' -> 'on'", async () => {
  const cfg = await resolveEndpoint({
    "llm.provider": "local",
    "llm.host": "192.168.1.10",
    "llm.port": "8080",
    "llm.reasoning": "auto",
    "llm.temperature": 0.3,
    "llm.topK": 40,
    "llm.reasoningBudget": 512,
  });
  assertEquals(cfg.baseUrl, "http://192.168.1.10:8080/v1");
  assertEquals(cfg.reasoning, "on");
  assertEquals(cfg.temperature, 0.3);
  assertEquals(cfg.topK, 40);
  assertEquals(cfg.reasoningBudget, 512);
});

Deno.test("resolveEndpoint: external provider routes to llm.external.* fields", async () => {
  const cfg = await resolveEndpoint({
    "llm.provider": "external",
    "llm.external.baseUrl": "https://api.openai.com/v1",
    "llm.external.model": "gpt-4o-mini",
    "llm.external.apiKey": "sk-real",
    "llm.reasoning": "on",
  });
  assertEquals(cfg.baseUrl, "https://api.openai.com/v1");
  assertEquals(cfg.apiKey, "sk-real");
  assertEquals(cfg.model, "gpt-4o-mini");
  assertEquals(cfg.reasoning, "on");
  // OpenAI-style samplers carry over; llama.cpp-only ones and the token budget
  // do not (the API rejects them).
  assertEquals(cfg.temperature, DEFAULT_SAMPLING.temperature);
  assertEquals(cfg.topP, DEFAULT_SAMPLING.topP);
  assertEquals(cfg.topK, undefined);
  assertEquals(cfg.minP, undefined);
  assertEquals(cfg.repeatPenalty, undefined);
  assertEquals(cfg.reasoningBudget, undefined);
  // External reasoning carries an effort level, defaulting to "high".
  assertEquals(cfg.reasoningEffort, "high");
});

Deno.test("resolveEndpoint: external provider forwards llm.reasoningEffort", async () => {
  const cfg = await resolveEndpoint({
    "llm.provider": "external",
    "llm.external.baseUrl": "https://api.openai.com/v1",
    "llm.external.model": "gpt-4o-mini",
    "llm.external.apiKey": "sk-real",
    "llm.reasoning": "on",
    "llm.reasoningEffort": "low",
  });
  assertEquals(cfg.reasoningEffort, "low");
});

Deno.test("resolveEndpoint: external provider without an API key resolves an empty key (chat path reports it)", async () => {
  // resolveEndpoint stays total: the chat orchestrator turns an empty apiKey
  // into a friendly "provider isn't set up" message rather than throwing here.
  const cfg = await resolveEndpoint({
    "llm.provider": "external",
    "llm.external.baseUrl": "https://api.openai.com/v1",
    "llm.external.model": "gpt-4o-mini",
  });
  assertEquals(cfg.apiKey, "");
});

Deno.test("resolveEndpoint: external localhost gateway needs no API key", async () => {
  const cfg = await resolveEndpoint({
    "llm.provider": "external",
    "llm.external.baseUrl": "http://127.0.0.1:1234/v1",
    "llm.external.model": "local-model",
  });
  assertEquals(cfg.baseUrl, "http://127.0.0.1:1234/v1");
  // A keyless loopback gateway gets a harmless placeholder so the SDK builds.
  assertEquals(cfg.apiKey, "sk-noauth");
});

Deno.test("resolveEndpoint: route=secondary reads dualModel.* fields", async () => {
  const cfg = await resolveEndpoint(
    {
      "dualModel.external.baseUrl": "https://api2.example.com/v1",
      "dualModel.external.model": "fast-model",
      "dualModel.external.apiKey": "sk-2",
    },
    "secondary",
  );
  assertEquals(cfg.baseUrl, "https://api2.example.com/v1");
  assertEquals(cfg.model, "fast-model");
  assertEquals(cfg.apiKey, "sk-2");
  // Secondary route never forwards reasoning or sampling.
  assertEquals(cfg.reasoning, "off");
  assertEquals(cfg.temperature, undefined);
  assertEquals(cfg.topP, undefined);
  assertEquals(cfg.topK, undefined);
});

Deno.test("resolveContextSize: returns local default 4096 when nothing is set", () => {
  assertEquals(resolveContextSize({}), 4096);
});

Deno.test("resolveContextSize: returns 128000 for external on default route", () => {
  assertEquals(resolveContextSize({ "llm.provider": "external" }), 128000);
});

Deno.test("resolveContextSize: secondary route reads dualModel.external.contextSize", () => {
  assertEquals(resolveContextSize({ "dualModel.external.contextSize": 64000 }, "secondary"), 64000);
});

Deno.test("resolveContextSize: coerces numeric string", () => {
  assertEquals(resolveContextSize({ "llm.contextSize": "8192" }), 8192);
});

Deno.test("resolveContextSize: ignores non-finite values and falls back to default", () => {
  assertEquals(resolveContextSize({ "llm.contextSize": "nope" }), 4096);
  assertEquals(resolveContextSize({ "llm.contextSize": Number.NaN }), 4096);
});
