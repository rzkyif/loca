// Builds the Tauri client bundle for the requested channel.
//
// Channel handling:
//   - The channel is written to the Tauri crate's `channel` file, which build.rs
//     bakes into the shipped bundle via `option_env!("LOCA_CHANNEL")` in
//     channel.rs (its runtime default → ~/.loca/<channel>/client,
//     loca-client-<channel> keychain).
//   - For non-stable channels we pass a `tauri build --config <json>` override
//     so the app gets a distinct productName + bundle identifier + updater
//     endpoint, letting stable and latest coexist as separate installed apps.
//
// Delegates the actual build to the client package's `build` task.

import { parseArgs } from "@std/cli/parse-args";
import { fromFileUrl, join } from "@std/path";

// Repo root as an OS path. `new URL("..").pathname` yields "/C:/work/" on Windows
// (an invalid cwd); fromFileUrl produces the correct native path on every OS.
const ROOT = fromFileUrl(new URL("..", import.meta.url));

// Must match the base tauri.conf.json updater host (stable's endpoint).
const STORAGE_BASE = "https://get.loca.rzkyif.com";

const args = parseArgs(Deno.args, {
  string: ["channel"],
  default: { channel: "stable" },
});

const channel = args.channel;
if (!["stable", "dev", "latest"].includes(channel)) {
  console.error(`invalid --channel: ${channel} (expected stable, dev, or latest)`);
  Deno.exit(1);
}

// Bake the channel for channel.rs (via build.rs). Desktop builds inherit
// LOCA_CHANNEL at runtime, but a shipped bundle has no such env, so the on-disk
// file is the deterministic source the compile reads (see build.rs).
await Deno.writeTextFile(join(ROOT, "packages/loca-client/src/tauri/channel"), channel);

// Windows-only: `workerd` (a website dep the client never uses) ships NO
// win-arm64 binary, so its lifecycle script aborts `deno install` mid frontend
// build on that runner. Drop it from the EPHEMERAL runner checkout's allowScripts
// so the install skips it with a harmless warning; macOS/Linux are untouched.
// Stripped line-based, not via JSON.parse: deno.json is JSONC and the CI
// pre-strip can leave a trailing comma a strict parse rejects.
if (Deno.build.os === "windows") {
  const confPath = join(ROOT, "deno.json");
  const conf = await Deno.readTextFile(confPath);
  const stripped = conf
    .split("\n")
    .filter((l) => !l.includes("npm:workerd@"))
    .join("\n");
  if (stripped !== conf) await Deno.writeTextFile(confPath, stripped);
}

// Non-stable channels get a distinct app identity so they install alongside
// stable. A plain local build (no updater signing key in the env) also turns off
// updater-artifact creation so Tauri does not try to sign the bundle and abort;
// the release path sets TAURI_SIGNING_PRIVATE_KEY and keeps it on. Tauri
// deep-merges this over tauri.conf.json.
const override: Record<string, unknown> = {};
if (channel !== "stable") {
  override.productName = `Loca (${channel})`;
  override.identifier = `com.rzkyif.loca.${channel}`;
  override.plugins = {
    updater: {
      endpoints: [`${STORAGE_BASE}/manifests/${channel}/client.json`],
    },
  };
}
if (!Deno.env.get("TAURI_SIGNING_PRIVATE_KEY")) {
  override.bundle = { createUpdaterArtifacts: false };
}

// The `--config` override must reach the Tauri CLI itself, so invoke the CLI
// directly: through `deno task build -- --config <json>` Tauri treats it as a
// cargo passthrough and cargo fails parsing it as a TOML dotted key. Calling the
// CLI directly also sidesteps `deno task`'s own `--config` flag.
const tauriArgs = ["run", "-A", "npm:@tauri-apps/cli@^2", "build"];
if (Object.keys(override).length > 0) {
  tauriArgs.push("--config", JSON.stringify(override));
}

const cmd = new Deno.Command("deno", {
  args: tauriArgs,
  cwd: join(ROOT, "packages/loca-client"),
  env: { ...Deno.env.toObject(), LOCA_CHANNEL: channel },
  stdout: "inherit",
  stderr: "inherit",
});
const { code } = await cmd.output();
Deno.exit(code);
