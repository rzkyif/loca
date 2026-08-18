//! The remote transport: streamable HTTP, in one of three auth modes.
//!
//! `none` is an open endpoint. `bearer` sends a stored static token as the
//! `Authorization` header. `oauth` attaches the access token from the vault and
//! refreshes it when the server says it is stale.
//!
//! # An OAuth server the row says is not signed in makes no request at all
//!
//! The connect is refused before any client is built, and the user sees a
//! status that tells them to sign in.
//!
//! The state this actually protects is tokens present in the vault while the
//! ROW says not authorized, and it is reachable: `Mcp::start_oauth`'s
//! completion callback stores the tokens through the SDK and only then updates
//! the row, and it logs and returns if that update fails. Without this check
//! such a server would resolve metadata and connect on every sync, using a
//! grant the row does not admit to holding, and the Sign in button would never
//! be offered.
//!
//! With no tokens at all the check is belt-and-braces: `initialize_from_store`
//! returns `false` without making a request. It is NOT true, at rmcp 3.1.2,
//! that a never-signed-in server would otherwise kick off discovery and dynamic
//! registration on its own; a previous version of this comment said so, and a
//! reviewer who deleted the check on the strength of it would have found every
//! test still passing.

use std::time::Duration;

use rmcp::transport::auth::AuthClient;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::ServiceExt;

use crate::error::{AppError, ErrorCode, Result};
use crate::secrets::Vault;

use super::oauth::store::VaultCredentials;
use super::registry::ServerConfig;
use super::session::{self, Connected, Handshake, HANDSHAKE_TIMEOUT};
use crate::contract::api::McpRemoteAuth;

/// The message an unsigned-in OAuth server reports. Matched on by nothing, read
/// by the user, and the client pairs it with a Sign in action.
pub const SIGN_IN_REQUIRED: &str = "OAuth sign-in required";

pub async fn connect(server: &ServerConfig, vault: &Vault) -> Result<Connected> {
    connect_bounded(server, vault, HANDSHAKE_TIMEOUT).await
}

/// The whole of [`connect`], with the handshake bound supplied. Production
/// passes [`HANDSHAKE_TIMEOUT`]; a test passes something short so it can drive a
/// black-holed listener all the way to the timeout.
pub(super) async fn connect_bounded(
    server: &ServerConfig,
    vault: &Vault,
    handshake_timeout: Duration,
) -> Result<Connected> {
    let url = server
        .url
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .ok_or_else(|| AppError::new(ErrorCode::ValidationError, "remote MCP server has no url"))?;

    match server.remote_auth {
        McpRemoteAuth::Oauth => connect_oauth(server, url, vault, handshake_timeout).await,
        McpRemoteAuth::Bearer | McpRemoteAuth::None => {
            connect_plain(server, url, vault, handshake_timeout).await
        }
    }
}

/// `none` and `bearer`. The token is fetched only in `bearer` mode, gated on
/// the mode rather than on `has_auth`, so switching a server to `none` stops
/// sending a token left over from a previous bearer setup.
async fn connect_plain(
    server: &ServerConfig,
    url: &str,
    vault: &Vault,
    handshake_timeout: Duration,
) -> Result<Connected> {
    let mut config = StreamableHttpClientTransportConfig::with_uri(url.to_string());
    if server.remote_auth == McpRemoteAuth::Bearer && server.has_auth {
        let name = super::secret_key::auth_secret_name(&server.id);
        if let Some(token) = vault.get(&name).await? {
            config.auth_header = Some(token);
        }
    }
    let transport = StreamableHttpClientTransport::with_client(http_client()?, config);
    serve(transport, handshake_timeout).await
}

async fn connect_oauth(
    server: &ServerConfig,
    url: &str,
    vault: &Vault,
    handshake_timeout: Duration,
) -> Result<Connected> {
    if !server.oauth_authorized {
        return Err(AppError::new(
            ErrorCode::ProviderUnauthorized,
            SIGN_IN_REQUIRED,
        ));
    }
    let store = VaultCredentials::new(vault.clone(), server.id.clone());
    let mut manager = rmcp::transport::auth::AuthorizationManager::new(url.to_string())
        .await
        .map_err(|err| oauth_error(err, "could not prepare the OAuth client"))?;
    manager.set_credential_store(store);
    manager
        .with_client(http_client()?)
        .map_err(|err| oauth_error(err, "could not attach the HTTP client to the OAuth client"))?;
    // Loads the stored registration and tokens, and resolves the authorization
    // server's metadata. `false` means the stored credentials are gone or were
    // discarded because the authorization server changed, which is the same
    // sign-in-required state as never having signed in.
    let ready = manager
        .initialize_from_store()
        .await
        .map_err(|err| oauth_error(err, "stored OAuth credentials could not be loaded"))?;
    if !ready {
        return Err(AppError::new(
            ErrorCode::ProviderUnauthorized,
            SIGN_IN_REQUIRED,
        ));
    }
    let client = AuthClient::new(http_client()?, manager);
    let config = StreamableHttpClientTransportConfig::with_uri(url.to_string());
    let transport = StreamableHttpClientTransport::with_client(client, config);
    serve(transport, handshake_timeout).await
}

/// One client per connection, from the one pinned `reqwest`. Its TLS roots come
/// from the OS store through the platform verifier, which is what lets a user
/// point Core at a gateway behind a private CA.
fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .build()
        .map_err(|err| AppError::new(ErrorCode::InternalError, format!("HTTP client: {err}")))
}

async fn serve<C>(
    transport: StreamableHttpClientTransport<C>,
    handshake_timeout: Duration,
) -> Result<Connected>
where
    C: rmcp::transport::streamable_http_client::StreamableHttpClient + Send + Sync + 'static,
{
    let (handler, events) = session::event_channel();
    let tx = session::sender(&handler);
    // Bounded: an endpoint that completes the TCP handshake and then never
    // answers `initialize` is indistinguishable from a healthy one until this
    // expires, and without it the connect never returns at all.
    let service = match session::handshake(handshake_timeout, handler.serve(transport)).await {
        Handshake::Ready(service) => service,
        Handshake::Failed(err) => {
            return Err(AppError::new(
                ErrorCode::ServerUnavailable,
                format!("{err}"),
            ))
        }
        Handshake::TimedOut => {
            return Err(AppError::new(
                ErrorCode::ServerUnavailable,
                session::handshake_timeout_message(handshake_timeout),
            ))
        }
    };
    let session = session::adopt(service, tx, None);
    Ok(Connected { session, events })
}

/// An OAuth failure the user has to act on reads as a provider authorization
/// failure rather than as an internal error, because the action is always the
/// same: sign in again. Only the message is user-visible here, since a connect
/// failure becomes the server's `error` status rather than an HTTP response.
fn oauth_error(err: rmcp::transport::auth::AuthError, context: &str) -> AppError {
    AppError::new(ErrorCode::ProviderUnauthorized, format!("{context}: {err}"))
}
