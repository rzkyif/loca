//! The service-level suite: the pairing lifecycle, the budgets, and the
//! credential paths.
//!
//! Every time-sensitive assertion drives a fixed [`Clock`] rather than sleeping,
//! so the TTL boundaries are asserted exactly rather than approximately.

use super::*;
use crate::db::migrate;
use tempfile::TempDir;

/// Core's certificate pin in these tests. A client that observed the same value
/// (no man in the middle) folds it too.
const SERVER_PIN: &str = "server-cert-pin";

struct Env {
    auth: Auth,
    clock: Clock,
    /// The same handle `auth` holds, so a test can assert on a column the
    /// service deliberately does not expose. `token_hash` is the one that
    /// matters: it is the whole security property of the reserved local row,
    /// and no public method returns it.
    db: Db,
    _dir: TempDir,
}

fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open_in_memory().unwrap();
    db.call_blocking(|conn| migrate(conn)).unwrap();
    let clock = Clock::fixed(1_700_000_000_000);
    let auth = Auth::new(db.clone(), Arc::new(Paths::rooted(dir.path()))).with_clock(clock.clone());
    Env {
        auth,
        clock,
        db,
        _dir: dir,
    }
}

/// The stored `token_hash` of one client row.
fn token_hash(db: &Db, id: &str) -> String {
    let id = id.to_string();
    db.call_blocking(move |conn| {
        Ok(conn.query_row(
            "SELECT token_hash FROM clients WHERE id = ?",
            (&id,),
            |row| row.get::<_, String>(0),
        )?)
    })
    .unwrap()
}

struct PakeOpts<'a> {
    client_name: &'a str,
    ip: &'a str,
    server_pin: &'a str,
    /// The pin the simulated client observed. Differs from `server_pin` under a
    /// man in the middle.
    client_pin: &'a str,
}

impl Default for PakeOpts<'_> {
    fn default() -> Self {
        Self {
            client_name: "c",
            ip: "127.0.0.1",
            server_pin: SERVER_PIN,
            client_pin: SERVER_PIN,
        }
    }
}

/// The full client half of the handshake, run against the service.
async fn run_pake(auth: &Auth, code: &str, opts: PakeOpts<'_>) -> Result<PakeFinished> {
    let sid = [0x5au8; 16];
    let mut initiator = cpace::initiator_with(
        code,
        &sid,
        opts.client_pin.as_bytes(),
        &[],
        &[],
        cpace::sample_scalar().unwrap(),
    );
    let started = auth
        .pake_start(
            &sid,
            &initiator.msg_a,
            opts.client_name,
            opts.ip,
            opts.server_pin,
        )
        .await?;
    // The client derives its own key from Core's reply and confirms under the
    // pin IT observed.
    let isk = initiator.finish(&started.msg_b).unwrap();
    let confirm_c = cpace::confirm_tag(
        &isk,
        cpace::Role::Client,
        &initiator.msg_a,
        &started.msg_b,
        opts.client_pin,
    )
    .unwrap();
    auth.pake_finish(&started.pake_id, &confirm_c, opts.server_pin)
        .await
}

#[track_caller]
fn assert_err<T: std::fmt::Debug>(result: Result<T>, code: ErrorCode, contains: &str) {
    let err = result.expect_err("expected an error");
    assert_eq!(err.code, code, "wrong code, message was {:?}", err.message);
    assert!(
        err.message.contains(contains),
        "expected {:?} to contain {contains:?}",
        err.message
    );
}

// --- pairing codes -----------------------------------------------------------

#[tokio::test]
async fn a_minted_code_is_six_digits_with_a_future_expiry() {
    let env = env();
    let minted = env.auth.mint_pairing_code(None).await.unwrap();
    assert_eq!(minted.code.len(), 6);
    assert!(minted.code.chars().all(|c| c.is_ascii_digit()));
    assert_eq!(
        minted.expires_at_ms,
        env.clock.now_ms() + DEFAULT_CODE_TTL_MS
    );
}

#[tokio::test]
async fn a_requested_lifetime_is_clamped_to_the_ceiling_not_refused() {
    let env = env();
    let now = env.clock.now_ms();
    let short = env.auth.mint_pairing_code(Some(60)).await.unwrap();
    assert_eq!(short.expires_at_ms, now + 60_000);
    let huge = env.auth.mint_pairing_code(Some(86_400)).await.unwrap();
    assert_eq!(huge.expires_at_ms, now + MAX_CODE_TTL_MS);
}

#[tokio::test]
async fn a_new_code_supersedes_every_outstanding_one() {
    let env = env();
    let first = env.auth.mint_pairing_code(None).await.unwrap();
    env.auth.mint_pairing_code(None).await.unwrap();
    // Core now answers with the NEW code, so a handshake keyed by the old one
    // derives a different key and fails confirmation.
    assert_err(
        run_pake(&env.auth, &first.code, PakeOpts::default()).await,
        ErrorCode::InvalidPairingCode,
        "confirmation failed",
    );
}

