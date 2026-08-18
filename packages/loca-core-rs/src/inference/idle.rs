//! The idle-unload timer, and the watchdog's recovery.
//!
//! Ported from `core/src/services/llm-idle.ts`, whose whole job is that a model
//! nobody is using stops holding a few gigabytes. `llama-server` has no native
//! idle unload; neither does llama.cpp, so this is ours either way.
//!
//! A turn touches it three times, and D4 already calls all three
//! (`chat/deps.rs:397-431`):
//!
//! - `note_activity` as the turn starts, cancelling any pending unload;
//! - `ensure_loaded` before a local turn is sent, reloading what idling stopped;
//! - `on_turn_end` when the last turn finishes, arming the timer.
//!
//! # What was dropped in the port, and why it is not a gap
//!
//! The TypeScript also has `configure(settings)`, called on boot and on every
//! settings change, purely to keep a copy of `llm.idleUnloadSeconds` inside a
//! process-global manager that has no settings handle. This one holds the
//! settings store, so it reads the value when it arms the timer and the copy has
//! nowhere to go stale. A change to the setting therefore takes effect on the
//! next turn end rather than immediately, which is the same moment the
//! TypeScript's copy would have mattered.

use std::sync::{Arc, Mutex};

use futures_util::future::BoxFuture;

use crate::chat::deps::ModelIdle;
use crate::error::Result;
use crate::llm::{WatchdogHandler, WatchdogInfo};
use crate::settings::access::num_setting;
use crate::settings::SettingsRecord;

use super::engine::LocalEngine;

/// The idle supervisor over one engine.
pub struct IdleUnload {
    engine: Arc<LocalEngine>,
    /// The pending unload, so a turn starting can cancel it. A `JoinHandle`
    /// rather than a flag: aborting the task is what guarantees the unload
    /// cannot land after the cancel, which a flag checked inside the task would
    /// still allow.
    pending: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl IdleUnload {
    pub fn new(engine: Arc<LocalEngine>) -> Self {
        Self {
            engine,
            pending: Mutex::new(None),
        }
    }

    /// The handler D3's scheduler calls when a local turn wedges.
    ///
    /// The scheduler's own doc says the job is "restart the model", which
    /// without a process to restart is an unload plus the reload every turn
    /// already does through [`ModelIdle::ensure_loaded`]. Nothing installs a
    /// handler today; wiring this one in is a `boot.rs` line and is on the
    /// interface queue.
    pub fn watchdog_handler(engine: Arc<LocalEngine>) -> WatchdogHandler {
        Arc::new(move |info: WatchdogInfo| {
            let engine = Arc::clone(&engine);
            tracing::warn!(
                target: "loca::inference",
                "local turn for client {} wedged for {}ms; unloading the model so the \
                 next turn reloads it",
                info.client_id,
                info.elapsed_ms,
            );
            tokio::spawn(async move { engine.unload().await });
        })
    }

    fn cancel(&self) {
        if let Ok(mut pending) = self.pending.lock() {
            if let Some(handle) = pending.take() {
                handle.abort();
            }
        }
    }
}

impl Drop for IdleUnload {
    fn drop(&mut self) {
        // A detached timer would keep the runtime from draining at shutdown,
        // which is the Rust shape of the TypeScript's note that a live
        // `setTimeout` stalls a `--watch` restart.
        self.cancel();
    }
}

impl ModelIdle for IdleUnload {
    fn note_activity(&self) {
        self.cancel();
    }

    fn on_turn_end(&self, active_streams: u32) {
        if active_streams > 0 {
            return;
        }
        let engine = Arc::clone(&self.engine);
        let settings = engine.settings().clone();
        self.cancel();

        let handle = tokio::spawn(async move {
            // Read at arm time, from the resolved record: the setting is a
            // number of seconds and zero means the feature is off. A record that
            // cannot be read leaves the model loaded, which is the safe
            // direction: the cost is memory, and the alternative is unloading a
            // model the user asked to keep.
            let seconds = match settings.load_resolved().await {
                Ok(record) => idle_seconds(&record),
                Err(err) => {
                    tracing::warn!(
                        target: "loca::inference",
                        "idle unload not armed: settings unreadable: {}",
                        err.message,
                    );
                    return;
                }
            };
            if seconds == 0 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_secs(u64::from(seconds))).await;
            engine.unload().await;
            tracing::info!(
                target: "loca::inference",
                "idle-unloaded the local model after {seconds}s of inactivity",
            );
        });

        if let Ok(mut pending) = self.pending.lock() {
            *pending = Some(handle);
        }
    }

    fn ensure_loaded<'a>(&'a self, settings: &'a SettingsRecord) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { self.engine.ensure_loaded(settings).await })
    }
}

