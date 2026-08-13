// launchApp: the high-level entry point for a headless E2E test. It spawns a
// real core (via the Node command bridge), installs the E2E platform, optionally
// runs the real pairing flow so the app boots already connected, mounts the real
// +page.svelte app shell in the browser, and returns page objects + dispose.

import { commands } from "vitest/browser";
import { render } from "vitest-browser-svelte";
import { mintCodeWithAdminToken, pairWithCode } from "@client/lib/core/pairing.ts";
import { cores, type PairedCoreEntry } from "@client/lib/core/cores.ts";
import { viewState } from "@client/state/index.ts";
import { installE2ePlatform, registerCorePin } from "./platform-e2e.ts";
import E2eApp from "./E2eApp.svelte";
import type { LlmScript, RecordedLlmRequest } from "./mock-services.ts";
import { ChatPage } from "./pages/chat.ts";
import { NavPage } from "./pages/nav.ts";
import { PairingPage } from "./pages/pairing.ts";
import { SettingsPage } from "./pages/settings.ts";
import { CoreBarPage } from "./pages/core-bar.ts";

const CLIENT_NAME = "e2e-client";

/** Run the real pairing PAKE against a core and register it in the cores list.
 *  Returns the new client's id + bearer token (the token lets a spec / the seed
 *  hit the REST API directly before any core is selected). */
async function pairCoreInto(
  baseUrl: string,
  adminToken: string,
  name: string,
): Promise<{ clientId: string; token: string }> {
  const { code } = await mintCodeWithAdminToken(baseUrl, adminToken);
  // E2E cores are self-signed on loopback, never behind an HTTPS proxy.
  const res = await pairWithCode(baseUrl, CLIENT_NAME, code, false);
  const entry: PairedCoreEntry = {
    id: res.clientId,
    name,
    baseUrl,
    trustMode: res.trustMode,
    tlsPin: res.tlsPin,
    addedAtMs: Date.now(),
  };
  await cores().addPaired(entry, res.token);
  return { clientId: res.clientId, token: res.token };
}

// Collect uncaught errors / unhandled rejections so a spec can assert a flow
// (e.g. navigation across modes) produced no render crash. Installed once.
const uncaughtErrors: string[] = [];
let errorHookInstalled = false;
function installErrorHook(): void {
  if (errorHookInstalled) return;
  errorHookInstalled = true;
  window.addEventListener("error", (e) => uncaughtErrors.push(String(e.message ?? e.error)));
  window.addEventListener("unhandledrejection", (e) =>
    uncaughtErrors.push(String((e as PromiseRejectionEvent).reason)),
  );
}

export interface LaunchOptions {
  /** "paired": boot already connected to the core. "fresh": no paired core, the
   *  app locks to the pairing wizard (welcome remote-connect page). "installer":
   *  no paired core, but the spawned core presents as an installer-provided
   *  LOCAL core, so first boot silently pairs with it. Default "paired". */
  scenario?: "paired" | "fresh" | "installer";
  /** Initial mock LLM behaviour (default: echo). */
  llm?: LlmScript;
  /** Extra core settings.json entries (destination: core). */
  settings?: Record<string, unknown>;
  /** Client-local settings seeded into the E2E platform's clientFiles before
   *  mount, so `client-on-client` keys (greetings.*, appearance.*, shortcuts.*)
   *  are in effect during the app's onMount boot - the point where, e.g., the
   *  greeting gate reads them. Core-destination keys go in `settings`. */
  clientSettings?: Record<string, unknown>;
  /** Model files present (default) or absent (downloader flow). */
  models?: "present" | "absent";
  /** Sidecar binaries present (default) or absent. */
  binaries?: "present" | "absent";
  /** Declarative starting state, seeded over REST after pairing but before the
   *  app mounts, so a spec can express "an app with N existing sessions" instead
   *  of scripting the clicks. Extend with the same pre-mount pattern (memories,
   *  snippets, pending downloads) as specs need them. Only applies when paired. */
  seed?: {
    /** Create this many empty sessions on the core before mount. */
    sessions?: number;
  };
}

export interface AppHandle {
  coreId: string;
  baseUrl: string;
  adminToken: string;
  /** This client's id on the primary core (the pairing PAKE result), for specs
   *  that revoke it. Undefined in the "fresh" (unpaired) scenario. */
  clientId?: string;
  chat: ChatPage;
  nav: NavPage;
  pairing: PairingPage;
  settings: SettingsPage;
  coreBar: CoreBarPage;
  /** Spawn + pair a second core into this same app (for multi-core / isolation /
   *  security specs). The new core has its OWN mock LLM (pass `llm` to script
   *  it). Returns the new core's ids; it is stopped on dispose too. */
  pairAnotherCore(opts?: {
    name?: string;
    llm?: LlmScript;
  }): Promise<{ coreId: string; baseUrl: string; clientId: string }>;
  /** Re-script the mock LLM mid-test. */
  setLlm(script: LlmScript): Promise<void>;
  /** Does a path under the core's home exist (for download assertions)? */
  coreFileExists(relPath: string): Promise<boolean>;
  /** The chat-completion requests core has sent to the mock (model, system
   *  prompt, tools) - for prompt/dual-model/tool assertions. */
  llmRequests(): Promise<RecordedLlmRequest[]>;
  /** The core's recent stderr lines (failure diagnostics). */
  coreLogs(): Promise<string[]>;
  /** Kill the core subprocess (keeping its home + port) to simulate a blip; the
   *  client should drop the WS and start reconnecting. */
  killCore(): Promise<void>;
  /** Bring a killed core back on the same home + port (same TLS pin), so the
   *  client reconnects without re-pairing. */
  bringCoreBack(): Promise<void>;
  /** Uncaught errors / unhandled rejections observed since launch. */
  uncaughtErrors(): string[];
  dispose(): Promise<void>;
}

