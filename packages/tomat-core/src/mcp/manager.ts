// MCP connection manager: one client per enabled server. Connects over stdio
// (spawned subprocess) or remote streamable HTTP, fetches each server's
// tools/prompts/resources on connect, and exposes call/getPrompt/readResource.
// Capabilities are cached in memory; the registry merges them with the DB rows
// to project McpServer/McpPrompt/McpResource. The manager owns no persistence.

import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";
import {
  PromptListChangedNotificationSchema,
  ResourceListChangedNotificationSchema,
  ToolListChangedNotificationSchema,
} from "@modelcontextprotocol/sdk/types.js";
import type { Transport } from "@modelcontextprotocol/sdk/shared/transport.js";
import type { McpConnectionStatus, McpServer } from "@tomat/shared";
import { errMessage } from "@tomat/shared";
import { getLogger } from "../shared/log.ts";
import { coreSecretDenyPaths } from "../shared/sandbox-deny.ts";
import { getSecret } from "@tomat/core-engine/services/secrets";
import { mcpAuthSecretName } from "./secret-key.ts";
import { McpOAuthProvider } from "./oauth-provider.ts";
import { requireWorkerDeno } from "../sidecars/worker-deno.ts";
import { paths } from "../paths.ts";

const log = getLogger("mcp");

// A single MCP request (tool call, prompt fetch, resource read) must not hang
// the turn forever if a server accepts the connection but never responds. The
// SDK aborts the request when this elapses; callers fold the rejection into a
// failed tool result / dropped token rather than crashing the turn.
const REQUEST_TIMEOUT_MS = 30_000;

// Backoff steps for auto-reconnecting an enabled server after an unexpected
// drop. After the last step the manager gives up and leaves the server in
// `error` until the user hits Reconnect or a CRUD resync retries it.
const RECONNECT_BACKOFF_MS = [1_000, 2_000, 5_000, 15_000, 30_000];

export interface McpToolDef {
  name: string;
  description?: string;
  inputSchema: Record<string, unknown>;
}

export interface McpPromptDef {
  name: string;
  description?: string;
  arguments: Array<{ name: string; description?: string; required?: boolean }>;
}

export interface McpResourceDef {
  uri: string;
  name: string;
  description?: string;
  mimeType?: string;
}

interface Connection {
  client: Client;
  status: McpConnectionStatus;
  error?: string;
  tools: McpToolDef[];
  prompts: McpPromptDef[];
  resources: McpResourceDef[];
}

class McpManager {
  private conns = new Map<string, Connection>();
  // Status for servers that failed to connect (no live client).
  private failed = new Map<string, string>();
  // Servers whose connect() is in flight, so status() can report "connecting".
  private connecting = new Set<string>();
  // Serializes sync() so two overlapping CRUD requests can't both pass the
  // `conns.has` guard in connect() and spawn the same server twice (the second
  // would overwrite the first connection and leak its child process).
  private syncChain: Promise<void> = Promise.resolve();
  // The enabled/disabled set from the most recent sync, so an unexpected drop
  // can reconcile against current config without importing the registry (which
  // imports us). Kept current because every CRUD path calls resync -> sync.
  private desired: McpServer[] = [];
  // Pending auto-reconnect timers and their attempt index, keyed by server id.
  private reconnectTimers = new Map<string, ReturnType<typeof setTimeout>>();
  private reconnectAttempts = new Map<string, number>();
  // Fired after an async status or capability change the manager makes on its
  // own (a dropped connection, an auto-reconnect, a server's list_changed
  // notification) so clients repaint. A no-op until wired at boot; the route
  // layer already broadcasts on CRUD.
  private onChange: () => void = () => {};
  // How a transport is built for a server. Swappable in tests so a client can be
  // linked to an in-memory MCP server instead of spawning a process / reaching a
  // URL.
  private transportFactory: (server: McpServer) => Promise<Transport> = makeTransport;

  /** Register the snapshot-broadcast callback (wired at boot). */
  notifyOn(fn: () => void): void {
    this.onChange = fn;
  }

