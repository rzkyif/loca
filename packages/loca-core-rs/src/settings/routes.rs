//! The settings route slice: `/api/v1/settings` and its secrets sub-resource.
//!
//! `GET` returns this client's effective view, the shared store overlaid with
//! its own per-client values. `PATCH` validates the whole body, then partitions
//! it by destination and writes each half to its own store.
//!
//! The secrets routes live here rather than in the secrets module because they
//! are a sub-resource of settings on the wire, and because the only thing they
//! add to the vault is the HTTP shape. Values go in and never come back out: the
//! client learns which names are configured, and renders a placeholder from
//! that.

use axum::extract::{Path, State};
use axum::response::Response;
use axum::routing::{get, put};
use axum::Router;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{not_found, validation, Result};
use crate::transport::http::{no_content, Json};
use crate::transport::middleware::{from_fn_with_state, require_client, AuthedClient};
use crate::transport::state::AppContext;

use super::schema::{schema, Destination};
use super::{validate_settings_patch, Settings, SettingsRecord};

/// Both settings destinations the core stores. The third,
/// `client-on-client`, lives in the Client's own file and is rejected here.
const CORE_STORED: &[Destination] = &[Destination::Core, Destination::ClientOnCore];

#[derive(Clone)]
struct SettingsState {
    ctx: AppContext,
    settings: Settings,
}

/// The settings slice.
///
/// The service is threaded in rather than constructed here: it holds the store's
/// cache and the write lock, so a second instance would let two PATCHes lose
/// each other's changes.
pub fn routes(ctx: &AppContext, settings: Settings) -> Router {
    settings.spawn_secret_name_relay(&ctx.vault);
    let state = SettingsState {
        ctx: ctx.clone(),
        settings,
    };
    Router::new()
        .route("/api/v1/settings", get(get_settings).patch(patch_settings))
        .route("/api/v1/settings/secrets", get(list_secrets))
        .route(
            "/api/v1/settings/secrets/{name}",
            put(put_secret).delete(delete_secret),
        )
        .layer(from_fn_with_state(ctx.clone(), require_client))
        .with_state(state)
}

// --- handlers ----------------------------------------------------------------

async fn get_settings(
    State(state): State<SettingsState>,
    client: AuthedClient,
) -> Result<Json<SettingsRecord>> {
    Ok(Json(sanitize(
        state.settings.load_effective(&client.id).await?,
    )))
}

async fn patch_settings(
    State(state): State<SettingsState>,
    client: AuthedClient,
    Json(body): Json<Value>,
) -> Result<Json<SettingsRecord>> {
    let Value::Object(patch) = body else {
        return Err(validation("object body required"));
    };
    // Strict on purpose: only known schema keys the core stores, well typed.
    // Secrets belong in the vault, `client-on-client` keys in the Client's own
    // file, and a wrong-typed value would flow into a sidecar's argv.
    let errors = validate_settings_patch(&patch, CORE_STORED);
    if !errors.is_empty() {
        return Err(validation(errors.join("; ")));
    }
    let mut shared = SettingsRecord::new();
    let mut per_client = SettingsRecord::new();
    for (key, value) in patch {
        if schema().destination(&key) == Some(Destination::ClientOnCore) {
            per_client.insert(key, value);
        } else {
            shared.insert(key, value);
        }
    }
    if !shared.is_empty() {
        state.settings.patch(shared).await?;
    }
    if !per_client.is_empty() {
        state.settings.patch_client(&client.id, per_client).await?;
    }
    Ok(Json(sanitize(
        state.settings.load_effective(&client.id).await?,
    )))
}

/// Names, never values. The client renders "saved" from this and cannot read
/// back what it stored, which is what makes a compromised client unable to
/// exfiltrate the vault.
#[derive(Debug, Serialize)]
struct SecretNames {
    names: Vec<String>,
}