#[tokio::test]
async fn minting_drops_every_in_flight_handshake() {
    let env = env();
    let minted = env.auth.mint_pairing_code(None).await.unwrap();
    let sid = [1u8; 16];
    let mut initiator = cpace::initiator_with(
        &minted.code,
        &sid,
        SERVER_PIN.as_bytes(),
        &[],
        &[],
        cpace::sample_scalar().unwrap(),
    );
    let started = env
        .auth
        .pake_start(&sid, &initiator.msg_a, "c", "127.0.0.1", SERVER_PIN)
        .await
        .unwrap();

    env.auth.mint_pairing_code(None).await.unwrap();

    let isk = initiator.finish(&started.msg_b).unwrap();
    let confirm = cpace::confirm_tag(
        &isk,
        cpace::Role::Client,
        &initiator.msg_a,
        &started.msg_b,
        SERVER_PIN,
    )
    .unwrap();
    assert_err(
        env.auth
            .pake_finish(&started.pake_id, &confirm, SERVER_PIN)
            .await,
        ErrorCode::InvalidPairingCode,
        "unknown or expired pairing handshake",
    );
}

// --- the handshake -----------------------------------------------------------

#[tokio::test]
async fn the_happy_path_pairs_and_claims_the_code() {
    let env = env();
    let minted = env.auth.mint_pairing_code(None).await.unwrap();
    let finished = run_pake(
        &env.auth,
        &minted.code,
        PakeOpts {
            client_name: "my-laptop",
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(finished.core_version, CORE_VERSION);
    let authed = env
        .auth
        .authenticate(Some(finished.token.expose()))
        .await
        .unwrap();
    assert_eq!(authed.id, finished.client_id);
    assert_eq!(authed.name, "my-laptop");

    // Single-use: a second handshake finds no active code at all.
    assert_err(
        run_pake(&env.auth, &minted.code, PakeOpts::default()).await,
        ErrorCode::InvalidPairingCode,
        "no active pairing code",
    );
}

#[tokio::test]
async fn the_client_can_authenticate_core_from_its_confirmation() {
    // The reverse direction of the handshake, which is what proves to the Client
    // that it is talking to the real Core and that its pin is correct.
    let env = env();
    let minted = env.auth.mint_pairing_code(None).await.unwrap();
    let sid = [2u8; 16];
    let mut initiator = cpace::initiator_with(
        &minted.code,
        &sid,
        SERVER_PIN.as_bytes(),
        &[],
        &[],
        cpace::sample_scalar().unwrap(),
    );
    let started = env
        .auth
        .pake_start(&sid, &initiator.msg_a, "c", "127.0.0.1", SERVER_PIN)
        .await
        .unwrap();
    let isk = initiator.finish(&started.msg_b).unwrap();
    let confirm_c = cpace::confirm_tag(
        &isk,
        cpace::Role::Client,
        &initiator.msg_a,
        &started.msg_b,
        SERVER_PIN,
    )
    .unwrap();
    let finished = env
        .auth
        .pake_finish(&started.pake_id, &confirm_c, SERVER_PIN)
        .await
        .unwrap();

    assert!(cpace::verify_confirm(
        &finished.confirm_s,
        &isk,
        cpace::Role::Server,
        &initiator.msg_a,
        &started.msg_b,
        SERVER_PIN,
    )
    .unwrap());
}

#[tokio::test]
async fn a_substituted_certificate_fails_confirmation() {
    let env = env();
    let minted = env.auth.mint_pairing_code(None).await.unwrap();
    // The client knows the right code but observed a different certificate than
    // Core serves: exactly the active man-in-the-middle-at-pairing case.
    assert_err(
        run_pake(
            &env.auth,
            &minted.code,
            PakeOpts {
                client_pin: "attacker-cert-pin",
                ..Default::default()
            },
        )
        .await,
        ErrorCode::InvalidPairingCode,
        "confirmation failed",
    );
}

#[tokio::test]
async fn a_client_folding_no_pin_against_a_self_signed_core_fails() {
    // The trust-mode boundary: Core behind no proxy always folds its real
    // fingerprint, so a webpki-mode client (or one a proxy stripped the pin
    // from) cannot complete. Empty on both sides is only reachable when the
    // operator actually set server.behindProxy.
    let env = env();
    let minted = env.auth.mint_pairing_code(None).await.unwrap();
    assert_err(
        run_pake(
            &env.auth,
            &minted.code,
            PakeOpts {
                client_pin: "",
                ..Default::default()
            },
        )
        .await,
        ErrorCode::InvalidPairingCode,
        "confirmation failed",
    );
}

#[tokio::test]
async fn behind_a_proxy_both_sides_fold_nothing_and_pairing_works() {
    let env = env();
    let minted = env.auth.mint_pairing_code(None).await.unwrap();
    let finished = run_pake(
        &env.auth,
        &minted.code,
        PakeOpts {
            server_pin: "",
            client_pin: "",
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(env
        .auth
        .authenticate(Some(finished.token.expose()))
        .await
        .is_ok());
}

#[tokio::test]
async fn five_failed_confirmations_poison_the_code() {
    let env = env();
    let minted = env.auth.mint_pairing_code(None).await.unwrap();
    let wrong = {
        let mut digits: Vec<char> = minted.code.chars().collect();
        let last = digits[5].to_digit(10).unwrap();
        digits[5] = char::from_digit((last + 1) % 10, 10).unwrap();
        digits.into_iter().collect::<String>()
    };
    for i in 0..MAX_ATTEMPTS_PER_CODE {
        // Each guess from its own address, so the per-IP limiter is not what
        // stops this: the per-code attempt cap is.
        let ip = format!("10.0.0.{}", i + 1);
        assert_err(
            run_pake(
                &env.auth,
                &wrong,
                PakeOpts {
                    ip: &ip,
                    ..Default::default()
                },
            )
            .await,
            ErrorCode::InvalidPairingCode,
            "confirmation failed",
        );
    }
    // The code is poisoned, so even the CORRECT one finds nothing active.
    assert_err(
        run_pake(&env.auth, &minted.code, PakeOpts::default()).await,
        ErrorCode::InvalidPairingCode,
        "no active pairing code",
    );
}

#[tokio::test]
async fn a_malformed_start_message_does_not_burn_an_attempt() {
    // Counting a structurally invalid message would let an unpaired peer destroy
    // the legitimate user's code with five cheap junk requests.
    let env = env();
    let minted = env.auth.mint_pairing_code(None).await.unwrap();
    for i in 0..10 {
        let ip = format!("10.1.0.{i}");
        assert_err(
            env.auth
                .pake_start(&[3u8; 16], &[0u8; 32], "c", &ip, SERVER_PIN)
                .await,
            ErrorCode::InvalidPairingCode,
            "invalid pairing message",
        );
    }
    // The code still works.
    assert!(run_pake(&env.auth, &minted.code, PakeOpts::default())
        .await
        .is_ok());
}

#[tokio::test]
async fn an_expired_code_is_rejected() {
    let env = env();
    let minted = env.auth.mint_pairing_code(Some(60)).await.unwrap();
    // The exact expiry millisecond is still valid; one past it is not.
    env.clock.advance(60_000);
    assert!(run_pake(&env.auth, &minted.code, PakeOpts::default())
        .await
        .is_ok());

    let minted = env.auth.mint_pairing_code(Some(60)).await.unwrap();
    env.clock.advance(60_001);
    assert_err(
        run_pake(&env.auth, &minted.code, PakeOpts::default()).await,
        ErrorCode::PairingCodeExpired,
        "pairing code has expired",
    );
}

#[tokio::test]
async fn an_unknown_or_replayed_handshake_is_rejected() {
    let env = env();
    let minted = env.auth.mint_pairing_code(None).await.unwrap();
    assert_err(
        env.auth
            .pake_finish("never-started", &[0u8; 32], SERVER_PIN)
            .await,
        ErrorCode::InvalidPairingCode,
        "unknown or expired pairing handshake",
    );

    let sid = [4u8; 16];
    let mut initiator = cpace::initiator_with(
        &minted.code,
        &sid,
        SERVER_PIN.as_bytes(),
        &[],
        &[],
        cpace::sample_scalar().unwrap(),
    );
    let started = env
        .auth
        .pake_start(&sid, &initiator.msg_a, "c", "127.0.0.1", SERVER_PIN)
        .await
        .unwrap();
    let isk = initiator.finish(&started.msg_b).unwrap();
    let confirm = cpace::confirm_tag(
        &isk,
        cpace::Role::Client,
        &initiator.msg_a,
        &started.msg_b,
        SERVER_PIN,
    )
    .unwrap();
    assert!(env
        .auth
        .pake_finish(&started.pake_id, &confirm, SERVER_PIN)
        .await
        .is_ok());
    // The session went on the first finish, so a replay finds nothing.
    assert_err(
        env.auth
            .pake_finish(&started.pake_id, &confirm, SERVER_PIN)
            .await,
        ErrorCode::InvalidPairingCode,
        "unknown or expired pairing handshake",
    );
}

#[tokio::test]
async fn a_handshake_past_its_own_ttl_is_rejected() {
    let env = env();
    let minted = env.auth.mint_pairing_code(Some(3600)).await.unwrap();
    let sid = [5u8; 16];
    let initiator = cpace::initiator_with(
        &minted.code,
        &sid,
        SERVER_PIN.as_bytes(),
        &[],
        &[],
        cpace::sample_scalar().unwrap(),
    );
    let started = env
        .auth
        .pake_start(&sid, &initiator.msg_a, "c", "127.0.0.1", SERVER_PIN)
        .await
        .unwrap();
    // The code is still live for another hour; the handshake is not.
    env.clock.advance(PAKE_SESSION_TTL_MS + 1);
    assert_err(
        env.auth
            .pake_finish(&started.pake_id, &[0u8; 32], SERVER_PIN)
            .await,
        ErrorCode::PairingCodeExpired,
        "pairing handshake expired",
    );
}

#[tokio::test]
async fn the_twenty_first_start_from_one_address_is_refused() {
    let env = env();
    // No active code, so each start fails for its own reason. The IP gate runs
    // first, so the bucket fills regardless.
    for _ in 0..CLAIMS_PER_IP_MAX {
        assert_err(
            env.auth
                .pake_start(&[6u8; 16], &[0u8; 32], "x", "192.168.1.1", SERVER_PIN)
                .await,
            ErrorCode::InvalidPairingCode,
            "no active pairing code",
        );
    }
    assert_err(
        env.auth
            .pake_start(&[6u8; 16], &[0u8; 32], "x", "192.168.1.1", SERVER_PIN)
            .await,
        ErrorCode::PairingRateLimited,
        "too many pairing attempts",
    );
    // A different address is unaffected, and fails for the original reason.
    assert_err(
        env.auth
            .pake_start(&[6u8; 16], &[0u8; 32], "x", "10.0.0.1", SERVER_PIN)
            .await,
        ErrorCode::InvalidPairingCode,
        "no active pairing code",
    );
}

// --- in-flight handshake eviction -------------------------------------------

fn session(ip: &str, expires_at_ms: i64) -> PakeSession {
    PakeSession {
        isk: Isk::from_bytes([0u8; 32]),
        msg_a: vec![0; 32],
        msg_b: [0; 32],
        code: "000000".into(),
        client_name: "c".into(),
        expires_at_ms,
        ip: ip.into(),
    }
}

#[test]
fn pruning_drops_expired_handshakes() {
    let mut sessions = IndexMap::new();
    sessions.insert("a".to_owned(), session("ip", 100));
    sessions.insert("b".to_owned(), session("ip", 200));
    prune_pake_sessions(&mut sessions, 150);
    assert_eq!(sessions.len(), 1);
    assert!(sessions.contains_key("b"));
    // The exact expiry millisecond is still live.
    prune_pake_sessions(&mut sessions, 200);
    assert_eq!(sessions.len(), 1);
}

/// The rule that matters: a flood from one address evicts its OWN oldest, so it
/// cannot push out another peer's single legitimate handshake and make that
/// user's finish fail with "unknown or expired". Evicting the globally oldest
/// instead looks equivalent and hands an attacker exactly that.
#[test]
fn eviction_targets_the_heaviest_address_not_the_globally_oldest() {
    let mut sessions = IndexMap::new();
    // The victim's handshake is the oldest thing in the map.
    sessions.insert("victim".to_owned(), session("10.0.0.9", i64::MAX));
    for i in 0..MAX_PAKE_SESSIONS {
        sessions.insert(format!("flood{i}"), session("10.0.0.1", i64::MAX));
    }
    prune_pake_sessions(&mut sessions, 0);
    assert!(sessions.contains_key("victim"), "the victim was evicted");
    assert!(
        !sessions.contains_key("flood0"),
        "the flooder's oldest went"
    );
    assert!(sessions.len() < MAX_PAKE_SESSIONS);
}

#[tokio::test]
async fn the_handshake_table_stays_bounded_under_a_flood() {
    let env = env();
    env.auth.mint_pairing_code(None).await.unwrap();
    // Four addresses, each up to its own per-IP cap, is the most in-flight
    // handshakes a set of peers can hold at once.
    for ip in ["10.0.0.1", "10.0.0.2", "10.0.0.3", "10.0.0.4"] {
        for i in 0..CLAIMS_PER_IP_MAX {
            let initiator = cpace::initiator_with(
                "000000",
                &[7u8; 16],
                SERVER_PIN.as_bytes(),
                &[],
                &[],
                cpace::sample_scalar().unwrap(),
            );
            env.auth
                .pake_start(
                    &[7u8; 16],
                    &initiator.msg_a,
                    &format!("c{i}"),
                    ip,
                    SERVER_PIN,
                )
                .await
                .unwrap();
        }
    }
    assert!(lock(&env.auth.pake_sessions).len() <= MAX_PAKE_SESSIONS);
}

// --- bearer authentication ---------------------------------------------------

#[tokio::test]
async fn authentication_rejects_missing_unknown_and_revoked_tokens() {
    let env = env();
    assert_err(
        env.auth.authenticate(None).await,
        ErrorCode::MissingToken,
        "missing bearer token",
    );
    assert_err(
        env.auth.authenticate(Some("not-a-real-token")).await,
        ErrorCode::InvalidToken,
        "not recognized",
    );

    let minted = env.auth.mint_pairing_code(None).await.unwrap();
    let finished = run_pake(&env.auth, &minted.code, PakeOpts::default())
        .await
        .unwrap();
    assert!(env.auth.revoke_client(&finished.client_id).await.unwrap());
    assert_err(
        env.auth.authenticate(Some(finished.token.expose())).await,
        ErrorCode::InvalidToken,
        "not recognized",
    );
}

#[tokio::test]
async fn last_seen_is_written_at_most_once_a_minute() {
    let env = env();
    let minted = env.auth.mint_pairing_code(None).await.unwrap();
    let finished = run_pake(&env.auth, &minted.code, PakeOpts::default())
        .await
        .unwrap();
    let token = finished.token.expose().to_owned();

    // The debounce is in memory and starts empty, so the first authenticated
    // request after a boot always writes. That is deliberate: it is what makes
    // "last seen" useful after a restart.
    env.clock.advance(1234);
    let first = env.clock.now_ms();
    env.auth.authenticate(Some(&token)).await.unwrap();
    assert_eq!(last_seen(&env.auth, &finished.client_id).await, first);

    // Inside the window, nothing is written.
    env.clock.advance(LAST_SEEN_DEBOUNCE_MS - 1);
    env.auth.authenticate(Some(&token)).await.unwrap();
    assert_eq!(last_seen(&env.auth, &finished.client_id).await, first);

    // One millisecond later it is.
    env.clock.advance(1);
    let next = env.clock.now_ms();
    env.auth.authenticate(Some(&token)).await.unwrap();
    assert_eq!(last_seen(&env.auth, &finished.client_id).await, next);
    assert_eq!(next - first, LAST_SEEN_DEBOUNCE_MS);
}

async fn last_seen(auth: &Auth, client_id: &str) -> i64 {
    auth.list_clients(client_id)
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.id == client_id)
        .unwrap()
        .last_seen_ms
}

/// The stored value is `sha256` of the token STRING's bytes, not of the 32 raw
/// bytes behind the base64url. Hashing the wrong one invalidates every
/// already-paired client, with no diagnosis beyond "nothing authenticates".
#[tokio::test]
async fn a_token_hash_written_by_the_typescript_still_authenticates() {
    let env = env();
    // A token exactly as `@std/encoding`'s encodeBase64Url produces, and its
    // sha256 as `shared/hash.ts` computes it.
    let token = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    let token_hash = secretgen::sha256_hex(token);
    let now = env.clock.now_ms();
    env.auth
        .db
        .call(move |conn| store::insert_client(conn, "cid", "legacy", &token_hash, now))
        .await
        .unwrap();
    let authed = env.auth.authenticate(Some(token)).await.unwrap();
    assert_eq!(authed.id, "cid");
    assert_eq!(authed.name, "legacy");
}

#[tokio::test]
async fn rotating_a_token_invalidates_the_previous_one() {
    let env = env();
    let minted = env.auth.mint_pairing_code(None).await.unwrap();
    let finished = run_pake(&env.auth, &minted.code, PakeOpts::default())
        .await
        .unwrap();
    let next = env.auth.rotate_token(&finished.client_id).await.unwrap();
    assert_ne!(next.expose(), finished.token.expose());
    assert_err(
        env.auth.authenticate(Some(finished.token.expose())).await,
        ErrorCode::InvalidToken,
        "not recognized",
    );
    assert_eq!(
        env.auth.authenticate(Some(next.expose())).await.unwrap().id,
        finished.client_id
    );
}

#[tokio::test]
async fn the_paired_list_marks_the_caller_and_omits_nobody_else() {
    let env = env();
    let mut ids = Vec::new();
    for name in ["one", "two"] {
        let minted = env.auth.mint_pairing_code(None).await.unwrap();
        let finished = run_pake(
            &env.auth,
            &minted.code,
            PakeOpts {
                client_name: name,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        ids.push(finished.client_id);
    }
    let listed = env.auth.list_clients(&ids[0]).await.unwrap();
    assert_eq!(listed.len(), 2);
    assert!(listed.iter().find(|c| c.id == ids[0]).unwrap().is_me);
    assert!(!listed.iter().find(|c| c.id == ids[1]).unwrap().is_me);
}

#[tokio::test]
async fn revoking_a_client_that_is_not_there_reports_so() {
    let env = env();
    assert!(!env.auth.revoke_client("nope").await.unwrap());
}

/// The hooks default to no-ops that do nothing deliberately, and a wired hook
/// fires with the revoked client's id. D1, D2 and F6 each owe one.
#[tokio::test]
async fn the_revocation_hooks_fire_for_the_client_that_went() {
    use std::sync::Mutex as StdMutex;

    let dir = tempfile::tempdir().unwrap();
    let db = Db::open_in_memory().unwrap();
    db.call_blocking(|conn| migrate(conn)).unwrap();
    let seen: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
    let record = |label: &'static str, seen: Arc<StdMutex<Vec<String>>>| -> RevocationHook {
        Arc::new(move |id: String| {
            let seen = Arc::clone(&seen);
            Box::pin(async move {
                seen.lock().unwrap().push(format!("{label}:{id}"));
            })
        })
    };
    let auth = Auth::new(db, Arc::new(Paths::rooted(dir.path())))
        .with_clock(Clock::fixed(0))
        .with_hooks(RevocationHooks {
            gc_sessions: record("sessions", Arc::clone(&seen)),
            drop_settings_cache: record("settings", Arc::clone(&seen)),
            close_sockets: record("sockets", Arc::clone(&seen)),
        });

    let minted = auth.mint_pairing_code(None).await.unwrap();
    let finished = run_pake(&auth, &minted.code, PakeOpts::default())
        .await
        .unwrap();
    let id = finished.client_id;
    assert!(auth.revoke_client(&id).await.unwrap());
    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            format!("sessions:{id}"),
            format!("settings:{id}"),
            format!("sockets:{id}"),
        ]
    );

    // Rotating closes the sockets too: they were authenticated with the old
    // token, which no longer exists.
    seen.lock().unwrap().clear();
    let minted = auth.mint_pairing_code(None).await.unwrap();
    let finished = run_pake(&auth, &minted.code, PakeOpts::default())
        .await
        .unwrap();
    auth.rotate_token(&finished.client_id).await.unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        vec![format!("sockets:{}", finished.client_id)]
    );
}

// --- WS tickets --------------------------------------------------------------

#[tokio::test]
async fn a_ws_ticket_converts_a_bearer_into_a_throwaway() {
    let env = env();
    let ticket = env.auth.mint_ws_ticket("client-42").unwrap();
    assert_eq!(
        env.auth.consume_ws_ticket(ticket.expose()).as_deref(),
        Some("client-42")
    );
    assert_eq!(env.auth.consume_ws_ticket(ticket.expose()), None);
    assert_eq!(env.auth.consume_ws_ticket("never-minted"), None);
}

#[tokio::test]
async fn a_ws_ticket_expires_thirty_seconds_after_it_is_minted() {
    let env = env();
    let ticket = env.auth.mint_ws_ticket("client-7").unwrap();
    env.clock.advance(WS_TICKET_TTL_MS + 1);
    assert_eq!(env.auth.consume_ws_ticket(ticket.expose()), None);
}

// --- admin token -------------------------------------------------------------

#[tokio::test]
async fn the_admin_token_is_compared_against_the_one_on_disk() {
    let env = env();
    let file = env.auth.paths.admin_token_file();
    // A trailing newline is what `bootstrap` leaves behind on some paths.
    std::fs::write(&file, "secret-admin\n").unwrap();

    env.auth
        .verify_admin_token(Some("secret-admin"))
        .await
        .unwrap();
    assert_err(
        env.auth.verify_admin_token(Some("wrong")).await,
        ErrorCode::AdminTokenRequired,
        "mismatch",
    );
    assert_err(
        env.auth.verify_admin_token(None).await,
        ErrorCode::AdminTokenRequired,
        "missing X-Admin-Token",
    );
}

#[tokio::test]
async fn a_missing_or_empty_admin_token_file_is_reported_distinctly() {
    let env = env();
    assert_err(
        env.auth.verify_admin_token(Some("anything")).await,
        ErrorCode::AdminTokenRequired,
        "no admin token on disk",
    );
    std::fs::write(env.auth.paths.admin_token_file(), "   \n").unwrap();
    assert_err(
        env.auth.verify_admin_token(Some("anything")).await,
        ErrorCode::AdminTokenRequired,
        "no admin token on disk",
    );
    // An empty presented token never matches an empty file either.
    assert_err(
        env.auth.verify_admin_token(Some("")).await,
        ErrorCode::AdminTokenRequired,
        "no admin token on disk",
    );
}

// --- admin password ----------------------------------------------------------

#[tokio::test]
async fn setting_the_admin_password_makes_it_verifiable() {
    let env = env();
    assert!(!env.auth.has_admin_password());
    env.auth
        .set_admin_password("a memorable pass")
        .await
        .unwrap();
    assert!(env.auth.has_admin_password());

    env.auth
        .verify_admin_password(Some("a memorable pass"), "127.0.0.1", "client-1")
        .await
        .unwrap();
    assert_err(
        env.auth
            .verify_admin_password(Some("nope"), "127.0.0.1", "client-1")
            .await,
        ErrorCode::AdminPasswordInvalid,
        "incorrect",
    );
}

#[tokio::test]
#[cfg(unix)]
async fn the_password_file_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let env = env();
    env.auth
        .set_admin_password("a memorable pass")
        .await
        .unwrap();
    let mode = std::fs::metadata(env.auth.paths.admin_password_file())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "a credential file must not be readable by all");
}

