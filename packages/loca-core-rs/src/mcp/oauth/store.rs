//! The OAuth bag: one vault entry holding everything the flow has to remember.
//!
//! The dynamic client registration, the issued tokens and the in-flight PKCE
//! verifier all live under `mcp.<id>.oauth`. The SDK splits that across two
//! traits over one stored value (`CredentialStore` for the registration and the
//! tokens, `StateStore` for the verifier keyed by its CSRF token), so both are
//! implemented here over the same key.
//!
//! Nothing here reaches `/api/v1/settings/secrets`: see
//! [`secret_key`](super::super::secret_key) for why that boundary matters.

use std::collections::BTreeMap;

use async_trait::async_trait;
use rmcp::transport::auth::{
    AuthError, CredentialStore, StateStore, StoredAuthorizationState, StoredCredentials,
};
use serde::{Deserialize, Serialize};

use crate::mcp::secret_key::oauth_secret_name;
use crate::secrets::Vault;

/// What is stored, as JSON, under the one vault key.
#[derive(Default, Deserialize, Serialize)]
struct Bag {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    credentials: Option<StoredCredentials>,
    /// Keyed by CSRF token: the SDK looks the verifier up by the `state` it
    /// gets back from the authorization server. Normally holds one entry.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    states: BTreeMap<String, StoredAuthorizationState>,
}

/// The vault-backed store, shared by both rmcp traits.
#[derive(Clone)]
pub struct VaultCredentials {
    vault: Vault,
    key: String,
}

impl VaultCredentials {
    pub fn new(vault: Vault, server_id: String) -> Self {
        Self {
            key: oauth_secret_name(&server_id),
            vault,
        }
    }

    async fn load(&self) -> Result<Bag, AuthError> {
        let raw = self
            .vault
            .get(&self.key)
            .await
            .map_err(|err| AuthError::InternalError(format!("vault read failed: {err}")))?;
        let Some(raw) = raw else {
            return Ok(Bag::default());
        };
        // A bag written by an older shape reads as absent rather than as a hard
        // failure: the cost is one re-authorization, where the alternative is a
        // server that can never connect again.
        Ok(serde_json::from_str(&raw).unwrap_or_default())
    }

    /// Drop every in-flight sign-in state, keeping the credentials.
    ///
    /// rmcp deletes a `states` entry only on a successful exchange (it is
    /// one-time use, consumed before the code is exchanged). Every other way a
    /// sign-in can end leaves the entry behind: the browser is closed, the
    /// 5-minute timeout fires, the server is reconfigured under the flow, or
    /// Core shuts down. Each abandoned entry holds a PKCE verifier, the
    /// expected issuer and the requested scopes, so without this the vault
    /// accumulates secret material forever with nothing to reap it.
    ///
    /// Called at the START of the next sign-in rather than from those teardown
    /// paths, which is the one place it cannot go wrong. The teardown paths are
    /// synchronous (`Flows::cancel`, `Flows::finish`) so they could only spawn
    /// the delete, and `Flows::start` cancels the previous flow before creating
    /// its own state: a spawned delete would race the new `StateStore::save`
    /// and could wipe the verifier of the sign-in that just started. Reaping
    /// here bounds the map at one entry per server, which is the same bound the
    /// TypeScript got from holding a single `codeVerifier` field it overwrote
    /// per attempt.
    pub async fn clear_states(&self) -> Result<(), AuthError> {
        let mut bag = self.load().await?;
        if bag.states.is_empty() {
            return Ok(());
        }
        bag.states.clear();
        self.save(&bag).await
    }

    async fn save(&self, bag: &Bag) -> Result<(), AuthError> {
        let raw = serde_json::to_string(bag)
            .map_err(|err| AuthError::InternalError(format!("vault write failed: {err}")))?;
        self.vault
            .set(&self.key, &raw)
            .await
            .map_err(|err| AuthError::InternalError(format!("vault write failed: {err}")))
    }
}

#[async_trait]
impl CredentialStore for VaultCredentials {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        Ok(VaultCredentials::load(self).await?.credentials)
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        let mut bag = VaultCredentials::load(self).await?;
        bag.credentials = Some(credentials);
        VaultCredentials::save(self, &bag).await
    }

    async fn clear(&self) -> Result<(), AuthError> {
        let mut bag = VaultCredentials::load(self).await?;
        bag.credentials = None;
        VaultCredentials::save(self, &bag).await
    }
}

#[async_trait]
impl StateStore for VaultCredentials {
    async fn save(
        &self,
        csrf_token: &str,
        state: StoredAuthorizationState,
    ) -> Result<(), AuthError> {
        let mut bag = VaultCredentials::load(self).await?;
        bag.states.insert(csrf_token.to_string(), state);
        VaultCredentials::save(self, &bag).await
    }

    async fn load(&self, csrf_token: &str) -> Result<Option<StoredAuthorizationState>, AuthError> {
        Ok(VaultCredentials::load(self)
            .await?
            .states
            .get(csrf_token)
            .cloned())
    }

    async fn delete(&self, csrf_token: &str) -> Result<(), AuthError> {
        let mut bag = VaultCredentials::load(self).await?;
        bag.states.remove(csrf_token);
        VaultCredentials::save(self, &bag).await
    }
}
