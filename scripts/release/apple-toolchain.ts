// Apple build-toolchain resolution + availability, single-sourced so the iOS
// build entry (scripts/build-ios.ts), the release item (scripts/release/ios.ts),
// and the gate that decides whether a release includes the iOS client all agree
// on what "the toolchain is present" and "signing is configured" mean.
//
// iOS is Apple-only: xcodebuild cannot cross-compile or cross-sign off macOS, so
// every check returns false on a non-mac host. Signing + App Store upload reuse
// the same APPLE_* env already carried on DeployEnv for the macOS client (see
// lib.ts), so no new secrets are introduced.
//
// Also home to the two CI-only secret materializers (the App Store Connect .p8
// and the Developer ID Installer keychain): both are no-ops when their env is
// empty, so a run without the secrets behaves exactly as before.

import { join } from "@std/path";
import { decodeBase64 } from "@std/encoding/base64";
import { encodeHex } from "@std/encoding/hex";
import { type DeployEnv, fail } from "./lib.ts";

/** Resolve the env a `tauri ios build` + App Store upload needs. Tauri/xcodebuild
 *  read the development team from APPLE_DEVELOPMENT_TEAM (or bundle.iOS.developmentTeam
 *  in the config); the signing identity/cert drive build-ios.ts's signed-vs-simulator
 *  decision; the App Store Connect API key authenticates Xcode's automatic (cloud)
 *  signing and the upload. Keys that could not be resolved are omitted, so callers
 *  can detect what is missing and a blank field never reaches a child process.
 *  Empty when no Apple env is set (the simulator/unsigned dev path). */
export function resolveAppleEnv(env: DeployEnv): Record<string, string> {
  const raw: Record<string, string> = {
    APPLE_DEVELOPMENT_TEAM: env.appleTeamId,
    APPLE_TEAM_ID: env.appleTeamId,
    APPLE_SIGNING_IDENTITY: env.appleSigningIdentity,
    APPLE_CERTIFICATE: env.appleCertificateB64,
    APPLE_CERTIFICATE_PASSWORD: env.appleCertificatePassword,
    APPLE_API_KEY: env.appleApiKey,
    APPLE_API_ISSUER: env.appleApiIssuer,
    APPLE_API_KEY_PATH: env.appleApiKeyPath,
  };
  return Object.fromEntries(Object.entries(raw).filter(([, v]) => v !== ""));
}

/** True when a real Apple **signing** identity is configured (a Team ID plus a
 *  certificate, either an installed identity or a base64 .p12 on CI). Without it
 *  only an unsigned simulator build is possible. */
export function hasAppleSigning(env: DeployEnv): boolean {
  return !!env.appleTeamId && (!!env.appleSigningIdentity || !!env.appleCertificateB64);
}

/** True when the App Store Connect API key trio is configured, so a signed .ipa
 *  can be uploaded to App Store Connect / TestFlight (the iOS distribution path;
 *  iOS has no R2 self-host). The .p8 arrives as a path locally and as base64 on
 *  CI (materializeAppleApiKey turns the latter into the former). */
export function hasAppStoreConnectApi(env: DeployEnv): boolean {
  return (
    !!env.appleApiKey && !!env.appleApiIssuer && (!!env.appleApiKeyPath || !!env.appleApiKeyB64)
  );
}

/** True when iOS can be both signed and shipped: the release orchestrator drops
 *  the iOS item (with a yellow warning) when this is false, exactly as it drops
 *  the desktop client without Tauri keys and android without a keystore. Until
 *  the Apple secrets exist, this is false and iOS is inert. */
export function appleReleaseConfigured(env: DeployEnv): boolean {
  return hasAppleSigning(env) && hasAppStoreConnectApi(env);
}

function commandSucceeds(cmd: string, args: string[]): boolean {
  try {
    return new Deno.Command(cmd, { args, stdout: "null", stderr: "null" }).outputSync().success;
  } catch {
    return false;
  }
}

/** True when this host can build for iOS at all: macOS with a working xcodebuild
 *  and the aarch64-apple-ios rust target installed. Used to gate the iOS build
 *  and to fail fast off-mac with a clear message instead of a cryptic xcodebuild
 *  error. Does NOT imply signing is configured (see hasAppleSigning). */
