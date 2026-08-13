// Test serving harness: a real Deno.serve on an ephemeral port wired exactly
// like main.ts's handler, so tests exercise the true request path - WS upgrades
// to the hub, engine-owned routes (sessions, settings, memories, ...) into
// engine.handleHttp, and everything else to the shell app.

import { buildApp } from "../../src/http/server.ts";
import { engine } from "../../src/host/engine.ts";
import { wsHub } from "../../src/ws/hub.ts";

export interface RunningServer {
  port: number;
  stop(): Promise<void>;
}

export async function startTestServer(): Promise<RunningServer> {
  const app = buildApp();
  const hub = wsHub();
  const engineInst = await engine();
  const abort = new AbortController();
  const server = Deno.serve({ port: 0, hostname: "127.0.0.1", signal: abort.signal }, (req) => {
    const url = new URL(req.url);
    if (url.pathname === "/ws/v1") return hub.handleUpgrade(req);
    if (engineInst.isEngineRoute(url.pathname)) return engineInst.handleHttp(req);
    return app.fetch(req);
  });
  return {
    port: (server.addr as Deno.NetAddr).port,
    async stop() {
      abort.abort();
      await server.finished.catch(() => {});
    },
  };
}
