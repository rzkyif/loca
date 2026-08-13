//! loca-core-keychain: tiny native helper that exposes the OS keychain
//! (macOS Keychain, Linux libsecret via secret-service, Windows Credential
//! Manager) to loca-core through a stdio protocol. loca-core shells out
//! to this binary instead of bundling Deno FFI bindings against the
//! platform keychain APIs.
//!
//! Usage:
//!   loca-core-keychain get <service> <account>
//!       Prints the stored password to stdout (with a single trailing \n).
//!       Exits 0 on success.
//!       Exits 1 if no such entry exists (stderr: "ENTRY_MISSING").
//!       Exits 2 on any other error (stderr: human-readable message).
//!
//!   loca-core-keychain set <service> <account>
//!       Reads the password to store from stdin (all bytes, no
//!       transformation). The caller is responsible for not appending a
//!       trailing newline if it doesn't want one stored. loca-core writes
//!       a single line of base64.
//!       Exits 0 on success, 2 on error.
//!
//!   loca-core-keychain delete <service> <account>
//!       Removes the entry. Idempotent: exits 0 whether or not the entry
//!       existed. Exits 2 on other errors.

#[cfg(any(test, feature = "in-memory"))]
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::process::ExitCode;
#[cfg(any(test, feature = "in-memory"))]
use std::sync::Mutex;

/// Test seam: every keychain operation goes through this trait so the unit
/// tests don't touch the real OS keychain (which would prompt the dev for
/// a password on macOS / fail on headless CI).
pub trait KeychainStore {
    fn get(&self, service: &str, account: &str) -> Result<String, KeychainError>;
    fn set(&self, service: &str, account: &str, password: &str) -> Result<(), KeychainError>;
    /// Idempotent: returning `Ok(())` for a missing entry is the contract.
    fn delete(&self, service: &str, account: &str) -> Result<(), KeychainError>;
}

#[derive(Debug)]
pub enum KeychainError {
    NoEntry,
    Other(String),
}

impl std::fmt::Display for KeychainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeychainError::NoEntry => f.write_str("ENTRY_MISSING"),
            KeychainError::Other(msg) => f.write_str(msg),
        }
    }
}

/// Production impl: delegates to keyring-core and the platform credential store.
pub struct RealKeychain;

impl KeychainStore for RealKeychain {
    fn get(&self, service: &str, account: &str) -> Result<String, KeychainError> {
        let entry = keyring_core::Entry::new(service, account)
            .map_err(|e| KeychainError::Other(format!("keyring_core::Entry::new failed: {}", e)))?;
        match entry.get_password() {
            Ok(pw) => Ok(pw),
            Err(keyring_core::Error::NoEntry) => Err(KeychainError::NoEntry),
            Err(e) => Err(KeychainError::Other(format!("get_password failed: {}", e))),
        }
    }
    fn set(&self, service: &str, account: &str, password: &str) -> Result<(), KeychainError> {
        let entry = keyring_core::Entry::new(service, account)
            .map_err(|e| KeychainError::Other(format!("keyring_core::Entry::new failed: {}", e)))?;
        entry
            .set_password(password)
            .map_err(|e| KeychainError::Other(format!("set_password failed: {}", e)))
    }
    fn delete(&self, service: &str, account: &str) -> Result<(), KeychainError> {
        let entry = keyring_core::Entry::new(service, account)
            .map_err(|e| KeychainError::Other(format!("keyring_core::Entry::new failed: {}", e)))?;
        match entry.delete_credential() {
            Ok(_) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(e) => Err(KeychainError::Other(format!(
                "delete_credential failed: {}",
                e
            ))),
        }
    }
}

/// In-memory impl used by the test suite. Gated behind `#[cfg(any(test,
/// feature = "in-memory"))]` so production builds of the helper don't
/// expose a non-persistent KeychainStore alongside the real one. The
/// feature flag exists so reverse-dependency crates' integration tests
/// can opt in without flipping `cfg(test)` on their build.
#[cfg(any(test, feature = "in-memory"))]
#[derive(Default)]
pub struct InMemoryKeychain {
    inner: Mutex<HashMap<(String, String), String>>,
}

