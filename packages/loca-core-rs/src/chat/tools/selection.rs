//! Which tools a hop is offered: the LLM-exposure gate, the relevance pipeline
//! (cosine phase 1, optional LLM phase 2), the `tools.maxTools` cap and its two
//! exemptions, and the MCP offering that runs after all of it.
//!
//! Four things are load-bearing, and each reads like an inconsistency:
//!
//! 1. **The final exposure gate runs again after the map back through the
//!    catalog.** The relevance index is not grant-aware, so a tool whose
//!    permission the user has explicitly denied can be RANKED. Re-applying
//!    `enabled && exposable` here is a security gate, not a tidy-up.
//! 2. **The MCP append runs OUTSIDE the failure guard.** An embedding failure
//!    errors the bubble and drops every extension tool, and MCP tools are still
//!    offered, because they were never in the embedding index to begin with.
//! 3. **`tools_sent` is counted AFTER the MCP append.** An MCP-only turn would
//!    otherwise report zero and suppress the system prompt's tools hint while
//!    the model really did get tools.
//! 4. **Name-matched and always-available tools are exempt from the cap.**
//!    Naming a tool by hand always exposes it, whatever the relevance says.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use crate::contract::api::{PermissionDecl, Tool, ToolDescriptor};
use crate::extensions::sandbox::permission_key;
use crate::llm::{thinking_budget, LlmEndpointConfig, ToolDef};
use crate::memories::relevance::{cosine_normalized, tool_embed_text, tool_name_mentioned};
use crate::memories::Embedding;
use crate::sessions::doc::StoredMessage;
use crate::settings::access::{bool_setting, num_setting};
use crate::settings::SettingsRecord;

use crate::chat::deps::{Phase1Options, ToolHost};
use crate::chat::writer::LiveRef;

/// Cosine cutoff for folding a non-always-available MCP tool into a turn. An
/// MCP tool carries no trigger phrases, so it is offered when the query names
/// it or its name-plus-description embedding is at least this similar.
/// Conservative: the failure guarded against is an irrelevant server tool
/// force-included on every turn.
const MCP_RELEVANCE_FLOOR: f64 = 0.25;

/// What one turn's selection produced.
pub struct ToolSelection {
    pub tools: Vec<ToolDef>,
    /// The query embedding the filter computed, reused by the memory relevance
    /// injection so a turn embeds its query at most once. `None` when the filter
    /// did not run: tools off, no query, or filtering skipped.
    pub query_vector: Option<Vec<f32>>,
    /// The `tool_filter` bubble, for the caller to finalize so it lands at the
    /// turn's cursor. `None` when the tool path is gated off entirely: a
    /// no-tools turn shows no bubble at all.
    pub filter_message: Option<LiveRef>,
}

/// Everything the selection reads that is not a store.
pub struct SelectionInputs<'a> {
    pub settings: &'a SettingsRecord,
    /// The turn's own endpoint, for the phase-2 pass. Already resolved by the
    /// caller, which is what keeps tool filtering on the same provider the turn
    /// streams from.
    pub endpoint: &'a LlmEndpointConfig,
    pub stream_id: &'a str,
    pub cancel: &'a CancellationToken,
    pub query_text: Option<&'a str>,
}

/// A tool reaches the model only when its extension is installed, the tool is
/// enabled, AND no non-optional required permission is explicitly denied. A
/// permission with no grant row behaves as "ask", so an unanswered prompt does
/// not silently shrink the catalog.
pub fn tool_exposable(tool: &Tool) -> bool {
    let denied: HashSet<&str> = tool
        .grants
        .iter()
        .filter(|g| matches!(g.state, crate::contract::api::GrantState::Denied))
        .map(|g| g.permission_key.as_str())
        .collect();
    tool.required_permissions
        .iter()
        .all(|decl| is_optional(decl) || !denied.contains(permission_key(decl).as_str()))
}

/// Every variant carries `optional`, so one or-pattern covers them and a new
/// permission kind is a compile error rather than a silent "not optional".
fn is_optional(decl: &PermissionDecl) -> bool {
    match decl {
        PermissionDecl::Net { optional, .. }
        | PermissionDecl::Read { optional, .. }
        | PermissionDecl::Write { optional, .. }
        | PermissionDecl::Run { optional, .. }
        | PermissionDecl::Env { optional, .. }
        | PermissionDecl::Ffi { optional, .. }
        | PermissionDecl::Sys { optional, .. }
        | PermissionDecl::Memories { optional, .. }
        | PermissionDecl::Llm { optional, .. }
        | PermissionDecl::Tts { optional, .. }
        | PermissionDecl::Stt { optional, .. } => optional.unwrap_or(false),
    }
}

