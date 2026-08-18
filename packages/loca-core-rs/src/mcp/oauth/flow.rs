//! Interactive OAuth 2.1 sign-in, the RFC 8252 native-app way.
//!
//! A short-lived loopback listener is the redirect target, so the browser never
//! has to trust Core's self-signed certificate. The SDK discovers the
//! authorization server's metadata, registers dynamically, and builds the PKCE
//! authorization URL; that URL goes back to the client to open in a browser.
//! When the browser lands on the loopback listener with the code, it is
//! exchanged for tokens, which the vault-backed store persists.
//!
//! This is user-initiated: a Sign in button is what starts it. Opening a
//! browser and reaching the authorization server is consented network, not
//! background reach.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use axum::extract::{Query, State};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use rmcp::transport::auth::{AuthorizationRequest, OAuthState};
use tokio::sync::oneshot;

use crate::error::{internal, AppError, ErrorCode, Result};
use crate::secrets::Vault;

use super::store::VaultCredentials;

/// Abandon a half-finished sign-in (the browser was closed, or never came back)
/// after this.
const FLOW_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// What the caller is told when a flow completes, so it can mark the server
/// authorized and reconnect.
pub type OnComplete = Box<dyn FnOnce(bool) + Send + Sync>;

struct Pending {
    state: Arc<tokio::sync::Mutex<OAuthState>>,
    /// Stops the loopback listener.
    shutdown: oneshot::Sender<()>,
    timer: tokio::task::JoinHandle<()>,
    on_complete: OnComplete,
}

/// Every sign-in currently waiting for a browser, keyed by server id.
#[derive(Clone, Default)]
pub struct Flows {
    pending: Arc<Mutex<HashMap<String, Pending>>>,
}

impl Flows {
    pub fn new() -> Self {
        Self::default()
    }

    /// Begin the authorization-code flow.
    ///
    /// Returns the URL to open, or `None` when the stored tokens already work,
    /// in which case `on_complete` has already run.
    pub async fn start(
        &self,
        server_id: &str,
        server_url: &str,
        vault: &Vault,
        on_complete: OnComplete,
    ) -> Result<Option<String>> {
        self.cancel(server_id);

        let store = VaultCredentials::new(vault.clone(), server_id.to_string());
        // Reap whatever a previous attempt for this server abandoned, before
        // this one writes its own. See `VaultCredentials::clear_states` for why
        // the reap lives here and not on the teardown paths. Hygiene rather
        // than the user's goal, so a failure is logged and the sign-in
        // continues: a vault this broken will fail the next write anyway.
        if let Err(err) = store.clear_states().await {
            tracing::warn!(
                target: "loca::mcp",
                "could not clear an abandoned MCP sign-in: {err}",
            );
        }
        let mut state = OAuthState::new(server_url.to_string(), Some(http_client()?))
            .await
            .map_err(|err| oauth_error(format!("OAuth start failed: {err}")))?;
        // The stores have to be in place before anything is written: the PKCE
        // verifier is saved during `start_authorization`, and the tokens during
        // the callback, and both belong in the vault rather than in memory.
        if let OAuthState::Unauthorized(manager) = &mut state {
            manager.set_credential_store(store.clone());
            manager.set_state_store(store);
            if manager.initialize_from_store().await.unwrap_or(false) {
                // Stored tokens still work, so there is nothing to open.
                on_complete(true);
                return Ok(None);
            }
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|err| oauth_error(format!("OAuth start failed: no loopback port: {err}")))?;
        let port = listener
            .local_addr()
            .map_err(|err| oauth_error(format!("OAuth start failed: {err}")))?
            .port();
        let redirect = format!("http://127.0.0.1:{port}/callback");

        let request = AuthorizationRequest::new(redirect).with_client_name("Loca");
        state
            .start_authorization(request)
            .await
            .map_err(|err| oauth_error(format!("OAuth start failed: {err}")))?;
        let url = state
            .get_authorization_url()
            .await
            .map_err(|_| oauth_error("OAuth start did not produce an authorization URL"))?;

        let (shutdown, shutdown_rx) = oneshot::channel();
        let state = Arc::new(tokio::sync::Mutex::new(state));
        let app = Router::new()
            .route("/callback", get(callback))
            // Only the redirect lands on /callback. A stray loopback hit (a
            // favicon probe, a prefetch, a port scan) gets a 404 rather than
            // aborting a sign-in that is still in progress.
            .fallback(|| async { (axum::http::StatusCode::NOT_FOUND, "Not found") })
            .with_state(CallbackState {
                flows: self.clone(),
                server_id: server_id.to_string(),
            });
        tokio::spawn(async move {
            let served = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await;
            if let Err(err) = served {
                tracing::warn!(target: "loca::mcp", "OAuth loopback listener stopped: {err}");
            }
        });

        let timer_flows = self.clone();
        let timer_id = server_id.to_string();
        let timer = tokio::spawn(async move {
            tokio::time::sleep(FLOW_TIMEOUT).await;
            timer_flows.finish(&timer_id, false);
        });

        lock(&self.pending).insert(
            server_id.to_string(),
            Pending {
                state,
                shutdown,
                timer,
                on_complete,
            },
        );
        Ok(Some(url))
    }