#[tokio::test]
async fn verifying_with_no_password_configured_says_so() {
    let env = env();
    assert_err(
        env.auth
            .verify_admin_password(Some("x"), "127.0.0.1", "client-1")
            .await,
        ErrorCode::AdminPasswordRequired,
        "no admin password",
    );
    assert_err(
        env.auth
            .verify_admin_password(None, "127.0.0.1", "client-1")
            .await,
        ErrorCode::AdminPasswordRequired,
        "missing admin password",
    );
}

#[tokio::test]
async fn the_password_locks_out_after_the_per_key_cap() {
    let env = env();
    env.auth
        .set_admin_password("the real password")
        .await
        .unwrap();
    for _ in 0..PASSWORD_FAIL_MAX_PER_KEY {
        assert_err(
            env.auth
                .verify_admin_password(Some("wrong"), "10.0.0.1", "c1")
                .await,
            ErrorCode::AdminPasswordInvalid,
            "incorrect",
        );
    }
    // Even the correct password is refused while locked.
    assert_err(
        env.auth
            .verify_admin_password(Some("the real password"), "10.0.0.1", "c1")
            .await,
        ErrorCode::AdminPasswordRateLimited,
        "too many",
    );
    // A different client and address keep their own budget.
    env.auth
        .verify_admin_password(Some("the real password"), "10.0.0.2", "c2")
        .await
        .unwrap();
    // The window rolling forward frees it.
    env.clock.advance(PASSWORD_FAIL_WINDOW_MS);
    env.auth
        .verify_admin_password(Some("the real password"), "10.0.0.1", "c1")
        .await
        .unwrap();
}

