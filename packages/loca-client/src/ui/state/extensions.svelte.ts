/**
 * Client-side extension registry: a reactive mirror of the core's installed
 * extensions and a thin pass-through for the npm-search install flow. The core
 * now owns tool execution end-to-end (the LLM hop loop runs server-side), so
 * this module no longer dispatches tool calls or proxies the per-call WS
 * channel itself - it just:
 *
 *   - keeps `installed`, `searchResults`, and `installJobs` in sync from
 *     `cores().api().extensions.*` REST calls and the `extension.*` /
 *     `tool.*` WS frames
 *   - exposes `findExtensionForTool` for any consumer that still wants to map a
 *     tool name to its owning extension
 *   - sends the two client-originating tool frames (`tool.askuser_response`
 *     and `tool.cancel`) through the active CoreClient
 *
 * The settings UI is the only writer; the chat-loop side just reads.
 */

import {
  BUILTIN_EXTENSION_ID,
  type DownloadExtensionRequest,
  type Extension,
  type ExtensionSearchResult,
  type Grant,
  type ServerToClientFrame,
  type Tool,
  type UndeclaredPolicy,
} from "@loca/shared";
import type { AskUserAnswer } from "$lib/util/types";
import { cores } from "$lib/core";
import { getLogger } from "$lib/util/log";
import { Subscriptions } from "$lib/util/subscriptions";
import { messagesState } from "./messages.svelte";
import { streamingState } from "./streaming.svelte";

const log = getLogger("extensions");

export type InstallJob = {
  /** Job id from `extensions.download` / `installDeps` - identifies the stream. */
  id: string;
  /** Extension id the job is installing (may be unknown until install_done). */
  extensionId: string;
  /** Display name used by the UI while the install runs; falls back to id. */
  label: string;
  lines: { stream: "stdout" | "stderr"; line: string }[];
  status: "running" | "done" | "failed";
};

class ExtensionsState {
  /** The flat list returned by `GET /api/v1/extensions`. Replaces the old
   *  trusted/untrusted split - the new core treats every installed extension
   *  as trusted (the gate is now per-permission grants, not a global
   *  trust flag). */
  installed = $state<Extension[]>([]);
  /** Flat list of every tool across all providers, for the Tools manager. */
  allTools = $state<Tool[]>([]);
  searchResults = $state<ExtensionSearchResult[]>([]);
  /** Keyed by job id. The core's `extension.install_log` and `install_done`
   *  frames key off `jobId`, so we mirror that here. */
  installJobs = $state<Record<string, InstallJob>>({});
  /** Mirrors the active core's WS connection state. Read by the settings
   *  UI to show a placeholder when the install action would be a no-op. */
  wsConnected = $state<boolean>(false);
  /** Latest-version status per extension id, populated by `checkUpdates()`.
   *  Transient (session-only); drives the "Update available" badge + action and
   *  the `@update-available` filter. */
  updateStatus = $state<Record<string, { latestVersion: string | null; updateAvailable: boolean }>>(
    {},
  );

  /** Whether the built-in extension is currently installed. */
  get isBuiltinInstalled(): boolean {
    return this.installed.some((t) => t.id === BUILTIN_EXTENSION_ID);
  }

  /** Whether the built-in is present on disk but its tools aren't installed yet
   *  (status 'downloaded'). This is the state the Tools install prompt targets;
   *  if the built-in is absent (user deleted it) this is false. */
  get isBuiltinPendingInstall(): boolean {
    return this.installed.some((t) => t.id === BUILTIN_EXTENSION_ID && t.status === "downloaded");
  }

  private subs = new Subscriptions();
  private hydrated = false;
  /** Resolvers awaiting a job's terminal state, keyed by job id (see awaitJob). */
  private jobWaiters = new Map<string, (job: InstallJob) => void>();

  /** Subscribe to the active core's WS feed and seed the installed list.
   *  Called from `+page.svelte` onMount (the spec talks about `attach()`).
   *  The CoreClient already handles reconnects internally; this method is
   *  idempotent so re-mounts don't double-subscribe. */
  attach(): void {
    this.subs.attach(() => {
      this.hydrated = true;
      return [
        cores().subscribeWs((f) => this.onFrame(f)),
        cores().subscribeConnectionState((state) => {
          this.wsConnected = state === "connected";
          // First successful connection: hydrate the installed list. The
          // CoreClient emits "connected" on every reconnect too, so we re-fetch
          // each time to pick up anything that changed while we were offline.
          if (state === "connected") {
            void this.refresh().catch((err) => log.warn("hydrate on ws connect failed:", err));
          }
        }),
      ];
    });
  }

