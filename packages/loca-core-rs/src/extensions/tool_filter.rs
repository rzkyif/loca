//! Tool relevance: which of this core's tools a turn is allowed to see.
//!
//! Two passes that fail in opposite directions on purpose. Phase 1 is cosine
//! similarity over the persisted per-tool embeddings plus the two bypass sets;
//! a tool it cannot score is skipped, so a failure NARROWS. Phase 2 asks the
//! model which phase-1 candidates are plausibly useful; every failure path
//! returns its input, so an unreachable provider never leaves a turn with no
//! tools. The tests pin the SUBSET phase 2 selects, since an unconditional
//! passthrough is indistinguishable at runtime from a working filter.

use std::collections::BTreeSet;

use async_trait::async_trait;

use crate::contract::api::{Tool, ToolDescriptor};
use crate::error::Result;

use super::wire::Phase1Response;
use super::Extensions;

/// The candidate-listing completion phase 2 makes. Everything in it is a
/// decision this module owns; the implementation only resolves an endpoint.
#[derive(Debug, Clone, PartialEq)]
pub struct Phase2Request {
    pub system_prompt: String,
    pub user_message: String,
    /// Zero: a filter that varies across identical turns reads to a user as the
    /// model forgetting a tool it just used.
    pub temperature: f64,
    /// The short answer allowance plus the user's `tools.filterThinkingBudget`.
    pub max_tokens: u32,
    pub reasoning_budget: u32,
}

/// The completion phase 2 runs. Declared here so `extensions::` never imports
/// `llm::`. `Err` fails OPEN in [`phase2`].
#[async_trait]
pub trait Phase2Completion: Send + Sync {
    async fn complete(&self, req: Phase2Request) -> Result<String>;
}

const PHASE2_ANSWER_TOKENS: u32 = 64;

const PHASE2_SYSTEM_PROMPT: &str =
    "You filter a candidate tool list down to those plausibly useful \
for the user's query. Be inclusive when uncertain. Respond with \
ONLY a comma-separated list of tool numbers (e.g. `1,3,5`) or the \
literal word `none`.";

/// The default candidate count. Always-available tools are added ON TOP.
pub const DEFAULT_TOP_K: usize = 10;

impl Extensions {
    /// The most similar enabled tools, plus the sets that bypass ranking. A
    /// tool with no stored embedding, or one embedded by a DIFFERENT model
    /// (stale source hash), is skipped until `POST /extensions/reindex`.
    pub async fn phase1(
        &self,
        query_vector: &[f32],
        top_k: Option<usize>,
        query_text: Option<&str>,
    ) -> Result<Phase1Response> {
        self.phase1_with(query_vector, top_k, query_text, true)
            .await
    }

    /// [`Self::phase1`] with the always-available bypass under the caller's
    /// control. With it off an always-available tool is ranked like any other.
    pub async fn phase1_with(
        &self,
        query_vector: &[f32],
        top_k: Option<usize>,
        query_text: Option<&str>,
        include_always_available: bool,
    ) -> Result<Phase1Response> {
        let top_k = top_k.unwrap_or(DEFAULT_TOP_K);
        let tools = self.enabled_tools().await?;
        // One batched read rather than a round trip per tool on every turn.
        let embeddings = self.registry().load_all_embeddings().await?;

        let mut scored: Vec<(Tool, f64)> = Vec::new();
        let mut always = Vec::new();
        let mut name_matched = Vec::new();

        for tool in tools {
            if tool.always_available && include_always_available {
                always.push(descriptor(&tool, None));
                continue;
            }
            // Before the embedding gate, so a tool the user named by hand is
            // force-included even with no fresh embedding.
            if let Some(query) = query_text {
                if crate::memories::relevance::tool_name_mentioned(query, &tool.name) {
                    name_matched.push(descriptor(&tool, None));
                }
            }
            let Some(stored) = embeddings.get(&tool.id) else {
                continue;
            };
            if let Some(embedder) = self.embedder() {
                let text = crate::memories::relevance::tool_embed_text(
                    &tool.name,
                    &tool.description,
                    &tool.triggers,
                );
                if stored.source_hash != embedder.source_hash(&text) {
                    continue;
                }
            }
            // A dimension mismatch means the vector belongs to another model.
            let Ok(score) =
                crate::memories::relevance::cosine_normalized(query_vector, &stored.vector)
            else {
                continue;
            };
            scored.push((tool, score));
        }

        // Descending, stable: identical scores keep the catalog's order.
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        let candidates = scored
            .into_iter()
            .take(top_k)
            .map(|(tool, score)| descriptor(&tool, Some(score)))
            .collect();

        Ok(Phase1Response {
            candidates,
            always_available: always,
            name_matched,
        })
    }
}