/// The throttle runs before every other check, which is what stops a compromised
/// client turning argon2id into a CPU denial of service. A locked key reports
/// `admin_password_rate_limited` even when the request carries no password at
/// all, which it could only do if the throttle came first.
#[tokio::test]
async fn the_throttle_is_checked_before_anything_expensive() {
    let env = env();
    env.auth
        .set_admin_password("the real password")
        .await
        .unwrap();
    for _ in 0..PASSWORD_FAIL_MAX_PER_KEY {
        let _ = env
            .auth
            .verify_admin_password(Some("wrong"), "10.0.0.1", "c1")
            .await;
    }
    assert_err(
        env.auth.verify_admin_password(None, "10.0.0.1", "c1").await,
        ErrorCode::AdminPasswordRateLimited,
        "too many",
    );
}

#[tokio::test]
async fn a_correct_guess_clears_the_budget_it_had_spent() {
    let env = env();
    env.auth
        .set_admin_password("the real password")
        .await
        .unwrap();
    for _ in 0..(PASSWORD_FAIL_MAX_PER_KEY - 1) {
        let _ = env
            .auth
            .verify_admin_password(Some("wrong"), "10.0.0.1", "c1")
            .await;
    }
    env.auth
        .verify_admin_password(Some("the real password"), "10.0.0.1", "c1")
        .await
        .unwrap();
    // The budget is back to full, so another four wrong guesses do not lock.
    for _ in 0..(PASSWORD_FAIL_MAX_PER_KEY - 1) {
        assert_err(
            env.auth
                .verify_admin_password(Some("wrong"), "10.0.0.1", "c1")
                .await,
            ErrorCode::AdminPasswordInvalid,
            "incorrect",
        );
    }
}

