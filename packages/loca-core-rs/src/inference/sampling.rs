//! Which samplers run, in what order, with what parameters.
//!
//! Pure: no shim types and no llama.cpp. The gated half turns a [`SamplerPlan`]
//! into `shim::SamplerSpec`, so the decisions here compile and are tested in the
//! DEFAULT gate, which is where a wrong sampler value belongs: it changes what
//! the model writes and nothing fails.
//!
//! # Where the values come from, and the reader that must not exist
//!
//! Every value is already resolved by D3 into `LlmEndpointConfig`, with the
//! catalog defaults applied by `llm/resolver.rs` and reconciled once already
//! (D-072 records what that cost). **Nothing here re-reads settings.** A second
//! reader would be a fourth hop for the D-071 family to hide in: a defaulted
//! value has as many defaults as it has hops, and no single file shows the
//! stack.
//!
//! # The gate that is copied, and the gate that must not be
//!
//! `build_body` gates the llama-only sampler fields on
//! `endpoint.llama_body_extras`, which is URL-derived locality and deliberately
//! NOT `is_local_provider` (D-042). Inside this engine the question does not
//! arise: **if this provider is running, the endpoint IS the local provider**,
//! so every llama field applies unconditionally. [`sampler_plan`] therefore
//! takes the merged values and tests neither flag, and a test below pins that
//! absence, because adding a fourth predicate that looks like "is this local" is
//! how the three that exist came to disagree.
//!
//! # Absent means absent
//!
//! An unset knob is left out of the plan rather than filled with llama.cpp's
//! default, so "not set" and "set to the default" stay distinct all the way to
//! `common_params_sampling`. That is also why `penalty_last_n`, the DRY base,
//! length and window, `typ_p`, `frequency_penalty` and the seed have no field
//! here: none of them is a setting, none is sent to `llama-server` today, and
//! writing llama.cpp's own defaults back in would turn a value upstream
//! maintains into a value this file would have to maintain.
//!
//! # What phase 4 still owes, stated so it is not lost
//!
//! The grammar half of the sampler is pass-through data from the applied turn
//! (`grammar`, `grammar_lazy`, `grammar_triggers`, `preserved_tokens`,
//! `generation_prompt`) and lives in the gated conversion with the rest of the
//! shim types. Two decisions ride with it there: an EMPTY grammar means no
//! grammar sampler at all, and a template-produced grammar is
//! `GrammarKind::ToolCalls`, which is what makes llama.cpp prefill the
//! generation prompt into the grammar sampler. Without that prefill a
//! tool-capable template constrains from the first token and the model is forced
//! to emit a tool call on every single turn.

use crate::llm::LlmRequest;

use super::prompt::Thinking;

/// The sampler knobs for one turn.
///
/// `None` means "leave llama.cpp's own default alone". The one non-optional
/// field is [`Self::reasoning_budget_tokens`], because its "no cap" state is a
/// value rather than an absence: see the field's own note.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SamplerPlan {
    /// The chain ORDER, as the user typed it in `llm.samplers`.
    ///
    /// **Passed through verbatim, not canonicalised.** llama.cpp owns the name
    /// table and derives aliases from it (`temp`, `top-k`, `topk`, `nucleus`,
    /// and the kebab and no-dash form of every canonical name), so a second
    /// table on this side could only ever accept fewer names than the server
    /// this replaces. The setting is free text with a placeholder the user is
    /// invited to copy, so narrowing it is reachable, not theoretical.
    pub samplers: Option<Vec<String>>,
    pub temperature: Option<f32>,
    pub top_k: Option<i32>,
    pub top_p: Option<f32>,
    pub min_p: Option<f32>,
    pub penalty_repeat: Option<f32>,
    /// OpenAI-standard, so it goes to every provider. Zero is its no-op and is
    /// omitted; a NEGATIVE value is legal and is sent.
    pub penalty_present: Option<f32>,
    /// DRY curbs the verbatim phrase loops a token-level repeat penalty misses,
    /// and small local models loop without it. Zero means OFF, which is an
    /// omission rather than a multiplier of zero.
    pub dry_multiplier: Option<f32>,
    /// The `<think>` token cap, or `-1` for no cap.
    ///
    /// `-1` is not the same as omitting the budget. llama.cpp builds the
    /// reasoning-budget sampler when the tags are present and the budget is
    /// non-negative OR a lazy grammar is active, and treats a negative budget as
    /// unlimited (`common/sampling.cpp:310-317`). That sampler is also what
    /// performs think-block suppression for a lazy grammar, which is behaviour
    /// `llama-server` has today because it always holds the template's tags. So
    /// "no cap" has to reach the sampler as a value; dropping the budget
    /// entirely would silently drop the suppression with it.
    pub reasoning_budget_tokens: i32,
}

/// No cap on the reasoning block.
///
/// Negative rather than absent, and negative rather than zero: zero is a cap of
/// zero tokens, which is the opposite instruction.
const REASONING_UNLIMITED: i32 = -1;

