// MCP server registry: the DB-backed list of configured servers plus the
// projections the API serves. Rows hold the connection config and the
// user's per-tool / per-prompt enablement; the live status and capability
// lists come from the manager. `listAllTools` maps a server's enabled tools
// into the shared Tool shape so the Tools UI shows them next to extension
// tools.

import type {
  McpPrompt,
  McpResource,
  McpServer,
  McpServerInput,
  McpTransportKind,
  Tool,
} from "@loca/shared";
import { db } from "@loca/core-engine";
import { AppError } from "@loca/core-engine";
import { newMcpServerId } from "@loca/core-engine";
import { mcpManager } from "./manager.ts";

/** A server can only connect with a transport target: a `command` for stdio, a
 *  `url` for remote. Checked at the moment a server is enabled, not on every
 *  save, so a disabled draft may keep these blank. */
function assertConnectable(kind: McpTransportKind, command?: string, url?: string): void {
  if (kind === "stdio" && !command?.trim()) {
    throw new AppError("validation_error", "stdio MCP server needs a command");
  }
  if (kind === "remote" && !url?.trim()) {
    throw new AppError("validation_error", "remote MCP server needs a url");
  }
}

interface Row {
  id: string;
  name: string;
  kind: string;
  command: string | null;
  args_json: string;
  runtime: string;
  deno_allow_all: number;
  deno_permissions_json: string;
  url: string | null;
  remote_auth: string;
  enabled: number;
  has_auth: number;
  oauth_authorized: number;
  tool_enabled_json: string;
  prompt_enabled_json: string;
  tool_always_available_off_json: string;
  created_at_ms: number;
  updated_at_ms: number;
}

class McpRegistry {
  list(): McpServer[] {
    const rows = db()
      .prepare(`SELECT * FROM mcp_servers ORDER BY name COLLATE NOCASE ASC`)
      .all() as Row[];
    return rows.map((r) => this.project(r));
  }

  get(id: string): McpServer | undefined {
    const row = db().prepare(`SELECT * FROM mcp_servers WHERE id = ?`).get(id) as Row | undefined;
    return row ? this.project(row) : undefined;
  }

  getOrThrow(id: string): McpServer {
    const s = this.get(id);
    if (!s) throw new AppError("not_found", `MCP server ${id} not found`);
    return s;
  }

  create(input: McpServerInput): McpServer {
    const name = input.name.trim();
    if (!name) {
      throw new AppError("validation_error", "MCP server name required");
    }
    // Only an enabled server needs a connection target; a disabled draft (the
    // shape a freshly added server starts in) can be saved with the command/url
    // still blank and filled in before it is enabled.
    if (input.enabled) {
      assertConnectable(input.kind, input.command, input.url);
    }
    const id = newMcpServerId();
    const now = Date.now();
    db()
      .prepare(`
      INSERT INTO mcp_servers (id, name, kind, command, args_json, runtime, deno_allow_all,
                               deno_permissions_json, url, remote_auth, enabled, has_auth,
                               oauth_authorized, tool_enabled_json, prompt_enabled_json,
                               created_at_ms, updated_at_ms)
      VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, '[]', '[]', ?, ?)
    `)
      .run(
        id,
        name,
        input.kind,
        input.command ?? null,
        JSON.stringify(input.args ?? []),
        input.runtime ?? "custom",
        input.denoAllowAll === false ? 0 : 1,
        JSON.stringify(input.denoPermissions ?? []),
        input.url ?? null,
        input.remoteAuth ?? "none",
        input.enabled ? 1 : 0,
        input.hasAuth ? 1 : 0,
        input.oauthAuthorized ? 1 : 0,
        now,
        now,
      );
    return this.getOrThrow(id);
  }

  update(id: string, input: Partial<McpServerInput>): McpServer {
    const cur = this.getOrThrow(id);
    // Block enabling a server that has no connection target yet (the same gate
    // as create, applied to the post-update state so a draft can't be flipped on
    // while still incomplete).
    const nextEnabled = input.enabled === undefined ? cur.enabled : input.enabled;
    if (nextEnabled) {
      assertConnectable(input.kind ?? cur.kind, input.command ?? cur.command, input.url ?? cur.url);
    }
    const now = Date.now();
    db()
      .prepare(`
      UPDATE mcp_servers
         SET name = ?, kind = ?, command = ?, args_json = ?, runtime = ?, deno_allow_all = ?,
             deno_permissions_json = ?, url = ?, remote_auth = ?, enabled = ?, has_auth = ?,
             oauth_authorized = ?, updated_at_ms = ?
       WHERE id = ?
    `)
      .run(
        input.name?.trim() || cur.name,
        input.kind ?? cur.kind,
        input.command ?? cur.command ?? null,
        JSON.stringify(input.args ?? cur.args),
        input.runtime ?? cur.runtime,
        (input.denoAllowAll === undefined ? cur.denoAllowAll : input.denoAllowAll) ? 1 : 0,
        JSON.stringify(input.denoPermissions ?? cur.denoPermissions),
        input.url ?? cur.url ?? null,
        input.remoteAuth ?? cur.remoteAuth,
        input.enabled === undefined ? (cur.enabled ? 1 : 0) : input.enabled ? 1 : 0,
        input.hasAuth === undefined ? (cur.hasAuth ? 1 : 0) : input.hasAuth ? 1 : 0,
        input.oauthAuthorized === undefined
          ? cur.oauthAuthorized
            ? 1
            : 0
          : input.oauthAuthorized
            ? 1
            : 0,
        now,
        id,
      );
    return this.getOrThrow(id);
  }