/// Every enabled and exposable extension tool.
pub async fn list_enabled_tools(host: &dyn ToolHost) -> Vec<Tool> {
    host.installed_extension_tools()
        .await
        .into_iter()
        .filter(|t| t.enabled && tool_exposable(t))
        .collect()
}

/// Tool name to its owner, for the muxer's call resolution.
///
/// Extension tools win a name collision because they are inserted first, which
/// matches the offering order: an MCP entry of the same name is skipped by the
/// append below, so dispatch would have nothing to route to anyway.
pub async fn enabled_tools_by_name(host: &dyn ToolHost) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for tool in host.installed_extension_tools().await {
        if tool.enabled && tool_exposable(&tool) && seen.insert(tool.name.clone()) {
            out.push((tool.name, tool.extension_id));
        }
    }
    for tool in host.list_mcp_tools().await {
        if tool.enabled && seen.insert(tool.name.clone()) {
            out.push((tool.name, tool.extension_id));
        }
    }
    out
}

/// Build the provider tool list for one turn.
pub async fn build_tool_list(
    host: &dyn ToolHost,
    embedding: &Embedding,
    inputs: SelectionInputs<'_>,
) -> ToolSelection {
    let settings = inputs.settings;
    if !bool_setting(settings, "tools.enabled", false) {
        return ToolSelection {
            tools: Vec::new(),
            query_vector: None,
            filter_message: None,
        };
    }

    // The bubble is emitted only once the pipeline reaches an end state
    // (complete or error). The in-flight period is covered client-side by the
    // synthetic loading bubble, which renders until the first model delta.
    let mut bubble = new_filter_message();
    let mut tools: Vec<ToolDef> = Vec::new();
    let mut query_vector: Option<Vec<f32>> = None;

    match select_extension_tools(host, embedding, &inputs, &mut bubble).await {
        Ok((selected, vector)) => {
            tools = selected;
            query_vector = vector;
        }
        Err(err) => {
            // Non-fatal: the embedding model is missing, or the catalog read
            // failed. The turn runs with no extension tools rather than not at
            // all, and the bubble says why.
            tracing::warn!(
                target: "loca::chat",
                "stream {}: tool filter failed, skipping tools: {err}",
                inputs.stream_id,
            );
            set(&mut bubble, "status", Value::String("error".into()));
            set(&mut bubble, "errorMessage", Value::String(err.to_string()));
        }
    }

    // Deliberately outside the guard above: MCP tools live on their servers and
    // were never in the embedding index, so an embedding failure has no bearing
    // on whether they can be offered.
    append_mcp_tools(
        host,
        embedding,
        &inputs,
        query_vector.as_deref(),
        &mut tools,
        &mut bubble,
    )
    .await;

    // After the append, so an MCP-only turn does not report zero.
    set(&mut bubble, "toolsSent", Value::from(tools.len()));

    ToolSelection {
        tools,
        query_vector,
        filter_message: Some(Arc::new(Mutex::new(StoredMessage::new(bubble)))),
    }
}

