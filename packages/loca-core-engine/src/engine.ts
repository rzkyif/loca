// The engine's public entry and the frozen in-process seam.
//
// The portable service graph (chat, sessions, settings, secrets, memories,
// embeddings/relevance, LLM, tools, external STT/TTS, titles) is already fully
// extracted into ./services/* and runs today: the Deno shell (`@loca/core`)
// serves it via Deno.serve + its Hono `buildApp`, and delivery flows through the
// shared engine FrameBus (frameBus()). So on desktop the engine's logic is live;
// what stays a designed-but-unwired seam is the *in-process transport* below.
//
// `handleHttp` / `connect` are the frozen in-process seam, and BOTH are already
// live on desktop: `handleHttp` builds a Hono app from the engine's OWN reduced
// route set and serves it (app.fetch) for the routes `isEngineRoute` claims,
// which the Deno shell's `main.ts` dispatches straight to it; `connect` runs
// against the real FrameBus (the same registry the WS hub uses, plus tests). That
// route set is the engine's app-domain slice only - NOT the desktop shell's
// shell-coupled routes (sidecars, models, worker-exec, downloads, update), which
// legitimately stay in @loca/core. What remains for the mobile pass is hosting
// this same interface INSIDE the webview (no Deno.serve, no network) via a
// transport that calls handleHttp/connect; the interface needs no engine rework.

import type { Host } from "./host.ts";
import { frameBus } from "./frame-bus.ts";
import { buildEngineApp, ENGINE_ROUTE_PREFIXES } from "./http/app.ts";
import type { ClientResolver } from "./http/middleware/auth.ts";

// A single in-process client connection to the engine, mirroring the client's
// NetSocket contract. Frames are the JSON-encoded @loca/shared frame unions.
export interface EngineConnection {
  // Client -> engine (chat.start, tool.* responses, ...).
  send(frameJson: string): void;
  // Engine -> client frames; returns a detach.
  subscribe(cb: (frameJson: string) => void): () => void;
  close(): void;
}

export interface EngineInitOpts {
  // Resolves each HTTP request to its authenticated client. The engine does not
  // own auth: the desktop shell injects a bearer -> authService resolver; a
  // future in-process mobile transport injects one that returns its fixed local
  // client. See http/middleware/auth.ts.
  resolveClient: ClientResolver;
}

// The engine, once initialized.
export interface EngineInstance {
  // Handle one HTTP request against the engine's app-domain slice of /api/v1/*
  // (the routes ENGINE_ROUTE_PREFIXES covers). The shell forwards only matching
  // requests here; the raw Request carries the bearer the injected resolver reads.
  handleHttp(req: Request): Promise<Response>;
  // True when `pathname` belongs to the engine's app (so the shell knows to
  // dispatch it here instead of into its own app).
  isEngineRoute(pathname: string): boolean;
  // Register an already-authenticated client for WS-equivalent frame exchange
  // over the shared FrameBus (the same registry the Deno shell's WS hub uses).
  connect(clientId: string): EngineConnection;
  shutdown(): Promise<void>;
}

export function init(host: Host, opts: EngineInitOpts): Promise<EngineInstance> {
  host.log("info", "engine", "engine init");
  const app = buildEngineApp({ resolveClient: opts.resolveClient });
  const bus = frameBus();

  const instance: EngineInstance = {
    handleHttp(req: Request): Promise<Response> {
      return Promise.resolve(app.fetch(req));
    },
    isEngineRoute(pathname: string): boolean {
      return ENGINE_ROUTE_PREFIXES.some((p) => pathname === p || pathname.startsWith(`${p}/`));
    },
    connect(clientId: string): EngineConnection {
      return bus.registerConnection(clientId);
    },
    // The FrameBus is the process-wide singleton the shell's WS hub also drives,
    // so tearing it down here would drop live shell sockets. Connection teardown
    // is the shell's job (wsHub.shutdown); engine shutdown is a no-op today.
    shutdown(): Promise<void> {
      return Promise.resolve();
    },
  };
  return Promise.resolve(instance);
}
