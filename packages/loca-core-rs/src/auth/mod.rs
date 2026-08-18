//! Pairing and authentication.
//!
//! Work package **F7** (`docs/rust-port/registry.md`). This module is that
//! package's owned path; nothing outside it writes here.
//!
//! # The pairing flow
//!
//! 1. The operator, or a client running on the host, calls `POST /pairing/codes`
//!    with `X-Admin-Token` set to the contents of `<core>/.admin-token`. Core
//!    mints a 6-digit code with a 10-minute TTL and stores it in the clear,
//!    since the PAKE keys off the code value itself.
//! 2. The Client runs a CPace handshake keyed by that code over the TLS
//!    connection: `POST /pairing/pake/start` then `POST /pairing/pake/finish`.
//!    The certificate pin each side observed is folded into **both** the CPace
//!    channel identifier (so the derived key is certificate-bound) and the key
//!    confirmation, so a man in the middle that re-terminates TLS with its own
//!    certificate is detected even though it relays the PAKE messages faithfully:
//!    the keys diverge and the confirmation fails. On success Core mints a
//!    32-byte bearer token, stores `sha256(token)` under a new `clients` row,
//!    claims the code, and returns the token plus its own confirmation so the
//!    Client can authenticate Core in turn.
//! 3. Every later request bearer-authenticates by `sha256(token)` lookup.
//!
//! # The budgets
//!
//! Per code, five failed confirmations poison it. A wrong code and a substituted
//! certificate both surface as a failed confirmation, so both are counted. Per
//! IP, twenty handshake starts per ten-minute sliding window. The admin password
//! has its own failure budget, charged to the client id and the peer address at
//! once. Every value is a constant below and none of them is a knob.

mod cpace;
mod middleware;
// The client-row to transport-identity conversion, re-exported for `boot`, which
// needs it for the embedded frontend's local client. Crate-visible rather than
// public: nothing outside the core has an `AuthedClient` to convert.
pub(crate) use middleware::to_identity;
mod password;
mod ratelimit;
pub mod routes;
mod secretgen;
mod store;
mod tickets;

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::future::BoxFuture;
use indexmap::IndexMap;

use crate::config::CORE_VERSION;
use crate::db::Db;
use crate::error::{internal, AppError, ErrorCode, Result};
use crate::files::write_owner_atomic;
use crate::paths::Paths;

use cpace::{Isk, Role};
use ratelimit::{IpRateLimiter, PasswordRateLimiter};
use store::ClientRow;
use tickets::TicketTable;

pub use secretgen::SecretString;

// --- budgets -----------------------------------------------------------------

/// Default pairing-code lifetime when the caller requests none.
const DEFAULT_CODE_TTL_MS: i64 = 10 * 60 * 1000;
/// Ceiling on a caller-requested lifetime. A larger request is clamped, not
/// refused, which is what the TypeScript does and what the Zod schema's own
/// 60-minute maximum already implies.
const MAX_CODE_TTL_MS: i64 = 60 * 60 * 1000;
/// Failed confirmations before a code is poisoned. This is what throttles a
/// brute-force run over the 6-digit space to five guesses per minted code.
const MAX_ATTEMPTS_PER_CODE: i64 = 5;
const CLAIMS_PER_IP_WINDOW_MS: i64 = 10 * 60 * 1000;
const CLAIMS_PER_IP_MAX: usize = 20;
/// Hard cap on distinct source addresses tracked at once, past which fully
/// expired entries are swept. Only reachable off loopback, and cheap.
const MAX_TRACKED_IPS: usize = 10_000;
/// `last_seen_ms` is written at most this often per client, so an authenticated
/// request is not also a database write.
const LAST_SEEN_DEBOUNCE_MS: i64 = 60_000;

// Admin-password verification throttle. A failed guess is counted against the
// caller's client id AND its peer address, so neither rotating one nor the other
// buys a fresh budget, plus a global bucket. Past the per-key cap the key is
// locked for the rest of the window. The password path is already gated behind a
// valid bearer, so this is defence in depth against a compromised but limited
// client; argon2id supplies the per-guess cost and runs only AFTER this passes.
const PASSWORD_FAIL_WINDOW_MS: i64 = 10 * 60 * 1000;
const PASSWORD_FAIL_MAX_PER_KEY: usize = 5;
const PASSWORD_FAIL_MAX_GLOBAL: usize = 50;