  detach(): void {
    this.subs.detach();
    this.hydrated = false;
  }

  // --- WS frame routing --------------------------------------------------

  private onFrame(frame: ServerToClientFrame): void {
    switch (frame.kind) {
      case "extension.snapshot":
        void this.refresh().catch((err) => log.warn("refresh on snapshot failed:", err));
        return;
      case "extension.install_log": {
        const job = this.installJobs[frame.jobId];
        if (!job) return;
        job.lines.push({ stream: frame.stream, line: frame.line });
        // Cap per-job buffer so a runaway npm install (huge dependency tree,
        // npm warnings) can't pin arbitrary RSS on the webview.
        if (job.lines.length > 1000) {
          job.lines.splice(0, job.lines.length - 1000);
        }
        // Reassign the record so Svelte's $state proxy picks up the change
        // (mutating nested objects in-place isn't always tracked).
        this.installJobs = { ...this.installJobs, [frame.jobId]: { ...job } };
        return;
      }
      case "extension.install_done": {
        const job = this.installJobs[frame.jobId];
        if (job) {
          const updated: InstallJob = {
            ...job,
            status: frame.ok ? "done" : "failed",
            extensionId: frame.id || job.extensionId,
          };
          this.installJobs = { ...this.installJobs, [frame.jobId]: updated };
          // Wake anyone awaiting this job (loading state + transient failure UI).
          const waiter = this.jobWaiters.get(frame.jobId);
          if (waiter) {
            this.jobWaiters.delete(frame.jobId);
            waiter(updated);
          }
        }
        // A successful (re)install clears any stale update flag for that id so
        // the "Update available" badge/action disappears.
        if (frame.ok && frame.id && this.updateStatus[frame.id]) {
          const { [frame.id]: _gone, ...rest } = this.updateStatus;
          this.updateStatus = rest;
        }
        // Refresh so the new extension (or the failure state) is reflected.
        void this.refresh().catch((err) => log.warn("refresh after install failed:", err));
        return;
      }
      // tool.* frames are routed to messagesState by streaming.svelte.ts so
      // the active tool-call bubble updates. We don't need to mirror them
      // here - the spec calls out emitting via a per-callId emitter, but the
      // new arch has already moved that listener up into streaming/messages.
      default:
        return;
    }
  }

  // --- CRUD --------------------------------------------------------------

  /** Pull the latest extension list from the core. */
  async refresh(): Promise<void> {
    const list = await cores().api().extensions.list();
    // The list endpoint never embeds tools (they're lazy-loaded per extension), so
    // carry over any tools already loaded. Otherwise a snapshot/refresh while a
    // detail view is open would blank its tool list and leave it spinning.
    const prev = new Map(this.installed.map((t) => [t.id, t.tools]));
    this.installed = list.map((t) => (t.tools ? t : { ...t, tools: prev.get(t.id) }));
  }

  /** npm-registry search; results land in `searchResults`. Empty query
   *  clears the result list without a round-trip. */
  async search(q: string): Promise<void> {
    const trimmed = q.trim();
    if (!trimmed) {
      this.searchResults = [];
      return;
    }
    const res = await cores().api().extensions.search(trimmed);
    this.searchResults = res.results;
  }

  /** Phase 1: download (acquire) a extension's files. Returns the job id so the
   *  caller can correlate it with a log panel. Optimistically registers an
   *  `InstallJob` row so the running indicator shows up immediately. The extension
   *  lands in status 'downloaded'; the user then calls installDeps. */
  async download(req: DownloadExtensionRequest): Promise<string> {
    const label =
      req.source === "npm" ? req.name : req.source === "seeded" ? req.id : (req.slug ?? req.path);
    const res = await cores().api().extensions.download(req);
    this.registerJob(res.jobId, res.extensionId, label);
    return res.jobId;
  }

  /** Phase 2: install a downloaded extension's deps + pin its hash. Registers a
   *  second streamed job; the install_log/install_done handling is phase-agnostic. */
  async installDeps(extensionId: string): Promise<string> {
    const label = this.installed.find((t) => t.id === extensionId)?.displayName || extensionId;
    const res = await cores().api().extensions.installDeps(extensionId);
    this.registerJob(res.jobId, res.extensionId, label);
    return res.jobId;
  }

  private registerJob(jobId: string, extensionId: string, label: string): void {
    this.installJobs = {
      ...this.installJobs,
      [jobId]: { id: jobId, extensionId, label, lines: [], status: "running" },
    };
  }