/// A failure is charged to the client id AND the peer address, so rotating
/// either one alone does not buy a fresh budget.
#[tokio::test]
async fn rotating_the_client_id_or_the_address_alone_buys_nothing() {
    let env = env();
    env.auth
        .set_admin_password("the real password")
        .await
        .unwrap();
    for _ in 0..PASSWORD_FAIL_MAX_PER_KEY {
        let _ = env
            .auth
            .verify_admin_password(Some("wrong"), "10.0.0.1", "c1")
            .await;
    }
    assert_err(
        env.auth
            .verify_admin_password(Some("the real password"), "10.9.9.9", "c1")
            .await,
        ErrorCode::AdminPasswordRateLimited,
        "too many",
    );
    assert_err(
        env.auth
            .verify_admin_password(Some("the real password"), "10.0.0.1", "c9")
            .await,
        ErrorCode::AdminPasswordRateLimited,
        "too many",
    );
}

// --- secret hygiene ----------------------------------------------------------

/// Nothing that holds a token, a password, a code, or a derived key may print
/// it. `scrub_secrets` is a backstop, not permission.
#[test]
fn no_secret_bearing_type_prints_its_contents() {
    let session = session("10.0.0.1", 0);
    assert_eq!(format!("{session:?}"), "PakeSession(<REDACTED>)");
    assert!(!format!("{session:?}").contains("000000"));

    let token = SecretString::new("a-real-token".into());
    assert!(!format!("{token:?}").contains("a-real-token"));

    let hooks = RevocationHooks::default();
    assert_eq!(format!("{hooks:?}"), "RevocationHooks { .. }");
}