/// In-flight handshakes live only between start and finish, which is seconds.
const PAKE_SESSION_TTL_MS: i64 = 2 * 60 * 1000;
const MAX_PAKE_SESSIONS: usize = 64;

// Short-lived, single-use WS upgrade tickets. The long-lived bearer stays in the
// Authorization header of the mint call; only the ticket travels in the ws:// URL
// query, so a fronting reverse proxy's access log leaks at most a 30-second
// single-use token, never the full-privilege bearer.
const WS_TICKET_TTL_MS: i64 = 30 * 1000;
const MAX_WS_TICKETS: usize = 256;

// --- the in-process client ---------------------------------------------------

/// The reserved client id the embedded (mobile) frontend acts as.
///
/// Doubles as the row's `token_hash`, which is what makes it unmatchable from
/// the wire: [`Auth::authenticate`] compares `sha256_hex(bearer)`, always 64
/// hex characters, so no bearer can ever hash to `local`.
pub const LOCAL_CLIENT_ID: &str = "local";

/// What the paired-devices list calls the reserved row. It IS a client of this
/// core, it owns `client_settings` rows, and the list is the factual answer to
/// who can act here, so it is shown rather than hidden.
const LOCAL_CLIENT_NAME: &str = "This device";

// --- values ------------------------------------------------------------------

/// An authenticated client, as the middleware puts it in the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthedClient {
    pub id: String,
    pub name: String,
    pub created_at_ms: i64,
    pub last_seen_ms: i64,
}

/// A freshly minted pairing code and when it stops working.
#[derive(Debug, Clone)]
pub struct PairingCode {
    pub code: String,
    pub expires_at_ms: i64,
}

/// One row of the paired-devices list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairedClient {
    pub id: String,
    pub name: String,
    pub created_at_ms: i64,
    pub last_seen_ms: i64,
    pub is_me: bool,
}

/// The responder's half of a started handshake.
#[derive(Debug, Clone)]
pub struct PakeStarted {
    /// Opaque handle for the in-flight handshake, echoed back on finish.
    pub pake_id: String,
    /// Core's CPace public element (Yb).
    pub msg_b: [u8; 32],
}

/// The result of a completed handshake. The route base64-encodes `confirm_s` and
/// takes the token out of its wrapper on the way to the response body.
#[derive(Debug)]
pub struct PakeFinished {
    pub token: SecretString,
    pub client_id: String,
    pub core_version: String,
    pub confirm_s: [u8; 32],
}

/// Milliseconds since the epoch, injectable so the TTL and window behaviour is
/// testable without sleeping.
#[derive(Clone, Debug)]
pub enum Clock {
    System,
    Fixed(Arc<AtomicI64>),
}

impl Clock {
    pub fn system() -> Self {
        Clock::System
    }

    /// A clock frozen at `now_ms`, moved only by [`Clock::advance`].
    pub fn fixed(now_ms: i64) -> Self {
        Clock::Fixed(Arc::new(AtomicI64::new(now_ms)))
    }

    /// Move a fixed clock forward. A no-op on the system clock.
    pub fn advance(&self, by_ms: i64) {
        if let Clock::Fixed(now) = self {
            now.fetch_add(by_ms, Ordering::SeqCst);
        }
    }

    pub fn now_ms(&self) -> i64 {
        match self {
            // A clock before 1970 is not a case worth a fallible signature: it
            // reads as the epoch and every TTL comparison still behaves.
            Clock::System => SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or_default(),
            Clock::Fixed(now) => now.load(Ordering::SeqCst),
        }
    }
}

/// A side effect of revoking a client, owed by a package this one cannot reach.
pub type RevocationHook = Arc<dyn Fn(String) -> BoxFuture<'static, ()> + Send + Sync>;