export async function launchApp(opts: LaunchOptions = {}): Promise<AppHandle> {
  const scenario = opts.scenario ?? "paired";
  installErrorHook();
  uncaughtErrors.length = 0;
  const launched = await commands.launchCore({
    llm: opts.llm,
    settings: opts.settings,
    models: opts.models,
    binaries: opts.binaries,
  });
  const { id: coreId, baseUrl, adminToken, tlsPin } = launched;
  const spawnedCoreIds = [coreId];

  installE2ePlatform({
    tlsPin,
    clientFiles: opts.clientSettings ? { settings: opts.clientSettings } : undefined,
    // Any scenario with a usable loopback core presents it as installed on
    // disk: the app's boot counts a loopback-only paired core only when
    // isLocalCoreInstalled() confirms the binary (and the deferred phase
    // prunes it otherwise), and the spawned harness core IS that local core.
    // "fresh" leaves it unset so the welcome remote-connect page shows.
    localCore: scenario === "fresh" ? undefined : { adminToken, baseUrl },
  });
  registerCorePin(baseUrl, tlsPin);

  let clientId: string | undefined;
  if (scenario === "paired") {
    const primary = await pairCoreInto(baseUrl, adminToken, "e2e-core");
    clientId = primary.clientId;
    // Pre-mount declarative seeding over REST with the pairing token (no core is
    // selected yet, so this can't go through cores().api()).
    for (let i = 0; i < (opts.seed?.sessions ?? 0); i++) {
      await fetch(`${baseUrl}/api/v1/sessions`, {
        method: "POST",
        headers: { authorization: `Bearer ${primary.token}`, "content-type": "application/json" },
        body: "{}",
      }).then((r) => r.body?.cancel());
    }
  }

  // Mount the real app shell. Its onMount boot reads the cores list: 0 cores ->
  // locks to the pairing wizard; >=1 -> restoreSelected() connects and lands in
  // chat. We do not drive navigation here; the app drives itself.
  render(E2eApp);

  const app: AppHandle = {
    coreId,
    baseUrl,
    adminToken,
    clientId,
    chat: new ChatPage(),
    nav: new NavPage(),
    pairing: new PairingPage(),
    settings: new SettingsPage(),
    coreBar: new CoreBarPage(),
    async pairAnotherCore(extraOpts = {}) {
      const { name = "e2e-core-2", llm } = extraOpts;
      const extra = await commands.launchCore({ llm });
      spawnedCoreIds.push(extra.id);
      registerCorePin(extra.baseUrl, extra.tlsPin);
      const { clientId } = await pairCoreInto(extra.baseUrl, extra.adminToken, name);
      return { coreId: extra.id, baseUrl: extra.baseUrl, clientId };
    },
    async setLlm(script) {
      await commands.setLlmScript(coreId, script);
    },
    async coreFileExists(relPath) {
      return commands.coreFileExists(coreId, relPath);
    },
    async llmRequests() {
      return commands.getLlmRequests(coreId);
    },
    async coreLogs() {
      return commands.getCoreLogs(coreId);
    },
    async killCore() {
      await commands.killCore(coreId);
    },
    async bringCoreBack() {
      await commands.bringCoreBack(coreId);
    },
    uncaughtErrors() {
      return [...uncaughtErrors];
    },
    async dispose() {
      // Grab the core's logs BEFORE stopping it, so a failure throw below can
      // surface what the core was doing (a bare suite otherwise yields a timeout
      // with no cause).
      let coreLogs: string[] = [];
      try {
        coreLogs = await commands.getCoreLogs(coreId);
      } catch {
        /* core already gone */
      }
      try {
        for (const c of await cores().list()) await cores().removePaired(c.id);
      } catch {
        /* */
      }
      viewState.setLocked(false);
      for (const id of spawnedCoreIds) await commands.stopCore(id);
      // Fail the spec if the run produced any uncaught error / unhandled
      // rejection (a silent render crash). Cleanup above always runs first so a
      // throw here never leaks a core. Happy-path specs must stay error-clean;
      // a spec that intentionally provokes an error should assert + clear it.
      if (uncaughtErrors.length > 0) {
        const errs = [...uncaughtErrors];
        uncaughtErrors.length = 0;
        const tail = coreLogs.slice(-40).join("\n");
        throw new Error(
          `uncaught errors during the test:\n  ${errs.join("\n  ")}` +
            (tail ? `\n--- core logs (tail) ---\n${tail}` : ""),
        );
      }
    },
  };
  return app;
}