/// The extension half: the whole thing the TypeScript wraps in one try/catch.
async fn select_extension_tools(
    host: &dyn ToolHost,
    embedding: &Embedding,
    inputs: &SelectionInputs<'_>,
    bubble: &mut Map<String, Value>,
) -> crate::error::Result<(Vec<ToolDef>, Option<Vec<f32>>)> {
    let settings = inputs.settings;
    let all_enabled = list_enabled_tools(host).await;
    let total = all_enabled.len();
    let filtering_enabled = bool_setting(settings, "tools.filteringEnabled", true);
    let min_tools_to_filter = num_setting(settings, "tools.filteringMinTools", 0.0) as usize;
    let max_tools = num_setting(settings, "tools.maxTools", 30.0) as usize;
    let always_available_enabled = bool_setting(settings, "tools.alwaysAvailableEnabled", true);
    let second_pass_enabled = bool_setting(settings, "tools.secondPassEnabled", true);

    // Skip the filter when it is off, when there is nothing to filter, or when
    // the catalog is below the user's minimum. Zero means "always filter".
    let should_filter = filtering_enabled
        && total > 0
        && (min_tools_to_filter == 0 || total >= min_tools_to_filter);

    let mut query_vector: Option<Vec<f32>> = None;
    let mut phase1_entries: Vec<ToolDescriptor> = Vec::new();
    let mut phase2_entries: Option<Vec<ToolDescriptor>> = None;
    let mut always_entries: Vec<ToolDescriptor> = Vec::new();
    let mut name_matched_entries: Vec<ToolDescriptor> = Vec::new();

    let chosen: Vec<String> = match inputs.query_text.filter(|_| should_filter) {
        None => {
            let mut capped: Vec<String> = all_enabled
                .iter()
                .take(max_tools)
                .map(|t| t.id.clone())
                .collect();
            // The cap can cut off always-available tools that sort past it.
            // Append them so their exposure does not depend on the filter
            // toggle, matching the filtered branch below.
            if always_available_enabled {
                let mut seen: HashSet<String> = capped.iter().cloned().collect();
                for tool in &all_enabled {
                    if tool.always_available && seen.insert(tool.id.clone()) {
                        capped.push(tool.id.clone());
                    }
                }
            }
            capped
        }
        Some(query_text) => {
            let vector = embedding
                .embed(&[query_text.to_string()])
                .await?
                .into_iter()
                .next()
                .unwrap_or_default();
            let result = host
                .filter_phase1(
                    &vector,
                    Phase1Options {
                        top_k: max_tools,
                        include_always_available: always_available_enabled,
                        query_text: Some(query_text.to_string()),
                    },
                )
                .await;
            query_vector = Some(vector);
            phase1_entries = result.candidates.clone();
            always_entries = if always_available_enabled {
                result.always_available
            } else {
                Vec::new()
            };
            name_matched_entries = result.name_matched;
            let mut candidates = result.candidates;
            if second_pass_enabled && !candidates.is_empty() {
                let budget = thinking_budget(settings, "tools.filterThinkingBudget");
                let phase2 =
                    host.filter_phase2(query_text, candidates.clone(), inputs.endpoint, budget);
                // The host's phase 2 already fails open on a provider error, and
                // an interrupt is the same kind of failure: the TypeScript's
                // aborted request lands in that same catch and returns the
                // phase-1 set. Selecting here just stops the wait.
                candidates = tokio::select! {
                    biased;
                    _ = inputs.cancel.cancelled() => candidates,
                    kept = phase2 => kept,
                };
                phase2_entries = Some(candidates.clone());
            }

            let mut final_ids: Vec<String> = Vec::new();
            let mut seen: HashSet<String> = HashSet::new();
            // Directly-named tools are force-included ahead of the cosine
            // candidates and exempt from the cap, so naming a tool by hand
            // always exposes it even with no fresh embedding.
            for entry in &name_matched_entries {
                if seen.insert(entry.tool_id.clone()) {
                    final_ids.push(entry.tool_id.clone());
                }
            }
            for entry in &candidates {
                if final_ids.len() >= max_tools {
                    break;
                }
                if seen.insert(entry.tool_id.clone()) {
                    final_ids.push(entry.tool_id.clone());
                }
            }
            for entry in &always_entries {
                if seen.insert(entry.tool_id.clone()) {
                    final_ids.push(entry.tool_id.clone());
                }
            }
            final_ids
        }
    };

    let mut tools: Vec<ToolDef> = Vec::new();
    for tool_id in &chosen {
        let Some(tool) = host.get_tool(tool_id).await else {
            continue;
        };
        // The security gate. The relevance index is not grant-aware, so this is
        // the only thing standing between a denied permission and the model
        // being offered the tool it guards.
        if !tool.enabled || !tool_exposable(&tool) {
            continue;
        }
        tools.push(ToolDef::function(
            tool.name.clone(),
            tool.description.clone(),
            Value::Object(tool.parameters.clone()),
        ));
    }

    set(bubble, "phase1", phase1_scores(&phase1_entries));
    if let Some(phase2) = &phase2_entries {
        set(bubble, "phase2", entries(phase2));
    }
    set(bubble, "alwaysAvailable", entries(&always_entries));
    set(bubble, "nameMatched", entries(&name_matched_entries));

    Ok((tools, query_vector))
}