/// The side effects of revoking or re-keying a client.
///
/// Each field is an obligation on a named package, and each defaults to a no-op
/// that does nothing on purpose rather than to an unset `Option` that might mean
/// "not wired yet" or "deliberately empty". A hook still at its default is
/// unimplemented; the package named in its doc comment owes it.
#[derive(Clone)]
pub struct RevocationHooks {
    /// **Owed by D2 (sessions).** Delete every session owned by the client and
    /// unlink its attachments. Sessions are JSON files on disk with no foreign
    /// key to cascade, so nothing else reclaims them.
    pub gc_sessions: RevocationHook,
    /// **Owed by D1 (settings).** Drop the client's in-memory settings overlay.
    /// Its `client_settings` rows cascade with the `clients` row; the cache in
    /// front of them does not.
    pub drop_settings_cache: RevocationHook,
    /// **Owed by F6 (transport).** Close every live WebSocket for the client. A
    /// socket authenticates once, at upgrade, so without this a revoked or
    /// re-keyed client keeps its existing connection.
    pub close_sockets: RevocationHook,
}

impl Default for RevocationHooks {
    fn default() -> Self {
        let noop: RevocationHook = Arc::new(|_| Box::pin(async {}));
        Self {
            gc_sessions: Arc::clone(&noop),
            drop_settings_cache: Arc::clone(&noop),
            close_sockets: noop,
        }
    }
}

impl fmt::Debug for RevocationHooks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RevocationHooks { .. }")
    }
}

/// One in-flight CPace handshake, parked between start and finish.
struct PakeSession {
    isk: Isk,
    msg_a: Vec<u8>,
    msg_b: [u8; 32],
    /// The code this handshake bound. Checked again at finish, because the code
    /// can be claimed, poisoned, or rotated in between.
    code: String,
    client_name: String,
    expires_at_ms: i64,
    /// The peer address, so over-cap eviction can target the heaviest holder.
    ip: String,
}

impl fmt::Debug for PakeSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Deliberately hand-written: this holds the derived key and the pairing
        // code, and a derived Debug would put both into any `{:?}`.
        f.write_str("PakeSession(<REDACTED>)")
    }
}

// --- the service -------------------------------------------------------------

/// Pairing, bearer authentication, and the admin credentials.
///
/// Constructed once and shared as an `Arc`. The in-memory state sits behind
/// short-lived mutexes that are never held across an `.await`.
pub struct Auth {
    db: Db,
    paths: Arc<Paths>,
    clock: Clock,
    hooks: RevocationHooks,
    hooks_wired: bool,
    ip_limiter: Mutex<IpRateLimiter>,
    password_limiter: Mutex<PasswordRateLimiter>,
    last_seen_write_at: Mutex<HashMap<String, i64>>,
    pake_sessions: Mutex<IndexMap<String, PakeSession>>,
    ws_tickets: Mutex<TicketTable>,
}

impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Auth").finish_non_exhaustive()
    }
}

impl Auth {
    pub fn new(db: Db, paths: Arc<Paths>) -> Self {
        Self {
            db,
            paths,
            clock: Clock::system(),
            hooks: RevocationHooks::default(),
            hooks_wired: false,
            ip_limiter: Mutex::new(IpRateLimiter::new()),
            password_limiter: Mutex::new(PasswordRateLimiter::new()),
            last_seen_write_at: Mutex::new(HashMap::new()),
            pake_sessions: Mutex::new(IndexMap::new()),
            ws_tickets: Mutex::new(TicketTable::new()),
        }
    }

    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// Whether real revocation hooks were installed.
    ///
    /// A read port for the boot-seam suite (D-165), set by the ACT of wiring
    /// rather than restated: the default hooks are no-ops and a no-op is
    /// indistinguishable from a real hook by calling it, so "did boot wire
    /// these" cannot be observed through the trait. What it guards is that
    /// revoking a client actually closes its live sockets, which a WebSocket
    /// authenticating once at upgrade makes a security property rather than
    /// tidiness.
    pub fn hooks_wired(&self) -> bool {
        self.hooks_wired
    }

    pub fn with_hooks(mut self, hooks: RevocationHooks) -> Self {
        self.hooks_wired = true;
        self.hooks = hooks;
        self
    }

    fn now(&self) -> i64 {
        self.clock.now_ms()
    }

    // --- admin token ---------------------------------------------------------