  /** Resolve once a download/install/update job reaches a terminal state
   *  (resolves immediately if it already has). Lets the UI hold a loading state
   *  for the whole async job and surface its failure (the job's stderr) once. */
  awaitJob(jobId: string): Promise<InstallJob> {
    const job = this.installJobs[jobId];
    if (job && job.status !== "running") return Promise.resolve(job);
    return new Promise((resolve) => this.jobWaiters.set(jobId, resolve));
  }

  /** True while a download/install/update job for this extension id is running.
   *  Drives the detail-view action loading state. */
  isJobRunning(extensionId: string): boolean {
    return Object.values(this.installJobs).some(
      (j) => j.status === "running" && j.extensionId === extensionId,
    );
  }

  /** Remove a extension entirely (files + rows). */
  async deleteExtension(id: string): Promise<void> {
    await cores().api().extensions.delete(id);
    await this.refresh();
  }

  /** Revert an installed extension to 'downloaded' (drop its installed deps); the
   *  source stays so it can be re-installed. Refresh so the status + tools
   *  reflect the change in an open detail view. */
  async uninstall(id: string): Promise<void> {
    await cores().api().extensions.uninstall(id);
    await this.refresh();
    await this.refreshTools(id);
  }

  /** Download the built-in extension from the CDN (codebase in dev). */
  async downloadBuiltin(): Promise<string> {
    return await this.download({ source: "seeded", id: BUILTIN_EXTENSION_ID });
  }

  /** Install the built-in's tools at the user's request (the Tools prompt).
   *  Returns `{ queued: true }` when the worker runtime isn't downloaded yet, in
   *  which case the install runs automatically once it lands. */
  async installBuiltinToolkit(): Promise<{ queued: boolean }> {
    return await cores().api().extensions.installBuiltin();
  }

  /** Update a extension to its latest version (npm registry or built-in CDN
   *  manifest). The core streams progress + a `extension.install_done` frame.
   *  Registers a job (like installDeps) so the caller can await + show loading. */
  async updateExtension(id: string): Promise<string> {
    const label = this.installed.find((t) => t.id === id)?.displayName || id;
    const res = await cores().api().extensions.update(id);
    this.registerJob(res.jobId, res.extensionId, label);
    return res.jobId;
  }

  /** Check installed extensions for newer versions and store the result so the UI
   *  can flag + filter the updatable ones. */
  async checkUpdates(): Promise<void> {
    const { results } = await cores().api().extensions.checkUpdates();
    const next: Record<string, { latestVersion: string | null; updateAvailable: boolean }> = {};
    for (const r of results) {
      next[r.id] = {
        latestVersion: r.latestVersion,
        updateAvailable: r.updateAvailable,
      };
    }
    this.updateStatus = next;
  }

  /** Reconcile the core's extensions directory with its registry (registers
   *  dropped-in folders, prunes removed ones). The core broadcasts
   *  `extension.snapshot`, so the installed list refreshes via onFrame. */
  async rescan(): Promise<{ added: number; updated: number; removed: number }> {
    return await cores().api().extensions.rescan();
  }

  /** Recompute tool-relevance embeddings for every enabled tool. */
  async reindex(): Promise<{ embedded: number }> {
    return await cores().api().extensions.reindex();
  }

  /** Load the flat all-providers tool list for the Tools manager. */
  async loadAllTools(): Promise<void> {
    const { tools } = await cores().api().extensions.listAllTools();
    this.allTools = tools;
  }

  async enableTool(extensionId: string, toolName: string): Promise<void> {
    await cores().api().extensions.enableTool(extensionId, toolName);
    await this.refreshTools(extensionId);
    await this.loadAllTools();
  }

  async disableTool(extensionId: string, toolName: string): Promise<void> {
    await cores().api().extensions.disableTool(extensionId, toolName);
    await this.refreshTools(extensionId);
    await this.loadAllTools();
  }

  /** Override whether a tool is offered every turn (bypassing relevance
   *  selection). Refresh so the tool detail reflects the new state at once. */
  async setToolAlwaysAvailable(
    extensionId: string,
    toolName: string,
    value: boolean,
  ): Promise<void> {
    await cores().api().extensions.setToolAlwaysAvailable(extensionId, toolName, value);
    await this.refreshTools(extensionId);
    await this.loadAllTools();
  }

  /** Re-pin the current on-disk content + clear the drift warning. Refresh so
   *  the extension returns to 'installed' with its (now disabled) tools shown. */
  async confirmReenable(extensionId: string): Promise<void> {
    await cores().api().extensions.confirmReenable(extensionId);
    await this.refresh();
    await this.refreshTools(extensionId);
  }

