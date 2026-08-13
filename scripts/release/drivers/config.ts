// Device-specific driver config, sourced from .env (see .env.example).
//
// The podman + windows drivers need machine-specific values (the podman machine
// name, the UTM VM name + SSH endpoint, guest paths) that must not be committed.
// loadOrSeedEnv (lib.ts) reads .env with export:false so the release SECRETS
// never reach the process env; this promotes only the non-secret LOCA_WIN_* /
// LOCA_PODMAN_* / LOCA_LINUX_BUILD_* driver keys into it, so the driver code
// can read them synchronously via Deno.env.get (including the sync teardown
// backstop). A value already present in the process env (e.g. exported in the
// shell) wins and is left untouched.

import { load as loadDotenv } from "@std/dotenv";
import { ENV_PATH } from "../lib.ts";

const DRIVER_KEY_PREFIXES = ["LOCA_WIN_", "LOCA_PODMAN_", "LOCA_LINUX_BUILD_"];

let loaded = false;

/** Promote the driver's device-specific config from .env into the process env,
 *  once. Call before using any driver (both the release and the dev cross-build
 *  entrypoints do). A missing .env is fine: the drivers' empty-config guards then
 *  report them unavailable. */
export async function loadDriverEnv(): Promise<void> {
  if (loaded) return;
  loaded = true;
  let fileMap: Record<string, string>;
  try {
    fileMap = await loadDotenv({ envPath: ENV_PATH, export: false });
  } catch {
    return;
  }
  for (const [key, value] of Object.entries(fileMap)) {
    if (!value) continue;
    if (!DRIVER_KEY_PREFIXES.some((p) => key.startsWith(p))) continue;
    if (Deno.env.get(key)) continue;
    Deno.env.set(key, value);
  }
}