export function iosToolchainReady(): boolean {
  if (Deno.build.os !== "darwin") return false;
  if (!commandSucceeds("xcodebuild", ["-version"])) return false;
  try {
    const out = new Deno.Command("rustup", {
      args: ["target", "list", "--installed"],
      stdout: "piped",
      stderr: "null",
    }).outputSync();
    return new TextDecoder().decode(out.stdout).includes("aarch64-apple-ios");
  } catch {
    // No rustup (a rustc-only toolchain); assume the target is available rather
    // than blocking the build over a probe we cannot run.
    return true;
  }
}

// ---------------------------------------------------------------------------
// CI secret materialization
//
// Both helpers return an async cleanup and install SIGINT/SIGTERM wipe handlers
// (the materializeKeystore pattern in android.ts) so decoded secrets never
// linger on disk, including on Ctrl-C.

/** Quiet subprocess runner for the materializers: never echoes the command line
 *  (argv may carry keychain passwords), surfaces stderr only on failure. */
async function runQuiet(argv: string[]): Promise<string> {
  const { code, stdout, stderr } = await new Deno.Command(argv[0], {
    args: argv.slice(1),
    stdout: "piped",
    stderr: "piped",
  }).output();
  if (code !== 0) {
    throw new Error(
      `${argv[0]} ${argv[1] ?? ""} exited ${code}: ` +
        new TextDecoder().decode(stderr).trim().slice(0, 2000),
    );
  }
  return new TextDecoder().decode(stdout);
}

/** Decode APPLE_API_KEY_B64 (the CI form of the App Store Connect key, honored
 *  locally too) into a temp `private_keys/AuthKey_<id>.p8` and fill
 *  env.appleApiKeyPath + the process env with its path, so notarytool, Tauri
 *  (macOS notarization + iOS cloud signing), and altool all find the key. The
 *  `private_keys/AuthKey_<id>.p8` layout is deliberate: consumers that take a
 *  path get APPLE_API_KEY_PATH, while altool discovers keys by directory
 *  convention via API_PRIVATE_KEYS_DIR. No-op (empty cleanup) when the base64 is
 *  absent or a path is already set, so path-form runs are untouched. */
export async function materializeAppleApiKey(env: DeployEnv): Promise<() => Promise<void>> {
  if (!env.appleApiKeyB64 || env.appleApiKeyPath) return async () => {};
  if (!env.appleApiKey) {
    fail(
      `APPLE_API_KEY_B64 is set but APPLE_API_KEY (the key id) is not; ` +
        `the key id names the materialized AuthKey_<id>.p8.`,
    );
  }
  const root = await Deno.makeTempDir({ prefix: "tomat-asc-key-" });
  const keysDir = join(root, "private_keys");
  const priorKeyPath = Deno.env.get("APPLE_API_KEY_PATH");
  const priorKeysDir = Deno.env.get("API_PRIVATE_KEYS_DIR");
  try {
    await Deno.mkdir(keysDir);
    const p8Path = join(keysDir, `AuthKey_${env.appleApiKey}.p8`);
    await Deno.writeFile(p8Path, decodeBase64(env.appleApiKeyB64));
    await Deno.chmod(p8Path, 0o600);
    env.appleApiKeyPath = p8Path;
    Deno.env.set("APPLE_API_KEY_PATH", p8Path);
    Deno.env.set("API_PRIVATE_KEYS_DIR", keysDir);
  } catch (err) {
    await Deno.remove(root, { recursive: true }).catch(() => {});
    throw err;
  }

  const wipeSync = (): void => {
    try {
      Deno.removeSync(root, { recursive: true });
    } catch {
      /* already gone */
    }
  };
  const onSignal = (): void => {
    wipeSync();
    Deno.exit(130);
  };
  Deno.addSignalListener("SIGINT", onSignal);
  Deno.addSignalListener("SIGTERM", onSignal);

  return async () => {
    Deno.removeSignalListener("SIGINT", onSignal);
    Deno.removeSignalListener("SIGTERM", onSignal);
    // Leave no dangling references to the removed key file behind.
    env.appleApiKeyPath = "";
    if (priorKeyPath === undefined) Deno.env.delete("APPLE_API_KEY_PATH");
    else Deno.env.set("APPLE_API_KEY_PATH", priorKeyPath);
    if (priorKeysDir === undefined) Deno.env.delete("API_PRIVATE_KEYS_DIR");
    else Deno.env.set("API_PRIVATE_KEYS_DIR", priorKeysDir);
    await Deno.remove(root, { recursive: true }).catch(() => {});
  };
}