  /** Set grants for a single tool and refresh the extension's tool list so
   *  `missingRequired` reflects the new state immediately. Throws if the
   *  REST call fails (the UI shows the error inline). */
  async setGrants(
    extensionId: string,
    toolName: string,
    grants: Array<{ key: string; state: Grant["state"] }>,
  ): Promise<void> {
    await cores().api().extensions.setGrants(extensionId, toolName, grants);
    await this.refreshTools(extensionId);
    await this.loadAllTools();
  }

  /** Set the extension-level policy for undeclared runtime permission requests
   *  and splice the returned extension row in place. */
  async setUndeclaredPolicy(extensionId: string, policy: UndeclaredPolicy): Promise<void> {
    const updated = await cores().api().extensions.setUndeclaredPolicy(extensionId, policy);
    this.installed = this.installed.map((tk) =>
      tk.id === extensionId ? { ...tk, undeclaredPolicy: updated.undeclaredPolicy } : tk,
    );
  }

  /** Refresh the tools embedded inside a single extension row. The /extensions
   *  list endpoint doesn't embed tools (per the spec: tools are nested via
   *  /extensions/:id/tools), so after a mutation we splice the fresh tool
   *  list into the matching installed row. */
  private async refreshTools(extensionId: string): Promise<void> {
    try {
      const { tools } = await cores().api().extensions.listTools(extensionId);
      this.spliceTools(extensionId, tools);
    } catch (err) {
      log.warn(`tools refresh for ${extensionId} failed:`, err);
    }
  }

  /** Splice a fresh tool list into the matching installed row AND recompute the
   *  tool counts from it, so the "N enabled" badge tracks a per-tool toggle
   *  immediately (the counts otherwise only update on a full list refresh). */
  private spliceTools(extensionId: string, tools: Tool[]): void {
    const idx = this.installed.findIndex((t) => t.id === extensionId);
    if (idx < 0) return;
    this.installed[idx] = {
      ...this.installed[idx],
      tools,
      toolCount: tools.length,
      enabledToolCount: tools.filter((t) => t.enabled).length,
    };
    this.installed = [...this.installed];
  }

  /** Load the full tool list for a extension so the per-tool UI can render.
   *  Public so the settings panel can lazy-load tools when the user expands
   *  a extension row. */
  async loadTools(extensionId: string): Promise<Tool[]> {
    const { tools } = await cores().api().extensions.listTools(extensionId);
    this.spliceTools(extensionId, tools);
    return tools;
  }

  // --- WS-frame senders --------------------------------------------------

  /** Send the user's askUser answers back to the worker. Mirrors the choice
   *  onto the local tool-call bubble so the UI flips out of "awaiting_user"
   *  immediately instead of waiting for the next progress frame. */
  respondAskUser(callId: string, requestId: string, answers: AskUserAnswer[]): void {
    cores().api().chat.respondAskUser(callId, requestId, answers);
    messagesState.recordToolCallAskUserAnswers(callId, answers);
  }

  /** Cancel a single tool call. */
  cancelToolCall(callId: string): void {
    cores().api().chat.cancelTool(callId);
  }

  /** Send cancel frames for every tool call bubble still in a non-terminal
   *  status. Wired into `streamingState.interruptStreaming()` so the global
   *  stop affordance in UserInput tears down running AND awaiting-user tool
   *  calls in one shot. */
  cancelAllActiveCalls(): void {
    for (const m of messagesState.messages) {
      if (m.role !== "tool" || !m.callId) continue;
      if (m.status === "pending" || m.status === "running" || m.status === "awaiting_user") {
        this.cancelToolCall(m.callId);
      }
    }
  }

  // --- read-only helpers -------------------------------------------------

  /** Resolve the owning extension for a given tool name. Returns null if the
   *  tool isn't enabled in any installed extension. Used by the LLM hop loop
   *  (when one runs client-side) to associate a tool name with its extension
   *  id - the chat orchestration is now server-side, but we keep this for
   *  any UI code that still needs to look up a tool by name (e.g. drawing
   *  a label next to a pending tool call). */
  findExtensionForTool(toolName: string): { extensionId: string; toolId: string } | null {
    for (const tk of this.installed) {
      if (tk.status !== "installed") continue;
      const tool = tk.tools?.find((t) => t.name === toolName && t.enabled);
      if (tool) return { extensionId: tk.id, toolId: tool.id };
    }
    return null;
  }
}

export const extensionsState = new ExtensionsState();

// Cancel every active tool call when the user interrupts streaming. Registered
// here so streaming.svelte doesn't have to import this module (extensions already
// imports streamingState; one-way is the cycle-free shape).
streamingState.onInterrupt(() => extensionsState.cancelAllActiveCalls());
