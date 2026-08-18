//! Tool relevance: which of this core's tools a turn is allowed to see.
//!
//! Ports `core-engine/src/services/tool-filter.ts`, which appears in no registry
//! row and was assigned here at D12's plan gate (Q8). The reason it lands in
//! this package rather than in D5's is that the tool catalog and the per-tool
//! embeddings are D12's tables; the scoring primitives themselves are D5's and
//! are called, not copied.
//!
//! Two passes, and they fail in opposite directions on purpose:
//!
//! - **Phase 1** is cosine similarity over the persisted per-tool embeddings,
//!   plus the two sets that bypass ranking entirely. A tool it cannot score is
//!   skipped, so a failure here NARROWS.
//! - **Phase 2** shows the model the query and the phase-1 candidates and asks
//!   which are plausibly useful. Every failure path returns its input, so a
//!   failure here does not narrow. That asymmetry is the whole design: an
//!   unreachable provider must not silently leave a turn with no tools.
//!
//! # Phase 2's fail-open is why it needs a test rather than a glance
//!
//! A phase 2 that returned its input unconditionally is indistinguishable at
//! runtime from one that works and approves everything: the model still gets
//! tools, every turn still completes, and nothing anywhere goes red. So the
//! tests below pin the SUBSET it selects, not merely that it answered.

use std::collections::BTreeSet;

use async_trait::async_trait;

use crate::contract::api::{Tool, ToolDescriptor};
use crate::error::Result;

use super::wire::Phase1Response;
use super::Extensions;

/// The candidate-listing completion phase 2 makes, as this module describes it.
///
/// A request rather than a set of arguments because everything in it is a
/// decision this module owns: the prompt wording, the temperature, and how much
/// room the answer gets. The implementation resolves an endpoint and runs it,
/// and knows none of that.
#[derive(Debug, Clone, PartialEq)]
pub struct Phase2Request {
    pub system_prompt: String,
    pub user_message: String,
    /// Zero. A filter that varies its answer across identical turns makes the
    /// tool surface of a conversation unpredictable, which reads to a user as
    /// the model forgetting a tool it just used.
    pub temperature: f64,
    /// The short answer allowance plus whatever thinking was asked for. The
    /// answer is a comma-separated list of numbers; the budget is what a user
    /// spent on `tools.filterThinkingBudget`.
    pub max_tokens: u32,
    pub reasoning_budget: u32,
}

/// The completion phase 2 runs, behind a seam this module declares.
///
/// Consumer declares the trait, per the crate's idiom, and here it also keeps
/// `extensions::` from importing `llm::` at all: the module that gates what a
/// tool may do has no business naming a chat provider, and the one place that
/// legitimately knows about both is the `ToolHost` adapter.
///
/// `Err` fails OPEN in [`phase2`], so an implementation reports rather than
/// papers over.
#[async_trait]
pub trait Phase2Completion: Send + Sync {
    async fn complete(&self, req: Phase2Request) -> Result<String>;
}

/// The answer allowance before the user's thinking budget is added.
const PHASE2_ANSWER_TOKENS: u32 = 64;

const PHASE2_SYSTEM_PROMPT: &str =
    "You filter a candidate tool list down to those plausibly useful \
for the user's query. Be inclusive when uncertain. Respond with \
ONLY a comma-separated list of tool numbers (e.g. `1,3,5`) or the \
literal word `none`.";

/// The default candidate count. Always-available tools are added ON TOP of it,
/// so the effective list the model sees can exceed it.
pub const DEFAULT_TOP_K: usize = 10;