    /// Compare a presented `X-Admin-Token` against the one on disk.
    ///
    /// The token is high-entropy and proves access to the machine's filesystem,
    /// so there is no rate limit here: guessing it is not the attack.
    pub async fn verify_admin_token(&self, provided: Option<&str>) -> Result<()> {
        let Some(provided) = provided else {
            return Err(AppError::new(
                ErrorCode::AdminTokenRequired,
                "missing X-Admin-Token",
            ));
        };
        let on_disk = read_trimmed(&self.paths.admin_token_file()).await;
        let Some(on_disk) = on_disk.filter(|t| !t.is_empty()) else {
            return Err(AppError::new(
                ErrorCode::AdminTokenRequired,
                "core has no admin token on disk; reinstall to mint one",
            ));
        };
        if !secretgen::constant_time_eq(provided, &on_disk) {
            return Err(AppError::new(
                ErrorCode::AdminTokenRequired,
                "admin token mismatch",
            ));
        }
        Ok(())
    }

    // --- admin password ------------------------------------------------------

    /// Whether an admin password is configured. Synchronous because the health
    /// route reports it without awaiting; a missing or empty file is "no".
    pub fn has_admin_password(&self) -> bool {
        std::fs::metadata(self.paths.admin_password_file())
            .map(|m| m.len() > 0)
            .unwrap_or(false)
    }

    /// Set or replace the admin password.
    ///
    /// The caller must already be authorized by the admin token; this does not
    /// re-check. The length floor is enforced by the generated inbound type, so
    /// there is no second check here either.
    pub async fn set_admin_password(&self, password: &str) -> Result<()> {
        let password = password.to_owned();
        // argon2id at 19 MiB is tens of milliseconds, which is far too long to
        // hold a runtime worker.
        let encoded = spawn_blocking(move || password::hash_admin_password(&password)).await??;
        write_owner_atomic(self.paths.admin_password_file(), encoded.into_bytes()).await?;
        tracing::info!(target: "loca::auth", "admin password set");
        Ok(())
    }

    /// Verify the admin password for a bearer-plus-password privileged action:
    /// minting a code, or revoking another device.
    ///
    /// **The order of the checks is the defence and must not be rearranged.**
    /// The throttle is cheap and runs first; argon2id is expensive and runs
    /// last, so a flood of wrong guesses from a compromised client cannot pin a
    /// CPU. A failure is charged to the client id and the peer address together,
    /// so rotating either alone buys nothing.
    pub async fn verify_admin_password(
        &self,
        provided: Option<&str>,
        ip: &str,
        client_id: &str,
    ) -> Result<()> {
        let keys = vec![format!("c:{client_id}"), format!("ip:{ip}")];
        let now = self.now();
        if lock(&self.password_limiter).locked(&keys, now) {
            return Err(AppError::new(
                ErrorCode::AdminPasswordRateLimited,
                "too many password attempts; try later",
            ));
        }
        let Some(provided) = provided else {
            return Err(AppError::new(
                ErrorCode::AdminPasswordRequired,
                "missing admin password",
            ));
        };
        let stored = read_trimmed(&self.paths.admin_password_file())
            .await
            .unwrap_or_default();
        if stored.is_empty() {
            return Err(AppError::new(
                ErrorCode::AdminPasswordRequired,
                "core has no admin password set; set one to pair remotely",
            ));
        }

        let candidate = provided.to_owned();
        let ok =
            spawn_blocking(move || password::verify_admin_password(&candidate, &stored)).await?;
        if !ok {
            lock(&self.password_limiter).record_failure(&keys, self.now());
            return Err(AppError::new(
                ErrorCode::AdminPasswordInvalid,
                "admin password incorrect",
            ));
        }
        lock(&self.password_limiter).reset(&keys);
        Ok(())
    }

    // --- pairing code --------------------------------------------------------

    /// Mint a pairing code, superseding any outstanding one.
    ///
    /// Only one code is ever live. Any in-flight handshake is dropped with the
    /// old code, so a handshake can never outlive the code it bound.
    pub async fn mint_pairing_code(&self, ttl_sec: Option<u32>) -> Result<PairingCode> {
        let ttl_ms = match ttl_sec {
            Some(secs) if secs > 0 => (i64::from(secs) * 1000).min(MAX_CODE_TTL_MS),
            _ => DEFAULT_CODE_TTL_MS,
        };
        let code = secretgen::random_numeric_code()?;
        let now = self.now();
        let expires_at_ms = now + ttl_ms;

        lock(&self.pake_sessions).clear();
        let stored = code.clone();
        self.db
            .call(move |conn| {
                store::delete_unclaimed_codes(conn)?;
                store::insert_code(conn, &stored, now, expires_at_ms)
            })
            .await?;
        tracing::info!(
            target: "loca::auth",
            "minted pairing code, expires in {}s",
            ttl_ms / 1000
        );
        Ok(PairingCode {
            code,
            expires_at_ms,
        })
    }

