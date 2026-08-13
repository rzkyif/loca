// The runtime-abstraction layer the engine is built on. Every stateful or
// OS-touching capability the portable services need is reached through a `Host`
// the embedder supplies: the Deno service (`@loca/core`) wires a `DenoHost`; a
// future mobile client wires a webview host. The engine itself imports no
// `Deno.*`, `@db/sqlite`, `@tauri-apps/*`, or `node:*` (enforced by the
// `loca/no-host-import` lint rule), so the same source runs in either runtime.

import type {
  McpPrompt,
  McpResource,
  McpServer,
  McpServerInput,
  ScheduledPromptDraft,
} from "@loca/shared";
import type { ToolHost } from "./services/tool-host.ts";

export type LogLevel = "debug" | "info" | "warn" | "error";

// What the running host can do. The engine gates subsystems on these rather than
// assuming a Deno service: a webview host leaves them all false and the engine
// never reaches sidecar / subprocess code.
export interface HostCapabilities {
  // Local model / STT / TTS / embedding inference is available (a Deno service
  // that can spawn sidecars). False on mobile: the engine routes to external
  // providers only.
  localInference: boolean;
  // The host can spawn subprocesses (tool-worker sandbox, stdio MCP, helper
  // binaries). False on mobile.
  subprocess: boolean;
  // Remote (HTTP-transport) MCP servers are reachable.
  remoteMcp: boolean;
}

// Options for an atomic write. `restrictPermissions` asks the host to make the
// file owner-only (POSIX 0600 on desktop); a host whose storage is already
// app-private (mobile) may ignore it.
export interface WriteOpts {
  restrictPermissions?: boolean;
}

// Async filesystem. Atomic writers do tmp-file + rename, matching the core's
// existing writeAtomic. `stat` returns null for a missing path rather than
// throwing, so callers branch without catching.
export interface HostFs {
  readTextFile(path: string): Promise<string>;
  writeTextFileAtomic(path: string, text: string, opts?: WriteOpts): Promise<void>;
  readFile(path: string): Promise<Uint8Array>;
  writeFileAtomic(path: string, bytes: Uint8Array, opts?: WriteOpts): Promise<void>;
  stat(path: string): Promise<{ size: number; isDir: boolean } | null>;
  mkdir(path: string, opts?: { recursive?: boolean }): Promise<void>;
  remove(path: string, opts?: { recursive?: boolean }): Promise<void>;
  rename(from: string, to: string): Promise<void>;
  readDir(path: string): Promise<Array<{ name: string; isDir: boolean }>>;
}

// A value bindable to a SQL statement parameter. Matches the native driver's
// accepted set so the engine stays driver-agnostic without importing one.
export type SqlBindValue =
  | boolean
  | number
  | bigint
  | string
  | null
  | undefined
  | Uint8Array
  | Date;

// Synchronous SQLite: the exact subset the core's ~250 db() call sites use, so
// moving them behind the host changes no call site. The Deno host adapts
// `@db/sqlite`; a mobile host adapts an in-memory WASM SQLite with a debounced
// flush through `HostFs`.
export interface HostDb {
  exec(sql: string): void;
  prepare(sql: string): HostStmt;
  close(): void;
}

// One prepared statement. `run` returns the affected-row count (the module
// broker's execute op reads it). `get`/`all` assert the caller's row type,
// exactly as the callers already do against the native driver.
export interface HostStmt {
  get<T = unknown>(...params: SqlBindValue[]): T | undefined;
  all<T = unknown>(...params: SqlBindValue[]): T[];
  run(...params: SqlBindValue[]): number;
  value<T = unknown>(...params: SqlBindValue[]): T | undefined;
  finalize(): void;
}

// Secret storage for the vault master key (and nothing else): the OS keychain on
// desktop, the platform secure store on mobile.
export interface HostSecureStore {
  get(key: string): Promise<string | null>;
  set(key: string, value: string): Promise<void>;
  delete(key: string): Promise<void>;
}

// Local (on-device) inference endpoints, present only on a host that can run the
// llama / embed sidecars. Absent entirely on a host without local inference
// (mobile), where LLM + embeddings are external-only.
export interface LocalEndpoints {
  // Default loopback port for the local llama-server, used as the fallback when
  // the user hasn't overridden llm.port (channel-adjusted on desktop).
  llmDefaultPort(): number;
  // The loopback embed sidecar's OpenAI-style endpoint + model id, or null when
  // the local embedding model isn't available (so the caller falls back /
  // degrades).
  embedEndpoint(): Promise<{ url: string; model: string } | null>;
  // Local speech-to-text via the speech sidecar, queued behind the single engine
  // for fair multi-client behavior (`clientId` keys that fairness). `signal`
  // bounds/cancels the call.
  transcribe(audio: File, clientId: string, signal?: AbortSignal): Promise<string>;
  // Local text-to-speech (Kokoro) to WAV bytes, queued the same way.
  synthesize(
    text: string,
    voice: string | undefined,
    speed: number | undefined,
    clientId: string,
  ): Promise<Uint8Array>;
  // The local speech sidecar's lifecycle, for the stt/tts `/status` polls. A host
  // without local inference omits localEndpoints entirely, so those routes treat
  // its absence as "not running".
  speechStatus(): { running: boolean; loading: boolean };
  // The voices of the currently-selected local TTS model (from the signed model
  // catalog), for the client's voice dropdown.
  ttsVoices(
    settings: Record<string, unknown>,
  ): Promise<Array<{ id: string; label: string; lang?: string }>>;
}

