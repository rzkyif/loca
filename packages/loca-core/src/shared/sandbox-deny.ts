// The core's secret / private paths that a sandboxed child - a tool worker OR a
// deno-runtime MCP server - must never read or write, even when it runs with a
// broad grant (or `--allow-all`). Deno's `--deny-*` flags take PRECEDENCE over
// any `--allow-*`, so passing these as `--deny-read` / `--deny-write` backstops
// any broad grant. Single-sourced here so the tool-worker sandbox and the MCP
// deno spawn can't drift apart in what they protect.
//
// A blanket deny of `root` isn't usable because sessions live under it (and a
// tool may be granted $sessions), so the sensitive subtrees and files are
// enumerated explicitly, including the transient/legacy siblings.

import { join } from "@std/path";
import { paths } from "../paths.ts";

export function coreSecretDenyPaths(): string[] {
  const p = paths();
  return [
    p.secretsEncFile,
    p.secretsEncFile + ".tmp", // transient write target during re-encrypt
    p.secretsPlainFile, // legacy plaintext path (declared but unused)
    join(p.root, ".master-key"),
    p.adminTokenFile,
    p.adminPasswordFile,
    p.dbFile,
    p.dbFile + "-wal",
    p.dbFile + "-shm",
    p.dbFile + "-journal", // non-WAL fallback journal
    // Every extension's private SQLite db and the memory store are reached ONLY
    // through the core-side module broker (proxied over stdio), so a sandboxed
    // child never needs fs access to them. Deny the whole subtrees so one granted
    // a broad ancestor (e.g. $home, which contains ~/.loca) still can't read
    // another extension's data or the memory store off disk.
    join(p.root, "extension-data"),
    p.memoriesDir,
  ];
}