/// Fold the enabled MCP tools into the offering.
///
/// An MCP tool defaults to always-available. With that turned off it folds into
/// relevance instead: no trigger phrases, so it is offered when the query names
/// it or its embedding clears the floor.
async fn append_mcp_tools(
    host: &dyn ToolHost,
    embedding: &Embedding,
    inputs: &SelectionInputs<'_>,
    query_vector: Option<&[f32]>,
    tools: &mut Vec<ToolDef>,
    bubble: &mut Map<String, Value>,
) {
    let settings = inputs.settings;
    let max_tools = num_setting(settings, "tools.maxTools", 30.0) as usize;
    let always_available_enabled = bool_setting(settings, "tools.alwaysAvailableEnabled", true);
    let mut offered: HashSet<String> = tools.iter().map(|t| t.function.name.clone()).collect();
    let mut mcp_entries: Vec<Value> = Vec::new();

    for tool in host.list_mcp_tools().await {
        if !tool.enabled || offered.contains(&tool.name) {
            continue;
        }
        // Bounded so a server with many enabled tools cannot blow the context
        // budget on its own.
        if mcp_entries.len() >= max_tools {
            break;
        }
        let is_always = tool.always_available && always_available_enabled;
        // With no query vector (filtering off or skipped) everything is offered,
        // matching the extension path.
        let mut include = is_always || query_vector.is_none();
        if !include {
            if let Some(query_vector) = query_vector {
                if tool_name_mentioned(inputs.query_text.unwrap_or_default(), &tool.name) {
                    include = true;
                } else {
                    let text = tool_embed_text(&tool.name, &tool.description, &tool.triggers);
                    match embedding.embed(&[text]).await {
                        Ok(vectors) => {
                            let vector = vectors.into_iter().next().unwrap_or_default();
                            include = cosine_normalized(query_vector, &vector)
                                .map(|score| score >= MCP_RELEVANCE_FLOOR)
                                .unwrap_or(false);
                        }
                        // The embedding model is unavailable, so the tool falls
                        // back to the name mention already checked above and is
                        // simply not offered this turn.
                        Err(err) => tracing::warn!(
                            target: "loca::chat",
                            "stream {}: MCP tool embed failed for {}: {err}",
                            inputs.stream_id, tool.name,
                        ),
                    }
                }
            }
        }
        if !include {
            continue;
        }
        offered.insert(tool.name.clone());
        mcp_entries.push(entry(&tool.id, &tool.name, &tool.description));
        tools.push(ToolDef::function(
            tool.name.clone(),
            tool.description.clone(),
            Value::Object(tool.parameters.clone()),
        ));
    }

    set(bubble, "mcp", Value::Array(mcp_entries));
}

fn new_filter_message() -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("id".into(), Value::String(ulid::Ulid::new().to_string()));
    map.insert("ord".into(), Value::from(-1));
    map.insert("role".into(), Value::String("tool_filter".into()));
    map.insert("status".into(), Value::String("complete".into()));
    map.insert("createdAtMs".into(), Value::from(now_ms()));
    map
}

fn set(bubble: &mut Map<String, Value>, key: &str, value: Value) {
    bubble.insert(key.to_string(), value);
}

fn entry(tool_id: &str, name: &str, description: &str) -> Value {
    let mut map = Map::new();
    map.insert("toolId".into(), Value::String(tool_id.to_string()));
    map.insert("name".into(), Value::String(name.to_string()));
    map.insert("description".into(), Value::String(description.to_string()));
    Value::Object(map)
}

fn entries(descriptors: &[ToolDescriptor]) -> Value {
    Value::Array(
        descriptors
            .iter()
            .map(|d| entry(&d.tool_id, &d.name, &d.description))
            .collect(),
    )
}

fn phase1_scores(descriptors: &[ToolDescriptor]) -> Value {
    Value::Array(
        descriptors
            .iter()
            .map(|d| {
                let Value::Object(mut map) = entry(&d.tool_id, &d.name, &d.description) else {
                    return Value::Null;
                };
                map.insert("score".into(), Value::from(d.similarity.unwrap_or(0.0)));
                Value::Object(map)
            })
            .collect(),
    )
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}