/// Phase 2: ask the model which candidates are plausibly useful. Always-available
/// tools are appended by the caller afterwards. Every failure returns
/// `candidates` unchanged; only the model saying `none` empties the list.
pub async fn phase2(
    llm: &dyn Phase2Completion,
    query: &str,
    candidates: Vec<ToolDescriptor>,
    reasoning_budget: u32,
) -> Vec<ToolDescriptor> {
    if candidates.is_empty() {
        return candidates;
    }
    let listing = candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            format!(
                "{}. {}: {}",
                index + 1,
                candidate.name,
                candidate.description
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let answer = llm
        .complete(Phase2Request {
            system_prompt: PHASE2_SYSTEM_PROMPT.to_string(),
            user_message: format!("User query: {query}\n\nCandidates:\n{listing}"),
            temperature: 0.0,
            max_tokens: PHASE2_ANSWER_TOKENS + reasoning_budget,
            reasoning_budget,
        })
        .await;
    let answer = match answer {
        Ok(answer) => answer,
        Err(err) => {
            tracing::warn!(
                target: "loca::extensions",
                "phase-2 LLM call failed; falling back to the phase-1 candidates: {err}",
            );
            return candidates;
        }
    };

    match parse_phase2(&answer, candidates.len()) {
        Phase2Picks::None => Vec::new(),
        Phase2Picks::Picked(picks) => picks
            .into_iter()
            .filter_map(|index| candidates.get(index).cloned())
            .collect(),
        Phase2Picks::Unparseable => {
            tracing::warn!(
                target: "loca::extensions",
                "phase-2 response unparseable: {:?}; falling back",
                answer.chars().take(80).collect::<String>(),
            );
            candidates
        }
    }
}

/// "none" and "I could not read that" must not collapse together: the first
/// empties the list on the model's say-so and the second must not empty anything.
#[derive(Debug, PartialEq, Eq)]
enum Phase2Picks {
    None,
    Picked(BTreeSet<usize>),
    Unparseable,
}

/// Zero-based indices, ascending and de-duplicated. An EMPTY answer reads as
/// `none`, the one place this pass does not fail open; kept as specified so the
/// two cores agree on which tools a turn sees.
fn parse_phase2(answer: &str, count: usize) -> Phase2Picks {
    let text = answer.trim().to_lowercase();
    if text == "none" || text.is_empty() {
        return Phase2Picks::None;
    }
    let picks: BTreeSet<usize> = text
        .split([',', ' ', '\t', '\n', '\r'])
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<usize>().ok())
        // One-based on the wire; out of range is dropped rather than clamped.
        .filter(|number| *number >= 1 && *number <= count)
        .map(|number| number - 1)
        .collect();
    if picks.is_empty() {
        return Phase2Picks::Unparseable;
    }
    Phase2Picks::Picked(picks)
}

