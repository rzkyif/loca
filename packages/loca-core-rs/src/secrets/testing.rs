//! Test doubles for the OS keychain.
//!
//! No test in this crate constructs a platform keychain store, so `cargo test`
//! never prompts for the developer's login keychain and never needs libsecret or
//! a dbus session on headless CI. [`SecureStore`] is the seam, and these four
//! cover the behaviours that matter: working, write-only, lying, and dead.
//!
//! Gated behind `cfg(any(test, feature = "in-memory"))` so a production build
//! carries no non-persistent store next to the real one. The feature exists for
//! reverse-dependency tests (F7, D11) that want the doubles without flipping
//! `cfg(test)` on their own build.

use std::collections::HashMap;
use std::sync::Mutex;

use super::store::SecureStore;
use crate::error::{internal, Result};

/// A keychain that works.
#[derive(Default)]
pub struct MemoryStore {
    entries: Mutex<HashMap<String, String>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl std::fmt::Debug for MemoryStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MemoryStore")
    }
}

impl SecureStore for MemoryStore {
    fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(lock(&self.entries)?.get(key).cloned())
    }

    fn set(&self, key: &str, value: &str) -> Result<()> {
        lock(&self.entries)?.insert(key.to_string(), value.to_string());
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<()> {
        lock(&self.entries)?.remove(key);
        Ok(())
    }
}

/// Accepts every write and serves none of them back. The macOS unsigned-binary
/// and locked-collection failure, which reports success and loses the key.
#[derive(Debug, Default)]
pub struct WriteOnlyStore {
    swallowed: Mutex<usize>,
}

impl WriteOnlyStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// How many writes it claimed to accept, so a test can prove the seal was
    /// attempted rather than skipped.
    pub fn swallowed(&self) -> usize {
        lock(&self.swallowed).map(|n| *n).unwrap_or(0)
    }
}

impl SecureStore for WriteOnlyStore {
    fn get(&self, _key: &str) -> Result<Option<String>> {
        Ok(None)
    }

    fn set(&self, _key: &str, _value: &str) -> Result<()> {
        *lock(&self.swallowed)? += 1;
        Ok(())
    }

    fn delete(&self, _key: &str) -> Result<()> {
        Ok(())
    }
}

/// Accepts every write and serves back something else. A store that transforms
/// or truncates the value, which a bare success check cannot catch.
#[derive(Debug, Default)]
pub struct LyingStore;

impl LyingStore {
    pub fn new() -> Self {
        Self
    }
}

impl SecureStore for LyingStore {
    fn get(&self, _key: &str) -> Result<Option<String>> {
        Ok(Some("not-what-you-wrote".to_string()))
    }

    fn set(&self, _key: &str, _value: &str) -> Result<()> {
        Ok(())
    }

    fn delete(&self, _key: &str) -> Result<()> {
        Ok(())
    }
}

/// Fails every call. Registration failure, a missing secret-service, a platform
/// with no backend at all.
#[derive(Debug)]
pub struct DeadStore;

impl SecureStore for DeadStore {
    fn get(&self, _key: &str) -> Result<Option<String>> {
        Err(internal("keychain get failed: platform failure"))
    }

    fn set(&self, _key: &str, _value: &str) -> Result<()> {
        Err(internal("keychain set failed: platform failure"))
    }

    fn delete(&self, _key: &str) -> Result<()> {
        Err(internal("keychain delete failed: platform failure"))
    }
}

/// A poisoned mutex is a panic in another test, not a condition worth
/// propagating; report it rather than unwrapping into a second panic.
fn lock<T>(m: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>> {
    m.lock().map_err(|_| internal("test store mutex poisoned"))
}