/** CI only: import the base64 .p12 Developer ID Installer cert into an ephemeral
 *  keychain added to the user search list, so productsign (which runs outside
 *  Tauri's own APPLE_CERTIFICATE import) can find the identity. No-op (empty
 *  cleanup) when the cert env is absent: locally the identity lives in the login
 *  keychain. Cleanup deletes the keychain, restores the prior search list, and
 *  removes every decoded file. */
export async function materializeInstallerKeychain(env: DeployEnv): Promise<() => Promise<void>> {
  if (!env.appleInstallerCertificateB64) return async () => {};

  const root = await Deno.makeTempDir({ prefix: "tomat-installer-kc-" });
  const p12Path = join(root, "installer.p12");
  const keychain = join(root, "tomat-installer.keychain-db");
  const pw = encodeHex(crypto.getRandomValues(new Uint8Array(32)));
  await Deno.writeFile(p12Path, decodeBase64(env.appleInstallerCertificateB64));
  await Deno.chmod(p12Path, 0o600);

  // The prior user search list, to restore on cleanup. `security list-keychains`
  // prints one quoted path per line.
  const priorList = (await runQuiet(["security", "list-keychains", "-d", "user"]))
    .split("\n")
    .map((l) => l.trim().replace(/^"|"$/g, ""))
    .filter((l) => l.length > 0);

  const wipeSync = (): void => {
    // Restoring the prior search list is idempotent, so it is safe to run even
    // when this keychain was never added to it.
    try {
      new Deno.Command("security", {
        args: ["list-keychains", "-d", "user", "-s", ...priorList],
        stdout: "null",
        stderr: "null",
      }).outputSync();
    } catch {
      /* best effort */
    }
    try {
      new Deno.Command("security", {
        args: ["delete-keychain", keychain],
        stdout: "null",
        stderr: "null",
      }).outputSync();
    } catch {
      /* best effort */
    }
    try {
      Deno.removeSync(root, { recursive: true });
    } catch {
      /* already gone */
    }
  };
  const onSignal = (): void => {
    wipeSync();
    Deno.exit(130);
  };
  Deno.addSignalListener("SIGINT", onSignal);
  Deno.addSignalListener("SIGTERM", onSignal);

  try {
    await runQuiet(["security", "create-keychain", "-p", pw, keychain]);
    // 6h auto-lock ceiling: far past any notarytool --wait, so the keychain
    // cannot relock mid-run on a slow notarization.
    await runQuiet(["security", "set-keychain-settings", "-lut", "21600", keychain]);
    await runQuiet(["security", "unlock-keychain", "-p", pw, keychain]);
    await runQuiet([
      "security",
      "import",
      p12Path,
      "-k",
      keychain,
      "-P",
      env.appleInstallerCertificatePassword,
      "-f",
      "pkcs12",
      "-T",
      "/usr/bin/productsign",
      "-T",
      "/usr/bin/security",
    ]);
    // Without this, macOS pops a UI confirmation on first key use, which would
    // hang a headless runner.
    await runQuiet([
      "security",
      "set-key-partition-list",
      "-S",
      "apple-tool:,apple:",
      "-s",
      "-k",
      pw,
      keychain,
    ]);
    await runQuiet(["security", "list-keychains", "-d", "user", "-s", keychain, ...priorList]);
    // The decoded cert is in the keychain now; drop the .p12 immediately.
    await Deno.remove(p12Path).catch(() => {});
  } catch (err) {
    Deno.removeSignalListener("SIGINT", onSignal);
    Deno.removeSignalListener("SIGTERM", onSignal);
    wipeSync();
    throw err;
  }

  return async () => {
    Deno.removeSignalListener("SIGINT", onSignal);
    Deno.removeSignalListener("SIGTERM", onSignal);
    await runQuiet(["security", "delete-keychain", keychain]).catch(() => {});
    await runQuiet(["security", "list-keychains", "-d", "user", "-s", ...priorList]).catch(
      () => {},
    );
    await Deno.remove(root, { recursive: true }).catch(() => {});
  };
}
