// MCP (Model Context Protocol) server shapes shared between core and client.
// An MCP server is a provider, alongside extensions: it supplies tools (run on
// the server, surfaced in the Tools UI), prompts (surfaced as "/"-triggered
// commands), and resources (referenceable via "@"). Servers are configured by
// the user; their capabilities are fetched live on connect and cached in core.

export type McpTransportKind = "stdio" | "remote";

// Which runtime launches a stdio server. "custom" runs `command` + `args`
// verbatim (npx, node, uvx, an absolute path); the user supplies a launcher
// that must exist on the machine. "deno" runs `command` (an npm: specifier, a
// URL, or a script path) through the bundled deno binary, so npm-based servers
// need no Node.js install.
export type McpStdioRuntime = "custom" | "deno";

export type McpConnectionStatus = "disconnected" | "connecting" | "connected" | "error";

// How a remote server authenticates. "none" is an open endpoint; "bearer" sends
// a stored static token; "oauth" runs the OAuth 2.1 (PKCE) authorization-code
// flow and sends the resulting access token, refreshing it as needed.
export type McpRemoteAuth = "none" | "bearer" | "oauth";

export interface McpServer {
  id: string;
  name: string;
  kind: McpTransportKind;
  // stdio transport: the executable to spawn (or the deno run target) and its
  // argv.
  command?: string;
  args: string[];
  // stdio transport: which runtime launches `command` (see McpStdioRuntime).
  runtime: McpStdioRuntime;
  // deno runtime: grant full access (--allow-all) for maximum compatibility, or
  // run with only `denoPermissions` (manual deno permission flags) when false.
  denoAllowAll: boolean;
  denoPermissions: string[];
  // remote transport: the streamable HTTP/SSE endpoint.
  url?: string;
  // remote transport: how the server authenticates (see McpRemoteAuth).
  remoteAuth: McpRemoteAuth;
  // remote "bearer": whether a static token is stored (in the secrets vault,
  // never on the wire) and sent as the Authorization header on connect.
  hasAuth: boolean;
  // remote "oauth": whether the authorization-code flow has completed and tokens
  // are stored (in the vault). Until then an oauth server can't connect; the UI
  // shows a Sign in action.
  oauthAuthorized: boolean;
  // Whether core connects to this server at all.
  enabled: boolean;
  // Names of the server's tools / prompts the user turned on (off by default so
  // a server can't flood tool selection or the "/" autocomplete).
  toolEnabled: string[];
  promptEnabled: string[];
  // Tool names the user turned "always available" OFF for. MCP tools default to
  // always-available (usually offered to the model); a name here folds that tool
  // into the relevance filter instead.
  toolAlwaysAvailableOff: string[];
  // Live connection state + capability counts from the last successful connect.
  status: McpConnectionStatus;
  statusError?: string;
  toolCount: number;
  promptCount: number;
  resourceCount: number;
  createdAtMs: number;
  updatedAtMs: number;
}

// A prompt offered by an MCP server, surfaced as a read-only "/"-triggered
// command. The user only toggles whether it appears in autocomplete.
export interface McpPromptArg {
  name: string;
  description?: string;
  required?: boolean;
}

export interface McpPrompt {
  serverId: string;
  serverName: string;
  name: string;
  description?: string;
  enabled: boolean;
  arguments: McpPromptArg[];
}

// A resource offered by an MCP server, referenceable via "@" and resolved live.
export interface McpResource {
  serverId: string;
  serverName: string;
  uri: string;
  name: string;
  description?: string;
  mimeType?: string;
}

// The create/update payload for a server: the user-editable subset of McpServer.
// The live connection state (status, capability counts) is never written this
// way; it is derived on connect.
export interface McpServerInput {
  name: string;
  kind: McpTransportKind;
  command?: string;
  args?: string[];
  runtime?: McpStdioRuntime;
  denoAllowAll?: boolean;
  denoPermissions?: string[];
  url?: string;
  remoteAuth?: McpRemoteAuth;
  enabled?: boolean;
  // Whether a bearer token is stored for this server (the token itself goes to
  // the secrets vault via the route, never through the registry/DB).
  hasAuth?: boolean;
  // Whether the OAuth authorization-code flow has completed (tokens in vault).
  oauthAuthorized?: boolean;
}