/// Build the sampler plan for one turn.
///
/// `thinking` is resolved once, by [`super::prompt::thinking`], and passed in
/// rather than re-derived here. The template flag and the budget sampler are two
/// consumers of one three-level precedence, and deriving it twice is exactly the
/// defect this package was asked to avoid reintroducing.
pub fn sampler_plan(req: &LlmRequest, thinking: Thinking) -> SamplerPlan {
    let endpoint = &req.endpoint;
    SamplerPlan {
        samplers: endpoint
            .samplers
            .as_ref()
            .filter(|names| !names.is_empty())
            .cloned(),
        temperature: req
            .overrides
            .temperature
            .or(endpoint.temperature)
            .map(narrow),
        top_k: endpoint.top_k.map(count),
        top_p: req.overrides.top_p.or(endpoint.top_p).map(narrow),
        min_p: endpoint.min_p.map(narrow),
        penalty_repeat: endpoint.repeat_penalty.map(narrow),
        penalty_present: endpoint.presence_penalty.filter(|v| *v != 0.0).map(narrow),
        dry_multiplier: endpoint.dry_multiplier.filter(|v| *v != 0.0).map(narrow),
        reasoning_budget_tokens: thinking
            .budget
            .map_or(REASONING_UNLIMITED, |b| count(b).max(0)),
    }
}

/// How many tokens this turn may generate, or `None` for no limit beyond the
/// context window.
///
/// Merged by the same precedence as the samplers, and kept beside them because
/// it is the same merge: splitting it into the token loop would put a third
/// copy of "overrides win" in a third file.
pub fn max_tokens(req: &LlmRequest) -> Option<u32> {
    req.overrides.max_tokens.or(req.endpoint.max_tokens)
}

/// llama.cpp's sampler parameters are `float`. The settings carry `f64` because
/// that is what JSON numbers deserialise to, so the narrowing happens once, on
/// the way in, rather than at each use.
#[allow(clippy::cast_possible_truncation)]
fn narrow(value: f64) -> f32 {
    value as f32
}