    // --- the handshake -------------------------------------------------------

    /// Step 1: run the CPace responder against the active code and park the
    /// session until finish.
    ///
    /// `server_pin` is Core's own certificate pin, folded into the CPace channel
    /// identifier so the derived key is certificate-bound. It is the empty
    /// string when Core is behind a terminating HTTPS proxy, where the Client
    /// validates the proxy's certificate through WebPKI and the two sides have
    /// no shared value to bind.
    ///
    /// This step does not reveal whether the Client used the right code. The
    /// confirmation at finish decides that.
    pub async fn pake_start(
        &self,
        sid: &[u8],
        msg_a: &[u8],
        client_name: &str,
        ip: &str,
        server_pin: &str,
    ) -> Result<PakeStarted> {
        let now = self.now();
        if !lock(&self.ip_limiter).record_and_check(ip, now) {
            return Err(AppError::new(
                ErrorCode::PairingRateLimited,
                "too many pairing attempts from your IP",
            ));
        }
        let Some(row) = self.db.call(|conn| store::active_code(conn)).await? else {
            return Err(AppError::new(
                ErrorCode::InvalidPairingCode,
                "no active pairing code",
            ));
        };
        if self.now() > row.expires_at_ms {
            return Err(AppError::new(
                ErrorCode::PairingCodeExpired,
                "pairing code has expired",
            ));
        }

        // A malformed or identity msgA deliberately does NOT burn an attempt: a
        // structurally invalid message is not a code guess, and counting it would
        // let an unpaired peer destroy the legitimate user's active code with
        // five cheap junk requests. Real wrong guesses are counted at finish,
        // where the confirmation actually fails, and the per-IP limiter already
        // bounds the junk.
        let result = cpace::responder(&row.code, sid, msg_a, server_pin.as_bytes())
            .map_err(|_| AppError::new(ErrorCode::InvalidPairingCode, "invalid pairing message"))?;

        let pake_id = secretgen::random_pake_id()?;
        {
            let mut sessions = lock(&self.pake_sessions);
            prune_pake_sessions(&mut sessions, self.now());
            sessions.insert(
                pake_id.clone(),
                PakeSession {
                    isk: result.isk,
                    msg_a: msg_a.to_vec(),
                    msg_b: result.msg_b,
                    code: row.code,
                    client_name: client_name.to_owned(),
                    expires_at_ms: self.now() + PAKE_SESSION_TTL_MS,
                    ip: ip.to_owned(),
                },
            );
        }
        Ok(PakeStarted {
            pake_id,
            msg_b: result.msg_b,
        })
    }