  /** Test seam: override transport creation. */
  setTransportFactory(fn: (server: McpServer) => Promise<Transport>): void {
    this.transportFactory = fn;
  }

  status(id: string): { status: McpConnectionStatus; error?: string } {
    const c = this.conns.get(id);
    if (c) return { status: c.status, error: c.error };
    if (this.connecting.has(id)) return { status: "connecting" };
    if (this.failed.has(id)) {
      return { status: "error", error: this.failed.get(id) };
    }
    return { status: "disconnected" };
  }

  capabilities(id: string): Pick<Connection, "tools" | "prompts" | "resources"> {
    const c = this.conns.get(id);
    return c
      ? { tools: c.tools, prompts: c.prompts, resources: c.resources }
      : { tools: [], prompts: [], resources: [] };
  }

  /** Connect every enabled server that isn't connected, and disconnect any
   *  connection whose server is now disabled or gone. Called on boot and after
   *  any server CRUD. Each connect is independent; one failure doesn't block
   *  the others. */
  sync(servers: McpServer[]): Promise<void> {
    // Chain onto the previous sync (run it even if the previous rejected) so
    // only one reconciliation touches `conns` at a time. Each call reconciles
    // against the latest desired state passed in.
    this.syncChain = this.syncChain.catch(() => {}).then(() => this.doSync(servers));
    return this.syncChain;
  }

  private async doSync(servers: McpServer[]): Promise<void> {
    this.desired = servers;
    const enabled = new Map(servers.filter((s) => s.enabled).map((s) => [s.id, s]));
    // Drop connections no longer wanted. Deleting the current key from a Map
    // during for-of iteration is safe (the spec won't revisit it).
    for (const id of this.conns.keys()) {
      if (!enabled.has(id)) await this.disconnect(id);
    }
    for (const id of this.failed.keys()) {
      if (!enabled.has(id)) this.failed.delete(id);
    }
    // Cancel a scheduled reconnect for any server no longer enabled. Deleting
    // the current key from a Map during for-of iteration is safe.
    for (const id of this.reconnectTimers.keys()) {
      if (!enabled.has(id)) this.cancelReconnect(id);
    }
    // Bring up newly enabled ones.
    await Promise.all(
      [...enabled.values()].filter((s) => !this.conns.has(s.id)).map((s) => this.connect(s)),
    );
  }

  async connect(server: McpServer): Promise<void> {
    if (this.conns.has(server.id)) return;
    this.failed.delete(server.id);
    this.connecting.add(server.id);
    const client = new Client(
      { name: "Tomat", version: "0.1.0" },
      {
        capabilities: {},
      },
    );
    try {
      const transport = await this.transportFactory(server);
      const readStderr = captureStderr(transport);
      await client.connect(transport).catch((err: unknown) => {
        // Spawn/handshake failure: fold in whatever the child printed to stderr
        // (a deno PermissionDenied from a too-narrow manual permission set, a
        // missing module) and map a missing launcher to an actionable hint, so
        // the user sees why instead of a bare "connection closed".
        throw new Error(describeConnectError(server, err, readStderr()));
      });
      // Fetch capabilities; a server may not implement every list method.
      const conn: Connection = {
        client,
        status: "connected",
        tools: await listTools(client),
        prompts: await listPrompts(client),
        resources: await listResources(client),
      };
      this.conns.set(server.id, conn);
      this.reconnectAttempts.delete(server.id);
      // Watch for a dropped connection or a server-pushed list change now that
      // this client is the active one.
      this.watchConnection(server.id, client);
      log.info(
        `connected ${server.name}: ${conn.tools.length} tools, ${conn.prompts.length} prompts, ` +
          `${conn.resources.length} resources`,
      );
    } catch (err) {
      this.failed.set(server.id, errMessage(err));
      try {
        await client.close();
      } catch {
        /* best effort */
      }
      log.warn(`connect ${server.name} failed: ${errMessage(err)}`);
    } finally {
      this.connecting.delete(server.id);
    }
  }

