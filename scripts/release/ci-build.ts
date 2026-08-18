#!/usr/bin/env -S deno run -A
// CI build half: runs on ONE native runner, builds that runner's host triple,
// and stages the artifacts + descriptors for the publish job (ci-publish.ts) to
// compose + sign + upload once. A dumb artifact producer: it emits the
// bundle/descriptor contract in artifacts.ts and never signs a manifest.
//
// It builds core + the desktop client (when Tauri keys are present) + Android
// (only with --android, on the Linux runner); --android-only builds just the
// Android APKs so the release matrix can run them on their own leg. It does build-
// time signing only (Tauri minisign for the client, the Java keystore for the
// APK) from secrets in the process env; it constructs the env directly rather
// than via loadOrSeedEnv, so the Ed25519 trust-root PRIVATE key never reaches a
// build runner. It writes the Ed25519 PUBLIC key into signing-keys.json so the
// compiled core embeds the key it verifies manifests with.
//
// Flags:
//   --channel=stable|latest   target channel (required; the workflow sets it)
//   --stage-dir=<path>         where to stage artifacts (default: ./stage)
//   --android                  also build + stage the Android APK(s) (Linux only)
//   --android-only             build + stage ONLY the Android APK(s), skipping
//                              core, the desktop client, and the installers
//                              (implies --android; its own release matrix leg)

import { parseArgs } from "@std/cli/parse-args";
import { ensureDir } from "@std/fs/ensure-dir";
import {
  channelBinSuffix,
  colors,
  detectHostTriple,
  envFromProcess,
  fail,
  hasReleaseChanges,
  info,
  ok,
  parseChannelFlag,
  readCoreVersion,
  REPO_ROOT,
  step,
  writeSigningKeys,
} from "./lib.ts";
import { join } from "@std/path";
import { buildCoreArtifacts } from "./core.ts";
import { buildLinuxCoreInstallers, type InstallerAsset } from "./core-installers.ts";
import { buildMacUnified, buildWindowsUnified } from "./unified-installers.ts";
import { buildClientBundle } from "./client.ts";
import { buildAndroidBundle } from "./android.ts";
import { buildAndUploadIos, iosItem, readIosVersion } from "./ios.ts";
import { appleReleaseConfigured, materializeAppleApiKey } from "./apple-toolchain.ts";
import {
  bundleCoreArtifacts,
  stageDistFile,
  writeAndroidDescriptor,
  writeBundle,
  writeClientDescriptor,
  writeIosReceipt,
} from "./artifacts.ts";

