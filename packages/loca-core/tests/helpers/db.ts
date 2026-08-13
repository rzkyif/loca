// Tempdir-isolated DB harness for tests. Creates a fresh sqlite file
// under a per-test temporary directory, points LOCA_CORE_HOME at it,
// runs migrations, and returns a teardown callback that closes the DB
// and removes the directory.
//
// Usage:
//   const env = await setupTestEnv();
//   try {
//     // ... tests using db()/paths()/repos/...
//   } finally {
//     await env.teardown();
//   }

import { attachHost } from "@loca/core-engine";
import { denoHost } from "../../src/host/deno-host.ts";
import { closeDb, db, openDb } from "@loca/core-engine";
import { ensureDirs } from "../../src/paths.ts";
import { migrate } from "../../src/db/migrate.ts";
import { newClientId } from "@loca/core-engine";
import { __resetForTesting as resetAuth } from "../../src/services/auth.ts";
import { __resetForTesting as resetSessionsRepo } from "@loca/core-engine/services/sessions-store";
import { __resetForTesting as resetSidecarManager } from "../../src/sidecars/manager.ts";
import { __resetForTesting as resetChat } from "@loca/core-engine/services/chat";
import { __resetForTesting as resetSecrets } from "@loca/core-engine/services/secrets";
import { __resetForTesting as resetTls } from "../../src/services/tls.ts";
import { __resetForTesting as resetWsHub } from "../../src/ws/hub.ts";
import { __resetForTesting as resetLlmScheduler } from "@loca/core-engine/services/llm-scheduler";
import { __resetForTesting as resetToolFilter } from "@loca/core-engine/services/tool-filter";
import { __resetForTesting as resetExtensionsRegistry } from "../../src/extensions/registry.ts";
import { __resetForTesting as resetWorkerPool } from "../../src/extensions/worker-pool.ts";
import { __resetForTesting as resetModelsManager } from "../../src/models/manager.ts";
import { __resetForTesting as resetBinariesManager } from "../../src/binaries/manager.ts";
import { __resetForTesting as resetDownloads } from "../../src/downloads/manager.ts";
import { __resetForTesting as resetCoreSettings } from "@loca/core-engine/services/core-settings";
import { __resetForTesting as resetBackgroundQueue } from "../../src/services/background-queue.ts";
import { __resetForTesting as resetMemoriesStore } from "@loca/core-engine/services/memories-store";
import { __resetForTesting as resetPromptScheduler } from "../../src/services/prompt-scheduler.ts";
import { __resetForTesting as resetMcpManager } from "../../src/mcp/manager.ts";
import * as log from "@std/log";

export interface TestEnv {
  /** Absolute path to the temporary LOCA_CORE_HOME for this test. */
  coreHome: string;
  /** Closes the DB, drops cached singletons, removes the tempdir, restores env. */
  teardown(): Promise<void>;
}

// Track which env vars we set so teardown can restore the prior value
// exactly (including the "was unset" case).
interface EnvSnapshot {
  key: string;
  prior: string | undefined;
}

// Initialize the logger exactly once per test process with a console-only
// setup. The production `initLogger()` also installs a RotatingFileHandler
// that opens a long-lived file handle under LOCA_CORE_HOME/logs/core.log;
// in tests the tempdir is removed after each test, leaving that handle
// dangling and tripping Deno's resource-leak sanitizer.
let _testLoggerReady = false;
function initTestLogger(): void {
  if (_testLoggerReady) return;
  _testLoggerReady = true;
  log.setup({
    handlers: { console: new log.ConsoleHandler("WARN") },
    loggers: { default: { level: "WARN", handlers: ["console"] } },
  });
}

/**
 * Inserts a synthetic paired-client row and returns its id. Use when a
 * test needs an `ownerClientId` that satisfies foreign-key constraints
 * without going through the full pairing-code flow.
 */
export function createTestClient(name = "test-client"): string {
  const id = newClientId();
  const now = Date.now();
  db()
    .prepare(`
    INSERT INTO clients (id, name, token_hash, created_at_ms, last_seen_ms, revoked)
    VALUES (?, ?, ?, ?, ?, 0)
  `)
    .run(id, name, `hash-${id}`, now, now);
  return id;
}

export async function setupTestEnv(): Promise<TestEnv> {
  initTestLogger();
  const coreHome = await Deno.makeTempDir({ prefix: "loca-test-" });
  const snapshot: EnvSnapshot = {
    key: "LOCA_CORE_HOME",
    prior: Deno.env.get("LOCA_CORE_HOME"),
  };
  Deno.env.set("LOCA_CORE_HOME", coreHome);
  // Install the engine runtime host so engine-hosted services resolve paths /
  // db / fs through the same DenoHost the real boot uses.
  attachHost(denoHost());
  await ensureDirs();
  openDb();
  migrate();

  return {
    coreHome,
    async teardown() {
      resetWsHub();
      resetPromptScheduler();
      closeDb();
      resetAuth();
      resetSessionsRepo();
      resetSidecarManager();
      resetChat();
      resetSecrets();
      resetTls();
      resetLlmScheduler();
      resetToolFilter();
      resetExtensionsRegistry();
      resetWorkerPool();
      resetModelsManager();
      resetBinariesManager();
      resetDownloads();
      resetCoreSettings();
      resetBackgroundQueue();
      resetMemoriesStore();
      resetMcpManager();
      if (snapshot.prior === undefined) Deno.env.delete(snapshot.key);
      else Deno.env.set(snapshot.key, snapshot.prior);
      await Deno.remove(coreHome, { recursive: true }).catch(() => {});
    },
  };
}