// Engine -> host notifications and lifecycle hooks the chat graph fires: status
// telemetry (broadcast to clients), the local-model idle lifecycle (no-ops on a
// host without local inference), and scheduled-prompt creation. A mobile host
// supplies no-op status/idle and its own scheduler (or omits it).
export interface StatusHost {
  // Report the current active chat-stream count (status view).
  noteActiveStreams(n: number): void;
  // Report the local LLM queue depth (active + queued).
  noteLlmQueue(active: number, queued: number): void;
  // Reload the local model if idle-unloading stopped it; no-op for an external
  // provider or a host without local inference.
  ensureLocalModelLoaded(settings: Record<string, unknown>): Promise<void>;
  // Cancel any pending idle-unload at the start of a turn.
  noteLlmActivity(): void;
  // Arm the idle-unload timer when the last active turn ends.
  onTurnEnd(activeStreams: number): void;
  // Persist a user-confirmed scheduled prompt.
  createScheduledPrompt(clientId: string, draft: ScheduledPromptDraft): void;
  // Queue background (re)indexing of a memory's summary + embedding, or of every
  // memory when no id is given. The desktop host runs it on the idle-gated
  // background queue; a host without one may index inline or defer.
  scheduleMemoryIndexing(memoryId?: string): void;
}

// Remote-MCP server administration behind the /api/v1/mcp routes: the persisted
// registry (CRUD + per-tool/prompt enablement), the live connection manager
// (reconcile / disconnect), the OAuth authorization-code flow, and the vault key
// names for a server's stored credentials. The desktop host wires its full MCP
// subsystem (stdio + remote + OAuth); a mobile host wires a remote-HTTP-only
// implementation but the same shape, so OAuth is supported by the contract even
// where a given host defers it. The route orchestrates; these are the primitives.
export interface McpAdminHost {
  list(): McpServer[];
  listPrompts(): McpPrompt[];
  listResources(): McpResource[];
  // Throws not_found if the id is unknown.
  get(id: string): McpServer;
  create(input: McpServerInput): McpServer;
  update(id: string, patch: Partial<McpServerInput>): McpServer;
  delete(id: string): void;
  setToolEnabled(id: string, tool: string, enabled: boolean): McpServer;
  setPromptEnabled(id: string, prompt: string, enabled: boolean): McpServer;
  // Set whether an MCP tool is always-available (default true). When off, the
  // tool folds into the relevance filter instead of being offered every turn.
  setToolAlwaysAvailable(id: string, tool: string, alwaysAvailable: boolean): McpServer;
  // Fetch a prompt from a connected server and flatten its messages into one
  // instruction string, applying the given arguments. Used to resolve a
  // `/prompt` reference into the turn's system prompt when the message is sent.
  resolvePrompt(id: string, prompt: string, args: Record<string, string>): Promise<string>;
  // Reconcile live connections to the current enabled set (connect newly-enabled,
  // drop disabled). Called after any change that affects connectivity.
  resync(): Promise<void>;
  // Drop a server's live session (so a following resync reconnects with new config).
  disconnect(id: string): Promise<void>;
  // Begin the OAuth authorization-code flow; resolves with the URL to open, or
  // null when stored tokens already work. `onComplete` fires later when the
  // browser redirect lands (true = authorized).
  startOAuth(
    id: string,
    url: string,
    onComplete: (ok: boolean) => void,
  ): Promise<{ authorizationUrl: string | null }>;
  cancelOAuth(id: string): void;
  // Vault key names for a server's stored bearer token / OAuth tokens.
  authSecretName(id: string): string;
  oauthSecretName(id: string): string;
}

// Everything the engine needs from its runtime. Injected once at init().
export interface Host {
  // Root of the engine's on-disk state; EnginePaths derives every file/dir from
  // it. Desktop passes the core root; mobile passes an app-private dir.
  rootDir: string;
  // Environment / config lookup, replacing Deno.env.get.
  config(key: string): string | undefined;
  capabilities: HostCapabilities;
  fs: HostFs;
  // Open (or return the cached handle for) a SQLite database at an absolute
  // path: the main core.sqlite plus any per-extension database.
  openDb(absPath: string): HostDb;
  secureStore: HostSecureStore;
  // Local inference endpoint provider, present only when the host can run the
  // llama / embed sidecars (capabilities.localInference). Absent on external-only
  // hosts (mobile).
  localEndpoints?: LocalEndpoints;
  // Tool catalog + execution provider (extension registry + worker sandbox + MCP
  // client). A mobile host supplies a remote-MCP-only implementation.
  tools?: ToolHost;
  // Status telemetry + local-model idle lifecycle + scheduled prompts.
  status?: StatusHost;
  // Remote-MCP server administration (registry + manager + OAuth). Present on any
  // host that offers MCP (desktop and mobile both do).
  mcp?: McpAdminHost;
  log(level: LogLevel, scope: string, message: string): void;
  // Current wall-clock ms; defaults to Date.now when omitted.
  now?(): number;
}
