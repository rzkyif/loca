//! [`AppContext`]: the handles every route slice is built from, and the two
//! traits the transport is written against rather than depending on.
//!
//! Constructed once by the frontend and cloned into each slice. It carries only
//! genuinely cross-cutting state: a package that needs its own service handle
//! builds it inside its `routes(ctx)` and captures it in the handlers.
//!
//! There is deliberately no process-global accessor for any of this: two cores
//! in one process must stay possible.

use std::sync::Arc;
use std::time::Instant;

use crate::config::Config;
use crate::contract::api::{CoreStatus, CoreStatusSnapshot};
use crate::db::Db;
use crate::error::{AppError, ErrorCode, Result};
use crate::paths::Paths;
use crate::secrets::Vault;

use super::frame_bus::FrameBus;

/// One authenticated client. The four fields are the wire-relevant half of the
/// pairing service's client row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientIdentity {
    pub id: String,
    pub name: String,
    pub created_at_ms: i64,
    pub last_seen_ms: i64,
}

/// The authentication seam, implemented by the pairing package.
///
/// The transport never reads the `clients` table, hashes a token, or knows what
/// a pairing code is; it knows there is something that turns a bearer into a
/// [`ClientIdentity`] and something that spends a WS ticket.
///
/// Sync on purpose: every operation behind it is a SQLite point query or an
/// in-memory map lookup.
pub trait ClientAuth: Send + Sync {
    /// `None` means no `Authorization` header at all, which is `missing_token`
    /// rather than `invalid_token`: the client shows a different path for each.
    fn authenticate(&self, bearer: Option<&str>) -> Result<ClientIdentity>;

    /// Mint a single-use, short-lived ticket for the next WS upgrade, so the
    /// long-lived bearer never travels in a `wss://` URL.
    fn mint_ws_ticket(&self, client_id: &str) -> String;

    /// Spend a ticket, returning the owning client id. Single use: a replayed
    /// or expired ticket returns `None`.
    fn consume_ws_ticket(&self, ticket: &str) -> Option<String>;

    /// Constant-time check of the on-disk admin token, for the pairing-code
    /// mint. Returns an `AppError` rather than a bool so the failure carries
    /// its own code.
    fn verify_admin_token(&self, provided: Option<&str>) -> Result<()>;

    /// Reported by `/health`, so a paired client knows whether it can mint
    /// codes with a password instead of reading the on-disk token.
    fn has_admin_password(&self) -> bool;
}

/// The aggregate core status, read by `/health` and broadcast as `core.status`.
///
/// A seam so a test can inject a fixed snapshot. Production uses
/// [`super::status::CoreStatusService`].
pub trait StatusProvider: Send + Sync {
    fn snapshot(&self) -> CoreStatusSnapshot;
}

/// Everything a route slice is handed.
#[derive(Clone)]
pub struct AppContext {
    pub config: Arc<Config>,
    pub paths: Arc<Paths>,
    pub db: Db,
    pub vault: Vault,
    pub auth: Arc<dyn ClientAuth>,
    /// The core-settings store. One handle for the whole process: D3, D6 and D8
    /// read it and D1 writes through it, and a second would mean two caches and
    /// two locks over one file.
    pub settings: crate::settings::Settings,
    pub frames: Arc<FrameBus>,
    pub status: Arc<dyn StatusProvider>,
    /// `server.behindProxy`: Core is fronted by a terminating HTTPS proxy, so
    /// clients validate the proxy's real certificate instead of pinning ours
    /// and pairing folds no pin. Resolved at boot from the raw settings file or
    /// `LOCA_CORE_BEHIND_PROXY`, never from an API-writable setting: a paired
    /// client must not be able to weaken the pairing posture.
    pub behind_proxy: bool,
    /// `base64(SHA-256(SPKI))` of the certificate this process serves, which
    /// the pairing handshake binds. `None` on the in-process path, where there
    /// is no TLS and no pairing.
    pub tls_pin: Option<String>,
    /// What this core can actually do, answered on `GET /api/v1/health` so the
    /// client gates on CORE capability rather than on which device renders: a
    /// mobile client paired to a desktop core must still show extensions.
    pub capabilities: super::health::Capabilities,
    /// Process start, for `uptimeMs`.
    pub started_at: Instant,
}

impl AppContext {
    pub fn uptime_ms(&self) -> i64 {
        self.started_at.elapsed().as_millis() as i64
    }
}

/// A [`StatusProvider`] that reports a fixed snapshot, for tests and for a
/// frontend with no status aggregator wired yet.
pub struct FixedStatus(pub CoreStatusSnapshot);

impl Default for FixedStatus {
    fn default() -> Self {
        Self(CoreStatusSnapshot {
            status: CoreStatus::Idle,
            detail: None,
            progress: None,
            queues: None,
            subsystems: Vec::new(),
        })
    }
}

impl StatusProvider for FixedStatus {
    fn snapshot(&self) -> CoreStatusSnapshot {
        self.0.clone()
    }
}

/// A [`ClientAuth`] that rejects everything, so a transport-only test has a
/// well-defined answer rather than an `Option` every call site unwraps.
pub struct DenyAllAuth;

impl ClientAuth for DenyAllAuth {
    fn authenticate(&self, bearer: Option<&str>) -> Result<ClientIdentity> {
        Err(match bearer {
            None => AppError::new(
                ErrorCode::MissingToken,
                "missing Authorization: Bearer header",
            ),
            Some(_) => AppError::new(ErrorCode::InvalidToken, "unknown token"),
        })
    }
    fn mint_ws_ticket(&self, _client_id: &str) -> String {
        String::new()
    }
    fn consume_ws_ticket(&self, _ticket: &str) -> Option<String> {
        None
    }
    fn verify_admin_token(&self, _provided: Option<&str>) -> Result<()> {
        Err(AppError::new(
            ErrorCode::AdminTokenRequired,
            "admin token required",
        ))
    }
    fn has_admin_password(&self) -> bool {
        false
    }
}
