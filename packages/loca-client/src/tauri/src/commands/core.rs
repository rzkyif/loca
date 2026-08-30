//! The embedded Core: the phone IS a Core, reached in process. Mobile only; on
//! desktop the Client talks to a separate `loca-core` process over HTTPS
//! (`commands::net`).
//!
//! There is deliberately NO loopback listener: `127.0.0.1` is not app-private on
//! Android, so binding a port would let any installed app reach this Core and
//! would drag in `usesCleartextTraffic` / `NSAllowsLocalNetworking`. Requests go
//! straight into the same router, middleware and error mapping the socket uses.
//!
//! This file stays thin: anything with an ordering or a lifetime to get wrong
//! (the start-once cell, base64 marshalling, the WebSocket pump) lives in
//! `loca-core-ffi`, where it compiles and is tested on the host. `cfg(mobile)`
//! code can never be run by the host-target `cargo test`.
//!
//! The commands mirror `net_fetch` / `net_ws_*`. `mode`, `pin` and `capturedPin`
//! are absent rather than ignored: there is no TLS in process, and accepting a
//! `mode` we did nothing with would let a caller believe it had pinned something.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use loca_core_ffi::loca_core::error::{AppError as CoreError, ErrorCode as CoreErrorCode};
use loca_core_ffi::{
    CoreHttpReply, CoreHttpRequest, CoreInfo, LocalCore, StartOnce, WsEvent, WsSink,
};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::error::{AppError, AppResult};

/// The embedded Core, started at most once. Managed by the mobile builder.
#[derive(Default)]
pub struct LocalCoreState(StartOnce<LocalCore>);

/// The Core's data root: the app-private directory plus `core`, beside the
/// `client` root the file commands use. There is no `$HOME` on Android to
/// derive one from, and the directory is already per-app and per-channel
/// through the applicationId / bundle id.
fn core_root(app: &AppHandle) -> AppResult<PathBuf> {
    Ok(app.path().app_data_dir()?.join("core"))
}

/// Translate a Core error into the client's. Only two shapes reach here: a
/// request that could not be formed, and a Core that would not start. An HTTP
/// failure is NOT one of them, because it comes back as a reply.
fn to_app_error(err: CoreError) -> AppError {
    match err.code {
        CoreErrorCode::ValidationError => AppError::validation(err.message),
        _ => AppError::external(err.message),
    }
}

/// The running Core, booting it on first use.
///
/// Every command goes through here, so the boot is lazy, shared and started at
/// most once however many commands race. A failed boot is not remembered, so a
/// transient failure is retryable rather than fatal for the session.
async fn core(app: &AppHandle, state: &LocalCoreState) -> AppResult<Arc<LocalCore>> {
    let root = core_root(app)?;
    let channel = crate::channel::channel();
    state
        .0
        .get_or_init(|| LocalCore::start_with_store(root.clone(), channel, vault_store(&root)))
        .await
        .map_err(to_app_error)
}

/// The secure store the embedded Core seals its vault master key with, or `None`
/// to let the Core choose.
///
/// Android must answer `Some`: it has no OS credential store, so
/// `secrets::store::select` would fall back to a plaintext dotfile holding the
/// key that unseals `secrets.enc`. The sealed store needs an AndroidKeyStore
/// key, reachable only through the JVM, so this crate is the only place that can
/// build one. Elsewhere `select` picks better than a caller could.
#[cfg_attr(not(target_os = "android"), allow(unused_variables))]
fn vault_store(
    root: &std::path::Path,
) -> Option<Arc<dyn loca_core_ffi::loca_core::secrets::SecureStore>> {
    #[cfg(target_os = "android")]
    {
        return Some(Arc::new(
            loca_core_ffi::loca_core::secrets::android::SealedStore::new(
                root,
                Arc::new(crate::commands::keychain::KeystoreBox::new(
                    crate::commands::keychain::core_keystore_alias(),
                )),
            ),
        ));
    }
    #[cfg(not(target_os = "android"))]
    None
}

/// Boot the embedded Core if it is not already running, and report it.
///
/// Idempotent, so the client may call it on every launch.
#[tauri::command]
pub async fn core_start(app: AppHandle, state: State<'_, LocalCoreState>) -> AppResult<CoreInfo> {
    Ok(core(&app, &state).await?.info())
}

/// Whether the embedded Core has been started. Never starts it.
#[tauri::command]
pub async fn core_running(state: State<'_, LocalCoreState>) -> AppResult<bool> {
    Ok(state.0.get().await.is_some())
}

/// One request into the embedded Core.
///
/// `net_fetch`'s shape with the network removed. An HTTP-level failure (401,
/// 404, 500) is a SUCCESSFUL command carrying that status and the Core's error
/// envelope, exactly as it would be over a socket, so the client's existing
/// response handling works unchanged.
#[tauri::command]
pub async fn core_http(
    app: AppHandle,
    state: State<'_, LocalCoreState>,
    url: String,
    method: String,
    headers: HashMap<String, String>,
    body_b64: Option<String>,
) -> AppResult<CoreHttpReply> {
    core(&app, &state)
        .await?
        .http(CoreHttpRequest {
            url,
            method,
            headers,
            body_b64,
        })
        .await
        .map_err(to_app_error)
}

/// Attach a client connection. Frames arrive as per-id Tauri events, mirroring
/// `net_ws_open`'s `net://ws/<id>/...`.
///
/// There is no `url` because the wire URL exists to carry `?ticket=`, which the
/// in-process identity replaces, and no `error` event because registering with
/// the frame bus cannot fail. It still awaits the Core's boot and rejects when it
/// will not start, so the client keeps its backoff and its give-up limit.
#[tauri::command]
pub async fn core_ws_open(
    app: AppHandle,
    state: State<'_, LocalCoreState>,
    ws_id: String,
) -> AppResult<()> {
    let core = core(&app, &state).await?;
    core.ws_open(&ws_id, Arc::new(TauriWsSink(app.clone())));
    Ok(())
}

/// Send one client frame. A frame for an unknown id is dropped, matching
/// `net_ws_send`: a send that races a close is ordinary on this boundary.
#[tauri::command]
pub async fn core_ws_send(
    state: State<'_, LocalCoreState>,
    ws_id: String,
    data: String,
) -> AppResult<()> {
    if let Some(core) = state.0.get().await {
        core.ws_send(&ws_id, data);
    }
    Ok(())
}

/// Close one connection. Deliberately does not start the Core: closing a
/// connection on a Core that never started is a no-op, not a reason to boot one.
#[tauri::command]
pub async fn core_ws_close(state: State<'_, LocalCoreState>, ws_id: String) -> AppResult<()> {
    if let Some(core) = state.0.get().await {
        core.ws_close(&ws_id);
    }
    Ok(())
}

/// Delivers pump events to the webview as Tauri events.
struct TauriWsSink(AppHandle);

fn ev(ws_id: &str, kind: &str) -> String {
    format!("core://ws/{ws_id}/{kind}")
}

impl WsSink for TauriWsSink {
    fn emit(&self, ws_id: &str, event: WsEvent) {
        // Each arm emits separately because the payload types differ, the same
        // shape `net_ws_open` uses. A failed emit means the webview is gone,
        // which is not something this layer can act on.
        match event {
            WsEvent::Open => {
                let _ = self.0.emit(&ev(ws_id, "open"), ());
            }
            WsEvent::Message(text) => {
                let _ = self.0.emit(&ev(ws_id, "message"), text);
            }
            WsEvent::Close { code } => {
                let _ = self.0.emit(&ev(ws_id, "close"), code);
            }
        }
    }
}
