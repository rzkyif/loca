// MCP server management: CRUD for configured servers, per-tool / per-prompt
// enablement, and the live prompt/resource listings the client's "/" and "@"
// autocomplete consume. The registry, connection manager, and OAuth flow live
// behind host.mcp (McpAdminHost); connecting/disconnecting is reconciled after
// any change that affects which servers are enabled.

import { Hono, type MiddlewareHandler } from "hono";
import { z } from "zod";
import type { McpAdminHost } from "../../host.ts";
import { host } from "../../platform/runtime.ts";
import { frameBus } from "../../frame-bus.ts";
import { deleteSecret, setSecret } from "../../services/secrets.ts";
import { AppError } from "../../platform/errors.ts";
import { parseBody, readJson } from "../body.ts";

const serverBody = z
  .object({
    name: z.string().min(1),
    kind: z.enum(["stdio", "remote"]),
    command: z.string().optional(),
    args: z.array(z.string()).optional(),
    runtime: z.enum(["custom", "deno"]).optional(),
    denoAllowAll: z.boolean().optional(),
    denoPermissions: z.array(z.string()).optional(),
    url: z.string().optional(),
    remoteAuth: z.enum(["none", "bearer", "oauth"]).optional(),
    enabled: z.boolean().optional(),
    // Remote bearer token: write-only. Omitted = leave as-is; "" = clear; a
    // value = store in the vault. Never echoed back (the projection only
    // exposes `hasAuth`).
    authToken: z.string().optional(),
  })
  .strict();

// The MCP admin provider, or a clear error when the host doesn't offer MCP.
function mcp(): McpAdminHost {
  const m = host().mcp;
  if (!m) throw new AppError("server_unavailable", "MCP is not available on this host");
  return m;
}

/** Persist a server's bearer token to the vault. Returns the hasAuth flag to
 *  store on the row. `undefined` token leaves the existing secret untouched. */
async function applyAuthToken(id: string, token: string | undefined): Promise<boolean | undefined> {
  if (token === undefined) return undefined;
  if (token === "") {
    await deleteSecret(mcp().authSecretName(id));
    return false;
  }
  await setSecret(mcp().authSecretName(id), token);
  return true;
}

/** Reconnect/disconnect to match the current enabled set, then repaint. */
async function resync(): Promise<void> {
  await mcp().resync();
  frameBus().broadcastAll({ kind: "mcp.snapshot" });
}

export function mcpRoutes(authed: MiddlewareHandler): Hono {
  const r = new Hono();
  r.use("*", authed);

  r.get("/", (c) => c.json({ servers: mcp().list() }));
  r.get("/prompts", (c) => c.json({ prompts: mcp().listPrompts() }));
  r.get("/resources", (c) => c.json({ resources: mcp().listResources() }));

  r.post("/", async (c) => {
    const { authToken, ...input } = parseBody(serverBody, await readJson(c));
    const server = mcp().create(input);
    const hasAuth = await applyAuthToken(server.id, authToken);
    if (hasAuth !== undefined) {
      mcp().update(server.id, { hasAuth });
    }
    await resync();
    return c.json(mcp().get(server.id), 201);
  });

  r.get("/:id", (c) => c.json(mcp().get(c.req.param("id"))));

  r.patch("/:id", async (c) => {
    const id = c.req.param("id");
    const { authToken, ...data } = parseBody(serverBody.partial(), await readJson(c));
    const hasAuth = await applyAuthToken(id, authToken);
    mcp().update(id, { ...data, hasAuth });
    // A change to connection config (transport, command/args, url, auth) must
    // drop the live session so resync reconnects with the new settings; a rename
    // alone (no connection fields) leaves the connection untouched.
    if (
      data.kind !== undefined ||
      data.command !== undefined ||
      data.args !== undefined ||
      data.runtime !== undefined ||
      data.denoAllowAll !== undefined ||
      data.denoPermissions !== undefined ||
      data.url !== undefined ||
      data.remoteAuth !== undefined ||
      hasAuth !== undefined
    ) {
      await mcp().disconnect(id);
    }
    await resync();
    return c.json(mcp().get(id));
  });

  r.delete("/:id", async (c) => {
    const id = c.req.param("id");
    mcp().cancelOAuth(id);
    mcp().delete(id);
    await deleteSecret(mcp().authSecretName(id)).catch(() => {});
    await deleteSecret(mcp().oauthSecretName(id)).catch(() => {});
    await resync();
    return c.json({ ok: true });
  });

  // Force a reconnect attempt (e.g. after fixing a misconfigured server).
  r.post("/:id/reconnect", async (c) => {
    await mcp().disconnect(c.req.param("id"));
    await resync();
    return c.json(mcp().get(c.req.param("id")));
  });

  // Begin the OAuth 2.1 sign-in for a remote server: returns the authorization
  // URL the client opens in a browser. The token exchange completes when the
  // browser redirects to the loopback listener, which flips the server to
  // authorized and reconnects. A null URL means stored tokens already worked.
  r.post("/:id/oauth/start", async (c) => {
    const id = c.req.param("id");
    const server = mcp().get(id);
    if (server.kind !== "remote" || !server.url) {
      throw new AppError("validation_error", "OAuth is only for a remote server with a url");
    }
    const { authorizationUrl } = await mcp().startOAuth(id, server.url, (ok) => {
      if (!ok) return;
      mcp().update(id, { remoteAuth: "oauth", oauthAuthorized: true });
      void resync();
    });
    return c.json({ authorizationUrl });
  });

  r.post("/:id/tools/:tool/:action", (c) => {
    const action = c.req.param("action");
    const enabled = action === "enable";
    if (!enabled && action !== "disable") {
      throw new AppError("validation_error", "bad action");
    }
    const server = mcp().setToolEnabled(c.req.param("id"), c.req.param("tool"), enabled);
    frameBus().broadcastAll({ kind: "mcp.snapshot" });
    return c.json(server);
  });

  // Resolve a prompt's messages into one instruction string with the given
  // arguments. The client calls this at send time to fold a `/prompt` reference
  // into the turn's system prompt (a live server round-trip).
  r.post("/:id/prompts/:prompt/resolve", async (c) => {
    const args = parseBody(
      z.object({ arguments: z.record(z.string(), z.string()).optional() }).strict(),
      await readJson(c),
    );
    const text = await mcp().resolvePrompt(
      c.req.param("id"),
      c.req.param("prompt"),
      args.arguments ?? {},
    );
    return c.json({ text });
  });

  r.post("/:id/tools/:tool/always-available/:action", (c) => {
    const action = c.req.param("action");
    const alwaysAvailable = action === "enable";
    if (!alwaysAvailable && action !== "disable") {
      throw new AppError("validation_error", "bad action");
    }
    const server = mcp().setToolAlwaysAvailable(
      c.req.param("id"),
      c.req.param("tool"),
      alwaysAvailable,
    );
    frameBus().broadcastAll({ kind: "mcp.snapshot" });
    return c.json(server);
  });

  r.post("/:id/prompts/:prompt/:action", (c) => {
    const action = c.req.param("action");
    const enabled = action === "enable";
    if (!enabled && action !== "disable") {
      throw new AppError("validation_error", "bad action");
    }
    const server = mcp().setPromptEnabled(c.req.param("id"), c.req.param("prompt"), enabled);
    frameBus().broadcastAll({ kind: "mcp.snapshot" });
    return c.json(server);
  });

  return r;
}