  delete(id: string): void {
    db().prepare(`DELETE FROM mcp_servers WHERE id = ?`).run(id);
  }

  setToolEnabled(id: string, toolName: string, enabled: boolean): McpServer {
    return this.toggleInJson(id, "tool_enabled_json", toolName, enabled);
  }

  setPromptEnabled(id: string, promptName: string, enabled: boolean): McpServer {
    return this.toggleInJson(id, "prompt_enabled_json", promptName, enabled);
  }

  /** Set whether a tool is always-available (default true). Stored as an opt-out
   *  set, so turning it OFF adds the name, turning it ON removes it. */
  setToolAlwaysAvailable(id: string, toolName: string, alwaysAvailable: boolean): McpServer {
    return this.toggleInJson(id, "tool_always_available_off_json", toolName, !alwaysAvailable);
  }

  /** Every enabled tool from every connected server, in the shared Tool shape. */
  listAllTools(): Tool[] {
    const out: Tool[] = [];
    for (const server of this.list()) {
      const enabled = new Set(server.toolEnabled);
      // MCP tools default to always-available (usually offered to the model);
      // a name in the opt-out set folds that tool into the relevance filter.
      const alwaysOff = new Set(server.toolAlwaysAvailableOff);
      for (const t of mcpManager().capabilities(server.id).tools) {
        out.push({
          id: `${server.id}::${t.name}`,
          extensionId: server.id,
          providerKind: "mcp",
          providerName: server.name,
          name: t.name,
          description: t.description ?? "",
          parameters: t.inputSchema ?? { type: "object" },
          triggers: [],
          fnExport: "",
          alwaysAvailable: !alwaysOff.has(t.name),
          platforms: [],
          enabled: enabled.has(t.name),
          requiredPermissions: [],
          missingRequired: [],
          grants: [],
        });
      }
    }
    return out;
  }

  /** Every prompt from every connected server, with the user's enable flag. */
  listPrompts(): McpPrompt[] {
    const out: McpPrompt[] = [];
    for (const server of this.list()) {
      const enabled = new Set(server.promptEnabled);
      for (const p of mcpManager().capabilities(server.id).prompts) {
        out.push({
          serverId: server.id,
          serverName: server.name,
          name: p.name,
          description: p.description,
          enabled: enabled.has(p.name),
          arguments: p.arguments,
        });
      }
    }
    return out;
  }

  /** Every resource from every connected server (all @-referenceable). */
  listResources(): McpResource[] {
    const out: McpResource[] = [];
    for (const server of this.list()) {
      for (const r of mcpManager().capabilities(server.id).resources) {
        out.push({
          serverId: server.id,
          serverName: server.name,
          uri: r.uri,
          name: r.name,
          description: r.description,
          mimeType: r.mimeType,
        });
      }
    }
    return out;
  }

  private toggleInJson(
    id: string,
    column: "tool_enabled_json" | "prompt_enabled_json" | "tool_always_available_off_json",
    name: string,
    enabled: boolean,
  ): McpServer {
    const row = db().prepare(`SELECT ${column} AS v FROM mcp_servers WHERE id = ?`).get(id) as
      | { v: string }
      | undefined;
    if (!row) throw new AppError("not_found", `MCP server ${id} not found`);
    const set = new Set<string>(JSON.parse(row.v));
    if (enabled) set.add(name);
    else set.delete(name);
    db()
      .prepare(`UPDATE mcp_servers SET ${column} = ?, updated_at_ms = ? WHERE id = ?`)
      .run(JSON.stringify([...set]), Date.now(), id);
    return this.getOrThrow(id);
  }

  private project(row: Row): McpServer {
    const live = mcpManager().status(row.id);
    const caps = mcpManager().capabilities(row.id);
    return {
      id: row.id,
      name: row.name,
      kind: row.kind === "remote" ? "remote" : "stdio",
      command: row.command ?? undefined,
      args: JSON.parse(row.args_json),
      runtime: row.runtime === "deno" ? "deno" : "custom",
      denoAllowAll: row.deno_allow_all !== 0,
      denoPermissions: JSON.parse(row.deno_permissions_json),
      url: row.url ?? undefined,
      remoteAuth:
        row.remote_auth === "bearer" || row.remote_auth === "oauth" ? row.remote_auth : "none",
      hasAuth: row.has_auth !== 0,
      oauthAuthorized: row.oauth_authorized !== 0,
      enabled: row.enabled !== 0,
      toolEnabled: JSON.parse(row.tool_enabled_json),
      promptEnabled: JSON.parse(row.prompt_enabled_json),
      // Defensive `?? "[]"`: an existing dev DB created before this column was
      // added has no value here (no migration under the no-active-users rule),
      // so it reads as an empty opt-out set (every tool stays always-available).
      toolAlwaysAvailableOff: JSON.parse(row.tool_always_available_off_json ?? "[]"),
      status: live.status,
      statusError: live.error,
      toolCount: caps.tools.length,
      promptCount: caps.prompts.length,
      resourceCount: caps.resources.length,
      createdAtMs: row.created_at_ms,
      updatedAtMs: row.updated_at_ms,
    };
  }
}

let _instance: McpRegistry | null = null;
export function mcpRegistry(): McpRegistry {
  if (!_instance) _instance = new McpRegistry();
  return _instance;
}