// --- the in-process client ---------------------------------------------------
//
// The reserved row the embedded (mobile) frontend acts as. It exists for its
// foreign keys: `client_settings.client_id` and
// `scheduled_prompts.owner_client_id` both reference `clients(id)`, so the
// in-process identity cannot be a value invented at the call site.

#[tokio::test]
async fn ensure_local_client_creates_the_reserved_row_and_is_idempotent() {
    let env = env();
    let first = env.auth.ensure_local_client().await.unwrap();
    assert_eq!(first.id, LOCAL_CLIENT_ID);
    assert_eq!(first.name, LOCAL_CLIENT_NAME);

    // Called on every start, so a second call must return the same row rather
    // than failing on the primary key or minting a second identity whose
    // per-client settings the first could not see.
    let second = env.auth.ensure_local_client().await.unwrap();
    assert_eq!(second, first);
    assert_eq!(
        env.auth.list_clients(LOCAL_CLIENT_ID).await.unwrap().len(),
        1,
    );
}

#[tokio::test]
async fn no_bearer_can_authenticate_as_the_reserved_row() {
    // The row's `token_hash` is the literal id, and `authenticate` compares
    // `sha256_hex(bearer)`, which is always 64 lowercase hex characters. So the
    // one identity that bypasses the bearer check by construction is
    // unreachable from the wire, which is what makes it safe to have at all.
    let env = env();
    env.auth.ensure_local_client().await.unwrap();

    for attempt in ["local", "", "LOCAL", &"f".repeat(64), &"0".repeat(64)] {
        assert!(
            env.auth.authenticate(Some(attempt)).await.is_err(),
            "bearer {attempt:?} resolved to a client",
        );
    }
}

