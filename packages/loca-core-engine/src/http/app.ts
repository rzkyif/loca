// The engine's Hono app: the app-domain slice of /api/v1/* that runs on portable
// services (sessions, settings, memories, llm, storage, voice, remote MCP). The
// shell's transport routes (health, pairing, admin) and its OS-bound routes
// (models, binaries, update, requirements, sidecars, scheduled-prompts,
// greetings, extensions, tools) stay in @loca/core's own app; the shell
// dispatches this app's path prefixes into engine.handleHttp.
//
// Auth is injected: every route group installs clientContextMiddleware with the
// resolver the embedder supplied, so the engine never imports the shell's
// authService.

import { Hono } from "hono";
import { bodyLimit } from "hono/body-limit";
import { corsMiddleware } from "./middleware/cors.ts";
import { sendError } from "./middleware/errors.ts";
import { type ClientResolver, clientContextMiddleware } from "./middleware/auth.ts";
import { settingsRoutes } from "./routes/settings.ts";
import { sessionsRoutes } from "./routes/sessions.ts";
import { llmRoutes } from "./routes/llm.ts";
import { memoriesRoutes } from "./routes/memories.ts";
import { sttRoutes } from "./routes/stt.ts";
import { ttsRoutes } from "./routes/tts.ts";
import { mcpRoutes } from "./routes/mcp.ts";

// Upper bound on a single request body. Generous enough for legitimate
// attachment uploads and STT audio, but it caps an unbounded body so a client
// can't exhaust memory. Mirrors the shell app's MAX_BODY_BYTES. WS frames are
// not affected (the upgrade path never reaches this app).
const MAX_BODY_BYTES = 100 * 1024 * 1024; // 100 MiB

export interface EngineAppOpts {
  // Resolves each request to its authenticated client. Desktop: bearer -> authService;
  // in-process mobile: the fixed local client.
  resolveClient: ClientResolver;
}

// The list of path prefixes this app serves. The shell reads it to decide which
// requests to dispatch into engine.handleHttp; everything else stays in the
// shell app. Grows as routes move into the engine.
export const ENGINE_ROUTE_PREFIXES: readonly string[] = [
  "/api/v1/settings",
  "/api/v1/sessions",
  "/api/v1/llm",
  "/api/v1/memories",
  "/api/v1/stt",
  "/api/v1/tts",
  "/api/v1/mcp",
];

export function buildEngineApp(opts: EngineAppOpts): Hono {
  const app = new Hono();
  app.use("*", corsMiddleware());
  app.use(
    "*",
    bodyLimit({
      maxSize: MAX_BODY_BYTES,
      onError: (c) =>
        c.json({ error: { code: "validation_error", message: "request body too large" } }, 413),
    }),
  );
  // onError catches throws from anywhere, including sub-apps mounted via
  // app.route(); the middleware-as-error-handler pattern would not.
  app.onError((err, c) => sendError(c, err));

  // The shared authed-client middleware every route group installs internally
  // (r.use("*", authed)), so auth runs identically to the shell's per-group
  // bearerMiddleware, just with the resolver injected.
  const authed = clientContextMiddleware(opts.resolveClient);

  app.route("/api/v1/settings", settingsRoutes(authed));
  app.route("/api/v1/sessions", sessionsRoutes(authed));
  app.route("/api/v1/llm", llmRoutes(authed));
  app.route("/api/v1/memories", memoriesRoutes(authed));
  app.route("/api/v1/stt", sttRoutes(authed));
  app.route("/api/v1/tts", ttsRoutes(authed));
  app.route("/api/v1/mcp", mcpRoutes(authed));

  return app;
}