    /// Step 2: verify the Client's confirmation, which binds the pin **it**
    /// observed, against Core's own pin.
    ///
    /// A wrong code and a substituted certificate both fail here, and both are
    /// counted against the code. On success this mints the token, creates the
    /// client, claims the code, and returns Core's own confirmation so the
    /// Client can authenticate Core and confirm the pin.
    pub async fn pake_finish(
        &self,
        pake_id: &str,
        confirm_c: &[u8],
        server_pin: &str,
    ) -> Result<PakeFinished> {
        // Single-use on entry: the session is gone whether or not the rest
        // succeeds, so a failed confirmation cannot be retried against the same
        // parked key.
        let Some(session) = lock(&self.pake_sessions).shift_remove(pake_id) else {
            return Err(AppError::new(
                ErrorCode::InvalidPairingCode,
                "unknown or expired pairing handshake",
            ));
        };
        if self.now() > session.expires_at_ms {
            return Err(AppError::new(
                ErrorCode::PairingCodeExpired,
                "pairing handshake expired",
            ));
        }
        let Some(row) = self.db.call(|conn| store::active_code(conn)).await? else {
            return Err(AppError::new(
                ErrorCode::PairingCodeClaimed,
                "pairing code no longer active",
            ));
        };
        if row.code != session.code {
            return Err(AppError::new(
                ErrorCode::PairingCodeClaimed,
                "pairing code no longer active",
            ));
        }
        if self.now() > row.expires_at_ms {
            return Err(AppError::new(
                ErrorCode::PairingCodeExpired,
                "pairing code has expired",
            ));
        }

        let ok = cpace::verify_confirm(
            confirm_c,
            &session.isk,
            Role::Client,
            &session.msg_a,
            &session.msg_b,
            server_pin,
        )
        .map_err(|_| internal("failed to compute the pairing confirmation"))?;
        if !ok {
            self.record_code_failure(row.id, row.attempts).await?;
            return Err(AppError::new(
                ErrorCode::InvalidPairingCode,
                "pairing confirmation failed",
            ));
        }

        // Confirmed: the Client knew the code AND observed Core's real
        // certificate. Mint and claim.
        let token = secretgen::random_token()?;
        let token_hash = secretgen::sha256_hex(token.expose());
        let client_id = ulid::Ulid::new().to_string();
        let now = self.now();
        let (id, name, code_id) = (client_id.clone(), session.client_name.clone(), row.id);
        self.db
            .call(move |conn| {
                store::insert_client(conn, &id, &name, &token_hash, now)?;
                store::claim_code(conn, code_id)
            })
            .await?;
        tracing::info!(
            target: "loca::auth",
            "paired new client \"{}\" (id={client_id})",
            session.client_name
        );

        let confirm_s = cpace::confirm_tag(
            &session.isk,
            Role::Server,
            &session.msg_a,
            &session.msg_b,
            server_pin,
        )
        .map_err(|_| internal("failed to compute the pairing confirmation"))?;
        Ok(PakeFinished {
            token,
            client_id,
            core_version: CORE_VERSION.to_owned(),
            confirm_s,
        })
    }

    /// Count a failed attempt against the active code, poisoning it at the cap.
    async fn record_code_failure(&self, id: i64, attempts: i64) -> Result<()> {
        let next = attempts + 1;
        self.db
            .call(move |conn| {
                store::set_code_attempts(conn, id, next)?;
                if next >= MAX_ATTEMPTS_PER_CODE {
                    store::claim_code(conn, id)?;
                }
                Ok(())
            })
            .await?;
        if next >= MAX_ATTEMPTS_PER_CODE {
            tracing::warn!(
                target: "loca::auth",
                "pairing code poisoned after {next} failed attempts"
            );
        }
        Ok(())
    }

    // --- bearer authentication -----------------------------------------------

    /// Resolve a bearer token to its client.
    ///
    /// The lookup is an index probe on `sha256(token)` and is therefore not
    /// constant time. That is deliberate and matches the TypeScript: the value
    /// compared is already a hash, and a database index probe cannot be made
    /// constant time anyway.
    pub async fn authenticate(&self, bearer: Option<&str>) -> Result<AuthedClient> {
        let Some(bearer) = bearer else {
            return Err(AppError::new(
                ErrorCode::MissingToken,
                "missing bearer token",
            ));
        };
        let token_hash = secretgen::sha256_hex(bearer);
        let row = self
            .db
            .call(move |conn| store::client_by_token_hash(conn, &token_hash))
            .await?;
        let Some(row) = row else {
            return Err(AppError::new(
                ErrorCode::InvalidToken,
                "bearer token not recognized",
            ));
        };
        if row.revoked {
            return Err(AppError::new(
                ErrorCode::Revoked,
                "this client has been revoked",
            ));
        }
        self.bump_last_seen(&row.id).await?;
        Ok(to_authed(row))
    }

    /// Update `last_seen_ms` at most once a minute per client, so an
    /// authenticated request is not also a write on the hot path.
    async fn bump_last_seen(&self, id: &str) -> Result<()> {
        let now = self.now();
        {
            let mut seen = lock(&self.last_seen_write_at);
            let last = seen.get(id).copied().unwrap_or(0);
            if now - last < LAST_SEEN_DEBOUNCE_MS {
                return Ok(());
            }
            seen.insert(id.to_owned(), now);
        }
        let id = id.to_owned();
        self.db
            .call(move |conn| store::update_last_seen(conn, &id, now))
            .await
    }

