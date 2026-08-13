// MCP server CRUD + prompt/resource listings around the core REST API. MCP
// servers live on the core (it owns the connections); the client mirrors the
// projected state for the settings manager and the "/" + "@" autocomplete.

import type { McpPrompt, McpResource, McpServer } from "@loca/shared";
import type { CoreClient } from "./client";

export interface McpServerInput {
  name: string;
  kind: "stdio" | "remote";
  command?: string;
  args?: string[];
  runtime?: "custom" | "deno";
  denoAllowAll?: boolean;
  denoPermissions?: string[];
  url?: string;
  // Remote: how the server authenticates ("none" | "bearer" | "oauth").
  remoteAuth?: "none" | "bearer" | "oauth";
  enabled?: boolean;
  // Remote bearer token: write-only. Omit to leave unchanged, "" to clear, a
  // value to store. The server only ever echoes back `hasAuth`.
  authToken?: string;
}

export class McpApi {
  constructor(private readonly client: CoreClient) {}

  async list(): Promise<McpServer[]> {
    const res = await this.client.get<{ servers: McpServer[] }>("/api/v1/mcp");
    return res.servers;
  }

  async listPrompts(): Promise<McpPrompt[]> {
    const res = await this.client.get<{ prompts: McpPrompt[] }>("/api/v1/mcp/prompts");
    return res.prompts;
  }

  async listResources(): Promise<McpResource[]> {
    const res = await this.client.get<{ resources: McpResource[] }>("/api/v1/mcp/resources");
    return res.resources;
  }

  create(input: McpServerInput): Promise<McpServer> {
    return this.client.post("/api/v1/mcp", input);
  }

  update(id: string, input: Partial<McpServerInput>): Promise<McpServer> {
    return this.client.patch(`/api/v1/mcp/${encodeURIComponent(id)}`, input);
  }

  async delete(id: string): Promise<void> {
    await this.client.delete(`/api/v1/mcp/${encodeURIComponent(id)}`);
  }

  reconnect(id: string): Promise<McpServer> {
    return this.client.post(`/api/v1/mcp/${encodeURIComponent(id)}/reconnect`, {});
  }

  // Begin OAuth sign-in: returns the authorization URL to open in a browser, or
  // null if stored tokens already authorized. Completion arrives later as an
  // mcp.snapshot once the browser redirect lands on core's loopback listener.
  startOAuth(id: string): Promise<{ authorizationUrl: string | null }> {
    return this.client.post(`/api/v1/mcp/${encodeURIComponent(id)}/oauth/start`, {});
  }

  setToolEnabled(id: string, tool: string, enabled: boolean): Promise<McpServer> {
    return this.client.post(
      `/api/v1/mcp/${encodeURIComponent(id)}/tools/${encodeURIComponent(
        tool,
      )}/${enabled ? "enable" : "disable"}`,
      {},
    );
  }

  // Resolve a prompt's messages into one instruction string with the given
  // arguments (a live server round-trip). Called at send time to fold a
  // `/prompt` reference into the turn's system prompt.
  async resolvePrompt(id: string, prompt: string, args: Record<string, string>): Promise<string> {
    const res = await this.client.post<{ text: string }>(
      `/api/v1/mcp/${encodeURIComponent(id)}/prompts/${encodeURIComponent(prompt)}/resolve`,
      { arguments: args },
    );
    return res.text;
  }

  setToolAlwaysAvailable(id: string, tool: string, alwaysAvailable: boolean): Promise<McpServer> {
    return this.client.post(
      `/api/v1/mcp/${encodeURIComponent(id)}/tools/${encodeURIComponent(
        tool,
      )}/always-available/${alwaysAvailable ? "enable" : "disable"}`,
      {},
    );
  }

  setPromptEnabled(id: string, prompt: string, enabled: boolean): Promise<McpServer> {
    return this.client.post(
      `/api/v1/mcp/${encodeURIComponent(id)}/prompts/${encodeURIComponent(
        prompt,
      )}/${enabled ? "enable" : "disable"}`,
      {},
    );
  }
}
