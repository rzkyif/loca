//! The two vault keys a remote MCP server's credentials live under.
//!
//! Both share the vault with the settings secrets but are deliberately NOT
//! reachable through `/api/v1/settings/secrets`, whose gate admits only keys the
//! settings schema declares as `type: "password"`. Never rename these into the
//! settings namespace.

/// Where a remote server's static bearer token is stored.
pub fn auth_secret_name(server_id: &str) -> String {
    format!("mcp.{server_id}.authToken")
}

/// Where a remote server's OAuth state (registration, tokens, PKCE verifier)
/// is stored, as one JSON bag.
pub fn oauth_secret_name(server_id: &str) -> String {
    format!("mcp.{server_id}.oauth")
}