#[tokio::test]
async fn the_reserved_row_is_listed_like_any_other_client() {
    // Shown rather than hidden: it genuinely owns `client_settings` rows, and a
    // list that omitted it would disagree with the foreign keys pointing at it.
    let env = env();
    env.auth.ensure_local_client().await.unwrap();
    let listed = env.auth.list_clients(LOCAL_CLIENT_ID).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, LOCAL_CLIENT_ID);
    assert!(listed[0].is_me);
}

#[tokio::test]
async fn the_reserved_row_cannot_be_revoked() {
    // Revoking cascades its `client_settings` and `scheduled_prompts` away and
    // strands the identity the embedded frontend is already holding, and there
    // is no pairing flow on a phone to make another one.
    let env = env();
    env.auth.ensure_local_client().await.unwrap();

    assert_err(
        env.auth.revoke_client(LOCAL_CLIENT_ID).await,
        ErrorCode::Forbidden,
        "cannot be removed",
    );
    // Still there, and still the only one.
    assert_eq!(
        env.auth.list_clients(LOCAL_CLIENT_ID).await.unwrap().len(),
        1,
    );
}

#[tokio::test]
async fn the_reserved_row_cannot_have_its_token_rotated() {
    // Sharper than symmetry with revoke: rotating replaces the unmatchable
    // `token_hash` with a real digest, which would mint a working bearer for
    // the one identity that is pre-authenticated by construction. Any other
    // paired client could ask for it by id.
    let env = env();
    env.auth.ensure_local_client().await.unwrap();

    let before = token_hash(&env.db, LOCAL_CLIENT_ID);

    assert_err(
        env.auth.rotate_token(LOCAL_CLIENT_ID).await,
        ErrorCode::Forbidden,
        "no access token",
    );

    // The property that would actually have been lost: the stored hash is
    // UNCHANGED, so it is still the literal id and still unmatchable.
    //
    // An earlier version of this asserted `authenticate(Some("local"))` fails,
    // which cannot fail either way: a successful rotation would store
    // `sha256_hex(<random>)`, and `sha256_hex("local")` does not match that
    // either. The comment claimed it checked this; it did not.
    assert_eq!(token_hash(&env.db, LOCAL_CLIENT_ID), before);
    assert_eq!(before, LOCAL_CLIENT_ID);
}

#[tokio::test]
async fn revoking_and_rotating_still_work_for_a_real_paired_client() {
    // The guard must be the reserved id and nothing wider. Without this, a
    // refusal that matched every id would pass every test above.
    let env = env();
    let minted = env.auth.mint_pairing_code(None).await.unwrap();
    let finished = run_pake(&env.auth, &minted.code, PakeOpts::default())
        .await
        .unwrap();
    assert!(env.auth.rotate_token(&finished.client_id).await.is_ok());
    assert!(env.auth.revoke_client(&finished.client_id).await.unwrap());
}
