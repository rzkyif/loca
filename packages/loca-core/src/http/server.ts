// Hono app construction. Wires middleware + route groups.
//
// The chat WS route (/ws/v1) is registered separately by ws/hub.ts because
// Hono's WS upgrade story varies across servers; we use the raw
// Deno.serve hook for upgrades.

import { Hono } from "hono";
import { bodyLimit } from "hono/body-limit";
import type { HealthResponse } from "@loca/shared";
import { CORE_VERSION } from "../config.ts";
import { coreStatus } from "../services/core-status.ts";
import { authService } from "../services/auth.ts";
import { isBehindProxy } from "../services/deployment.ts";
import { corsMiddleware } from "@loca/core-engine/http/middleware/cors";
import { sendError } from "@loca/core-engine/http/middleware/errors";
import { adminRoutes } from "./routes/admin.ts";
import { binariesRoutes } from "./routes/binaries.ts";
import { greetingsRoutes } from "./routes/greetings.ts";
import { modelsRoutes } from "./routes/models.ts";
import { pairingRoutes } from "./routes/pairing.ts";
import { requirementsRoutes } from "./routes/requirements.ts";
import { scheduledPromptsRoutes } from "./routes/scheduled-prompts.ts";
import { sidecarsRoutes } from "./routes/sidecars.ts";
import { storageRoutes } from "./routes/storage.ts";
import { extensionsRoutes } from "./routes/extensions.ts";
import { toolsRoutes } from "./routes/tools.ts";
import { updateRoutes } from "./routes/update.ts";
import { wsRoutes } from "./routes/ws.ts";

// Upper bound on a single request body. Generous enough for legitimate
// attachment uploads and STT audio, but it caps an unbounded body so a client
// (or the unauthenticated /pairing/claim caller) can't exhaust memory. WS
// frames are not affected (the upgrade path bypasses app.fetch).
const MAX_BODY_BYTES = 100 * 1024 * 1024; // 100 MiB

export function buildApp(): Hono {
  const app = new Hono();
  app.use("*", corsMiddleware());
  app.use(
    "*",
    bodyLimit({
      maxSize: MAX_BODY_BYTES,
      onError: (c) =>
        c.json(
          {
            error: {
              code: "validation_error",
              message: "request body too large",
            },
          },
          413,
        ),
    }),
  );
  // app.onError catches throws from anywhere (including sub-apps mounted
  // via app.route()), which the older try/catch middleware did NOT do.
  // Hono 4's middleware-as-error-handler pattern only sees errors from
  // the SAME app's middleware chain; sub-route throws skip past it. The
  // onError hook is the only sanctioned way to centralize error handling
  // when sub-routes are in play.
  app.onError((err, c) => sendError(c, err));

  app.get("/api/v1/health", (c) => {
    const body: HealthResponse = {
      status: "ok",
      version: CORE_VERSION,
      uptimeMs: Math.floor(performance.now()),
      core: coreStatus().snapshot(),
      adminPasswordSet: authService().hasAdminPassword(),
      behindProxy: isBehindProxy(),
    };
    return c.json(body);
  });

  app.route("/api/v1/admin", adminRoutes());
  app.route("/api/v1/pairing", pairingRoutes());
  app.route("/api/v1/models", modelsRoutes());
  app.route("/api/v1/binaries", binariesRoutes());
  app.route("/api/v1/requirements", requirementsRoutes());
  app.route("/api/v1/extensions", extensionsRoutes());
  app.route("/api/v1/tools", toolsRoutes());
  app.route("/api/v1/sidecars", sidecarsRoutes());
  app.route("/api/v1/storage", storageRoutes());
  app.route("/api/v1/scheduled-prompts", scheduledPromptsRoutes());
  app.route("/api/v1/greetings", greetingsRoutes());
  app.route("/api/v1/update", updateRoutes());
  app.route("/api/v1/ws", wsRoutes());

  return app;
}