/// The vault holds three classes of secret and these routes manage exactly one:
/// the schema's `type: "password"` fields. The other two belong to their owners
/// and must not be reachable here.
///
///   - `mcp.<id>.authToken` / `mcp.<id>.oauth`, written by the MCP routes.
///   - `tls-key`, the sealed private key of the certificate every paired client
///     pinned.
///
/// Without this gate a paired client could DELETE `tls-key`, and the next boot
/// would generate a fresh keypair with a different SPKI pin, locking out every
/// paired client at once; a PUT of something that is not a valid JWK is worse,
/// because then the core does not boot.
fn assert_settings_secret(name: &str) -> Result<()> {
    if !schema().is_secret_key(name) {
        return Err(not_found(format!("unknown secret setting \"{name}\"")));
    }
    Ok(())
}

async fn list_secrets(State(state): State<SettingsState>) -> Result<Json<SecretNames>> {
    let names = state.ctx.vault.list_names().await?;
    Ok(Json(SecretNames {
        names: names
            .into_iter()
            .filter(|n| schema().is_secret_key(n))
            .collect(),
    }))
}

/// The PUT body.
///
/// Deliberately not `Debug`: the field is a plaintext credential, and a derived
/// `Debug` is one `tracing::debug!(?body)` away from putting it in `core.log`.
/// The generated contract types take the same precaution, for the same reason.
#[derive(Deserialize)]
struct SecretBody {
    value: String,
}

async fn put_secret(
    State(state): State<SettingsState>,
    Path(name): Path<String>,
    Json(body): Json<Value>,
) -> Result<Response> {
    let body: SecretBody =
        serde_json::from_value(body).map_err(|_| validation("body must be { value: string }"))?;
    assert_settings_secret(&name)?;
    state.ctx.vault.set(&name, &body.value).await?;
    Ok(no_content())
}

async fn delete_secret(
    State(state): State<SettingsState>,
    Path(name): Path<String>,
) -> Result<Response> {
    assert_settings_secret(&name)?;
    if !state.ctx.vault.delete(&name).await? {
        return Err(not_found(format!("secret \"{name}\" not set")));
    }
    Ok(no_content())
}

