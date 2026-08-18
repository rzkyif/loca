//! The extension route slice: `/api/v1/extensions` and the flat
//! `/api/v1/tools`.
//!
//! Twenty-one routes plus the merged catalog, all behind the same bearer layer
//! (`http/routes/extensions.ts:72`, `tools.ts:15`). None is admin-gated: a
//! paired device is already trusted to manage tool providers.
//!
//! **No route here streams.** Install progress is a background job that returns
//! `{ jobId, extensionId }` immediately and reports over WS, which is what lets
//! the client show one modal for a download that takes minutes without holding
//! a request open across it.
//!
//! # `requiredPermissions` is derived at the route, not read from the row
//!
//! [`attach_required_permissions`] re-parses `loca.json` from disk rather than
//! reading the persisted column, because `missingRequired` depends on the
//! CURRENT grant state and the column does not. The registry leaves both empty
//! by design; filling them is this layer's job.

use std::collections::HashSet;
use std::path::Path;

use axum::body::Bytes;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::Router;
use serde::Deserialize;

use crate::contract::api::{
    CheckUpdatesRequest, CheckUpdatesResponse, DownloadExtensionRequest, EmbedRequest,
    EmbedResponse, Extension, ExtensionActionResponse, ExtensionJobResponse, ExtensionSource,
    ExtensionStatus, ExtensionUpdateStatus, ListExtensionToolsResponse, ListExtensionsResponse,
    OpenAiToolDef, OpenAiToolDefFunction, OpenAiToolDefType, SearchExtensionsResponse,
    ServerToClientFrame, SetGrantsRequest, SetGrantsResponse, Tool, ToolFilterRequest,
    ToolSchemasResponse,
};
use crate::contract::inbound::ToolSchemasBody;
use crate::error::{AppError, ErrorCode, Result};
use crate::transport::http::Json;
use crate::transport::middleware::{from_fn_with_state, require_client};
use crate::transport::state::AppContext;

use super::hash::{hash_extension, sha256_hex};
use super::install::register::flatten_permissions;
use super::install::runner::parse_manifest_or_err;
use super::install::{npm, seed, InstallSource};
use super::registry::GrantEntry;
use super::sandbox::permission_key;
use super::wire::{
    present, AlwaysAvailableRequest, BuiltinInstallResponse, OkResponse, Phase1Response,
    ReindexResponse, RescanResponse, UndeclaredPolicyRequest,
};
use super::Extensions;

/// `?ids=` is not used; `POST /check-updates` carries its body instead. The two
/// query surfaces in the slice are search's paging and nothing else.
#[derive(Debug, Default, Deserialize)]
struct SearchQuery {
    q: Option<String>,
    limit: Option<String>,
    offset: Option<String>,
}

pub fn routes(ctx: &AppContext, extensions: Extensions) -> Router {
    Router::new()
        .route("/api/v1/extensions", get(list))
        .route("/api/v1/extensions/search", get(search))
        .route("/api/v1/extensions/download", post(download))
        .route("/api/v1/extensions/builtin-install", post(builtin_install))
        .route("/api/v1/extensions/check-updates", post(check_updates))
        .route("/api/v1/extensions/rescan", post(rescan))
        .route("/api/v1/extensions/reindex", post(reindex))
        .route("/api/v1/extensions/filter", post(filter))
        .route("/api/v1/extensions/tool-schemas", post(tool_schemas))
        .route("/api/v1/extensions/embed", post(embed))
        .route("/api/v1/extensions/{id}", delete(delete_one))
        .route("/api/v1/extensions/{id}/install", post(install))
        .route("/api/v1/extensions/{id}/uninstall", post(uninstall))
        .route("/api/v1/extensions/{id}/update", post(update))
        .route("/api/v1/extensions/{id}/tools", get(list_tools))
        .route(
            "/api/v1/extensions/{id}/confirm-reenable",
            post(confirm_reenable),
        )
        .route(
            "/api/v1/extensions/{id}/undeclared-policy",
            post(undeclared_policy),
        )
        .route(
            "/api/v1/extensions/{id}/tools/{tool}/enable",
            post(enable_tool),
        )
        .route(
            "/api/v1/extensions/{id}/tools/{tool}/disable",
            post(disable_tool),
        )
        .route(
            "/api/v1/extensions/{id}/tools/{tool}/always-available",
            post(always_available),
        )
        .route(
            "/api/v1/extensions/{id}/tools/{tool}/grants",
            post(set_grants),
        )
        .layer(from_fn_with_state(ctx.clone(), require_client))
        .with_state(extensions)
}

/// The flat, provider-agnostic catalog.
///
/// A separate router because it mounts at a different prefix, not because it is
/// a different concern: this is the one route where D12 and D11 meet, and the
/// MCP half arrives through a trait D12 declares over D11's existing
/// `Mcp::list_all_tools`.
pub fn tools_routes(ctx: &AppContext, extensions: Extensions) -> Router {
    Router::new()
        .route("/api/v1/tools", get(list_all_tools))
        .layer(from_fn_with_state(ctx.clone(), require_client))
        .with_state(extensions)
}

// --- the registry views ------------------------------------------------------

async fn list(State(ext): State<Extensions>) -> Result<Json<ListExtensionsResponse>> {
    Ok(Json(ListExtensionsResponse(ext.registry().list().await?)))
}