    // --- WS upgrade tickets --------------------------------------------------

    /// Mint a single-use, short-lived ticket for an already-authenticated
    /// client to present on its next WebSocket upgrade.
    ///
    /// This converts a proven identity into a URL-safe throwaway credential. It
    /// grants nothing the caller did not already have; its whole purpose is to
    /// keep the long-lived bearer out of the `ws://` URL, and therefore out of
    /// any reverse-proxy access log.
    pub fn mint_ws_ticket(&self, client_id: &str) -> Result<SecretString> {
        let ticket = secretgen::random_token()?;
        lock(&self.ws_tickets).insert(ticket.expose().to_owned(), client_id, self.now());
        Ok(ticket)
    }

    /// Validate and consume a ticket, returning the owning client id.
    pub fn consume_ws_ticket(&self, ticket: &str) -> Option<String> {
        lock(&self.ws_tickets).consume(ticket, self.now())
    }

    // --- the in-process client -----------------------------------------------

    /// Create, or return, the reserved row the embedded frontend acts as.
    ///
    /// A core running in process inside the app has no bearer token and no
    /// pairing: `transport::inproc` puts a [`ClientIdentity`] straight into the
    /// request extensions and `require_client` honours it, which only
    /// server-side code can do. What that identity cannot be is invented,
    /// because two tables key off a real row: `client_settings.client_id` and
    /// `scheduled_prompts.owner_client_id` are both
    /// `REFERENCES clients(id) ON DELETE CASCADE`, so a synthetic id with no row
    /// makes every per-client settings write fail on a foreign key.
    ///
    /// The stored `token_hash` is the literal [`LOCAL_CLIENT_ID`], which
    /// [`Auth::authenticate`] can never produce: it hashes the bearer with
    /// `sha256_hex`, and a 64-character hex digest is never the string `local`.
    /// So the row exists for its foreign keys and is unreachable from the wire
    /// even on a core that somehow had both a listener and an embedded
    /// frontend.
    ///
    /// Idempotent, and atomically so: [`Db::call`] holds the connection for the
    /// whole closure, so the read and the insert cannot interleave with a second
    /// caller's.
    pub async fn ensure_local_client(&self) -> Result<AuthedClient> {
        let now = self.now();
        let row = self
            .db
            .call(move |conn| {
                if let Some(row) = store::client_by_id(conn, LOCAL_CLIENT_ID)? {
                    return Ok(row);
                }
                store::insert_client(
                    conn,
                    LOCAL_CLIENT_ID,
                    LOCAL_CLIENT_NAME,
                    LOCAL_CLIENT_ID,
                    now,
                )?;
                store::client_by_id(conn, LOCAL_CLIENT_ID)?
                    .ok_or_else(|| internal("the local client row vanished after being inserted"))
            })
            .await?;
        Ok(to_authed(row))
    }

    // --- the paired-device list ----------------------------------------------

    pub async fn list_clients(&self, caller_id: &str) -> Result<Vec<PairedClient>> {
        let rows = self.db.call(|conn| store::list_clients(conn)).await?;
        Ok(rows
            .into_iter()
            .filter(|r| !r.revoked)
            .map(|r| PairedClient {
                is_me: r.id == caller_id,
                id: r.id,
                name: r.name,
                created_at_ms: r.created_at_ms,
                last_seen_ms: r.last_seen_ms,
            })
            .collect())
    }

    /// Remove a paired client and everything that belongs to it.
    ///
    /// The side effects live here rather than in the route so a second caller
    /// cannot forget one: sessions are garbage collected before the row goes,
    /// and the settings cache and live sockets are dropped after. The
    /// `client_settings` and `scheduled_prompts` rows cascade with the `clients`
    /// row. Returns whether the client existed.
    pub async fn revoke_client(&self, client_id: &str) -> Result<bool> {
        // The in-process client is not revocable. Deleting the row cascades its
        // `client_settings` and `scheduled_prompts` away and strands the
        // identity the embedded frontend already holds, pointing at a row that
        // is gone, and there is no pairing flow on a phone to make another one.
        if client_id == LOCAL_CLIENT_ID {
            return Err(AppError::new(
                ErrorCode::Forbidden,
                "This device runs the Core, so its own access cannot be removed.",
            ));
        }
        (self.hooks.gc_sessions)(client_id.to_owned()).await;
        let id = client_id.to_owned();
        let existed = self
            .db
            .call(move |conn| store::delete_client(conn, &id))
            .await?;
        if existed {
            (self.hooks.drop_settings_cache)(client_id.to_owned()).await;
            // A socket authenticates once, at upgrade, so revocation only
            // actually removes access if the live connections go too.
            (self.hooks.close_sockets)(client_id.to_owned()).await;
        }
        Ok(existed)
    }