/// Keep only what the core stores and may hand back: known schema keys whose
/// destination is `core` or `client-on-core`, never a secret-typed one.
///
/// This is what stops a stray key placed directly in `settings.json` reaching a
/// client, and it is why the two boot-only keys (`server.bindHost`,
/// `server.behindProxy`) are invisible over the API even though they sit in the
/// same file. A plaintext API key someone pasted into that file is dropped here
/// too: secret values live in the vault and are never returned.
fn sanitize(settings: SettingsRecord) -> SettingsRecord {
    let schema = schema();
    settings
        .into_iter()
        .filter(|(key, _)| {
            schema
                .destination(key)
                .is_some_and(Destination::is_core_stored)
                && !schema.is_secret_key(key)
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Arc;

    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use serde_json::json;
    use tower::ServiceExt as _;

    use super::*;
    use crate::db::migrate;
    use crate::settings::Settings;
    use crate::transport::testing::{context_with, StubAuth};
    use crate::transport::FrameBus;

    const TOKEN: &str = "token-abc";
    const CLIENT: &str = "client-1";

    struct Env {
        router: Router,
        ctx: AppContext,
        settings: Settings,
        _dir: tempfile::TempDir,
    }

    fn env() -> Env {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context_with(Arc::new(StubAuth::new().with_client(TOKEN, CLIENT)));
        // The vault writes `secrets.enc` under the context's root, and a write
        // into a directory that does not exist surfaces as `not_found`, which
        // reads as a routing failure rather than a filesystem one.
        ctx.paths.ensure_dirs().unwrap();
        ctx.db.call_blocking(|conn| migrate(conn)).unwrap();
        // The client row has to exist: `client_settings.client_id` references it.
        ctx.db
            .call_blocking(|conn| {
                conn.execute(
                    "INSERT INTO clients (id, name, token_hash, created_at_ms, last_seen_ms) \
                     VALUES (?1, 'test', 'hash', 0, 0)",
                    [CLIENT],
                )?;
                Ok(())
            })
            .unwrap();
        let settings = Settings::with_file(
            dir.path().join("settings.json"),
            ctx.db.clone(),
            Arc::new(FrameBus::new()),
        );
        Env {
            router: routes(&ctx, settings.clone()),
            ctx,
            settings,
            _dir: dir,
        }
    }

    async fn call(env: &Env, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("authorization", format!("Bearer {TOKEN}"));
        let body = match body {
            Some(value) => {
                request = request.header("content-type", "application/json");
                Body::from(value.to_string())
            }
            None => Body::empty(),
        };
        let response = env
            .router
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status().as_u16();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, value)
    }

    #[tokio::test]
    async fn a_fresh_core_returns_an_empty_object() {
        let env = env();
        assert_eq!(
            call(&env, "GET", "/api/v1/settings", None).await,
            (200, json!({}))
        );
    }

    #[tokio::test]
    async fn a_patch_persists_and_returns_the_merged_view() {
        let env = env();
        let (status, body) = call(
            &env,
            "PATCH",
            "/api/v1/settings",
            Some(json!({ "llm.host": "0.0.0.0", "llm.temperature": 0.4 })),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(body["llm.host"], "0.0.0.0");
        // The overlay key comes back in the same effective view.
        assert_eq!(body["llm.temperature"], 0.4);
        assert_eq!(
            call(&env, "GET", "/api/v1/settings", None).await.1["llm.host"],
            "0.0.0.0"
        );
    }

    #[tokio::test]
    async fn a_null_reverts_a_key_to_its_default() {
        let env = env();
        call(
            &env,
            "PATCH",
            "/api/v1/settings",
            Some(json!({ "llm.host": "0.0.0.0" })),
        )
        .await;
        let (status, body) = call(
            &env,
            "PATCH",
            "/api/v1/settings",
            Some(json!({ "llm.host": null })),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(body.get("llm.host"), None);
    }

    #[tokio::test]
    async fn every_rejected_patch_is_a_validation_error() {
        let env = env();
        for body in [
            json!({ "ui.theme": "dark" }),               // unknown key
            json!({ "appearance.theme": "dark" }),       // client-on-client
            json!({ "llm.host": 1234 }),                 // wrong type
            json!({ "llm.host": "not-an-ip" }),          // regex
            json!({ "llm.provider": "banana" }),         // not an option
            json!({ "memories.maxRelevant": 42 }),       // out of range
            json!({ "llm.external.apiKey": "sk-nope" }), // secret, vault only
            json!({ "server.bindHost": "0.0.0.0" }),     // boot-only, not a schema key
            json!([1, 2, 3]),                            // not an object
        ] {
            let (status, response) = call(&env, "PATCH", "/api/v1/settings", Some(body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
            assert_eq!(response["error"]["code"], "validation_error");
        }
        // None of them reached the store.
        assert_eq!(
            call(&env, "GET", "/api/v1/settings", None).await.1,
            json!({})
        );
    }

    #[tokio::test]
    async fn a_stray_key_in_the_file_never_reaches_a_client() {
        let env = env();
        // Written through the service, which the route layer would have
        // rejected: this is the hand-edited settings.json case, and the two
        // boot-only keys land here legitimately.
        env.settings
            .patch(
                json!({
                    "internal.junk": 1,
                    "server.bindHost": "0.0.0.0",
                    "llm.host": "0.0.0.0",
                })
                .as_object()
                .unwrap()
                .clone(),
            )
            .await
            .unwrap();
        let (status, body) = call(&env, "GET", "/api/v1/settings", None).await;
        assert_eq!(status, 200);
        assert_eq!(body, json!({ "llm.host": "0.0.0.0" }));
    }

    #[tokio::test]
    async fn a_secret_value_in_the_file_never_reaches_a_client() {
        let env = env();
        env.settings
            .patch(
                json!({ "llm.external.apiKey": "sk-must-be-redacted" })
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .await
            .unwrap();
        let (status, body) = call(&env, "GET", "/api/v1/settings", None).await;
        assert_eq!(status, 200);
        assert!(
            !body.to_string().contains("sk-must-be-redacted"),
            "secret reached the wire: {body}"
        );
    }

    #[tokio::test]
    async fn a_secret_goes_in_and_only_its_name_comes_back() {
        let env = env();
        let (status, _) = call(
            &env,
            "PUT",
            "/api/v1/settings/secrets/llm.external.apiKey",
            Some(json!({ "value": "sk-secret-do-not-leak" })),
        )
        .await;
        assert_eq!(status, 204);

        let (status, body) = call(&env, "GET", "/api/v1/settings/secrets", None).await;
        assert_eq!(status, 200);
        assert_eq!(body, json!({ "names": ["llm.external.apiKey"] }));
        assert!(!body.to_string().contains("sk-secret"));

        // And the vault holds what was sent.
        assert_eq!(
            env.ctx
                .vault
                .get("llm.external.apiKey")
                .await
                .unwrap()
                .as_deref(),
            Some("sk-secret-do-not-leak")
        );
    }

    #[tokio::test]
    async fn a_malformed_secret_body_is_rejected() {
        let env = env();
        let (status, body) = call(
            &env,
            "PUT",
            "/api/v1/settings/secrets/llm.external.apiKey",
            Some(json!({ "value": 42 })),
        )
        .await;
        assert_eq!(status, 400);
        assert_eq!(body["error"]["code"], "validation_error");
    }

    #[tokio::test]
    async fn deleting_a_secret_that_was_never_set_is_a_404() {
        let env = env();
        let (status, body) = call(
            &env,
            "DELETE",
            "/api/v1/settings/secrets/stt.external.apiKey",
            None,
        )
        .await;
        assert_eq!(status, 404);
        assert_eq!(body["error"]["code"], "not_found");

        call(
            &env,
            "PUT",
            "/api/v1/settings/secrets/tts.external.apiKey",
            Some(json!({ "value": "v" })),
        )
        .await;
        assert_eq!(
            call(
                &env,
                "DELETE",
                "/api/v1/settings/secrets/tts.external.apiKey",
                None
            )
            .await
            .0,
            204
        );
    }

    #[tokio::test]
    async fn every_route_requires_a_bearer() {
        let env = env();
        for (method, path) in [
            ("GET", "/api/v1/settings"),
            ("PATCH", "/api/v1/settings"),
            ("GET", "/api/v1/settings/secrets"),
            ("PUT", "/api/v1/settings/secrets/x"),
            ("DELETE", "/api/v1/settings/secrets/x"),
        ] {
            let response = env
                .router
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {path}"
            );
        }
    }

    /// The vault is one bag: the sealed TLS private key and D11's MCP token
    /// bags live beside the settings secrets. These routes filter to the
    /// schema's `password` fields so neither is listed or reachable.
    ///
    /// Without the filter, any paired client can delete `tls-key`, and the next
    /// boot generates a fresh keypair with a different SPKI pin, locking out
    /// every paired client at once. No admin token required.
    #[tokio::test]
    async fn the_tls_key_is_not_reachable_through_the_settings_routes() {
        let env = env();
        // Seed the two names a real core has beside the settings secrets.
        env.ctx
            .vault
            .set("tls-key", "{\"kty\":\"EC\"}")
            .await
            .unwrap();
        env.ctx
            .vault
            .set("mcp.server1.authToken", "t")
            .await
            .unwrap();

        for name in ["tls-key", "mcp.server1.authToken"] {
            let path = format!("/api/v1/settings/secrets/{name}");
            let (status, _) = call(&env, "DELETE", &path, None).await;
            assert_eq!(status, 404, "DELETE {name} must not reach the vault");
            let (status, _) = call(
                &env,
                "PUT",
                &path,
                Some(serde_json::json!({ "value": "x" })),
            )
            .await;
            assert_eq!(status, 404, "PUT {name} must not reach the vault");
        }

        // Still there: the routes refused rather than half-acting.
        assert!(env.ctx.vault.get("tls-key").await.unwrap().is_some());

        // And neither name is disclosed by the listing.
        let (status, body) = call(&env, "GET", "/api/v1/settings/secrets", None).await;
        assert_eq!(status, 200);
        assert_eq!(body["names"], serde_json::json!([]));
    }
}