async fn search(
    State(ext): State<Extensions>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<SearchExtensionsResponse>> {
    // The oracle parses with `Number(v)` and falls back on anything
    // non-finite, so a junk `?limit=abc` is the default rather than a 400.
    let limit = num_query(query.limit.as_deref(), 50);
    let offset = num_query(query.offset.as_deref(), 0);
    Ok(Json(SearchExtensionsResponse {
        results: npm::search(
            ext.installer().http(),
            query.q.as_deref().unwrap_or_default(),
            limit,
            offset,
        )
        .await?,
    }))
}

async fn list_tools(
    State(ext): State<Extensions>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<ListExtensionToolsResponse>> {
    let tools = ext.registry().list_tools(&id).await?;
    Ok(Json(ListExtensionToolsResponse {
        tools: ext.enrich_all(tools).await,
    }))
}

async fn list_all_tools(State(ext): State<Extensions>) -> Result<Json<ListExtensionToolsResponse>> {
    let extension_tools = ext.registry().list_all_tools().await?;
    let mut tools = ext.enrich_all(extension_tools).await;
    // MCP tools run on their own server and hold no local sandbox permissions,
    // so they union in unenriched. The Tools UI shows every tool from every
    // provider, and `providerKind` is what tells them apart.
    if let Some(mcp) = ext.mcp_tools() {
        tools.extend(mcp.list_all_tools().await?);
    }
    Ok(Json(ListExtensionToolsResponse { tools }))
}

// --- the install phases ------------------------------------------------------

async fn download(
    State(ext): State<Extensions>,
    Json(body): Json<DownloadExtensionRequest>,
) -> Result<Json<ExtensionJobResponse>> {
    let spec = match body {
        DownloadExtensionRequest::Npm { name, version } => InstallSource::Npm {
            name,
            // D-043: `""` is a JavaScript falsy discriminator on the oracle, so
            // it means "latest" rather than "the empty version".
            version: present(version.as_deref()).map(str::to_string),
        },
        DownloadExtensionRequest::Local { path, slug } => {
            let slug = present(slug.as_deref()).ok_or_else(|| {
                AppError::new(ErrorCode::ValidationError, "slug required for local")
            })?;
            if !super::install::register::valid_local_slug(slug) {
                return Err(AppError::new(
                    ErrorCode::ValidationError,
                    format!("invalid extension slug \"{slug}\""),
                ));
            }
            InstallSource::Local {
                path,
                slug: slug.to_string(),
            }
        }
        DownloadExtensionRequest::Seeded { id } => {
            // Refused HERE as well as inside the job, so the user gets a 400
            // rather than a job id followed by a failure frame.
            let seeded = seed::seeded_by_id(&id).ok_or_else(|| {
                AppError::new(
                    ErrorCode::ValidationError,
                    format!("unknown seeded extension \"{id}\""),
                )
            })?;
            seed::assert_installable(seeded, ext.installer().channel())?;
            InstallSource::Seeded { id, planted: None }
        }
    };
    Ok(Json(started(ext.installer().start_download(spec)?)))
}

async fn install(
    State(ext): State<Extensions>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<ExtensionJobResponse>> {
    let extension = ext.registry().get_or_err(&id).await?;
    if extension.status == ExtensionStatus::Drift {
        return Err(drifted(&id));
    }
    Ok(Json(started(ext.installer().start_install_deps(&id)?)))
}

async fn builtin_install(State(ext): State<Extensions>) -> Result<Json<BuiltinInstallResponse>> {
    Ok(Json(BuiltinInstallResponse {
        queued: ext.request_builtin_install().await?,
    }))
}

async fn update(
    State(ext): State<Extensions>,
    UrlPath(id): UrlPath<String>,
    raw: Bytes,
) -> Result<Json<ExtensionJobResponse>> {
    let extension = ext.registry().get_or_err(&id).await?;
    let body: crate::contract::api::UpdateExtensionRequest = parse_after_lookup(&raw)?;
    let requested = present(body.version.as_deref()).map(str::to_string);
    let spec = match extension.source {
        ExtensionSource::Seeded => InstallSource::Seeded {
            id: id.clone(),
            // No plant: this is a user press, so resolving and fetching the
            // signed CDN tarball is authorised.
            planted: None,
        },
        ExtensionSource::Npm => {
            // The id is the FLATTENED npm name, so it is un-flattened before it
            // goes back to the registry. Only the first `__` is a separator: a
            // package whose own name contains one keeps it.
            let npm_name = id.replacen("__", "/", 1);
            let resolved =
                npm::resolve_version(ext.installer().http(), &npm_name, requested.as_deref())
                    .await?;
            InstallSource::Npm {
                name: npm_name,
                version: Some(resolved.version),
            }
        }
        ExtensionSource::Local => {
            return Err(AppError::new(
                ErrorCode::ValidationError,
                "only npm and seeded extensions can be updated",
            ))
        }
    };
    Ok(Json(started(ext.installer().start_update(spec)?)))
}

async fn check_updates(
    State(ext): State<Extensions>,
    // Required rather than optional: the oracle reads it with plain
    // `readJson`, so an absent body is a 400 there too. `{}` parses, because
    // `ids` is optional and means "every installed extension".
    Json(body): Json<CheckUpdatesRequest>,
) -> Result<Json<CheckUpdatesResponse>> {
    let wanted: Option<HashSet<String>> = body.ids.map(|ids| ids.into_iter().collect());
    let all = ext.registry().list().await?;
    let targets = all
        .into_iter()
        .filter(|row| wanted.as_ref().is_none_or(|ids| ids.contains(&row.id)));

    let mut results = Vec::new();
    // Sequential ON PURPOSE, to stay polite to the npm registry
    // (`routes/extensions.ts:401-402`). A `join_all` here is a behaviour change
    // the user meets as a rate limit.
    for row in targets {
        results.push(ext.check_one(&row).await);
    }
    Ok(Json(CheckUpdatesResponse { results }))
}

async fn rescan(State(ext): State<Extensions>) -> Result<Json<RescanResponse>> {
    let out = ext.rescan().await?;
    ext.broadcast_snapshot();
    Ok(Json(out))
}

// --- removal -----------------------------------------------------------------

async fn delete_one(
    State(ext): State<Extensions>,
    UrlPath(id): UrlPath<String>,
) -> Result<Response> {
    ext.delete_extension(&id).await?;
    ext.broadcast_snapshot();
    // 204 with no body, matching the oracle. A `{ id }` envelope here would be
    // a diff on a route the client ignores the body of anyway.
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn uninstall(
    State(ext): State<Extensions>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<ExtensionActionResponse>> {
    ext.uninstall_extension(&id).await?;
    ext.broadcast_snapshot();
    Ok(Json(ExtensionActionResponse { id }))
}

// --- per-tool state ----------------------------------------------------------

async fn enable_tool(
    State(ext): State<Extensions>,
    UrlPath((id, tool)): UrlPath<(String, String)>,
) -> Result<Json<OkResponse>> {
    let extension = ext.registry().get_or_err(&id).await?;
    if extension.status == ExtensionStatus::Drift {
        return Err(drifted(&id));
    }
    require_tool(&ext, &id, &tool).await?;
    // Enabling is allowed even with ungranted required permissions: the tool is
    // then "enabled but not exposed", a warning state in the UI. The chat
    // exposure gate is the authority on what the model actually sees.
    ext.registry().set_tool_enabled(&id, &tool, true).await?;
    ext.refresh_permissions(&id).await;
    ext.broadcast_snapshot();
    Ok(Json(OkResponse::ok()))
}

async fn disable_tool(
    State(ext): State<Extensions>,
    UrlPath((id, tool)): UrlPath<(String, String)>,
) -> Result<Json<OkResponse>> {
    ext.registry().set_tool_enabled(&id, &tool, false).await?;
    ext.refresh_permissions(&id).await;
    ext.broadcast_snapshot();
    Ok(Json(OkResponse::ok()))
}

async fn always_available(
    State(ext): State<Extensions>,
    UrlPath((id, tool)): UrlPath<(String, String)>,
    Json(body): Json<AlwaysAvailableRequest>,
) -> Result<Json<OkResponse>> {
    require_tool(&ext, &id, &tool).await?;
    // No worker refresh: this changes tool SELECTION, not sandbox grants, so
    // tearing down a warm worker would cost a spawn for nothing.
    ext.registry()
        .set_tool_always_available(&id, &tool, body.value)
        .await?;
    ext.broadcast_snapshot();
    Ok(Json(OkResponse::ok()))
}

async fn set_grants(
    State(ext): State<Extensions>,
    UrlPath((id, tool)): UrlPath<(String, String)>,
    raw: Bytes,
) -> Result<Json<SetGrantsResponse>> {
    let row = require_tool(&ext, &id, &tool).await?;
    let body: SetGrantsRequest = parse_after_lookup(&raw)?;
    let enriched = ext.enrich(row).await;
    let mut decoded = Vec::with_capacity(body.grants.len());
    for grant in body.grants {
        // Validated against the tool's CURRENT declarations, so a grant key
        // from a previous `loca.json` cannot be written back: the user would be
        // approving something the author no longer declares, and the sandbox
        // derivation would then ignore it silently.
        let declared = enriched
            .required_permissions
            .iter()
            .find(|decl| permission_key(decl) == grant.key)
            .ok_or_else(|| {
                AppError::new(
                    ErrorCode::ValidationError,
                    format!(
                        "grant key {} not in this tool's required permissions",
                        grant.key
                    ),
                )
            })?;
        decoded.push(GrantEntry {
            key: grant.key,
            kind: super::sandbox::permission_kind_of(declared),
            state: grant.state,
        });
    }
    let grants = ext.registry().set_grants(&enriched.id, decoded).await?;
    ext.refresh_permissions(&id).await;
    Ok(Json(SetGrantsResponse { grants }))
}

async fn undeclared_policy(
    State(ext): State<Extensions>,
    UrlPath(id): UrlPath<String>,
    raw: Bytes,
) -> Result<Json<Extension>> {
    ext.registry().get_or_err(&id).await?;
    let body: UndeclaredPolicyRequest = parse_after_lookup(&raw)?;
    ext.registry()
        .set_undeclared_policy(&id, body.policy)
        .await?;
    ext.refresh_permissions(&id).await;
    Ok(Json(ext.registry().get_or_err(&id).await?))
}

async fn confirm_reenable(
    State(ext): State<Extensions>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<ExtensionActionResponse>> {
    let extension = ext.registry().get_or_err(&id).await?;
    // The user reviewed an out-of-band content change and trusts what is on
    // disk NOW, so the hash is re-read here rather than taken from the row.
    // This is the only path out of `drift`, and it is never automatic.
    let hash = hash_extension(Path::new(&extension.installed_path))?;
    ext.registry().repin_and_clear(&id, &hash).await?;
    Ok(Json(ExtensionActionResponse { id }))
}

// --- relevance ---------------------------------------------------------------

async fn reindex(State(ext): State<Extensions>) -> Result<Json<ReindexResponse>> {
    ext.reindex().await
}

async fn filter(
    State(ext): State<Extensions>,
    Json(body): Json<ToolFilterRequest>,
) -> Result<Json<Phase1Response>> {
    let vector: Vec<f32> = body.vector.iter().map(|v| *v as f32).collect();
    Ok(Json(
        ext.phase1(&vector, body.top_k.map(|k| k as usize), None)
            .await?,
    ))
}

async fn tool_schemas(
    State(ext): State<Extensions>,
    Json(body): Json<ToolSchemasBody>,
) -> Result<Json<ToolSchemasResponse>> {
    // The one zod-validated body in the slice
    // (`ids: z.array(z.string().min(1)).max(512)`), and it takes the INBOUND
    // type rather than the `api` one. That distinction is the whole point:
    // `api::ToolSchemasRequest` is generated from the TypeScript and carries no
    // constraints, so `minLength` was silently absent and an empty id reached
    // the registry and came back `tool_not_found` where the oracle answers
    // `validation_error`. `inbound::ToolSchemasBody` is generated from the Zod
    // schema and enforces it in the deserializer. See D-093.
    //
    // The array `.max(512)` does NOT survive the bridge either way (D-095: a
    // constraint survives only if some Rust type expresses it, and nothing is
    // shaped like "at most 512 items"), so the cardinality is restated here
    // rather than assumed.
    if body.ids.len() > TOOL_SCHEMAS_MAX_IDS {
        return Err(AppError::new(
            ErrorCode::ValidationError,
            format!("at most {TOOL_SCHEMAS_MAX_IDS} tool ids per request"),
        ));
    }
    let mut tools = Vec::with_capacity(body.ids.len());
    for id in &body.ids {
        let tool = ext
            .registry()
            .get_tool(id)
            .await?
            .ok_or_else(|| AppError::new(ErrorCode::ToolNotFound, id.clone()))?;
        tools.push(OpenAiToolDef {
            type_: OpenAiToolDefType::Function,
            function: OpenAiToolDefFunction {
                name: tool.name,
                description: tool.description,
                parameters: tool.parameters,
            },
        });
    }
    Ok(Json(ToolSchemasResponse { tools }))
}

/// `validation/extension.ts`'s `.max(512)`, restated because typify drops array
/// cardinality (D-083).
const TOOL_SCHEMAS_MAX_IDS: usize = 512;

async fn embed(
    State(ext): State<Extensions>,
    Json(body): Json<EmbedRequest>,
) -> Result<Json<EmbedResponse>> {
    let embedder = ext.embedder().ok_or_else(embed_unavailable)?;
    if !embedder.is_ready().await {
        return Err(embed_unavailable());
    }
    let vectors = embedder.embed(&body.texts).await?;
    Ok(Json(EmbedResponse {
        vectors: vectors
            .into_iter()
            .map(|v| v.into_iter().map(f64::from).collect())
            .collect(),
    }))
}

fn embed_unavailable() -> AppError {
    AppError::new(
        ErrorCode::ServerUnavailable,
        "embedding model not downloaded yet",
    )
}

// --- shared helpers ----------------------------------------------------------

fn started(started: super::install::runner::InstallStarted) -> ExtensionJobResponse {
    ExtensionJobResponse {
        job_id: started.job_id,
        extension_id: started.extension_id,
    }
}

fn drifted(id: &str) -> AppError {
    AppError::new(
        ErrorCode::ExtensionHashDrift,
        format!("{id} content drift; confirm re-enable first"),
    )
}

/// A JSON body parsed where the handler chooses, rather than before it runs.
///
/// Three routes in this slice look the subject up BEFORE they look at the body:
/// `:id/update` and `:id/undeclared-policy` check the extension exists
/// (`routes/extensions.ts:167`, `:287`), and `:id/tools/:tool/grants` checks the
/// tool does (`:256`). A `Json<T>` extractor runs before the handler body and
/// inverts that, so an unknown id with a malformed body answers
/// `validation_error` where the specification answers `extension_not_found`.
///
/// The ordering is contract rather than detail: validating first tells a caller
/// whether an id exists only once it supplies a well-formed body, so the two
/// orderings leak different information.
///
/// `:id/tools/:tool/always-available` is deliberately NOT one of the three. The
/// specification validates `value` there BEFORE it looks the tool up
/// (`:228-235`), so its `Json<T>` extractor already matches and moving it would
/// introduce the divergence this function exists to remove.
fn parse_after_lookup<T: serde::de::DeserializeOwned>(raw: &Bytes) -> Result<T> {
    serde_json::from_slice(raw)
        .map_err(|err| AppError::new(ErrorCode::ValidationError, err.to_string()))
}

async fn require_tool(ext: &Extensions, id: &str, name: &str) -> Result<Tool> {
    ext.registry()
        .list_tools(id)
        .await?
        .into_iter()
        .find(|tool| tool.name == name)
        .ok_or_else(|| AppError::new(ErrorCode::ToolNotFound, format!("{id}::{name}")))
}

/// `Number(v)`, with a non-finite result falling back to the default.
fn num_query(raw: Option<&str>, default: u32) -> u32 {
    match raw {
        None | Some("") => default,
        Some(raw) => raw
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite())
            .map_or(default, |n| if n < 0.0 { default } else { n as u32 }),
    }
}

impl Extensions {
    /// Fill `requiredPermissions` and `missingRequired` from the manifest on
    /// disk.
    ///
    /// Re-parsed rather than read from the persisted column because
    /// `missingRequired` is a function of the CURRENT grant rows: it is the
    /// indices of non-optional declarations with no decision yet, and an absent
    /// row behaves as `ask` at runtime.
    ///
    /// An unreadable or changed manifest yields EMPTY lists rather than an
    /// error, matching the oracle: the tool still exists and the UI still needs
    /// to render it, and drift is reported by the status column instead.
    pub async fn enrich(&self, tool: Tool) -> Tool {
        match self.required_for(&tool).await {
            Some((required, missing)) => Tool {
                required_permissions: required,
                missing_required: missing,
                ..tool
            },
            None => Tool {
                required_permissions: Vec::new(),
                missing_required: Vec::new(),
                ..tool
            },
        }
    }

    pub async fn enrich_all(&self, tools: Vec<Tool>) -> Vec<Tool> {
        let mut out = Vec::with_capacity(tools.len());
        for tool in tools {
            out.push(self.enrich(tool).await);
        }
        out
    }

    async fn required_for(
        &self,
        tool: &Tool,
    ) -> Option<(Vec<crate::contract::api::PermissionDecl>, Vec<i64>)> {
        let extension = self.registry().get(&tool.extension_id).await.ok()??;
        let raw = tokio::fs::read_to_string(Path::new(&extension.installed_path).join("loca.json"))
            .await
            .ok()?;
        let manifest = parse_manifest_or_err(&raw).ok()?;
        let declared = manifest
            .tools
            .iter()
            .flatten()
            .find(|entry| String::from(entry.name.clone()) == tool.name)?;
        let required = flatten_permissions(&declared.permissions);
        let decided: HashSet<&str> = tool
            .grants
            .iter()
            .map(|grant| grant.permission_key.as_str())
            .collect();
        let missing = required
            .iter()
            .enumerate()
            .filter(|(_, decl)| !is_optional(decl))
            .filter(|(_, decl)| !decided.contains(permission_key(decl).as_str()))
            .map(|(at, _)| at as i64)
            .collect();
        Some((required, missing))
    }

    /// One extension's upstream check, with its own error isolated.
    ///
    /// A failure is a FIELD on the row rather than a failed batch, so one
    /// unreachable registry does not hide every other extension's answer.
    pub async fn check_one(&self, row: &Extension) -> ExtensionUpdateStatus {
        let base = ExtensionUpdateStatus {
            id: row.id.clone(),
            installed_version: row.version.clone(),
            latest_version: None,
            update_available: false,
            error: None,
        };
        let seeded = match row.source {
            ExtensionSource::Seeded => seed::seeded_by_id(&row.id),
            _ => None,
        };
        // No upstream to check: a local extension, and a seeded row whose id is
        // no longer a known seeded extension. The second must NOT fall through
        // to an npm lookup, which would query a package that does not exist.
        if row.source == ExtensionSource::Local
            || (row.source == ExtensionSource::Seeded && seeded.is_none())
        {
            return base;
        }
        let latest = match seeded {
            Some(seeded) => self
                .installer()
                .load_seeded_manifest(seeded, true)
                .await
                .map(|manifest| manifest.version),
            None => {
                npm::resolve_latest_version(self.installer().http(), &row.id.replacen("__", "/", 1))
                    .await
            }
        };
        match latest {
            Ok(latest) => ExtensionUpdateStatus {
                update_available: latest != row.version,
                latest_version: Some(latest),
                ..base
            },
            Err(err) => ExtensionUpdateStatus {
                error: Some(err.message),
                ..base
            },
        }
    }

    /// Reconcile the extensions directory against the registry.
    ///
    /// Never installs dependencies and never pins a hash: a re-registered
    /// folder drops back to `downloaded` so the user must press Install before
    /// its tools can run again.
    pub async fn rescan(&self) -> Result<RescanResponse> {
        let mut out = RescanResponse::default();
        let on_disk = extension_dir_ids(&self.installer().paths().extensions_dir()).await;

        for id in &on_disk {
            let Some(raw) = tokio::fs::read_to_string(
                self.installer().paths().extension_dir(id).join("loca.json"),
            )
            .await
            .ok() else {
                // A stray folder with no manifest is ignored rather than being
                // an error: the extensions directory is a place users put
                // things.
                continue;
            };
            let existing = self.registry().get(id).await?;
            // Cheap heuristic, and deliberately so: a code-only edit with an
            // unchanged `loca.json` is left alone, because the content hash is
            // what notices that and drift is what reports it.
            if let Some(existing) = &existing {
                if existing.manifest_hash == sha256_hex(raw.as_bytes()) {
                    continue;
                }
            }
            match self.installer().register_local_downloaded(id).await {
                Ok(()) if existing.is_some() => out.updated += 1,
                Ok(()) => out.added += 1,
                // One folder with an invalid manifest must not fail the whole
                // rescan, or a single bad extension makes the button useless.
                Err(err) => tracing::warn!(
                    target: "loca::extensions",
                    "rescan: skipping {id}: {}", err.message,
                ),
            }
        }

        for row in self.registry().list().await? {
            if row.source == ExtensionSource::Local && !on_disk.contains(&row.id) {
                self.refresh_permissions(&row.id).await;
                self.registry().delete(&row.id).await?;
                out.removed += 1;
            }
        }
        Ok(out)
    }

    /// Re-embed every enabled tool of every installed extension.
    pub async fn reindex(&self) -> Result<Json<ReindexResponse>> {
        // Tool-relevance needs the embed model. With it absent this skips
        // CLEANLY rather than raising an opaque error: the requirements flow
        // prompts for that download separately, and a 500 here would look like
        // a broken core.
        let Some(embedder) = self.embedder() else {
            return Ok(Json(ReindexResponse {
                embedded: 0,
                skipped: true,
            }));
        };
        if !embedder.is_ready().await {
            return Ok(Json(ReindexResponse {
                embedded: 0,
                skipped: true,
            }));
        }
        let mut embedded = 0;
        for tool in self.enabled_tools().await? {
            let text = crate::memories::relevance::tool_embed_text(
                &tool.name,
                &tool.description,
                &tool.triggers,
            );
            if let Some(result) = embedder.embed_with_hash(&text).await? {
                self.registry()
                    .store_embedding(&tool.id, result.0, &result.1)
                    .await?;
                embedded += 1;
            }
        }
        Ok(Json(ReindexResponse {
            embedded,
            skipped: false,
        }))
    }

    pub fn broadcast_snapshot(&self) {
        self.frames()
            .broadcast_all(&ServerToClientFrame::ExtensionSnapshot);
    }
}

fn is_optional(decl: &crate::contract::api::PermissionDecl) -> bool {
    use crate::contract::api::PermissionDecl as D;
    match decl {
        D::Net { optional, .. }
        | D::Read { optional, .. }
        | D::Write { optional, .. }
        | D::Run { optional, .. }
        | D::Env { optional, .. }
        | D::Ffi { optional, .. }
        | D::Sys { optional, .. }
        | D::Memories { optional, .. }
        | D::Llm { optional, .. }
        | D::Tts { optional, .. }
        | D::Stt { optional, .. } => optional.unwrap_or(false),
    }
}

/// Extension directory ids on disk, skipping the installer's staging dirs.
///
/// A `.new` or `.old` directory is a swap in progress or its leftover, never an
/// extension, and registering one would create a row for an id that vanishes.
async fn extension_dir_ids(root: &Path) -> HashSet<String> {
    let mut ids = HashSet::new();
    let Ok(mut entries) = tokio::fs::read_dir(root).await else {
        return ids;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        if !entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".new") || name.ends_with(".old") {
            continue;
        }
        ids.insert(name);
    }
    ids
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    use std::sync::Arc;

    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use serde_json::{json, Value};
    use tower::ServiceExt;

    use crate::config::{Channel, Config};
    use crate::db::migrate;
    use crate::extensions::install::net::NoNetwork;
    use crate::extensions::install::runner::{
        Installer, InstallerConfig, NoMemories, SilentEvents,
    };
    use crate::paths::Paths;
    use crate::sidecars::Manager;
    use crate::transport::testing::{context_with, StubAuth};

    const TOKEN: &str = "tok";
    const CLIENT: &str = "client-1";

    struct Env {
        router: Router,
        ext: Extensions,
        paths: Paths,
        _dir: tempfile::TempDir,
    }

    fn env() -> Env {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = context_with(Arc::new(StubAuth::new().with_client(TOKEN, CLIENT)));
        ctx.paths = Arc::new(Paths::rooted(dir.path()));
        ctx.config = Arc::new(Config {
            channel: Channel::Stable,
            ..(*ctx.config).clone()
        });
        ctx.paths.ensure_dirs().unwrap();
        std::fs::create_dir_all(ctx.paths.extensions_dir()).unwrap();
        ctx.db.call_blocking(|conn| migrate(conn)).unwrap();
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

        // The registry shares the context's database rather than opening a
        // second one: `context_with` already migrated it, and two databases
        // would make the auth middleware and the registry disagree about what
        // exists.
        let registry = super::super::registry::Registry::new(ctx.db.clone(), (*ctx.paths).clone());
        let installer = Installer::new(InstallerConfig {
            paths: (*ctx.paths).clone(),
            config: (*ctx.config).clone(),
            settings: ctx.settings.clone(),
            registry,
            // Every route these tests reach is offline. The two that are not
            // (search, and an npm update) would PANIC here rather than
            // silently reaching the real npm registry from a unit test.
            http: Arc::new(NoNetwork),
            sidecars: Arc::new(Manager::new()),
            memories: Arc::new(NoMemories),
            events: Arc::new(SilentEvents),
        });
        let ext =
            Extensions::builder(installer, Arc::new(NoMemories), Arc::clone(&ctx.frames)).build();
        let router = routes(&ctx, ext.clone()).merge(tools_routes(&ctx, ext.clone()));
        Env {
            router,
            ext,
            paths: (*ctx.paths).clone(),
            _dir: dir,
        }
    }

    async fn call_as(
        env: &Env,
        token: Option<&str>,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
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

    async fn call(env: &Env, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        call_as(env, Some(TOKEN), method, path, body).await
    }

    const MANIFEST: &str = r#"{
      "name": "demo",
      "displayName": "Demo",
      "description": "a demo extension",
      "tools": [{
        "name": "fetch_thing",
        "description": "fetches a thing",
        "function": "fetchThing",
        "parameters": { "type": "object" },
        "permissions": {
          "net": [{ "host": "example.test", "ports": [443], "reason": "to fetch" }],
          "read": [{ "path": "$home/notes", "reason": "to read notes", "optional": true }]
        }
      }]
    }"#;

    /// A registered, installed extension with one tool.
    async fn seed_extension(env: &Env, id: &str) {
        let folder = env.paths.extension_dir(id);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("loca.json"), MANIFEST).unwrap();
        env.ext
            .installer()
            .register_local_downloaded(id)
            .await
            .unwrap();
    }

    // --- auth --------------------------------------------------------------

    #[tokio::test]
    async fn every_route_in_the_slice_needs_a_paired_client() {
        // None of these is admin-gated, and none is open. An unauthenticated
        // caller reading the extension list learns which tools the machine can
        // run and what paths they were granted.
        let env = env();
        for (method, path) in [
            ("GET", "/api/v1/extensions"),
            ("GET", "/api/v1/tools"),
            ("POST", "/api/v1/extensions/rescan"),
            ("DELETE", "/api/v1/extensions/demo"),
        ] {
            let (status, _) = call_as(&env, None, method, path, None).await;
            assert_eq!(status, 401, "{method} {path} answered without a token");
        }
    }

    // --- the registry views ------------------------------------------------

    #[tokio::test]
    async fn a_fresh_core_lists_no_extensions_and_no_tools() {
        let env = env();
        assert_eq!(
            call(&env, "GET", "/api/v1/extensions", None).await,
            (200, json!([]))
        );
        assert_eq!(
            call(&env, "GET", "/api/v1/tools", None).await,
            (200, json!({ "tools": [] }))
        );
    }

    #[tokio::test]
    async fn a_tools_listing_carries_the_declarations_the_registry_leaves_empty() {
        // `requiredPermissions` and `missingRequired` are derived HERE from the
        // manifest on disk, because `missingRequired` is a function of the
        // current grant rows and the persisted column is not.
        let env = env();
        seed_extension(&env, "demo").await;
        let (status, body) = call(&env, "GET", "/api/v1/extensions/demo/tools", None).await;
        assert_eq!(status, 200);
        let tool = &body["tools"][0];
        assert_eq!(tool["name"], "fetch_thing");
        assert_eq!(
            tool["requiredPermissions"].as_array().unwrap().len(),
            2,
            "both declarations must survive the flattening: {tool}"
        );
        // Index 0 is the required net declaration, index 1 the OPTIONAL read.
        // An optional declaration is never "missing": the user is not asked
        // about it, so counting it would show a permanent warning badge.
        assert_eq!(tool["missingRequired"], json!([0]));
    }

    #[tokio::test]
    async fn a_decided_permission_stops_being_missing() {
        let env = env();
        seed_extension(&env, "demo").await;
        let (_, before) = call(&env, "GET", "/api/v1/extensions/demo/tools", None).await;
        let key = before["tools"][0]["requiredPermissions"][0].clone();
        let key = crate::extensions::sandbox::permission_key(&serde_json::from_value(key).unwrap());
        let (status, body) = call(
            &env,
            "POST",
            "/api/v1/extensions/demo/tools/fetch_thing/grants",
            Some(json!({ "grants": [{ "key": key, "state": "denied" }] })),
        )
        .await;
        assert_eq!(status, 200, "{body}");
        let (_, after) = call(&env, "GET", "/api/v1/extensions/demo/tools", None).await;
        assert_eq!(
            after["tools"][0]["missingRequired"],
            json!([]),
            "a DENIED permission is decided; missingRequired is about the absence of a decision"
        );
    }

    #[tokio::test]
    async fn a_grant_key_the_tool_does_not_declare_is_refused() {
        // Writing it would record an approval for something the author never
        // declared, and the sandbox derivation would then ignore it silently:
        // the user would believe they granted something they did not.
        let env = env();
        seed_extension(&env, "demo").await;
        let (status, body) = call(
            &env,
            "POST",
            "/api/v1/extensions/demo/tools/fetch_thing/grants",
            Some(json!({ "grants": [{ "key": "net:evil.test:*", "state": "granted" }] })),
        )
        .await;
        assert_eq!(status, 400);
        assert_eq!(body["error"]["code"], "validation_error");
    }

    #[tokio::test]
    async fn a_grant_on_an_unknown_tool_is_a_tool_not_found() {
        let env = env();
        seed_extension(&env, "demo").await;
        let (status, body) = call(
            &env,
            "POST",
            "/api/v1/extensions/demo/tools/nope/grants",
            Some(json!({ "grants": [] })),
        )
        .await;
        assert_eq!(status, 404);
        assert_eq!(body["error"]["code"], "tool_not_found");
    }

    // --- drift -------------------------------------------------------------

    #[tokio::test]
    async fn a_drifted_extension_can_be_neither_installed_nor_enabled() {
        // Two routes, one gate. Dropping the check on either leaves a path
        // where content the user has not reviewed reaches the model.
        let env = env();
        seed_extension(&env, "demo").await;
        env.ext.registry().mark_drift("demo").await.unwrap();

        for path in [
            "/api/v1/extensions/demo/install",
            "/api/v1/extensions/demo/tools/fetch_thing/enable",
        ] {
            let (status, body) = call(&env, "POST", path, None).await;
            assert_eq!(
                body["error"]["code"], "extension_hash_drift",
                "{path}: {body}"
            );
            assert_eq!(status, 409, "{path}");
        }
    }

    #[tokio::test]
    async fn confirm_reenable_repins_the_current_content_and_clears_drift() {
        let env = env();
        seed_extension(&env, "demo").await;
        // An out-of-band edit, which is what drift reports.
        std::fs::write(env.paths.extension_dir("demo").join("extra.ts"), "// new").unwrap();
        env.ext.registry().mark_drift("demo").await.unwrap();

        let (status, body) = call(
            &env,
            "POST",
            "/api/v1/extensions/demo/confirm-reenable",
            None,
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(body, json!({ "id": "demo" }));
        let row = env.ext.registry().get_or_err("demo").await.unwrap();
        assert_eq!(row.status, ExtensionStatus::Installed);
        // Re-pinned against what is on disk NOW, so the next verify passes.
        env.ext.registry().verify_fresh("demo").await.unwrap();
    }

    // --- toggles -----------------------------------------------------------

    #[tokio::test]
    async fn enabling_and_disabling_a_tool_round_trips() {
        let env = env();
        seed_extension(&env, "demo").await;
        assert_eq!(
            call(
                &env,
                "POST",
                "/api/v1/extensions/demo/tools/fetch_thing/enable",
                None
            )
            .await,
            (200, json!({ "ok": true }))
        );
        assert!(env.ext.registry().list_tools("demo").await.unwrap()[0].enabled);
        assert_eq!(
            call(
                &env,
                "POST",
                "/api/v1/extensions/demo/tools/fetch_thing/disable",
                None
            )
            .await,
            (200, json!({ "ok": true }))
        );
        assert!(!env.ext.registry().list_tools("demo").await.unwrap()[0].enabled);
    }

    #[tokio::test]
    async fn enabling_a_tool_with_no_grants_is_allowed() {
        // "Enabled but not exposed" is a real state and a warning in the UI.
        // Refusing here would make the permission review impossible to reach:
        // the user enables first and grants second.
        let env = env();
        seed_extension(&env, "demo").await;
        let (status, _) = call(
            &env,
            "POST",
            "/api/v1/extensions/demo/tools/fetch_thing/enable",
            None,
        )
        .await;
        assert_eq!(status, 200);
    }

    #[tokio::test]
    async fn always_available_requires_a_boolean() {
        let env = env();
        seed_extension(&env, "demo").await;
        let (status, _) = call(
            &env,
            "POST",
            "/api/v1/extensions/demo/tools/fetch_thing/always-available",
            Some(json!({ "value": "yes" })),
        )
        .await;
        assert_eq!(status, 400);
        let (status, _) = call(
            &env,
            "POST",
            "/api/v1/extensions/demo/tools/fetch_thing/always-available",
            Some(json!({ "value": true })),
        )
        .await;
        assert_eq!(status, 200);
        assert!(env.ext.registry().list_tools("demo").await.unwrap()[0].always_available);
    }

    #[tokio::test]
    async fn the_undeclared_policy_accepts_only_the_two_it_declares() {
        let env = env();
        seed_extension(&env, "demo").await;
        let (status, body) = call(
            &env,
            "POST",
            "/api/v1/extensions/demo/undeclared-policy",
            Some(json!({ "policy": "ask" })),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(body["undeclaredPolicy"], "ask");
        let (status, _) = call(
            &env,
            "POST",
            "/api/v1/extensions/demo/undeclared-policy",
            Some(json!({ "policy": "allow" })),
        )
        .await;
        assert_eq!(
            status, 400,
            "a third policy would match no branch at runtime"
        );
    }

    #[tokio::test]
    async fn three_routes_look_their_subject_up_before_they_look_at_the_body() {
        // The specification checks the extension (or the tool) exists first, so
        // an unknown one answers 404 whatever the body says. An extractor that
        // parses first inverts this and answers `validation_error`, which tells
        // a caller whether an id exists only once it supplies a well-formed
        // body: the two orderings leak different information.
        //
        // Every body below is INVALID for its route, so a 404 can only come
        // from the lookup having run first.
        let env = env();
        for (path, body) in [
            (
                "/api/v1/extensions/ghost/undeclared-policy",
                json!({ "policy": "allow" }),
            ),
            ("/api/v1/extensions/ghost/update", json!({ "version": 5 })),
        ] {
            let (status, body) = call(&env, "POST", path, Some(body)).await;
            assert_eq!(status, 404, "{path}");
            assert_eq!(body["error"]["code"], "extension_not_found", "{path}");
        }
        let (status, body) = call(
            &env,
            "POST",
            "/api/v1/extensions/ghost/tools/nope/grants",
            Some(json!({ "grants": "not an array" })),
        )
        .await;
        assert_eq!(status, 404);
        assert_eq!(body["error"]["code"], "tool_not_found");
    }

    #[tokio::test]
    async fn always_available_is_the_one_route_that_validates_before_it_looks_up() {
        // Not an oversight on either side, and the reason it is pinned: the
        // specification reads and screens `value` BEFORE the tool lookup
        // (`routes/extensions.ts:228-235`), so "check existence first" applied
        // uniformly would be a divergence rather than a tidy-up.
        let env = env();
        let (status, body) = call(
            &env,
            "POST",
            "/api/v1/extensions/ghost/tools/nope/always-available",
            Some(json!({ "value": "not a boolean" })),
        )
        .await;
        assert_eq!(status, 400);
        assert_eq!(body["error"]["code"], "validation_error");
    }

    // --- removal -----------------------------------------------------------

    #[tokio::test]
    async fn delete_removes_the_rows_and_the_files_and_answers_204() {
        let env = env();
        seed_extension(&env, "demo").await;
        let response = env
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/v1/extensions/demo")
                    .header("authorization", format!("Bearer {TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 204);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(bytes.is_empty(), "204 carries no body");
        assert!(env.ext.registry().get("demo").await.unwrap().is_none());
        assert!(!env.paths.extension_dir("demo").exists());
    }

    #[tokio::test]
    async fn uninstalling_a_dependency_free_extension_is_refused() {
        // It has nothing to uninstall: `node_modules` and `deno.lock` are what
        // uninstall removes, and reverting the status alone would leave the
        // user pressing Install on something already complete.
        let env = env();
        seed_extension(&env, "demo").await;
        let (status, body) = call(&env, "POST", "/api/v1/extensions/demo/uninstall", None).await;
        assert_eq!(status, 400);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("delete it instead"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn an_unknown_extension_is_a_404_on_every_route_that_names_one() {
        let env = env();
        for (method, path) in [
            ("POST", "/api/v1/extensions/ghost/install"),
            ("POST", "/api/v1/extensions/ghost/uninstall"),
            ("POST", "/api/v1/extensions/ghost/confirm-reenable"),
            ("DELETE", "/api/v1/extensions/ghost"),
        ] {
            let (status, body) = call(&env, method, path, None).await;
            assert_eq!(status, 404, "{method} {path}: {body}");
            assert_eq!(body["error"]["code"], "extension_not_found");
        }
    }

    // --- rescan ------------------------------------------------------------

    #[tokio::test]
    async fn rescan_registers_a_dropped_in_folder_and_prunes_a_removed_one() {
        let env = env();
        let folder = env.paths.extension_dir("dropped");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("loca.json"), MANIFEST).unwrap();

        let (status, body) = call(&env, "POST", "/api/v1/extensions/rescan", None).await;
        assert_eq!(status, 200);
        assert_eq!(body, json!({ "added": 1, "updated": 0, "removed": 0 }));

        // A second rescan is a no-op: the manifest hash is unchanged, so the
        // row is left alone rather than being reset to `downloaded` and forcing
        // the user to press Install again on every rescan.
        let (_, body) = call(&env, "POST", "/api/v1/extensions/rescan", None).await;
        assert_eq!(body, json!({ "added": 0, "updated": 0, "removed": 0 }));

        std::fs::remove_dir_all(&folder).unwrap();
        let (_, body) = call(&env, "POST", "/api/v1/extensions/rescan", None).await;
        assert_eq!(body, json!({ "added": 0, "updated": 0, "removed": 1 }));
    }

    #[tokio::test]
    async fn rescan_re_registers_a_folder_whose_manifest_changed() {
        let env = env();
        seed_extension(&env, "demo").await;
        std::fs::write(
            env.paths.extension_dir("demo").join("loca.json"),
            MANIFEST.replace("a demo extension", "a changed extension"),
        )
        .unwrap();
        let (_, body) = call(&env, "POST", "/api/v1/extensions/rescan", None).await;
        assert_eq!(body, json!({ "added": 0, "updated": 1, "removed": 0 }));
    }

    #[tokio::test]
    async fn rescan_skips_the_installers_staging_directories() {
        // `demo.new` is a swap in progress and `demo.old` its predecessor.
        // Registering either creates a row for an id that is about to vanish.
        let env = env();
        for name in ["demo.new", "demo.old"] {
            let folder = env.paths.extensions_dir().join(name);
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(folder.join("loca.json"), MANIFEST).unwrap();
        }
        let (_, body) = call(&env, "POST", "/api/v1/extensions/rescan", None).await;
        assert_eq!(body, json!({ "added": 0, "updated": 0, "removed": 0 }));
    }

    #[tokio::test]
    async fn rescan_ignores_a_stray_folder_and_survives_a_broken_manifest() {
        let env = env();
        std::fs::create_dir_all(env.paths.extension_dir("stray")).unwrap();
        let broken = env.paths.extension_dir("broken");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join("loca.json"), "{ not json").unwrap();
        let good = env.paths.extension_dir("good");
        std::fs::create_dir_all(&good).unwrap();
        std::fs::write(good.join("loca.json"), MANIFEST).unwrap();

        let (status, body) = call(&env, "POST", "/api/v1/extensions/rescan", None).await;
        assert_eq!(status, 200);
        assert_eq!(
            body,
            json!({ "added": 1, "updated": 0, "removed": 0 }),
            "one bad folder must not fail the whole rescan"
        );
    }

    // --- the relevance routes ----------------------------------------------

    #[tokio::test]
    async fn reindex_skips_cleanly_when_the_embed_model_is_absent() {
        // A 500 here would look like a broken core. The requirements flow
        // prompts for the download separately.
        let env = env();
        assert_eq!(
            call(&env, "POST", "/api/v1/extensions/reindex", None).await,
            (200, json!({ "embedded": 0, "skipped": true }))
        );
    }

    #[tokio::test]
    async fn embed_reports_the_missing_model_rather_than_an_opaque_failure() {
        let env = env();
        let (status, body) = call(
            &env,
            "POST",
            "/api/v1/extensions/embed",
            Some(json!({ "texts": ["hello"] })),
        )
        .await;
        assert_eq!(status, 503);
        assert_eq!(body["error"]["code"], "server_unavailable");
    }

    #[tokio::test]
    async fn filter_answers_all_three_lists_even_with_nothing_embedded() {
        let env = env();
        seed_extension(&env, "demo").await;
        let (status, body) = call(
            &env,
            "POST",
            "/api/v1/extensions/filter",
            Some(json!({ "vector": [0.0, 1.0], "topK": 5 })),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(body["candidates"], json!([]));
        assert_eq!(body["alwaysAvailable"], json!([]));
        assert_eq!(
            body["nameMatched"],
            json!([]),
            "the client reads all three; dropping two is the F2 shape bug"
        );
    }

    #[tokio::test]
    async fn an_always_available_tool_bypasses_ranking_entirely() {
        let env = env();
        seed_extension(&env, "demo").await;
        env.ext
            .registry()
            .set_tool_enabled("demo", "fetch_thing", true)
            .await
            .unwrap();
        env.ext
            .registry()
            .set_tool_always_available("demo", "fetch_thing", true)
            .await
            .unwrap();
        let (_, body) = call(
            &env,
            "POST",
            "/api/v1/extensions/filter",
            Some(json!({ "vector": [0.0, 1.0] })),
        )
        .await;
        assert_eq!(
            body["alwaysAvailable"][0]["name"], "fetch_thing",
            "it is offered every turn, with no embedding and no candidate slot: {body}"
        );
        assert_eq!(body["candidates"], json!([]));
    }

    #[tokio::test]
    async fn candidates_come_back_most_similar_first() {
        // Nothing else in this slice compares two tools, so an inverted sort
        // is invisible: with one tool the ranking is the same in both
        // directions, and the model is then offered the LEAST relevant tools
        // on every turn while every other assertion passes.
        let env = env();
        seed_extension(&env, "demo").await;
        seed_extension(&env, "other").await;
        let mut ids = Vec::new();
        for id in ["demo", "other"] {
            env.ext
                .registry()
                .set_tool_enabled(id, "fetch_thing", true)
                .await
                .unwrap();
            ids.push(
                env.ext.registry().list_tools(id).await.unwrap()[0]
                    .id
                    .clone(),
            );
        }
        // Unit vectors, so cosine is the dot product: the first scores 1.0
        // against the query and the second 0.0.
        env.ext
            .registry()
            .store_embedding(&ids[0], vec![1.0, 0.0], "h")
            .await
            .unwrap();
        env.ext
            .registry()
            .store_embedding(&ids[1], vec![0.0, 1.0], "h")
            .await
            .unwrap();

        let (status, body) = call(
            &env,
            "POST",
            "/api/v1/extensions/filter",
            Some(json!({ "vector": [1.0, 0.0] })),
        )
        .await;
        assert_eq!(status, 200);
        let candidates = body["candidates"].as_array().unwrap();
        assert_eq!(candidates.len(), 2, "{body}");
        assert_eq!(candidates[0]["toolId"], json!(ids[0]), "{body}");
        assert_eq!(candidates[1]["toolId"], json!(ids[1]), "{body}");
        assert!(
            candidates[0]["similarity"].as_f64().unwrap()
                > candidates[1]["similarity"].as_f64().unwrap(),
            "the score travels with the descriptor, and it must agree with the order: {body}"
        );
    }

    #[tokio::test]
    async fn top_k_bounds_the_candidate_list() {
        let env = env();
        seed_extension(&env, "demo").await;
        seed_extension(&env, "other").await;
        for id in ["demo", "other"] {
            env.ext
                .registry()
                .set_tool_enabled(id, "fetch_thing", true)
                .await
                .unwrap();
            let tool_id = env.ext.registry().list_tools(id).await.unwrap()[0]
                .id
                .clone();
            env.ext
                .registry()
                .store_embedding(&tool_id, vec![1.0, 0.0], "h")
                .await
                .unwrap();
        }
        let (_, body) = call(
            &env,
            "POST",
            "/api/v1/extensions/filter",
            Some(json!({ "vector": [1.0, 0.0], "topK": 1 })),
        )
        .await;
        assert_eq!(body["candidates"].as_array().unwrap().len(), 1, "{body}");
    }

    #[tokio::test]
    async fn a_disabled_tool_is_never_a_candidate() {
        // The relevance filter is not a second place to decide what the model
        // sees; a tool the user turned off must not come back with a score.
        let env = env();
        seed_extension(&env, "demo").await;
        let tool_id = env.ext.registry().list_tools("demo").await.unwrap()[0]
            .id
            .clone();
        env.ext
            .registry()
            .store_embedding(&tool_id, vec![1.0, 0.0], "h")
            .await
            .unwrap();
        let (_, body) = call(
            &env,
            "POST",
            "/api/v1/extensions/filter",
            Some(json!({ "vector": [1.0, 0.0] })),
        )
        .await;
        assert_eq!(body["candidates"], json!([]), "{body}");
    }

    #[tokio::test]
    async fn tool_schemas_projects_the_openai_shape_and_refuses_an_unknown_id() {
        let env = env();
        seed_extension(&env, "demo").await;
        let id = env.ext.registry().list_tools("demo").await.unwrap()[0]
            .id
            .clone();
        let (status, body) = call(
            &env,
            "POST",
            "/api/v1/extensions/tool-schemas",
            Some(json!({ "ids": [id] })),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "fetch_thing");

        let (status, body) = call(
            &env,
            "POST",
            "/api/v1/extensions/tool-schemas",
            Some(json!({ "ids": ["demo::nope"] })),
        )
        .await;
        assert_eq!(status, 404);
        assert_eq!(body["error"]["code"], "tool_not_found");
    }

    #[tokio::test]
    async fn tool_schemas_enforces_the_id_cap_the_generated_type_dropped() {
        // typify drops array cardinality (D-083), so `.max(512)` compiles into
        // nothing and the cap is restated at the route. Without it a caller can
        // make the core do unbounded per-id lookups on one request.
        let env = env();
        let ids: Vec<String> = (0..513).map(|n| format!("demo::t{n}")).collect();
        let (status, body) = call(
            &env,
            "POST",
            "/api/v1/extensions/tool-schemas",
            Some(json!({ "ids": ids })),
        )
        .await;
        assert_eq!(status, 400);
        assert_eq!(body["error"]["code"], "validation_error");
        // And 512 exactly is accepted, so the cap is a bound rather than an
        // off-by-one that refuses the documented maximum.
        let ids: Vec<String> = (0..512).map(|n| format!("demo::t{n}")).collect();
        let (status, body) = call(
            &env,
            "POST",
            "/api/v1/extensions/tool-schemas",
            Some(json!({ "ids": ids })),
        )
        .await;
        assert_eq!(status, 404, "refused for the id, not for the count: {body}");
    }

    // --- download and update ------------------------------------------------

    #[tokio::test]
    async fn a_local_download_needs_a_slug_and_an_empty_one_does_not_count() {
        // D-043: `""` is falsy on the oracle, so it is ABSENT rather than a
        // slug that happens to be empty, which would resolve to the extensions
        // directory itself.
        let env = env();
        for body in [
            json!({ "source": "local", "path": "/tmp/x" }),
            json!({ "source": "local", "path": "/tmp/x", "slug": "" }),
        ] {
            let (status, response) =
                call(&env, "POST", "/api/v1/extensions/download", Some(body)).await;
            assert_eq!(status, 400);
            assert!(
                response["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("slug required"),
                "{response}"
            );
        }
    }

    #[tokio::test]
    async fn a_local_slug_that_could_escape_the_extensions_directory_is_refused() {
        let env = env();
        for slug in ["..", "a/b", "a\\b", ".", "with space"] {
            let (status, _) = call(
                &env,
                "POST",
                "/api/v1/extensions/download",
                Some(json!({ "source": "local", "path": "/tmp/x", "slug": slug })),
            )
            .await;
            assert_eq!(status, 400, "slug {slug:?} was accepted");
        }
    }

    #[tokio::test]
    async fn an_unknown_seeded_id_is_refused_at_the_route_rather_than_inside_the_job() {
        // Refused here as well as in the job, so the user gets a 400 instead of
        // a job id followed by a failure frame nothing is showing yet.
        let env = env();
        let (status, body) = call(
            &env,
            "POST",
            "/api/v1/extensions/download",
            Some(json!({ "source": "seeded", "id": "loca-extension-invented" })),
        )
        .await;
        assert_eq!(status, 400);
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unknown"));
    }

    #[tokio::test]
    async fn a_dev_only_seeded_extension_is_refused_on_the_stable_channel() {
        let env = env();
        let (status, body) = call(
            &env,
            "POST",
            "/api/v1/extensions/download",
            Some(json!({ "source": "seeded", "id": "loca-extension-samples" })),
        )
        .await;
        assert_eq!(status, 400);
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("dev-only"));
    }

    #[tokio::test]
    async fn a_local_extension_cannot_be_updated() {
        // There is no upstream to update from, and falling through to an npm
        // lookup would query a package that does not exist.
        let env = env();
        seed_extension(&env, "demo").await;
        let (status, body) = call(
            &env,
            "POST",
            "/api/v1/extensions/demo/update",
            Some(json!({})),
        )
        .await;
        assert_eq!(status, 400);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("only npm and seeded"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn a_local_extension_reports_no_upstream_rather_than_an_error() {
        let env = env();
        seed_extension(&env, "demo").await;
        let (status, body) = call(
            &env,
            "POST",
            "/api/v1/extensions/check-updates",
            Some(json!({})),
        )
        .await;
        assert_eq!(status, 200);
        let row = &body["results"][0];
        assert_eq!(row["id"], "demo");
        assert_eq!(row["latestVersion"], Value::Null);
        assert_eq!(row["updateAvailable"], json!(false));
        assert!(
            row.get("error").is_none(),
            "having no upstream is not a failure: {row}"
        );
    }

    #[tokio::test]
    async fn check_updates_honours_the_id_filter() {
        let env = env();
        seed_extension(&env, "demo").await;
        seed_extension(&env, "other").await;
        let (_, all) = call(
            &env,
            "POST",
            "/api/v1/extensions/check-updates",
            Some(json!({})),
        )
        .await;
        assert_eq!(all["results"].as_array().unwrap().len(), 2);
        let (_, one) = call(
            &env,
            "POST",
            "/api/v1/extensions/check-updates",
            Some(json!({ "ids": ["demo"] })),
        )
        .await;
        assert_eq!(one["results"].as_array().unwrap().len(), 1);
        assert_eq!(one["results"][0]["id"], "demo");
    }

    #[tokio::test]
    async fn builtin_install_is_a_no_op_when_the_builtin_is_not_present() {
        let env = env();
        assert_eq!(
            call(&env, "POST", "/api/v1/extensions/builtin-install", None).await,
            (200, json!({ "queued": false }))
        );
    }

    #[tokio::test]
    async fn builtin_install_queues_while_the_runtime_is_missing() {
        // Queued is not a failure: the user pressed a button, and the `deno`
        // download is a separate consent-gated step. Reporting an error would
        // read as "the built-in is broken".
        let env = env();
        seed_extension(&env, "loca-extension-builtin").await;
        env.ext
            .registry()
            .mark_downloaded("loca-extension-builtin")
            .await
            .unwrap();
        assert_eq!(
            call(&env, "POST", "/api/v1/extensions/builtin-install", None).await,
            (200, json!({ "queued": true }))
        );
    }

    #[tokio::test]
    async fn builtin_install_leaves_a_drifted_builtin_alone() {
        let env = env();
        seed_extension(&env, "loca-extension-builtin").await;
        env.ext
            .registry()
            .mark_drift("loca-extension-builtin")
            .await
            .unwrap();
        assert_eq!(
            call(&env, "POST", "/api/v1/extensions/builtin-install", None).await,
            (200, json!({ "queued": false })),
            "re-installing over drift would re-pin content the user has not reviewed"
        );
    }

    #[tokio::test]
    async fn an_installed_builtin_has_its_tools_turned_on() {
        let env = env();
        seed_extension(&env, "loca-extension-builtin").await;
        assert!(
            !env.ext
                .registry()
                .list_tools("loca-extension-builtin")
                .await
                .unwrap()[0]
                .enabled
        );
        call(&env, "POST", "/api/v1/extensions/builtin-install", None).await;
        assert!(
            env.ext
                .registry()
                .list_tools("loca-extension-builtin")
                .await
                .unwrap()[0]
                .enabled,
            "the starter set is default-on; runtime prompts still gate what it does"
        );
    }

    // --- execution ----------------------------------------------------------

    #[tokio::test]
    async fn a_core_with_no_worker_pool_refuses_a_tool_call_rather_than_hanging() {
        // The pool is an optional seam, and every OTHER absent seam degrades to
        // an empty answer. This one must not: a call that arrives here means
        // the catalog offered a tool this core cannot run, and answering
        // `server_unavailable` is what turns that into something a user sees
        // instead of a turn that waits out the call budget.
        let env = env();
        let (events, _events_rx) = tokio::sync::mpsc::unbounded_channel();
        let err = env
            .ext
            .start_tool_call(
                crate::extensions::worker::call::ToolCallStart {
                    extension_id: "acme".to_string(),
                    tool_name: "t".to_string(),
                    fn_export: "run".to_string(),
                    arguments_json: "{}".to_string(),
                    chat_context: crate::extensions::worker::protocol::ChatContext {
                        user_message: "do the thing".to_string(),
                        session_id: None,
                        locale: None,
                    },
                },
                events,
            )
            .await
            .err()
            .expect("a core with no pool started a tool call");
        assert_eq!(err.code, ErrorCode::ServerUnavailable);
    }

    #[test]
    fn the_handle_validates_tool_arguments_against_the_authors_schema() {
        // The delegation, not the validator: `args.rs` has its own tests and
        // they would all pass against a handle that never called it. The cache
        // lives here so a call whose arguments are wrong costs no process.
        let env = env();
        let mut tool = crate::contract::api::Tool {
            always_available: false,
            description: "d".to_string(),
            enabled: true,
            extension_id: "acme".to_string(),
            fn_export: "run".to_string(),
            grants: Vec::new(),
            id: "acme::t".to_string(),
            missing_required: Vec::new(),
            name: "t".to_string(),
            parameters: serde_json::Map::new(),
            platforms: Vec::new(),
            provider_kind: crate::contract::api::ToolProviderKind::Extension,
            provider_name: None,
            required_permissions: Vec::new(),
            triggers: Vec::new(),
        };
        tool.parameters = serde_json::from_value(json!({
            "type": "object",
            "properties": { "count": { "type": "number", "default": 7 } },
            "required": ["name"],
        }))
        .unwrap();

        let normalized = env
            .ext
            .validate_tool_args(&tool, r#"{"name":"x"}"#)
            .expect("valid arguments were refused");
        assert_eq!(
            serde_json::from_str::<Value>(&normalized).unwrap(),
            json!({ "name": "x", "count": 7 }),
            "the schema default was not applied"
        );
        assert_eq!(
            env.ext
                .validate_tool_args(&tool, "{}")
                .expect_err("a missing required argument was accepted")
                .code,
            ErrorCode::ValidationError
        );
    }

    // --- query parsing ------------------------------------------------------

    #[test]
    fn a_junk_page_size_falls_back_rather_than_failing_the_request() {
        // `Number(v)` on the oracle, so a non-finite result is the default.
        assert_eq!(num_query(None, 50), 50);
        assert_eq!(num_query(Some(""), 50), 50);
        assert_eq!(num_query(Some("abc"), 50), 50);
        assert_eq!(num_query(Some("-1"), 50), 50);
        assert_eq!(num_query(Some("25"), 50), 25);
        assert_eq!(num_query(Some("0"), 50), 0);
    }
}