/// `llm.idleUnloadSeconds`, floored at zero.
///
/// Zero is OFF, which is the schema's default, and a negative value from a
/// hand-edited file reads as off rather than as an immediate unload.
fn idle_seconds(settings: &SettingsRecord) -> u32 {
    let raw = num_setting(settings, "llm.idleUnloadSeconds", 0.0);
    if raw <= 0.0 {
        return 0;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let seconds = raw.floor() as u32;
    seconds
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use serde_json::{json, Value};

    use super::*;

    fn record(pairs: Value) -> SettingsRecord {
        match pairs {
            Value::Object(map) => map,
            _ => panic!("test records are objects"),
        }
    }

    #[test]
    fn the_feature_is_off_by_default_and_off_for_nonsense() {
        // Off is the schema's default and it is what most users run: a timer
        // armed by accident unloads a multi-GB model between two messages and
        // the next turn waits for it to come back.
        assert_eq!(idle_seconds(&record(json!({}))), 0);
        assert_eq!(
            idle_seconds(&record(json!({ "llm.idleUnloadSeconds": 0 }))),
            0,
        );
        assert_eq!(
            idle_seconds(&record(json!({ "llm.idleUnloadSeconds": -30 }))),
            0,
            "a negative value must not read as unload immediately",
        );
        assert_eq!(
            idle_seconds(&record(json!({ "llm.idleUnloadSeconds": "junk" }))),
            0,
        );
    }

    /// A loaded engine over the tiny model, through the production path.
    async fn loaded() -> Option<(Arc<LocalEngine>, crate::transport::state::AppContext)> {
        let model = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models/test.gguf");
        if !model.is_file() {
            return None;
        }
        let ctx = crate::transport::testing::context();
        let engine = Arc::new(LocalEngine::with_install_dir(
            ctx.settings.clone(),
            (*ctx.paths).clone(),
            std::path::PathBuf::from(env!("LOCA_LLAMA_BACKEND_DIR")),
        ));
        let settings = record(json!({
            "llm.provider": "local",
            "llm.modelPath": model.to_string_lossy(),
            "llm.contextSize": 256,
            "llm.idleUnloadSeconds": 300,
        }));
        // The store too, because `on_turn_end` reads the store rather than a
        // record handed to it.
        ctx.paths.ensure_dirs().unwrap();
        ctx.settings.patch(settings.clone()).await.unwrap();
        engine.ensure_loaded(&settings).await.unwrap();
        Some((engine, ctx))
    }

    const NO_GGUF: &str = "SKIP: packages/loca-core-rs/models/test.gguf is absent, so the         idle-unload tests did not run.";

    #[tokio::test(start_paused = true)]
    async fn the_timer_unloads_the_model_once_the_last_turn_has_ended() {
        // The feature itself. Time is paused, so this is the real timer with the
        // real setting and no five minutes of waiting.
        let Some((engine, _ctx)) = loaded().await else {
            eprintln!("{NO_GGUF}");
            return;
        };
        let idle = IdleUnload::new(Arc::clone(&engine));

        idle.on_turn_end(0);
        tokio::time::sleep(std::time::Duration::from_secs(301)).await;
        assert!(
            engine.ready().await.is_none(),
            "the idle timer never unloaded the model",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_turn_starting_cancels_a_pending_unload() {
        // The half that costs a user their next answer: unloading while a turn
        // is starting means that turn waits for a multi-GB reload, and the
        // TypeScript cancels on exactly this signal.
        let Some((engine, _ctx)) = loaded().await else {
            eprintln!("{NO_GGUF}");
            return;
        };
        let idle = IdleUnload::new(Arc::clone(&engine));

        idle.on_turn_end(0);
        idle.note_activity();
        tokio::time::sleep(std::time::Duration::from_secs(301)).await;
        assert!(
            engine.ready().await.is_some(),
            "a cancelled unload landed anyway",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_feature_being_off_never_unloads_anything() {
        // The DEFAULT configuration, and therefore the one most users run. A
        // timer that armed anyway would unload a multi-GB model between two
        // messages for someone who never asked for it, and the only symptom is
        // that the next answer takes a long time to start.
        let Some((engine, ctx)) = loaded().await else {
            eprintln!("{NO_GGUF}");
            return;
        };
        ctx.settings
            .patch(record(json!({ "llm.idleUnloadSeconds": 0 })))
            .await
            .unwrap();
        let idle = IdleUnload::new(Arc::clone(&engine));

        idle.on_turn_end(0);
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        assert!(
            engine.ready().await.is_some(),
            "the model was unloaded with idle unloading switched off",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_turn_still_running_does_not_arm_the_timer() {
        // `active_streams > 0` is the other guard, and it is the one that keeps
        // a long generation from having the model pulled out from under it while
        // a second stream is still going.
        let Some((engine, _ctx)) = loaded().await else {
            eprintln!("{NO_GGUF}");
            return;
        };
        let idle = IdleUnload::new(Arc::clone(&engine));

        idle.on_turn_end(1);
        tokio::time::sleep(std::time::Duration::from_secs(301)).await;
        assert!(engine.ready().await.is_some(), "unloaded while busy");
    }

    #[test]
    fn a_configured_number_of_seconds_is_read_as_written() {
        assert_eq!(
            idle_seconds(&record(json!({ "llm.idleUnloadSeconds": 300 }))),
            300,
        );
        // The client writes a number-typed field as a string often enough that
        // the reader has to take both, and 300 seconds is what the Full preset
        // turns on.
        assert_eq!(
            idle_seconds(&record(json!({ "llm.idleUnloadSeconds": "300" }))),
            300,
        );
    }
}
