// In-process transport parity: the engine exposes two ways to receive a client's
// frames - the Deno shell's real WebSocket (registered with the FrameBus by the
// WS hub) and `engine.connect(clientId)` (the in-process equivalent a future
// mobile client drives). Both are FrameBus connections for the same client, so a
// chat turn must deliver the byte-identical frame sequence to each. This guards
// that the in-process seam the mobile pass will wire behaves exactly like the
// networked path it replaces.

import { assertEquals } from "@std/assert";
import { pairClient } from "../../tests/helpers/pairing.ts";
import { startTestServer } from "../../tests/helpers/serve.ts";
import { dialWs } from "../../tests/helpers/ws.ts";
import { engine } from "./engine.ts";
import { patchCoreSettings } from "@loca/core-engine/services/core-settings";
import { setupTestEnv } from "../../tests/helpers/db.ts";
import { fakeOpenAIFetch } from "../../tests/helpers/fake-openai.ts";

type Frame = Record<string, unknown> & { kind: string };

Deno.test({
  name: "in-process connect() gets the same frame sequence as the WS path for a chat turn",
  sanitizeOps: false,
  sanitizeResources: false,
  async fn() {
    const env = await setupTestEnv();
    const server = await startTestServer();
    const openai = fakeOpenAIFetch("streaming-basic.sse");
    const originalFetch = globalThis.fetch;
    globalThis.fetch = ((input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
      const url =
        typeof input === "string"
          ? input
          : input instanceof URL
            ? input.toString()
            : (input as Request).url;
      if (url.startsWith("https://stub.test/")) return openai(input, init);
      return originalFetch(input, init);
    }) as typeof fetch;

    let ws: WebSocket | undefined;
    let detachInproc: (() => void) | undefined;
    try {
      await patchCoreSettings({
        "llm.provider": "external",
        "llm.external.baseUrl": "https://stub.test/v1",
        "llm.external.apiKey": "sk-test",
        "llm.external.model": "test-model",
      });
      const { token, clientId } = await pairClient("parity-t", "127.0.0.1");
      const api = (path: string, init?: RequestInit) =>
        fetch(`http://127.0.0.1:${server.port}/api/v1${path}`, {
          ...init,
          headers: {
            authorization: `Bearer ${token}`,
            "content-type": "application/json",
            ...init?.headers,
          },
        });

      // Create the session + user message through the HTTP path.
      const created = await api("/sessions", {
        method: "POST",
        body: JSON.stringify({ title: "" }),
      });
      const session = await created.json();
      await api(`/sessions/${session.id}/messages`, {
        method: "POST",
        body: JSON.stringify({ role: "user", content: "hi" }),
      });

      // In-process connection for the same client, collecting outbound frames.
      const inprocFrames: Frame[] = [];
      const inproc = (await engine()).connect(clientId);
      detachInproc = inproc.subscribe((json) => inprocFrames.push(JSON.parse(json) as Frame));

      // Real WebSocket for the same client, collecting frames until chat.done.
      const wsFrames: Frame[] = [];
      ws = await dialWs(server.port, token);
      await new Promise<void>((resolve, reject) => {
        ws!.addEventListener("open", () => resolve(), { once: true });
        ws!.addEventListener("error", reject, { once: true });
      });
      const socket = ws;
      const done = new Promise<void>((resolve) => {
        socket.addEventListener("message", (ev) => {
          if (typeof ev.data !== "string") return;
          const f = JSON.parse(ev.data) as Frame;
          wsFrames.push(f);
          if (f.kind === "chat.done") resolve();
        });
      });

      socket.send(
        JSON.stringify({
          kind: "chat.start",
          streamId: "s-1",
          sessionId: session.id,
          route: "default",
        }),
      );
      await done;
      // Let any trailing in-process delivery settle (same broadcast, delivered
      // synchronously, but the WS frames arrive across the event loop).
      await new Promise((r) => setTimeout(r, 50));

      // The WS gets a core.status seed on open that the in-process connect() does
      // not; drop it, then the chat frame sequences must be identical.
      const wsChat = wsFrames.filter((f) => f.kind !== "core.status");
      const inChat = inprocFrames.filter((f) => f.kind !== "core.status");
      assertEquals(
        inChat.map((f) => f.kind),
        wsChat.map((f) => f.kind),
      );
      assertEquals(inChat, wsChat);
      // And it must have been a real turn, not two empty streams.
      assertEquals(
        wsChat.some((f) => f.kind === "chat.done"),
        true,
      );
    } finally {
      detachInproc?.();
      try {
        ws?.close();
      } catch {
        /* already closing */
      }
      globalThis.fetch = originalFetch;
      await server.stop();
      await env.teardown();
    }
  },
});