impl Extensions {
    /// The most similar enabled tools, plus the sets that bypass ranking.
    ///
    /// A tool with no stored embedding is skipped silently, and so is one whose
    /// stored vector was produced by a DIFFERENT model: a stale source hash
    /// means the vector is not comparable to the query vector, so scoring it
    /// would mis-rank rather than under-rank. Both read as "not embedded" until
    /// `POST /extensions/reindex` backfills them.
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
    /// control.
    ///
    /// The global "allow always-available tools" toggle rides in here. With it
    /// off an always-available tool does not vanish: it falls through and is
    /// ranked like any other, which is what the setting means.
    pub async fn phase1_with(
        &self,
        query_vector: &[f32],
        top_k: Option<usize>,
        query_text: Option<&str>,
        include_always_available: bool,
    ) -> Result<Phase1Response> {
        let top_k = top_k.unwrap_or(DEFAULT_TOP_K);
        let tools = self.enabled_tools().await?;
        // One batched read rather than a round trip per tool. With eight warm
        // extensions and a few dozen tools the per-tool form is the difference
        // between one query and forty on every single chat turn.
        let embeddings = self.registry().load_all_embeddings().await?;

        let mut scored: Vec<(Tool, f64)> = Vec::new();
        let mut always = Vec::new();
        let mut name_matched = Vec::new();

        for tool in tools {
            // An always-available tool is offered every turn, so it bypasses
            // the relevance pipeline entirely: no cosine score, no candidate
            // slot, no phase-2 pass.
            if tool.always_available && include_always_available {
                always.push(descriptor(&tool, None));
                continue;
            }
            // Checked BEFORE the embedding gate, so a tool the user named by
            // hand is force-included even when it has no fresh embedding yet.
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
            let Ok(score) =
                crate::memories::relevance::cosine_normalized(query_vector, &stored.vector)
            else {
                // A dimension mismatch is the same situation as a stale hash:
                // the vector belongs to another model. Skipping is what keeps
                // one bad row from failing the whole turn's tool selection.
                continue;
            };
            scored.push((tool, score));
        }

        // Descending, and a stable sort so two tools with identical scores keep
        // the catalog's order rather than shuffling per call.
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

/// Phase 2: ask the model which candidates are plausibly useful.
///
/// Always-available tools are NOT passed through here; the caller appends them
/// afterwards, because a tool the user marked always-available has already been
/// decided about.
///
/// **Every failure returns `candidates` unchanged**, and the list of failures is
/// the point: no provider, a refused request, a stream that dies mid-answer, a
/// reply in prose rather than numbers. The one answer that legitimately empties
/// the list is the model saying so, in the one word it was told to use.
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

/// What the model's answer resolved to.
///
/// Three outcomes rather than two, because "none" and "I could not read that"
/// must not collapse together: the first empties the tool list on the model's
/// say-so and the second must not empty anything.
#[derive(Debug, PartialEq, Eq)]
enum Phase2Picks {
    None,
    Picked(BTreeSet<usize>),
    Unparseable,
}

/// Zero-based indices, in ascending order and de-duplicated.
///
/// **An EMPTY answer reads as `none`**, which is the one place this pass does
/// not fail open: a provider that returns no content at all (some refuse that
/// way) empties the turn's tool list rather than falling back. Ported as the
/// specification has it (`tool-filter.ts:151`) rather than corrected, because
/// changing it would diverge the two cores on which tools a turn sees.
fn parse_phase2(answer: &str, count: usize) -> Phase2Picks {
    let text = answer.trim().to_lowercase();
    if text == "none" || text.is_empty() {
        return Phase2Picks::None;
    }
    let picks: BTreeSet<usize> = text
        .split([',', ' ', '\t', '\n', '\r'])
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<usize>().ok())
        // One-based on the wire, because that is what the model was shown, and
        // out of range is dropped rather than clamped: a model naming tool 9 of
        // 3 has not selected the last one.
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

    /// A model that says whatever the test told it to, and records what it was
    /// asked.
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

    /// A model that must never be reached.
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
        // The one assertion that distinguishes a working phase 2 from one that
        // approves everything. Everything else here is a failure path, and
        // every failure path returns the input, so without this test the whole
        // module could be `return candidates`.
        let llm = Scripted::saying("1,3");
        let picked = phase2(&llm, "send a file", three(), 0).await;
        assert_eq!(names(&picked), vec!["read", "send"]);
    }

    #[tokio::test]
    async fn the_candidates_are_numbered_from_one_in_the_prompt() {
        // The numbering IS the protocol: the model answers with the numbers it
        // was shown, so an off-by-one here silently selects the neighbouring
        // tool of everything the model picked.
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
        // A filter that varies across identical turns makes a conversation's
        // tool surface unpredictable.
        assert_eq!(asked.temperature, 0.0);

        let llm = Scripted::saying("1");
        phase2(&llm, "q", three(), 0).await;
        assert_eq!(llm.request().unwrap().max_tokens, 64);
    }

    #[tokio::test]
    async fn the_word_none_and_an_empty_answer_both_empty_the_list() {
        // "none" is the model saying so, in the one word it was told to use.
        let llm = Scripted::saying("  NONE\n");
        assert!(phase2(&llm, "q", three(), 0).await.is_empty());

        // An EMPTY answer empties it too, which is the specification's
        // behaviour and the one place this function does not fail open. It is
        // ported rather than corrected: see the note at `parse_phase2`.
        let llm = Scripted::saying("");
        assert!(phase2(&llm, "q", three(), 0).await.is_empty());
    }

    #[tokio::test]
    async fn every_other_unusable_answer_falls_back_to_the_whole_candidate_set() {
        // Fail OPEN, and the reason is that the alternative is invisible: a
        // turn with no tools looks exactly like a turn that needed none.
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
        // A completion per turn for a list of nothing, on a core with no
        // extensions installed, which is most of them.
        let picked = phase2(&Unreachable, "q", Vec::new(), 0).await;
        assert!(picked.is_empty());
    }

    #[test]
    fn the_picks_are_deduplicated_and_ordered_by_position_not_by_the_answer() {
        // The model's order is not a ranking: the candidates were already
        // ranked by cosine, and re-ordering them here would silently override
        // phase 1 with whatever order the model happened to type.
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
