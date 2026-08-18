//! Doubles for the seams the transport is written against, so a transport test
//! needs no pairing service, no keychain and no listener.
//!
//! Also compiled under the `in-memory` feature, so the conformance harness and
//! other packages' tests can build a working [`AppContext`] without repeating
//! this.

// Test support: a failure to build a fixture is a broken test, and panicking
// says so at the line that caused it rather than propagating a `Result` through
// every helper. Never compiled into a production build.
#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::config::{Channel, Config, CORE_VERSION};
use crate::db::Db;
use crate::error::{AppError, ErrorCode, Result};
use crate::paths::Paths;
use crate::secrets::Vault;

use super::frame_bus::FrameBus;
use super::state::{AppContext, ClientAuth, ClientIdentity, FixedStatus, StatusProvider};

/// A [`ClientAuth`] backed by a map, with a fixed admin token.
///
/// It answers the same questions the real service answers and nothing else,
/// which is the whole value of the seam: none of the transport's behaviour
/// depends on how a token became a client.
pub struct StubAuth {
    clients: Mutex<HashMap<String, ClientIdentity>>,
    tickets: Mutex<HashMap<String, String>>,
    admin_token: Option<String>,
    admin_password_set: bool,
    next_ticket: Mutex<u64>,
}

impl StubAuth {
    pub fn new() -> Self {
        Self {
            clients: Mutex::new(HashMap::new()),
            tickets: Mutex::new(HashMap::new()),
            admin_token: None,
            admin_password_set: false,
            next_ticket: Mutex::new(0),
        }
    }

    /// Register `token` as belonging to a client named `id`.
    pub fn with_client(self, token: &str, id: &str) -> Self {
        lock(&self.clients).insert(
            token.to_string(),
            ClientIdentity {
                id: id.to_string(),
                name: format!("{id}-device"),
                created_at_ms: 0,
                last_seen_ms: 0,
            },
        );
        self
    }

    pub fn with_admin_token(mut self, token: &str) -> Self {
        self.admin_token = Some(token.to_string());
        self
    }

    pub fn with_admin_password(mut self) -> Self {
        self.admin_password_set = true;
        self
    }
}

impl Default for StubAuth {
    fn default() -> Self {
        Self::new()
    }
}

impl ClientAuth for StubAuth {
    fn authenticate(&self, bearer: Option<&str>) -> Result<ClientIdentity> {
        let Some(token) = bearer else {
            return Err(AppError::new(
                ErrorCode::MissingToken,
                "missing Authorization: Bearer header",
            ));
        };
        lock(&self.clients)
            .get(token)
            .cloned()
            .ok_or_else(|| AppError::new(ErrorCode::InvalidToken, "unknown token"))
    }

    fn mint_ws_ticket(&self, client_id: &str) -> String {
        let mut next = lock(&self.next_ticket);
        *next += 1;
        let ticket = format!("ticket-{next}");
        lock(&self.tickets).insert(ticket.clone(), client_id.to_string());
        ticket
    }

    fn consume_ws_ticket(&self, ticket: &str) -> Option<String> {
        // Single use, exactly like the real one: gone whether or not it was
        // still valid.
        lock(&self.tickets).remove(ticket)
    }

    fn verify_admin_token(&self, provided: Option<&str>) -> Result<()> {
        match (&self.admin_token, provided) {
            (Some(expected), Some(got)) if expected == got => Ok(()),
            _ => Err(AppError::new(
                ErrorCode::AdminTokenRequired,
                "admin token required",
            )),
        }
    }

    fn has_admin_password(&self) -> bool {
        self.admin_password_set
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// An [`AppContext`] over an in-memory database, a temp-free vault and stub
/// auth. The database and vault are real types, so a slice mounted against this
/// context behaves as it does in production.
pub fn context() -> AppContext {
    context_with(Arc::new(StubAuth::new()))
}

/// A `Core` over [`context`], with a real `Auth` sharing that context's
/// database and paths.
///
/// The pairing slice needs the concrete handle, so a test that builds a router
/// cannot use the `StubAuth` double alone. Everything else about the context is
/// unchanged, including the in-memory database, so this stays a unit-test
/// fixture rather than a boot.
pub fn core() -> super::Core {
    core_with(Arc::new(StubAuth::new()))
}

/// The same, over an auth double the caller configured.
///
/// A slice test that wants to reach an authenticated route through the fully
/// assembled router needs this rather than [`core`], whose `StubAuth` knows no
/// clients and answers every bearer with 401.
pub fn core_with(auth: Arc<dyn ClientAuth>) -> super::Core {
    let ctx = context_with(auth);
    // The concrete `Auth` alongside the double: the pairing slice needs the
    // real handle and a trait object cannot be narrowed back. It shares this
    // context's database and paths, so this stays a unit-test fixture.
    let real = Arc::new(crate::auth::Auth::new(
        ctx.db.clone(),
        Arc::clone(&ctx.paths),
    ));
    let services = super::router::Services::unwired(&ctx, real)
        .unwrap_or_else(|e| panic!("unwired services: {e}"));
    super::Core::new(ctx, services)
}

/// The same, with an auth double the caller configured.
pub fn context_with(auth: Arc<dyn ClientAuth>) -> AppContext {
    let root = std::env::temp_dir().join(format!("loca-transport-test-{}", unique()));
    let paths = Paths::rooted(&root);
    let db = Db::open_in_memory().unwrap_or_else(|e| panic!("in-memory db: {e}"));
    let frames = Arc::new(FrameBus::new());
    AppContext {
        // The desktop shape, which is what every fixture here assumes: these
        // helpers build a core with a listener. A test that needs the embedded
        // answers overrides this field rather than booting.
        capabilities: super::health::Capabilities::detect(crate::boot::Frontend::Service),
        config: Arc::new(Config {
            channel: Channel::Stable,
            host: "127.0.0.1".to_string(),
            port: 0,
            version: CORE_VERSION,
            storage_base_url: "https://get.loca.rzkyif.com".to_string(),
            hf_base_url: "https://huggingface.co".to_string(),
        }),
        db: db.clone(),
        vault: Vault::with_store(
            paths.secrets_enc_file(),
            Arc::new(crate::secrets::testing::MemoryStore::new()),
        ),
        settings: crate::settings::Settings::with_file(
            paths.settings_file(),
            db.clone(),
            Arc::clone(&frames),
        ),
        paths: Arc::new(paths),
        auth,
        frames,
        status: Arc::new(FixedStatus::default()) as Arc<dyn StatusProvider>,
        behind_proxy: false,
        tls_pin: None,
        started_at: Instant::now(),
    }
}

fn unique() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let seq = NEXT.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id() as u64;
    pid << 20 | seq
}