  async disconnect(id: string): Promise<void> {
    // An intentional disconnect (disable, delete, config change, manual
    // reconnect) cancels any pending backoff and resets the attempt count so a
    // later re-enable starts fresh.
    this.cancelReconnect(id);
    this.reconnectAttempts.delete(id);
    const c = this.conns.get(id);
    if (!c) return;
    // Delete before close so the onclose watcher sees a stale client (the active
    // conn no longer matches) and treats this as intentional, not a drop.
    this.conns.delete(id);
    try {
      await c.client.close();
    } catch {
      /* best effort */
    }
  }

  /** Watch the active client for an unexpected close (-> error + auto-reconnect)
   *  and for server-pushed list_changed notifications (-> refresh the affected
   *  capability). A close we initiate is ignored because disconnect() removes
   *  the conn first, so the active client no longer matches `client`. */
  private watchConnection(id: string, client: Client): void {
    client.onclose = () => {
      if (this.conns.get(id)?.client !== client) return;
      this.conns.delete(id);
      this.failed.set(id, "Connection closed");
      this.onChange();
      // Re-establish a connection the user already enabled (never a new
      // endpoint). doSync cancels this if the server is meanwhile disabled.
      this.scheduleReconnect(id);
    };
    client.setNotificationHandler(
      ToolListChangedNotificationSchema,
      () => void this.refreshCapability(id, client, "tools"),
    );
    client.setNotificationHandler(
      PromptListChangedNotificationSchema,
      () => void this.refreshCapability(id, client, "prompts"),
    );
    client.setNotificationHandler(
      ResourceListChangedNotificationSchema,
      () => void this.refreshCapability(id, client, "resources"),
    );
  }

  /** Re-fetch one capability list after a server's list_changed notification and
   *  repaint. Ignores a notification from a client that's no longer active. */
  private async refreshCapability(
    id: string,
    client: Client,
    kind: "tools" | "prompts" | "resources",
  ): Promise<void> {
    const conn = this.conns.get(id);
    if (!conn || conn.client !== client) return;
    if (kind === "tools") conn.tools = await listTools(client);
    else if (kind === "prompts") conn.prompts = await listPrompts(client);
    else conn.resources = await listResources(client);
    this.onChange();
  }

  /** Schedule the next backoff-spaced reconnect attempt for a dropped enabled
   *  server, reconciling against the latest desired set. Stops after the backoff
   *  steps are exhausted. */
  private scheduleReconnect(id: string): void {
    this.cancelReconnect(id);
    const attempt = this.reconnectAttempts.get(id) ?? 0;
    const delay = RECONNECT_BACKOFF_MS[attempt];
    if (delay === undefined) return; // exhausted; wait for manual / CRUD retry
    this.reconnectAttempts.set(id, attempt + 1);
    const timer = setTimeout(() => {
      this.reconnectTimers.delete(id);
      void this.sync(this.desired).then(() => {
        // connect() clears the attempt count on success; if still down and
        // wanted, step to the next backoff delay.
        if (!this.conns.has(id) && this.desired.some((s) => s.id === id && s.enabled)) {
          this.scheduleReconnect(id);
        }
      });
    }, delay);
    this.reconnectTimers.set(id, timer);
  }

  private cancelReconnect(id: string): void {
    const timer = this.reconnectTimers.get(id);
    if (timer !== undefined) {
      clearTimeout(timer);
      this.reconnectTimers.delete(id);
    }
  }

  private clientOrThrow(id: string): Client {
    const c = this.conns.get(id);
    if (!c) throw new Error(`MCP server ${id} is not connected`);
    return c.client;
  }

  callTool(
    serverId: string,
    name: string,
    args: Record<string, unknown>,
    signal?: AbortSignal,
  ): Promise<unknown> {
    return this.clientOrThrow(serverId).callTool({ name, arguments: args }, undefined, {
      timeout: REQUEST_TIMEOUT_MS,
      signal,
    });
  }

  getPrompt(
    serverId: string,
    name: string,
    args: Record<string, string>,
    signal?: AbortSignal,
  ): Promise<{ messages: Array<{ role: string; content: unknown }> }> {
    return this.clientOrThrow(serverId).getPrompt(
      { name, arguments: args },
      { timeout: REQUEST_TIMEOUT_MS, signal },
    ) as Promise<{
      messages: Array<{ role: string; content: unknown }>;
    }>;
  }

