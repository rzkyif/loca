import { Hono, type MiddlewareHandler } from "hono";
import {
  isSecretSettingKey,
  isValidSettingKey,
  settingKeyDestination,
  validateSettingsPatch,
} from "@loca/shared";
import {
  loadEffective,
  patchClientSettings,
  patchCoreSettings,
} from "../../services/core-settings.ts";
import { deleteSecret, listSecretNames, setSecret } from "../../services/secrets.ts";
import { AppError } from "../../platform/errors.ts";
import { readJson } from "../body.ts";
import { requireClient } from "../middleware/auth.ts";

// Sanitize a settings record before it crosses the API: keep only known
// schema keys the core stores - the shared `core` keys plus this client's
// `client-on-core` overlay keys - and never secret-typed keys (API keys etc.
// live in the encrypted vault and are never returned; the client learns which
// are configured from GET /settings/secrets and renders a placeholder). A stray
// value placed directly in settings.json must not leak to clients.
function sanitizeSettings(settings: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(settings)) {
    const dest = settingKeyDestination(k);
    if (
      isValidSettingKey(k) &&
      (dest === "core" || dest === "client-on-core") &&
      !isSecretSettingKey(k)
    ) {
      out[k] = v;
    }
  }
  return out;
}

export function settingsRoutes(authed: MiddlewareHandler): Hono {
  const r = new Hono();
  r.use("*", authed);

  // The effective view for this client: shared core settings overlaid with the
  // client's own per-client (client-on-core) overrides.
  r.get("/", async (c) => c.json(sanitizeSettings(await loadEffective(requireClient(c).id))));

  r.patch("/", async (c) => {
    const me = requireClient(c);
    const body = await readJson(c);
    if (!body || typeof body !== "object" || Array.isArray(body)) {
      throw new AppError("validation_error", "object body required");
    }
    const patch = body as Record<string, unknown>;
    // Strict: only known schema keys the core stores, with well-typed values,
    // are accepted (secrets belong in the vault, client-on-client keys in the
    // client's own local file). This guards against honest client mistakes
    // (wrong types flowing into core or sidecar argv) and keeps the store
    // schema-only.
    const errors = validateSettingsPatch(patch, { allow: ["core", "client-on-core"] });
    if (errors.length > 0) {
      throw new AppError("validation_error", errors.join("; "));
    }
    // Partition by destination: shared keys go to the core-global store, the
    // client's own keys go to its per-client overlay. Both are sparse upserts.
    const global: Record<string, unknown> = {};
    const perClient: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(patch)) {
      if (settingKeyDestination(k) === "client-on-core") perClient[k] = v;
      else global[k] = v;
    }
    if (Object.keys(global).length > 0) await patchCoreSettings(global);
    if (Object.keys(perClient).length > 0) await patchClientSettings(me.id, perClient);
    return c.json(sanitizeSettings(await loadEffective(me.id)));
  });

  // --- secrets -------------------------------------------------------------
  // Stored encrypted on disk in secrets.enc (see services/secrets.ts).
  // Plan §9 / client refactor: "Client receives only boolean flags
  // indicating which keys are configured, never the values." So GET
  // returns names only; values are never read back over the API.

  // The vault holds three classes of secret and these routes manage exactly
  // one: the schema's `type: "password"` fields, which is what
  // `isSecretSettingKey` tests. The other two are reachable only by their owners, and must not be
  // reachable here.
  //
  //   - `mcp.<id>.authToken` / `mcp.<id>.oauth`, written by the MCP routes.
  //   - `tls-key`, the sealed private key of the certificate every paired
  //     client pinned.
  //
  // Without this gate a paired client can DELETE `tls-key`, and the next boot
  // generates a fresh keypair with a different SPKI pin, which locks out every
  // paired client at once. PUT is worse: a value that is not a valid JWK means
  // the core does not boot at all. Revoking one other device is deliberately
  // gated behind the admin token so a single device cannot lock the others out;
  // an ungated route reaching the same outcome defeats that.
  function assertSettingsSecret(name: string): void {
    if (!isSecretSettingKey(name)) {
      throw new AppError("not_found", `unknown secret setting "${name}"`);
    }
  }

  r.get("/secrets", async (c) => {
    const names = await listSecretNames();
    return c.json({ names: names.filter(isSecretSettingKey) });
  });

  r.put("/secrets/:name", async (c) => {
    const name = c.req.param("name");
    assertSettingsSecret(name);
    const body = await readJson(c);
    if (
      !body ||
      typeof body !== "object" ||
      typeof (body as Record<string, unknown>).value !== "string"
    ) {
      throw new AppError("validation_error", "body must be { value: string }");
    }
    await setSecret(name, (body as { value: string }).value);
    return c.body(null, 204);
  });

  r.delete("/secrets/:name", async (c) => {
    const name = c.req.param("name");
    assertSettingsSecret(name);
    const ok = await deleteSecret(name);
    if (!ok) throw new AppError("not_found", `secret "${name}" not set`);
    return c.body(null, 204);
  });

  return r;
}
