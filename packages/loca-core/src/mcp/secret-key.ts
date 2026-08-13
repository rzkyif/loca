// Vault key for a remote MCP server's bearer token. Kept in its own module so
// both the manager (which reads the token) and the routes (which write it) can
// import it without pulling in the registry<->manager pair.

/** Vault key under which a remote server's bearer token is stored. */
export function mcpAuthSecretName(serverId: string): string {
  return `mcp.${serverId}.authToken`;
}

/** Vault key under which a remote server's OAuth state (client registration,
 *  tokens, PKCE verifier) is stored as a single JSON bag. */
export function mcpOAuthSecretName(serverId: string): string {
  return `mcp.${serverId}.oauth`;
}
