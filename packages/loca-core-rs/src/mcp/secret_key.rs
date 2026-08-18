//! The two vault keys a remote MCP server's credentials live under.
//!
//! Its own module, as in the TypeScript, so the manager (which reads the bearer
//! token), the OAuth stores (which read and write the bag) and the routes
//! (which write the token) all name them from one place.
//!
//! # These are not settings secrets (D-027)
//!
//! Both keys live in the same vault as the settings secrets and are
//! deliberately NOT reachable through `/api/v1/settings/secrets`. That gate
//! filters to keys the settings schema declares as `type: "password"`, so an
//! `mcp.*` key is invisible to GET and rejected by PUT and DELETE. The reason
//! is the one D-027 records: before the gate existed, any paired client could
//! delete `tls-key` through those routes and lock out every other paired
//! client at the next boot. Nothing here may be renamed into the settings
//! namespace, and no route outside this module may write these keys.

/// Where a remote server's static bearer token is stored.
pub fn auth_secret_name(server_id: &str) -> String {
    format!("mcp.{server_id}.authToken")
}

/// Where a remote server's OAuth state (the dynamic client registration, the
/// issued tokens, and the in-flight PKCE verifier) is stored, as one JSON bag.
pub fn oauth_secret_name(server_id: &str) -> String {
    format!("mcp.{server_id}.oauth")
}