  readResource(
    serverId: string,
    uri: string,
    signal?: AbortSignal,
  ): Promise<{
    contents: Array<{ uri: string; mimeType?: string; text?: string; blob?: string }>;
  }> {
    return this.clientOrThrow(serverId).readResource(
      { uri },
      { timeout: REQUEST_TIMEOUT_MS, signal },
    ) as Promise<{
      contents: Array<{ uri: string; mimeType?: string; text?: string; blob?: string }>;
    }>;
  }

  async shutdown(): Promise<void> {
    for (const id of this.reconnectTimers.keys()) this.cancelReconnect(id);
    await Promise.all([...this.conns.keys()].map((id) => this.disconnect(id)));
  }
}

async function makeTransport(server: McpServer) {
  if (server.kind === "stdio") {
    // The deno runtime runs the command through the bundled deno binary, so
    // npm-based servers need no Node.js install; custom runs it verbatim.
    if (server.runtime === "deno") return await makeDenoStdioTransport(server);
    if (!server.command) throw new Error("stdio MCP server has no command");
    return new StdioClientTransport({
      command: server.command,
      args: server.args,
      stderr: "pipe",
    });
  }
  if (!server.url) throw new Error("remote MCP server has no url");
  // OAuth 2.1: connect with the vault-backed provider so the SDK attaches the
  // stored access token and refreshes it as needed. A server that hasn't been
  // signed into yet has no tokens; rather than let the SDK kick off a dynamic
  // registration on every failed connect (no browser is open here), surface a
  // clear "sign in" status and make no network call.
  if (server.remoteAuth === "oauth") {
    if (!server.oauthAuthorized) throw new Error("OAuth sign-in required");
    const authProvider = new McpOAuthProvider(server.id, "http://127.0.0.1/callback");
    return new StreamableHTTPClientTransport(new URL(server.url), { authProvider });
  }
  // "bearer" mode: send the stored token as the Authorization header on every
  // request. Gated on the mode (not just `hasAuth`) so switching a server to
  // "none" stops sending a token left over from a previous bearer setup. The
  // token lives in the secrets vault, keyed by server id; it never touches the
  // DB or the wire projection.
  let requestInit: RequestInit | undefined;
  if (server.remoteAuth === "bearer" && server.hasAuth) {
    const token = await getSecret(mcpAuthSecretName(server.id));
    if (token) {
      requestInit = { headers: { Authorization: `Bearer ${token}` } };
    }
  }
  return new StreamableHTTPClientTransport(new URL(server.url), { requestInit });
}

/** Build a stdio transport that runs the server through the bundled deno binary.
 *  `command` is the deno run target (an npm: specifier, URL, or script); the
 *  permission flags come from the server's allow-all toggle or manual list. */
async function makeDenoStdioTransport(server: McpServer): Promise<StdioClientTransport> {
  const target = server.command?.trim();
  if (!target) throw new Error("deno MCP server has no package or script to run");
  const denoBin = await requireWorkerDeno();
  // Even under the (default) allow-all toggle, backstop the grant with the same
  // secret-file --deny list the extension tool workers use: Deno's --deny-* wins
  // over --allow-*, so a user-added deno MCP server still can't read/write the
  // core's secrets, admin token, sqlite db, memory store, or other extensions'
  // data. A least-privilege (denoAllowAll=false) server runs on its manual list
  // only, so the deny is redundant-but-harmless there.
  const deny = coreSecretDenyPaths().join(",");
  const perms = server.denoAllowAll
    ? ["--allow-all", `--deny-read=${deny}`, `--deny-write=${deny}`]
    : server.denoPermissions;
  return new StdioClientTransport({
    command: denoBin,
    args: ["run", ...perms, target, ...server.args],
    // Contain the npm/deno cache under Tomat's channel dir rather than the
    // user's global cache. The SDK merges this over a safe default env that
    // already carries PATH and HOME.
    env: { DENO_DIR: paths().denoCacheDir },
    stderr: "pipe",
  });
}

