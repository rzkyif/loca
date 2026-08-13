// Builds the Tauri client bundle for the requested channel.
//
// Channel handling:
//   - The channel is written to the Tauri crate's `channel` file, which build.rs
//     bakes into the shipped bundle via `option_env!("TOMAT_CHANNEL")` in
//     channel.rs (its runtime default → ~/.tomat/<channel>/client,
//     tomat-client-<channel> keychain).
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
const STORAGE_BASE = "https://get.au.tomat.ing";

const args = parseArgs(Deno.args, {
  string: ["channel", "target", "bundles"],
  collect: ["target"],
  default: { channel: "stable" },
});

const channel = args.channel;
if (!["stable", "dev", "latest"].includes(channel)) {
  console.error(`invalid --channel: ${channel} (expected stable, dev, or latest)`);
  Deno.exit(1);
}

// Bake the channel for channel.rs (via build.rs). Desktop builds inherit
// TOMAT_CHANNEL at runtime, but a shipped bundle has no such env, so the on-disk
// file is the deterministic source the compile reads (see build.rs).
await Deno.writeTextFile(join(ROOT, "packages/tomat-client/src/tauri/channel"), channel);

// Optional cross-arch target(s): the all-targets release builds the host's second
// arch (e.g. x86_64-apple-darwin on an arm64 mac) and, in a build environment,
// the env's other arch. When set, Tauri emits under target/<triple>/release/bundle.
const targets = (args.target as string[])
  .flatMap((t) => t.split(","))
  .map((t) => t.trim())
  .filter(Boolean);
// Optional bundle-target override: a CSV of Tauri bundle targets, narrowing what
// gets built instead of every target in tauri.conf.json. The Linux cross-build
// passes just "appimage" - the AppImage is the sole Linux client format.
const bundles = (args.bundles ?? "").trim();

// Windows-only: the build host is the win-arm64 UTM guest, and `workerd` (the
// website's Cloudflare Worker runtime, a workspace dep the client never uses)
// ships NO win-arm64 binary, so its lifecycle script aborts `deno install` mid
// frontend build. Drop workerd from this EPHEMERAL synced repo's allowScripts so
// the install skips it (a harmless "ignored build script" warning) and proceeds.
// Host (macOS/Linux) builds are untouched; the guest's deno.json is re-synced
// every run, so this never persists.
// Strip line-based, not via JSON.parse: deno.json is JSONC and the CI pre-strip
// (release.yml drops workerd before the first deno command) can leave a trailing
// comma that a strict JSON.parse rejects. deno itself tolerates it, so filtering
// the workerd line and writing the text back verbatim is enough (and idempotent
// when the pre-strip already removed it).
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
// stable instead of overwriting it. And a plain local build (no updater signing
// key in the env) turns off updater-artifact creation so Tauri doesn't try to
// sign the bundle and abort; the release path sets TAURI_SIGNING_PRIVATE_KEY and
// so keeps createUpdaterArtifacts on, still emitting the signed .sig client.json
// needs. Tauri deep-merges this over tauri.conf.json.
const override: Record<string, unknown> = {};
if (channel !== "stable") {
  override.productName = `Tomat (${channel})`;
  override.identifier = `au.tomat.ing.${channel}`;
  override.plugins = {
    updater: {
      endpoints: [`${STORAGE_BASE}/manifests/${channel}/client.json`],
    },
  };
}
if (!Deno.env.get("TAURI_SIGNING_PRIVATE_KEY")) {
  override.bundle = { createUpdaterArtifacts: false };
}

// The `--config` override must reach the Tauri CLI itself (it accepts inline
// JSON). We invoke the CLI directly rather than via `deno task build`: routing
// through `deno task build -- --config <json>` makes Tauri treat `--config`
// as a cargo passthrough (everything after `--`), and cargo parses it as a TOML
// dotted-key expression and fails. Calling the CLI directly also sidesteps
// `deno task`'s own `--config` flag.
const tauriArgs = ["run", "-A", "npm:@tauri-apps/cli@^2", "build"];
for (const t of targets) tauriArgs.push("--target", t);
if (bundles) tauriArgs.push("--bundles", bundles);
if (Object.keys(override).length > 0) {
  tauriArgs.push("--config", JSON.stringify(override));
}

const cmd = new Deno.Command("deno", {
  args: tauriArgs,
  cwd: join(ROOT, "packages/tomat-client"),
  env: { ...Deno.env.toObject(), TOMAT_CHANNEL: channel },
  stdout: "inherit",
  stderr: "inherit",
});
const { code } = await cmd.output();
Deno.exit(code);
