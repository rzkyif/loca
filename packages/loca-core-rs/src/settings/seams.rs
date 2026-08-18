//! What the extension supervisor reads settings through.
//!
//! [`BrokerSettings`] is DECLARED by `extensions::broker` and implemented here.
//! The broker reads exactly two keys, `tts.enabled` and `stt.enabled`, and both
//! are the user's own switch: a tool must not be able to start the speech stack
//! by asking for it after the user turned it off.
//!
//! The SPARSE record is what the broker gets, and that is deliberate. Both keys
//! are read with a `false` default, so an absent key is off; resolving the
//! schema defaults underneath would make the answer depend on what the schema
//! happens to default to rather than on what the user chose. There is also no
//! client on a tool call, so there is no overlay to apply.

use async_trait::async_trait;

use crate::error::Result;
use crate::extensions::broker::BrokerSettings;

use super::{Settings, SettingsRecord};

/// The core settings, as the module broker reads them.
#[derive(Clone)]
pub struct BrokerSettingsStore {
    settings: Settings,
}

impl BrokerSettingsStore {
    pub fn new(settings: Settings) -> Self {
        Self { settings }
    }
}

#[async_trait]
impl BrokerSettings for BrokerSettingsStore {
    async fn snapshot(&self) -> Result<SettingsRecord> {
        self.settings.load().await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use serde_json::json;

    use crate::settings::access::bool_setting;
    use crate::transport::testing;

    use super::*;

    async fn seam(entries: &[(&str, serde_json::Value)]) -> BrokerSettingsStore {
        let ctx = testing::context();
        if let Some(dir) = ctx.paths.settings_file().parent() {
            std::fs::create_dir_all(dir).unwrap();
        }
        let mut patch = SettingsRecord::new();
        for (key, value) in entries {
            patch.insert((*key).to_string(), value.clone());
        }
        ctx.settings.patch(patch).await.unwrap();
        BrokerSettingsStore::new(ctx.settings.clone())
    }

    #[tokio::test]
    async fn the_speech_switches_are_off_until_the_user_turns_them_on() {
        let untouched = seam(&[]).await.snapshot().await.unwrap();
        assert!(!bool_setting(&untouched, "tts.enabled", false));
        assert!(!bool_setting(&untouched, "stt.enabled", false));

        let on = seam(&[("tts.enabled", json!(true))])
            .await
            .snapshot()
            .await
            .unwrap();
        assert!(bool_setting(&on, "tts.enabled", false));
        assert!(
            !bool_setting(&on, "stt.enabled", false),
            "the two switches are independent: one on does not imply the other",
        );
    }

    /// The record is the SPARSE store, not the resolved one. Both would answer
    /// the two speech keys the same way today, so the discriminator is a key the
    /// schema defaults to something non-empty: it must be ABSENT here.
    #[tokio::test]
    async fn the_broker_reads_what_the_user_set_rather_than_the_schema_defaults() {
        let snapshot = seam(&[("tts.enabled", json!(true))])
            .await
            .snapshot()
            .await
            .unwrap();
        assert_eq!(snapshot.len(), 1, "only the key the user set");

        let defaulted: Vec<&String> = crate::settings::schema()
            .defaults()
            .keys()
            .filter(|key| !snapshot.contains_key(*key))
            .collect();
        assert!(
            !defaulted.is_empty(),
            "the schema has defaults the sparse record does not carry, which is what \
             makes the previous assertion meaningful",
        );
    }
}