/** Drain a stdio transport's piped stderr so a connect failure can report what
 *  the child actually printed. The SDK exposes the stderr PassThrough before
 *  start(), so attaching here loses no early output. */
function captureStderr(transport: { stderr?: unknown }): () => string {
  const stream = transport.stderr;
  if (!stream || typeof (stream as { on?: unknown }).on !== "function") return () => "";
  const chunks: string[] = [];
  let len = 0;
  (stream as { on(event: "data", cb: (chunk: unknown) => void): void }).on("data", (chunk) => {
    if (len >= STDERR_CAP) return;
    const s = String(chunk);
    chunks.push(s);
    len += s.length;
  });
  return () => chunks.join("").trim();
}
const STDERR_CAP = 4000;

/** Turn a connect failure into a user-facing status. A missing custom launcher
 *  gets an actionable hint; otherwise the child's stderr tail is appended. */
function describeConnectError(server: McpServer, err: unknown, stderrTail: string): string {
  if (server.kind === "stdio" && server.runtime === "custom" && isCommandNotFound(err)) {
    return launcherUnavailableMessage(server.command ?? "");
  }
  const base = errMessage(err);
  return stderrTail ? `${base}: ${stderrTail}` : base;
}

function isCommandNotFound(err: unknown): boolean {
  if (err && typeof err === "object" && (err as { code?: unknown }).code === "ENOENT") return true;
  return /\bENOENT\b/.test(errMessage(err));
}

// Launchers that ship with Node.js / a Python tool, so a "not found" can point
// the user somewhere concrete instead of a bare "command not found".
const NODE_LAUNCHERS = new Set(["npx", "npm", "node", "bunx", "bun", "yarn", "pnpm"]);
const PYTHON_LAUNCHERS = new Set(["uvx", "uv", "pipx", "python", "python3"]);

function launcherUnavailableMessage(command: string): string {
  const name = launcherBasename(command);
  if (NODE_LAUNCHERS.has(name)) {
    return (
      `"${name}" was not found. Install Node.js, or switch Runtime to ` +
      `Bundled deno and set Package or Script to an npm specifier (for example ` +
      `npm:@scope/server), which runs many npm-based MCP servers without Node.js.`
    );
  }
  if (PYTHON_LAUNCHERS.has(name)) {
    return `"${name}" was not found. Install it to run this MCP server.`;
  }
  return `"${name}" was not found on this machine. Check the command or install it.`;
}

function launcherBasename(command: string): string {
  const base = command.trim().split(/[/\\]/).pop() ?? command.trim();
  return base.replace(/\.(exe|cmd|bat|com)$/i, "").toLowerCase();
}

async function safeList<R, T>(call: () => Promise<R>, pick: (r: R) => T[]): Promise<T[]> {
  try {
    return pick(await call());
  } catch {
    return [];
  }
}

// One fetcher per capability list, used on connect and on a list_changed
// refresh. A server may not implement every list method, so each defaults to
// empty on error.
function listTools(client: Client): Promise<McpToolDef[]> {
  return safeList(
    () => client.listTools(),
    (r) => (r as { tools: McpToolDef[] }).tools,
  );
}
function listPrompts(client: Client): Promise<McpPromptDef[]> {
  return safeList(
    () => client.listPrompts(),
    (r) =>
      (r as { prompts: McpPromptDef[] }).prompts.map((p) => ({
        ...p,
        arguments: p.arguments ?? [],
      })),
  );
}
function listResources(client: Client): Promise<McpResourceDef[]> {
  return safeList(
    () => client.listResources(),
    (r) => (r as { resources: McpResourceDef[] }).resources,
  );
}

let _instance: McpManager | null = null;
export function mcpManager(): McpManager {
  if (!_instance) _instance = new McpManager();
  return _instance;
}

/** Drop the cached singleton (and its live connections) between tests. */
export function __resetForTesting(): void {
  void _instance?.shutdown();
  _instance = null;
}