#[cfg(any(test, feature = "in-memory"))]
impl InMemoryKeychain {
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(any(test, feature = "in-memory"))]
impl KeychainStore for InMemoryKeychain {
    fn get(&self, service: &str, account: &str) -> Result<String, KeychainError> {
        let guard = self
            .inner
            .lock()
            .map_err(|e| KeychainError::Other(format!("mutex poisoned: {}", e)))?;
        guard
            .get(&(service.to_string(), account.to_string()))
            .cloned()
            .ok_or(KeychainError::NoEntry)
    }
    fn set(&self, service: &str, account: &str, password: &str) -> Result<(), KeychainError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| KeychainError::Other(format!("mutex poisoned: {}", e)))?;
        guard.insert(
            (service.to_string(), account.to_string()),
            password.to_string(),
        );
        Ok(())
    }
    fn delete(&self, service: &str, account: &str) -> Result<(), KeychainError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| KeychainError::Other(format!("mutex poisoned: {}", e)))?;
        guard.remove(&(service.to_string(), account.to_string()));
        Ok(())
    }
}

/// Register the platform credential store as keyring-core's process-global
/// default. Must run once before any `RealKeychain` operation; each target
/// links exactly one store crate (see Cargo.toml), so the body is selected
/// per OS.
fn init_default_store() -> Result<(), KeychainError> {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    let store = apple_native_keyring_store::keychain::Store::new();
    #[cfg(target_os = "windows")]
    let store = windows_native_keyring_store::Store::new();
    #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "openbsd"))]
    let store = dbus_secret_service_keyring_store::Store::new();

    let store =
        store.map_err(|e| KeychainError::Other(format!("keychain store init failed: {e}")))?;
    keyring_core::set_default_store(store);
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        usage();
        return ExitCode::from(2);
    }
    let (mode, service, account) = (args[1].as_str(), args[2].as_str(), args[3].as_str());
    if let Err(e) = init_default_store() {
        eprintln!("{e}");
        return ExitCode::from(2);
    }
    let store = RealKeychain;
    ExitCode::from(run(
        &store,
        mode,
        service,
        account,
        &mut io::stdin().lock(),
        &mut io::stdout().lock(),
    ))
}

fn usage() {
    eprintln!("usage: loca-core-keychain {{get|set|delete}} <service> <account>");
}

/// Dispatch a single command. Returns the intended process exit code so unit
/// tests can compare it directly without going through `ExitCode` (which has
/// no `PartialEq`).
pub fn run<S: KeychainStore + ?Sized>(
    store: &S,
    mode: &str,
    service: &str,
    account: &str,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
) -> u8 {
    match mode {
        "get" => run_get(store, service, account, stdout),
        "set" => run_set(store, service, account, stdin),
        "delete" => run_delete(store, service, account),
        other => {
            eprintln!("unknown mode: {}", other);
            usage();
            2
        }
    }
}

fn run_get<S: KeychainStore + ?Sized>(
    store: &S,
    service: &str,
    account: &str,
    stdout: &mut dyn Write,
) -> u8 {
    match store.get(service, account) {
        Ok(pw) => {
            // Write raw bytes + a single trailing newline so callers can
            // either `.trim()` the result or read the line directly.
            if let Err(e) = stdout.write_all(pw.as_bytes()) {
                return fail(format!("stdout write failed: {}", e));
            }
            let _ = stdout.write_all(b"\n");
            0
        }
        Err(KeychainError::NoEntry) => {
            eprintln!("ENTRY_MISSING");
            1
        }
        Err(KeychainError::Other(msg)) => fail(msg),
    }
}

/// Hard cap on the password byte length accepted from stdin. Larger than
/// any realistic credential (the actual paired-client token is 32 bytes
/// base64url-encoded → 43 chars; an OAuth refresh token tops out around
/// 1 KiB) but small enough that a malicious or buggy caller can't pin a
/// gigabyte of memory by piping `/dev/zero` at us.
const MAX_PASSWORD_BYTES: u64 = 64 * 1024;

fn run_set<S: KeychainStore + ?Sized>(
    store: &S,
    service: &str,
    account: &str,
    stdin: &mut dyn Read,
) -> u8 {
    // Bounded read: take MAX_PASSWORD_BYTES + 1 so we can detect "input
    // hit the cap" vs "input was exactly cap-sized" and reject the former.
    let mut buf = Vec::with_capacity(1024);
    let limit = MAX_PASSWORD_BYTES + 1;
    if let Err(e) = stdin.take(limit).read_to_end(&mut buf) {
        return fail(format!("stdin read failed: {}", e));
    }
    if buf.len() as u64 > MAX_PASSWORD_BYTES {
        return fail(format!(
            "password too large (max {} bytes)",
            MAX_PASSWORD_BYTES
        ));
    }
    let s = match std::str::from_utf8(&buf) {
        Ok(s) => s,
        Err(e) => return fail(format!("password is not valid utf-8: {}", e)),
    };
    match store.set(service, account, s) {
        Ok(()) => 0,
        Err(KeychainError::Other(msg)) => fail(msg),
        Err(KeychainError::NoEntry) => fail("unexpected NoEntry from set".to_string()),
    }
}

