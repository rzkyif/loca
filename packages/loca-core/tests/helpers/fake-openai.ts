// Build a fetch implementation that serves a recorded SSE file from
// `tests/fixtures/openai/`. Used by services/llm-provider.ts and
// services/llm-scheduler.ts tests to drive deterministic stream parsing
// without touching the network.

import { fromFileUrl, join } from "@std/path";

const FIXTURES_DIR = fromFileUrl(new URL("../fixtures/openai/", import.meta.url));

/**
 * Returns a fetch that responds to any URL with the contents of the named
 * SSE fixture. Headers + status are minimal but enough for the OpenAI SDK
 * to parse the stream.
 */
export function fakeOpenAIFetch(fixtureName: string): typeof fetch {
  return async (_input: RequestInfo | URL, _init?: RequestInit): Promise<Response> => {
    const path = join(FIXTURES_DIR, fixtureName);
    const text = await Deno.readTextFile(path);
    return new Response(text, {
      status: 200,
      headers: {
        "content-type": "text/event-stream",
        "cache-control": "no-cache",
      },
    });
  };
}
