// Android build-toolchain resolution + availability, single-sourced so the
// android build entry (scripts/build-android.ts) and the gate that decides
// whether `deno task build` includes the android client agree on what "the
// toolchain is present" means.

import { join } from "@std/path";

/** Resolve the env the Android build (Gradle + cargo-ndk) needs: ANDROID_HOME,
 *  an NDK path, and a JDK. Honors whatever the environment already sets;
 *  otherwise falls back to the standard macOS locations (Android Studio + its
 *  bundled JBR), so a local android build works out of the box on a stock mac.
 *  On other hosts, set these in the env. Keys that could not be resolved are
 *  omitted (NDK_HOME / JAVA_HOME), so callers can detect what is missing. */
export function resolveAndroidEnv(): Record<string, string> {
  const home = Deno.env.get("HOME") ?? "";
  const out: Record<string, string> = {};

  const androidHome =
    Deno.env.get("ANDROID_HOME") ??
    Deno.env.get("ANDROID_SDK_ROOT") ??
    join(home, "Library/Android/sdk");
  out.ANDROID_HOME = androidHome;
  out.ANDROID_SDK_ROOT = androidHome;

  let ndk = Deno.env.get("NDK_HOME") ?? Deno.env.get("ANDROID_NDK_HOME") ?? "";
  if (!ndk) {
    try {
      const ndkRoot = join(androidHome, "ndk");
      const versions = [...Deno.readDirSync(ndkRoot)]
        .filter((e) => e.isDirectory)
        .map((e) => e.name)
        .sort();
      if (versions.length > 0) ndk = join(ndkRoot, versions[versions.length - 1]);
    } catch {
      // no NDK dir; gradle will surface a clearer error than we can here
    }
  }
  if (ndk) {
    out.NDK_HOME = ndk;
    out.ANDROID_NDK_HOME = ndk;
    // ANDROID_NDK_ROOT specifically, and it is not redundant with the other
    // two: cc-rs resolves the NDK from THIS name alone, and the Core's llama.cpp
    // shim is C++, so without it the C dependencies build and the shim does not.
    // Measured rather than assumed; the CI android job carries the same note.
    out.ANDROID_NDK_ROOT = ndk;
  }

  let javaHome = Deno.env.get("JAVA_HOME") ?? "";
  if (!javaHome) {
    const jbr = "/Applications/Android Studio.app/Contents/jbr/Contents/Home";
    try {
      if (Deno.statSync(jbr).isDirectory) javaHome = jbr;
    } catch {
      // no Android Studio JBR; rely on a system JDK on PATH
    }
  }
  if (javaHome) out.JAVA_HOME = javaHome;

  return out;
}

function dirExists(path: string): boolean {
  try {
    return Deno.statSync(path).isDirectory;
  } catch {
    return false;
  }
}

/** A JDK is usable when JAVA_HOME points at a real dir, or a `java` is on PATH
 *  (the build falls back to a system JDK when JAVA_HOME is unset). */
function javaAvailable(javaHome: string | undefined): boolean {
  if (javaHome && dirExists(javaHome)) return true;
  try {
    return new Deno.Command("java", {
      args: ["-version"],
      stdout: "null",
      stderr: "null",
    }).outputSync().success;
  } catch {
    return false;
  }
}

/** True when the SDK, an NDK, and a JDK are all present, so an android build can
 *  actually run. Used to gate the android item in `deno task build`. */
export function androidToolchainReady(): boolean {
  const env = resolveAndroidEnv();
  return (
    dirExists(env.ANDROID_HOME) &&
    !!env.NDK_HOME &&
    dirExists(env.NDK_HOME) &&
    javaAvailable(env.JAVA_HOME)
  );
}
