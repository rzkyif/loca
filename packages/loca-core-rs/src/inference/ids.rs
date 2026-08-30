//! Tool-call ids for the calls llama.cpp does not give one. Upstream's id
//! generator lives in `llama-server`, which we do not build, so most templates
//! yield an empty id; an empty `tool_call_id` renders as an orphaned tool
//! response and garbles every hop after it. Fragments of one call share an
//! index, so the id is minted once per index and remembered for the turn.

use std::collections::HashMap;

/// The ids assigned during one turn; indices restart per turn.
#[derive(Debug, Default)]
pub struct ToolCallIds {
    assigned: HashMap<u32, String>,
}

impl ToolCallIds {
    pub fn new() -> Self {
        Self::default()
    }

    /// The id for `index`, minting one on first sight. A non-empty `parsed` id
    /// is authoritative and remembered, so a partial parse that later loses it
    /// still resolves to the same value.
    pub fn id_for(&mut self, index: u32, parsed: &str) -> String {
        if !parsed.is_empty() {
            return self
                .assigned
                .entry(index)
                .or_insert_with(|| parsed.to_string())
                .clone();
        }
        self.assigned.entry(index).or_insert_with(generate).clone()
    }
}

/// Uuid rather than a counter: ids reach the persisted transcript, and a
/// per-turn counter would collide across turns of one session.
fn generate() -> String {
    format!("call_{}", uuid::Uuid::new_v4().simple())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn an_index_keeps_one_id_across_fragments() {
        let mut ids = ToolCallIds::new();
        let first = ids.id_for(0, "");
        assert_eq!(ids.id_for(0, ""), first);
        assert_eq!(ids.id_for(0, ""), first);
    }

    #[test]
    fn different_indices_get_different_ids() {
        let mut ids = ToolCallIds::new();
        assert_ne!(ids.id_for(0, ""), ids.id_for(1, ""));
    }

    #[test]
    fn a_generated_id_is_not_empty() {
        let mut ids = ToolCallIds::new();
        assert!(!ids.id_for(0, "").is_empty());
    }

    #[test]
    fn a_model_supplied_id_wins_and_is_then_remembered() {
        let mut ids = ToolCallIds::new();
        assert_eq!(ids.id_for(0, "call_from_model"), "call_from_model");
        assert_eq!(ids.id_for(0, ""), "call_from_model");
    }

    #[test]
    fn a_model_supplied_id_does_not_overwrite_one_already_assigned() {
        let mut ids = ToolCallIds::new();
        let minted = ids.id_for(0, "");
        assert_eq!(ids.id_for(0, "call_late"), minted);
    }

    #[test]
    fn two_turns_do_not_share_ids() {
        let mut first = ToolCallIds::new();
        let mut second = ToolCallIds::new();
        assert_ne!(first.id_for(0, ""), second.id_for(0, ""));
    }
}