async function main(): Promise<void> {
  const args = parseArgs(
    Deno.args.filter((a) => a !== "--"),
    {
      string: ["channel", "stage-dir"],
      boolean: ["android", "android-only", "ios"],
      default: { android: false, "android-only": false, ios: false },
    },
  );
  const androidOnly = args["android-only"];
  const android = args.android || androidOnly;
  const channel = parseChannelFlag(args.channel);
  const stageDir = args["stage-dir"] ? String(args["stage-dir"]) : join(REPO_ROOT, "stage");
  const suffix = channelBinSuffix(channel);
  const hostTriple = detectHostTriple();
  const version = await readCoreVersion();

  console.log(colors.bold(`\nloca ci-build: ${channel} channel, ${hostTriple}\n`));

  const env = envFromProcess();
  // CI carries the App Store Connect .p8 as base64 (a runner cannot carry a
  // file path as a secret); materialize it to a temp file so notarytool, Tauri,
  // and altool all find it. No-op locally / when the secret is absent.
  const cleanupApiKey = await materializeAppleApiKey(env);
  try {
    // Fresh staging dir per run.
    await Deno.remove(stageDir, { recursive: true }).catch(() => {});
    await ensureDir(stageDir);

    // --- core + desktop client + installers (skipped with --android-only) ---
    if (!androidOnly) {
      // Bake the Ed25519 public key into the core binary (data/signing-keys.json)
      // so it can verify manifests at runtime. Must precede the core build. Only
      // core reads this file, so the android-only leg leaves the tracked copy
      // alone (the APK's trust root is the key pinned in lib/platform/mobile.ts).
      await writeSigningKeys(Deno.env.get("LOCA_SIGNING_PUBLIC_KEY_B64")!);

      step(`Building core for ${hostTriple}`);
      const coreBuilt = await buildCoreArtifacts([hostTriple], suffix);
      const bundle = bundleCoreArtifacts(coreBuilt, version, channel, [hostTriple]);
      const installers: InstallerAsset[] = [];
      // Linux keeps its standalone Core packages; the Windows/macOS user-facing
      // installers are the unified ones built AFTER the client below (they wrap
      // the client artifact). Degrades to fewer/no assets when a packaging tool
      // isn't provisioned, so a plain build still succeeds.
      if (hostTriple.endsWith("unknown-linux-gnu")) {
        installers.push(
          ...(await buildLinuxCoreInstallers(coreBuilt, {
            version,
            channel,
            triple: hostTriple,
            env,
          })),
        );
      }

      // --- desktop client (when Tauri keys are present) ---
      let clientBuilt: { relPath: string; version: string } | undefined;
      if (env.tauriUpdaterPublicKey && env.tauriUpdaterPrivateKey) {
        const descriptor = await buildClientBundle(env, channel);
        await writeClientDescriptor(stageDir, descriptor);
        await stageDistFile(stageDir, descriptor.relPath);
        await stageDistFile(stageDir, descriptor.sigRelPath);
        ok(`staged client bundle ${descriptor.filename}`);
        clientBuilt = descriptor;
      } else {
        info(colors.yellow("Tauri updater keys absent; skipping the client on this runner"));
      }

      // --- unified installer (Windows/macOS; wraps the client just built) ---
      const unifiedOpts = clientBuilt && {
        coreVersion: version,
        clientVersion: clientBuilt.version,
        channel,
        triple: hostTriple,
        env,
      };
      if (unifiedOpts && hostTriple.endsWith("apple-darwin")) {
        installers.push(...(await buildMacUnified(coreBuilt, unifiedOpts)));
      } else if (unifiedOpts && hostTriple.endsWith("pc-windows-msvc")) {
        installers.push(
          ...(await buildWindowsUnified(coreBuilt, clientBuilt!.relPath, unifiedOpts)),
        );
      } else if (!clientBuilt && !hostTriple.endsWith("unknown-linux-gnu")) {
        info(colors.yellow("no client bundle to wrap; skipping the unified installer"));
      }

      if (installers.length) {
        bundle.installers = installers;
        for (const a of installers) await stageDistFile(stageDir, a.relPath);
      }
      await writeBundle(stageDir, bundle);
      for (const rec of bundle.records) await stageDistFile(stageDir, rec.relPath);
      ok(`staged ${bundle.records.length} core record(s), ${installers.length} installer(s)`);
    }

    // --- Android (Linux runner only, with --android / --android-only) ---
    if (android) {
      if (!env.androidKeystoreB64) {
        fail(
          `--android${
            androidOnly ? "-only" : ""
          } given but LOCA_ANDROID_KEYSTORE_B64 is not set in the env.`,
        );
      }
      const descriptor = await buildAndroidBundle(env, channel);
      await writeAndroidDescriptor(stageDir, descriptor);
      for (const apk of descriptor.apks) await stageDistFile(stageDir, apk.relPath);
      ok(`staged ${descriptor.apks.length} APK(s)`);
    }

    // --- iOS (macOS runner only, with --ios) ---
    // iOS ships through the App Store, so the signing mac builds AND uploads the
    // .ipa here rather than staging it for the Linux publish coordinator. Inert
    // until the Apple secrets exist: without them this no-ops so the runner
    // stays green (mirrors the macOS APPLE_* signing being secret-driven).
    //
    // The upload is its own side effect (App Store Connect rejects a duplicate
    // build number), so unlike the staged artifacts it must be diffed against the
    // cursor HERE rather than by the coordinator's release plan. The cursor is
    // public on R2, so envFromProcess (public key + storage domain) is enough to
    // read it - same as ci-preflight. On success a receipt is staged; the
    // coordinator turns that into the cursor entry.
    if (args.ios) {
      if (!appleReleaseConfigured(env)) {
        info(
          colors.yellow(
            "--ios given but Apple signing / App Store Connect is not configured; skipping iOS.",
          ),
        );
      } else if (!(await hasReleaseChanges(env, [iosItem], channel))) {
        info("ios client unchanged since the published cursor; skipping the App Store upload");
      } else {
        await buildAndUploadIos(env, channel, { dryRun: false });
        await writeIosReceipt(stageDir, { version: await readIosVersion(), channel });
        ok("built + uploaded the iOS .ipa to App Store Connect");
      }
    }

    console.log(
      "\n" + colors.green(colors.bold(`✓ ci-build complete`)) + `  staged under ${stageDir}\n`,
    );
  } finally {
    await cleanupApiKey();
  }
}

if (import.meta.main) {
  try {
    await main();
  } catch (err) {
    fail(err instanceof Error ? err.message : String(err));
  }
}