    /// Resolve a pending flow once: run its callback and tear the listener down.
    fn finish(&self, server_id: &str, ok: bool) {
        let Some(pending) = lock(&self.pending).remove(server_id) else {
            return;
        };
        pending.timer.abort();
        let _ = pending.shutdown.send(());
        (pending.on_complete)(ok);
    }

    /// Drop any in-flight sign-in for a server, because it was deleted or
    /// reconfigured under the flow.
    pub fn cancel(&self, server_id: &str) {
        let Some(pending) = lock(&self.pending).remove(server_id) else {
            return;
        };
        pending.timer.abort();
        let _ = pending.shutdown.send(());
    }

    /// Abandon every waiting sign-in, on the way down. A listener left bound
    /// would hold a loopback port open past the process it belongs to.
    pub fn cancel_all(&self) {
        let pending: Vec<String> = lock(&self.pending).keys().cloned().collect();
        for server_id in pending {
            self.cancel(&server_id);
        }
    }

    fn state_of(&self, server_id: &str) -> Option<Arc<tokio::sync::Mutex<OAuthState>>> {
        lock(&self.pending)
            .get(server_id)
            .map(|p| Arc::clone(&p.state))
    }
}

#[derive(Clone)]
struct CallbackState {
    flows: Flows,
    server_id: String,
}

/// What the authorization server sends the browser back with.
#[derive(serde::Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    iss: Option<String>,
}

async fn callback(
    State(ctx): State<CallbackState>,
    Query(query): Query<CallbackQuery>,
) -> Response {
    let Some(state) = ctx.flows.state_of(&ctx.server_id) else {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            "No sign-in in progress.",
        )
            .into_response();
    };
    let (Some(code), Some(csrf)) = (query.code, query.state) else {
        if let Some(error) = query.error {
            tracing::warn!(target: "loca::mcp", "OAuth sign-in was refused: {error}");
        }
        ctx.flows.finish(&ctx.server_id, false);
        return page("Sign-in failed. You can close this tab and try again.");
    };
    let exchanged = state
        .lock()
        .await
        .handle_callback_with_issuer(&code, &csrf, query.iss.as_deref())
        .await;
    match exchanged {
        Ok(()) => {
            ctx.flows.finish(&ctx.server_id, true);
            page("Signed in. You can close this tab.")
        }
        Err(err) => {
            tracing::warn!(target: "loca::mcp", "MCP OAuth exchange failed: {err}");
            ctx.flows.finish(&ctx.server_id, false);
            page("Sign-in failed. You can close this tab and try again.")
        }
    }
}

/// The one page a user ever sees from this listener.
fn page(message: &str) -> Response {
    Html(format!(
        "<!doctype html><meta charset=\"utf-8\"><title>Loca</title>\
         <body style=\"font:16px system-ui;padding:3rem;text-align:center\">{message}</body>"
    ))
    .into_response()
}

fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .build()
        .map_err(|err| AppError::new(ErrorCode::InternalError, format!("HTTP client: {err}")))
}

/// A failure to START the flow reaches the client as an HTTP error, and the
/// TypeScript raises a plain `Error` here, which its handler renders as a 500
/// `internal_error`. Same code, same message text, so the two cores answer a
/// failed sign-in identically.
fn oauth_error(message: impl Into<String>) -> AppError {
    internal(message.into())
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
