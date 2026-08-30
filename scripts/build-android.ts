// Builds the Tauri client as an Android APK for the requested channel.
//
// Channel handling mirrors build-client.ts:
//   - The channel is written to the Tauri crate's `channel` file, which build.rs
//     bakes into the APK via `option_env!("LOCA_CHANNEL")`. A file rather than
//     the build env because a persistent Gradle daemon pins whatever channel it
//     first started with.
//   - Non-stable channels get a `--config` override for a distinct productName,
//     which names the APK. The on-device LABEL is `@string/app_name`, generated
//     per channel by resValue() in build.gradle.kts, because Tauri writes
//     strings.xml only at `tauri android init`. The distinct applicationId is
//     likewise a gradle applicationIdSuffix, NOT a Tauri `identifier` override:
//     the identifier drives the MainActivity java package path, fixed at init
//     time, so overriding it points Tauri at a package dir that does not exist.
//
// Distribution is self-hosted: gradle's signingConfigs sign the APK, and
// scripts/release/android.ts publishes it with an Ed25519-signed android.json.
// The Tauri updater plugin is desktop-only, so there is no endpoint override.

import { parseArgs } from "@std/cli/parse-args";
import { fromFileUrl, join } from "@std/path";
import { resolveAndroidEnv } from "./release/android-toolchain.ts";

const ROOT = fromFileUrl(new URL("..", import.meta.url));

const args = parseArgs(Deno.args, {
  string: ["channel"],
  default: { channel: "stable" },
});

const channel = args.channel;
if (!["stable", "dev", "latest"].includes(channel)) {
  console.error(`invalid --channel: ${channel} (expected stable, dev, or latest)`);
  Deno.exit(1);
}

// Bake the channel for channel.rs (via build.rs). A persistent Gradle daemon
// pins whatever LOCA_CHANNEL it first started with, so the ambient env can't be
// trusted to reach the cargo compile; the on-disk file is the deterministic
// source (see build.rs).
await Deno.writeTextFile(join(ROOT, "packages/loca-client/src/tauri/channel"), channel);

// Non-stable channels get a distinct on-device label so they read as separate
// apps; the distinct applicationId (for side-by-side install) is added by gradle's
// applicationIdSuffix. We deliberately do NOT override `identifier` here (see the
// header note). Tauri deep-merges this over tauri.conf.json + tauri.android.conf.json.
const override: Record<string, unknown> = {};
if (channel !== "stable") {
  override.productName = `Loca (${channel})`;
}

// Invoke the Tauri CLI directly (not via `deno task ... --`) so `--config`
// reaches the CLI rather than being treated as a cargo passthrough; see the
// matching note in build-client.ts.
const tauriArgs = ["run", "-A", "npm:@tauri-apps/cli@^2", "android", "build", "--apk"];
if (Object.keys(override).length > 0) {
  tauriArgs.push("--config", JSON.stringify(override));
}

const cmd = new Deno.Command("deno", {
  args: tauriArgs,
  cwd: join(ROOT, "packages/loca-client"),
  env: { ...Deno.env.toObject(), ...resolveAndroidEnv(), LOCA_CHANNEL: channel },
  stdout: "inherit",
  stderr: "inherit",
});
const { code } = await cmd.output();
Deno.exit(code);
