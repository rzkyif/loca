// ToolFilter.phase2 parser. Accepts comma-separated picks, "none",
// or unparseable responses (fails open by returning the candidate set).
// Phase 2 calls streamChatCompletion through the injected fetch seam, so
// we drive responses with hand-rolled SSE payloads.

import { assertEquals } from "@std/assert";
import { attachHost } from "@loca/core-engine";
import { denoHost } from "../host/deno-host.ts";
import { ToolFilter } from "@loca/core-engine/services/tool-filter";
import type { LlmEndpointConfig } from "@loca/core-engine/services/llm-provider";
import type { ToolDescriptor } from "@loca/shared";

// phase2 logs a warning on its fail-open paths, routed through the engine host
// logger; attach the DenoHost so those lines resolve (this suite is a pure
// phase2 parser test that never touches fs/db).
attachHost(denoHost());

function fixedSse(text: string): typeof fetch {
  return () => {
    const body =
      `data: {"id":"x","object":"chat.completion.chunk","created":1,"model":"t","choices":[{"index":0,"delta":{"role":"assistant","content":${JSON.stringify(
        text,
      )}},"finish_reason":null}]}\n\n` +
      `data: {"id":"x","object":"chat.completion.chunk","created":1,"model":"t","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}\n\n` +
      `data: [DONE]\n\n`;
    return Promise.resolve(
      new Response(body, {
        status: 200,
        headers: { "content-type": "text/event-stream" },
      }),
    );
  };
}

function descriptor(name: string): ToolDescriptor {
  return {
    toolId: `t::${name}`,
    extensionId: "t",
    name,
    description: `desc-${name}`,
  };
}

function endpoint(text: string): LlmEndpointConfig {
  return {
    baseUrl: "https://stub.test/v1",
    apiKey: "sk-test",
    model: "test",
    fetch: fixedSse(text),
  };
}

const CANDIDATES = ["a", "b", "c"].map(descriptor);

Deno.test("phase2: returns the picked subset for a comma-separated response", async () => {
  const filter = new ToolFilter();
  const picked = await filter.phase2("query", CANDIDATES, endpoint("1, 3"), 0);
  assertEquals(
    picked.map((c) => c.name),
    ["a", "c"],
  );
});

Deno.test("phase2: 'none' returns an empty list", async () => {
  const filter = new ToolFilter();
  const picked = await filter.phase2("query", CANDIDATES, endpoint("none"), 0);
  assertEquals(picked, []);
});

Deno.test("phase2: empty response is treated as 'none'", async () => {
  const filter = new ToolFilter();
  const picked = await filter.phase2("q", CANDIDATES, endpoint(""), 0);
  assertEquals(picked, []);
});

Deno.test("phase2: fails open with the candidate set on unparseable response", async () => {
  const filter = new ToolFilter();
  const picked = await filter.phase2("q", CANDIDATES, endpoint("I think tool A is best"), 0);
  assertEquals(
    picked.map((c) => c.name),
    ["a", "b", "c"],
  );
});

Deno.test("phase2: silently drops out-of-range numbers", async () => {
  const filter = new ToolFilter();
  const picked = await filter.phase2("q", CANDIDATES, endpoint("1,99,2"), 0);
  assertEquals(
    picked.map((c) => c.name),
    ["a", "b"],
  );
});

Deno.test("phase2: empty candidates short-circuits to []", async () => {
  const filter = new ToolFilter();
  const picked = await filter.phase2("q", [], endpoint("1,2,3"), 0);
  assertEquals(picked, []);
});
