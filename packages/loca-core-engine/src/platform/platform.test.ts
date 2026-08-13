// Engine platform layer tests: the runtime host handle, the host-backed logger,
// path derivation from rootDir, portable SHA-256 parity, and the moved id/error
// utilities. Everything runs against an in-memory fake Host so the engine package
// tests in isolation (no Deno FS / sqlite).

import { assertEquals, assertThrows } from "@std/assert";
import type { Host, HostDb, LogLevel } from "../host.ts";
import { __detachHostForTesting, attachHost, host } from "./runtime.ts";
import { getLogger } from "./log.ts";
import { enginePaths, extensionDataDir, sessionAttachmentsDir, sessionDir } from "./paths.ts";
import { sha256HexSync, toHex } from "./hash.ts";
import { AppError, isAppError, isNoSpaceError } from "./errors.ts";
import { newMessageId, newRequestId } from "./ids.ts";

type LogLine = { level: LogLevel; scope: string; message: string };

// The smallest Host that satisfies the platform surface under test: a rootDir,
// env lookups, and a log sink that records every line. fs/openDb/secureStore are
// never exercised here, so they throw if touched (a guard against silent use).
function fakeHost(rootDir: string, logs: LogLine[]): Host {
  const unused = (): never => {
    throw new Error("not exercised by the platform suite");
  };
  return {
    rootDir,
    config: () => undefined,
    capabilities: { localInference: false, subprocess: false, remoteMcp: true },
    fs: {
      readTextFile: unused,
      writeTextFileAtomic: unused,
      readFile: unused,
      writeFileAtomic: unused,
      stat: unused,
      mkdir: unused,
      remove: unused,
      rename: unused,
      readDir: unused,
    },
    openDb: (): HostDb => unused(),
    secureStore: { get: unused, set: unused, delete: unused },
    log: (level, scope, message) => logs.push({ level, scope, message }),
  };
}

Deno.test("runtime: host() throws until attachHost is called, then returns it", () => {
  __detachHostForTesting();
  assertThrows(() => host(), Error, "engine host not attached");
  const logs: LogLine[] = [];
  const h = fakeHost("/tmp/root", logs);
  attachHost(h);
  assertEquals(host(), h);
  __detachHostForTesting();
});

Deno.test("log: getLogger routes every level to host.log with the scope", () => {
  const logs: LogLine[] = [];
  attachHost(fakeHost("/tmp/root", logs));
  const log = getLogger("myscope");
  log.debug("d");
  log.info("i");
  log.warn("w");
  log.error("e");
  assertEquals(logs, [
    { level: "debug", scope: "myscope", message: "d" },
    { level: "info", scope: "myscope", message: "i" },
    { level: "warn", scope: "myscope", message: "w" },
    { level: "error", scope: "myscope", message: "e" },
  ]);
  __detachHostForTesting();
});

Deno.test("paths: derived from host().rootDir and track a rootDir change", () => {
  const logs: LogLine[] = [];
  attachHost(fakeHost("/data/core", logs));
  const p = enginePaths();
  assertEquals(p.root, "/data/core");
  assertEquals(p.dbFile, "/data/core/core.sqlite");
  assertEquals(p.settingsFile, "/data/core/settings.json");
  assertEquals(p.secretsEncFile, "/data/core/secrets.enc");
  assertEquals(p.sessionsDir, "/data/core/sessions");
  assertEquals(p.memoriesDir, "/data/core/memories");
  assertEquals(sessionDir("s1"), "/data/core/sessions/s1");
  assertEquals(sessionAttachmentsDir("s1"), "/data/core/sessions/s1/attachments");
  assertEquals(extensionDataDir("ext1"), "/data/core/extension-data/ext1");

  // A fresh host with a different root re-derives (the getter is read per call).
  attachHost(fakeHost("/other/root", logs));
  assertEquals(enginePaths().dbFile, "/other/root/core.sqlite");
  __detachHostForTesting();
});

Deno.test("hash: sha256HexSync matches the canonical vectors (node/subtle parity)", () => {
  // FIPS 180-2 test vectors.
  assertEquals(
    sha256HexSync(""),
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
  );
  assertEquals(
    sha256HexSync("abc"),
    "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
  );
  // Byte input and equivalent string input agree.
  assertEquals(sha256HexSync(new TextEncoder().encode("abc")), sha256HexSync("abc"));
  assertEquals(toHex(new Uint8Array([0, 15, 255])), "000fff");
});

Deno.test("ids: generators produce distinct, well-formed ids", () => {
  const a = newMessageId();
  const b = newMessageId();
  assertEquals(a === b, false);
  assertEquals(a.length, 26); // ULID
  // newRequestId is a UUID v4.
  const uuid = newRequestId();
  assertEquals(/^[0-9a-f-]{36}$/.test(uuid), true);
});

Deno.test("errors: AppError carries code + status; guards behave", () => {
  const err = new AppError("not_found", "nope");
  assertEquals(err.code, "not_found");
  assertEquals(err.status, 404);
  assertEquals(isAppError(err), true);
  assertEquals(isAppError(new Error("plain")), false);
  assertEquals(isNoSpaceError(new Error("No space left on device")), true);
  assertEquals(isNoSpaceError(new Error("something else")), false);
});