fn descriptor(tool: &Tool, similarity: Option<f64>) -> ToolDescriptor {
    ToolDescriptor {
        tool_id: tool.id.clone(),
        extension_id: tool.extension_id.clone(),
        name: tool.name.clone(),
        description: tool.description.clone(),
        similarity,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Mutex;

    use crate::error::{AppError, ErrorCode};

    use super::*;

    /// Says whatever the test told it to, and records what it was asked.
    struct Scripted {
        /// `None` is a provider that could not answer at all.
        answer: Option<String>,
        seen: Mutex<Option<Phase2Request>>,
    }

    impl Scripted {
        fn saying(answer: &str) -> Self {
            Self {
                answer: Some(answer.to_string()),
                seen: Mutex::new(None),
            }
        }

        fn failing() -> Self {
            Self {
                answer: None,
                seen: Mutex::new(None),
            }
        }

        fn request(&self) -> Option<Phase2Request> {
            self.seen.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Phase2Completion for Scripted {
        async fn complete(&self, req: Phase2Request) -> Result<String> {
            *self.seen.lock().unwrap() = Some(req);
            self.answer
                .clone()
                .ok_or_else(|| AppError::new(ErrorCode::ServerUnavailable, "no provider"))
        }
    }

    struct Unreachable;

    #[async_trait]
    impl Phase2Completion for Unreachable {
        async fn complete(&self, _req: Phase2Request) -> Result<String> {
            panic!("phase 2 called the model when it had nothing to ask about");
        }
    }

    fn candidate(name: &str) -> ToolDescriptor {
        ToolDescriptor {
            tool_id: format!("acme::{name}"),
            extension_id: "acme".to_string(),
            name: name.to_string(),
            description: format!("does {name}"),
            similarity: None,
        }
    }

    fn names(picked: &[ToolDescriptor]) -> Vec<String> {
        picked.iter().map(|c| c.name.clone()).collect()
    }

    fn three() -> Vec<ToolDescriptor> {
        vec![candidate("read"), candidate("write"), candidate("send")]
    }

    #[tokio::test]
    async fn the_model_picks_a_subset_by_number() {
        let llm = Scripted::saying("1,3");
        let picked = phase2(&llm, "send a file", three(), 0).await;
        assert_eq!(names(&picked), vec!["read", "send"]);
    }

    #[tokio::test]
    async fn the_candidates_are_numbered_from_one_in_the_prompt() {
        let llm = Scripted::saying("2");
        let picked = phase2(&llm, "write it down", three(), 0).await;
        assert_eq!(names(&picked), vec!["write"]);

        let asked = llm.request().expect("the model was never asked");
        assert_eq!(
            asked.user_message,
            "User query: write it down\n\nCandidates:\n\
             1. read: does read\n2. write: does write\n3. send: does send"
        );
        assert!(asked
            .system_prompt
            .contains("comma-separated list of tool numbers"));
    }

    #[tokio::test]
    async fn the_answer_allowance_carries_the_users_thinking_budget_on_top() {
        let llm = Scripted::saying("1");
        phase2(&llm, "q", three(), 512).await;
        let asked = llm.request().unwrap();
        assert_eq!(asked.max_tokens, 64 + 512);
        assert_eq!(asked.reasoning_budget, 512);
        assert_eq!(asked.temperature, 0.0);

        let llm = Scripted::saying("1");
        phase2(&llm, "q", three(), 0).await;
        assert_eq!(llm.request().unwrap().max_tokens, 64);
    }

    #[tokio::test]
    async fn the_word_none_and_an_empty_answer_both_empty_the_list() {
        let llm = Scripted::saying("  NONE\n");
        assert!(phase2(&llm, "q", three(), 0).await.is_empty());

        let llm = Scripted::saying("");
        assert!(phase2(&llm, "q", three(), 0).await.is_empty());
    }

    #[tokio::test]
    async fn every_other_unusable_answer_falls_back_to_the_whole_candidate_set() {
        for answer in [
            "I think you should use the read tool!",
            "0",
            "4",
            "-1",
            "1.5",
        ] {
            let llm = Scripted::saying(answer);
            let picked = phase2(&llm, "q", three(), 0).await;
            assert_eq!(
                names(&picked),
                vec!["read", "write", "send"],
                "answer {answer:?} did not fall back"
            );
        }
    }

    #[tokio::test]
    async fn a_provider_failure_falls_back_rather_than_narrowing() {
        let llm = Scripted::failing();
        let picked = phase2(&llm, "q", three(), 0).await;
        assert_eq!(names(&picked), vec!["read", "write", "send"]);
    }

    #[tokio::test]
    async fn an_empty_candidate_set_never_reaches_the_model() {
        let picked = phase2(&Unreachable, "q", Vec::new(), 0).await;
        assert!(picked.is_empty());
    }

    #[test]
    fn the_picks_are_deduplicated_and_ordered_by_position_not_by_the_answer() {
        assert_eq!(
            parse_phase2("3, 1, 3", 3),
            Phase2Picks::Picked(BTreeSet::from([0, 2]))
        );
        assert_eq!(
            parse_phase2("1 2\n3", 3),
            Phase2Picks::Picked(BTreeSet::from([0, 1, 2]))
        );
        assert_eq!(parse_phase2("none", 3), Phase2Picks::None);
        assert_eq!(parse_phase2("", 3), Phase2Picks::None);
        assert_eq!(parse_phase2("   ", 3), Phase2Picks::None);
        assert_eq!(
            parse_phase2("maybe the first one", 3),
            Phase2Picks::Unparseable
        );
    }
}