    /// Issue a fresh bearer token for a client, invalidating the old one.
    ///
    /// The existing sockets were authenticated with the old token, so they are
    /// closed here; the Client reconnects with the new one.
    pub async fn rotate_token(&self, client_id: &str) -> Result<SecretString> {
        // Refused for the same row as `revoke_client`, and for a sharper reason
        // than symmetry: rotating would replace the unmatchable `token_hash`
        // with a real digest, minting a working bearer for the one identity that
        // is pre-authenticated by construction. Any other paired client could
        // ask for it by id.
        if client_id == LOCAL_CLIENT_ID {
            return Err(AppError::new(
                ErrorCode::Forbidden,
                "This device runs the Core, so it has no access token to replace.",
            ));
        }
        let token = secretgen::random_token()?;
        let token_hash = secretgen::sha256_hex(token.expose());
        let id = client_id.to_owned();
        self.db
            .call(move |conn| store::update_token_hash(conn, &id, &token_hash))
            .await?;
        (self.hooks.close_sockets)(client_id.to_owned()).await;
        Ok(token)
    }
}

// --- helpers -----------------------------------------------------------------

fn to_authed(row: ClientRow) -> AuthedClient {
    AuthedClient {
        id: row.id,
        name: row.name,
        created_at_ms: row.created_at_ms,
        last_seen_ms: row.last_seen_ms,
    }
}

/// Drop expired handshakes, then bound the map so a peer that starts but never
/// finishes cannot pin memory.
///
/// Over-cap eviction targets the **oldest session of whichever address holds the
/// most**, so a flood of unauthenticated starts from one peer evicts its own
/// sessions first and cannot push out another peer's lone legitimate handshake,
/// which would make that user's finish fail with "unknown or expired". Evicting
/// the globally oldest instead looks equivalent and hands an attacker exactly
/// that denial of service.
fn prune_pake_sessions(sessions: &mut IndexMap<String, PakeSession>, now: i64) {
    sessions.retain(|_, s| now <= s.expires_at_ms);
    while sessions.len() >= MAX_PAKE_SESSIONS {
        if !evict_heaviest_ip_oldest(sessions) {
            break;
        }
    }
}

/// Evict the oldest session of the address with the most. Insertion order is
/// preserved, so the first id collected for an address is its oldest.
fn evict_heaviest_ip_oldest(sessions: &mut IndexMap<String, PakeSession>) -> bool {
    let mut by_ip: IndexMap<&str, Vec<&str>> = IndexMap::new();
    for (id, session) in sessions.iter() {
        by_ip.entry(&session.ip).or_default().push(id);
    }
    let Some(heaviest) = by_ip.values().max_by_key(|ids| ids.len()) else {
        return false;
    };
    let Some(oldest) = heaviest.first().map(|id| (*id).to_owned()) else {
        return false;
    };
    sessions.shift_remove(&oldest);
    true
}

/// Read a file and trim it, or `None` if it cannot be read.
///
/// Every caller treats an unreadable credential file the same as an absent one,
/// and says so in its own error message.
async fn read_trimmed(path: &std::path::Path) -> Option<String> {
    let path = path.to_path_buf();
    spawn_blocking(move || std::fs::read_to_string(path).ok())
        .await
        .ok()
        .flatten()
        .map(|raw| raw.trim().to_owned())
}

/// Take a lock, recovering from poisoning rather than propagating it.
///
/// The guarded state is a plain map or counter, so a panic mid-update leaves it
/// structurally sound, and the release profile aborts on panic: a poisoned mutex
/// is only reachable under test.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Run CPU-bound work off the runtime.
async fn spawn_blocking<T, F>(f: F) -> Result<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|err| internal(format!("auth task failed: {err}")))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