fn run_delete<S: KeychainStore + ?Sized>(store: &S, service: &str, account: &str) -> u8 {
    match store.delete(service, account) {
        Ok(()) => 0,
        Err(KeychainError::Other(msg)) => fail(msg),
        Err(KeychainError::NoEntry) => 0,
    }
}

fn fail(msg: String) -> u8 {
    eprintln!("{}", msg);
    2
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn store() -> InMemoryKeychain {
        InMemoryKeychain::new()
    }

    #[test]
    fn get_returns_stored_password_with_trailing_newline() {
        let s = store();
        s.set("svc", "acct", "hunter2").unwrap();
        let mut stdin: &[u8] = &[];
        let mut stdout: Vec<u8> = Vec::new();
        let code = run(&s, "get", "svc", "acct", &mut stdin, &mut stdout);
        assert_eq!(code, 0);
        assert_eq!(stdout, b"hunter2\n");
    }

    #[test]
    fn get_returns_exit_1_when_no_entry() {
        let s = store();
        let mut stdin: &[u8] = &[];
        let mut stdout: Vec<u8> = Vec::new();
        let code = run(&s, "get", "svc", "missing", &mut stdin, &mut stdout);
        assert_eq!(code, 1);
        assert!(stdout.is_empty());
    }

    #[test]
    fn set_reads_stdin_byte_exact() {
        let s = store();
        let mut stdin: &[u8] = b"raw bytes no newline";
        let mut stdout: Vec<u8> = Vec::new();
        let code = run(&s, "set", "svc", "acct", &mut stdin, &mut stdout);
        assert_eq!(code, 0);
        assert_eq!(s.get("svc", "acct").unwrap(), "raw bytes no newline");
    }

    #[test]
    fn delete_is_idempotent() {
        let s = store();
        s.set("svc", "acct", "x").unwrap();
        let mut stdin: &[u8] = &[];
        let mut stdout: Vec<u8> = Vec::new();
        assert_eq!(run(&s, "delete", "svc", "acct", &mut stdin, &mut stdout), 0);
        assert_eq!(run(&s, "delete", "svc", "acct", &mut stdin, &mut stdout), 0);
        assert!(matches!(s.get("svc", "acct"), Err(KeychainError::NoEntry)));
    }

    #[test]
    fn unknown_mode_returns_exit_2() {
        let s = store();
        let mut stdin: &[u8] = &[];
        let mut stdout: Vec<u8> = Vec::new();
        let code = run(&s, "bogus", "svc", "acct", &mut stdin, &mut stdout);
        assert_eq!(code, 2);
    }

    #[test]
    fn set_accepts_password_up_to_the_limit() {
        let s = store();
        let payload = vec![b'a'; MAX_PASSWORD_BYTES as usize];
        let mut stdin: &[u8] = &payload;
        let mut stdout: Vec<u8> = Vec::new();
        let code = run(&s, "set", "svc", "acct", &mut stdin, &mut stdout);
        assert_eq!(code, 0);
        assert_eq!(
            s.get("svc", "acct").unwrap().len(),
            MAX_PASSWORD_BYTES as usize
        );
    }

    #[test]
    fn set_rejects_password_one_byte_over_the_limit() {
        let s = store();
        let payload = vec![b'a'; (MAX_PASSWORD_BYTES + 1) as usize];
        let mut stdin: &[u8] = &payload;
        let mut stdout: Vec<u8> = Vec::new();
        let code = run(&s, "set", "svc", "acct", &mut stdin, &mut stdout);
        assert_eq!(code, 2);
        assert!(matches!(s.get("svc", "acct"), Err(KeychainError::NoEntry)));
    }

    #[test]
    fn set_rejects_invalid_utf8() {
        let s = store();
        // Lone continuation byte: invalid UTF-8.
        let mut stdin: &[u8] = &[0x80];
        let mut stdout: Vec<u8> = Vec::new();
        let code = run(&s, "set", "svc", "acct", &mut stdin, &mut stdout);
        assert_eq!(code, 2);
        assert!(matches!(s.get("svc", "acct"), Err(KeychainError::NoEntry)));
    }
}