/// A count on the C side is a signed `int`. Saturating rather than wrapping:
/// a `top_k` above two billion is a broken settings file, and wrapping it to a
/// negative number would mean something entirely different to llama.cpp.
fn count(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::llm::{ChatMessage, LlmEndpointConfig, Overrides};

    /// An endpoint carrying the catalog defaults the resolver applies for a
    /// local provider (`llm/resolver.rs:160-197`), so these tests read against
    /// the values a real turn actually gets.
    fn resolved_endpoint() -> LlmEndpointConfig {
        LlmEndpointConfig {
            model: "local".to_string(),
            is_local_provider: true,
            llama_body_extras: true,
            temperature: Some(0.7),
            top_p: Some(0.95),
            top_k: Some(20),
            min_p: Some(0.0),
            repeat_penalty: Some(1.0),
            dry_multiplier: Some(0.8),
            ..LlmEndpointConfig::default()
        }
    }

    fn request() -> LlmRequest {
        LlmRequest::new(resolved_endpoint(), vec![ChatMessage::user("hi")])
    }

    fn thinking_off() -> Thinking {
        Thinking {
            enabled: false,
            budget: None,
        }
    }

    #[test]
    fn a_per_call_override_beats_the_endpoint_and_absence_leaves_it_alone() {
        // The two knobs the chat path can override per turn. An override that
        // did not win would make every temperature slider in the client inert;
        // an override that won while absent would replace the catalog's own
        // tuning with a hardcoded number.
        let mut req = request();
        req.overrides = Overrides {
            temperature: Some(0.1),
            top_p: Some(0.5),
            max_tokens: Some(64),
            ..Overrides::default()
        };
        let plan = sampler_plan(&req, thinking_off());
        assert_eq!(plan.temperature, Some(0.1));
        assert_eq!(plan.top_p, Some(0.5));
        assert_eq!(max_tokens(&req), Some(64));

        let plain = request();
        let plan = sampler_plan(&plain, thinking_off());
        assert_eq!(plan.temperature, Some(0.7), "the endpoint's own value");
        assert_eq!(plan.top_p, Some(0.95));
        assert_eq!(max_tokens(&plain), None, "no cap beyond the context window");
    }

    #[test]
    fn the_llama_only_samplers_are_not_gated_on_any_locality_predicate() {
        // `build_body` gates these on `llama_body_extras` because it can be
        // pointed at an external endpoint. This engine cannot: if it is running,
        // the endpoint IS the local provider. Re-testing a locality flag here
        // would add a fourth predicate to the three that already look alike and
        // already disagree (D-042), and the failure would be silent: a turn
        // sampled with none of the catalog's tuning, which reads as the model
        // being worse rather than as a bug.
        let mut req = request();
        req.endpoint.llama_body_extras = false;
        req.endpoint.is_local_provider = false;
        let plan = sampler_plan(&req, thinking_off());
        assert_eq!(plan.top_k, Some(20));
        assert_eq!(plan.min_p, Some(0.0));
        assert_eq!(plan.penalty_repeat, Some(1.0));
        assert_eq!(plan.dry_multiplier, Some(0.8));
    }

    #[test]
    fn dry_is_off_at_zero_rather_than_applied_at_zero_strength() {
        // 0 is the setting's "off", and llama.cpp's own default multiplier is
        // non-zero, so sending 0 and omitting the field are opposite
        // instructions: one disables DRY, the other silently enables it.
        let mut req = request();
        req.endpoint.dry_multiplier = Some(0.0);
        assert_eq!(sampler_plan(&req, thinking_off()).dry_multiplier, None);

        req.endpoint.dry_multiplier = Some(0.8);
        assert_eq!(
            sampler_plan(&req, thinking_off()).dry_multiplier,
            Some(0.8),
            "the catalog default must survive, or small models loop",
        );
    }

    #[test]
    fn a_zero_presence_penalty_is_omitted_and_a_negative_one_is_sent() {
        // Zero is the no-op and is left off. Negative is a REAL setting (it
        // pushes the model toward words it has already used) and filtering on
        // `> 0` rather than `!= 0` would swallow it, which is the mistake the
        // shipping body builder documents avoiding.
        let mut req = request();
        req.endpoint.presence_penalty = Some(0.0);
        assert_eq!(sampler_plan(&req, thinking_off()).penalty_present, None);

        req.endpoint.presence_penalty = Some(-0.5);
        assert_eq!(
            sampler_plan(&req, thinking_off()).penalty_present,
            Some(-0.5),
            "a negative presence penalty is legal and is sent",
        );
    }

    #[test]
    fn the_configured_chain_order_reaches_the_sampler_verbatim() {
        // Verbatim, and that is the assertion. `typ_p` is llama.cpp's canonical
        // name (`common/sampling.cpp:816`) and `temp` is one of its aliases; a
        // canonicalising pass on this side would have to re-derive a table
        // llama.cpp already owns, and would reject exactly the string the
        // settings placeholder invites the user to copy
        // (`settings/groups/llm.ts:307`).
        let mut req = request();
        req.endpoint.samplers = Some(vec![
            "top_k".to_string(),
            "typ_p".to_string(),
            "temp".to_string(),
            "penalties".to_string(),
        ]);
        assert_eq!(
            sampler_plan(&req, thinking_off()).samplers,
            Some(vec![
                "top_k".to_string(),
                "typ_p".to_string(),
                "temp".to_string(),
                "penalties".to_string(),
            ]),
        );
    }

    #[test]
    fn no_configured_order_leaves_llama_cpps_own_chain_alone() {
        // Both the unset and the empty case, because the resolver produces
        // `None` for a blank setting and a hand-built endpoint can carry `[]`.
        // An empty list reaching llama.cpp is not "the default order", it is a
        // chain with no samplers in it.
        let mut req = request();
        assert_eq!(req.endpoint.samplers, None);
        assert_eq!(sampler_plan(&req, thinking_off()).samplers, None);

        req.endpoint.samplers = Some(Vec::new());
        assert_eq!(sampler_plan(&req, thinking_off()).samplers, None);
    }

    #[test]
    fn a_thinking_cap_reaches_the_budget_sampler_and_no_cap_is_minus_one() {
        let req = request();
        assert_eq!(
            sampler_plan(
                &req,
                Thinking {
                    enabled: true,
                    budget: Some(256)
                }
            )
            .reasoning_budget_tokens,
            256,
        );

        // Uncapped and thinking-off both mean "build the sampler, do not limit
        // it". Zero would be a cap of zero tokens, and omitting the budget
        // altogether would drop the think-block suppression a lazy grammar
        // relies on, which is behaviour llama-server has today.
        for thinking in [
            Thinking {
                enabled: true,
                budget: None,
            },
            thinking_off(),
        ] {
            assert_eq!(
                sampler_plan(&req, thinking).reasoning_budget_tokens,
                -1,
                "no cap must be -1, not 0",
            );
        }
    }

    #[test]
    fn an_endpoint_with_nothing_set_produces_a_plan_with_nothing_set() {
        // The distinction the whole file rests on: an unset knob must arrive as
        // absent so llama.cpp's own default applies. Filling in a default here
        // would make every one of them this file's to maintain, and the first
        // llama.cpp bump that retunes one would silently not apply.
        let req = LlmRequest::new(LlmEndpointConfig::default(), vec![ChatMessage::user("hi")]);
        let plan = sampler_plan(&req, thinking_off());
        assert_eq!(
            plan,
            SamplerPlan {
                reasoning_budget_tokens: -1,
                ..SamplerPlan::default()
            },
        );
        assert_eq!(max_tokens(&req), None);
    }

    #[test]
    fn an_absurd_top_k_saturates_rather_than_wrapping_negative() {
        // A hand-edited settings file. Saturating gives llama.cpp a very large
        // k, which is a no-op truncation; wrapping would give it a negative one,
        // which is a different instruction entirely.
        let mut req = request();
        req.endpoint.top_k = Some(u32::MAX);
        assert_eq!(sampler_plan(&req, thinking_off()).top_k, Some(i32::MAX));
    }
}
