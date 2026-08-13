// Host-triple detection + canonical binary names.
//
// The on-disk layout strips the triple from filenames - core only ever has
// one platform's binary installed at a time. The triple appears in download
// URLs (per the manifest) and in cache keys.

import { join } from "@std/path";
import type { BinaryKind, Triple } from "@tomat/shared";
import { channelBinName } from "../paths.ts";

export function hostTriple(): Triple {
  return Deno.build.target as Triple;
}

export function platformExe(): "" | ".exe" {
  return Deno.build.os === "windows" ? ".exe" : "";
}

// Canonical on-disk filename per sidecar binary (llama-server, tomat-core-speech,
// deno), with .exe on Windows. NOT channel-suffixed: sidecar binaries are
// isolated by the per-channel bin dir and keep their original names.
export function binaryName(kind: BinaryKind): string {
  return `${kind}${platformExe()}`;
}

// Per-kind shared-library directory under a binaries root (`<binRoot>/lib/<kind>`).
// Each sidecar gets its OWN lib dir so llama's and whisper's identically-named
// ggml-*.dll never collide on disk, and on Windows - where the sidecar's cwd is
// set to its lib dir and ggml_backend_load_all() scans cwd for plugins - each
// server only ever sees its own backends. Used by both the extractor (install
// time) and the sidecar launchers (run time), so the convention stays in lockstep.
export function libDirFor(binRoot: string, kind: BinaryKind): string {
  return join(binRoot, "lib", kind);
}

// On-disk filename for one of Tomat's OWN binaries (tomat-core,
// tomat-core-updater, tomat-core-keychain): channel-suffixed (so latest's
// tomat-core-latest coexists with stable's tomat-core) + .exe on Windows.
export function coreBinaryName(base: string): string {
  return `${channelBinName(base)}${platformExe()}`;
}
