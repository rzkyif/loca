//! Which samplers run, in what order, with what parameters. Pure: the gated
//! half converts a [`SamplerPlan`] into `shim::SamplerSpec`. Values come from
//! the already-resolved `LlmEndpointConfig`; nothing here re-reads settings, and
//! nothing tests a locality flag (if this engine runs, the endpoint is local).
//! An unset knob is left absent so llama.cpp's own default applies.

use crate::llm::LlmRequest;

use super::prompt::Thinking;

/// The sampler knobs for one turn. `None` leaves llama.cpp's default alone.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SamplerPlan {
    /// The chain order, passed through verbatim: llama.cpp owns the name table
    /// and its aliases, so a table here could only accept fewer names.
    pub samplers: Option<Vec<String>>,
    pub temperature: Option<f32>,
    pub top_k: Option<i32>,
    pub top_p: Option<f32>,
    pub min_p: Option<f32>,
    pub penalty_repeat: Option<f32>,
    /// Zero is the no-op and is omitted; a negative value is legal and sent.
    pub penalty_present: Option<f32>,
    /// Zero means off (omitted), not a multiplier of zero.
    pub dry_multiplier: Option<f32>,
    /// The `<think>` token cap, or `-1` for no cap. A value rather than an
    /// absence: llama.cpp builds the reasoning-budget sampler for a
    /// non-negative budget or a lazy grammar, treats negative as unlimited, and
    /// that sampler also does the think-block suppression a lazy grammar needs.
    pub reasoning_budget_tokens: i32,
}

/// Negative, not zero: zero is a cap of zero tokens.
const REASONING_UNLIMITED: i32 = -1;

/// Build the sampler plan for one turn. `thinking` is resolved once by
/// [`super::prompt::thinking`] and passed in, never re-derived.
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
/// context window. Same override precedence as the samplers.
pub fn max_tokens(req: &LlmRequest) -> Option<u32> {
    req.overrides.max_tokens.or(req.endpoint.max_tokens)
}

/// llama.cpp's sampler parameters are `float`; settings carry `f64`.
#[allow(clippy::cast_possible_truncation)]
fn narrow(value: f64) -> f32 {
    value as f32
}

/// Saturating: wrapping to negative would mean something else to llama.cpp.
fn count(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::llm::{ChatMessage, LlmEndpointConfig, Overrides};

    /// The catalog defaults the resolver applies for a local provider.
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
        assert_eq!(plan.temperature, Some(0.7));
        assert_eq!(plan.top_p, Some(0.95));
        assert_eq!(max_tokens(&plain), None);
    }

    #[test]
    fn the_llama_only_samplers_are_not_gated_on_any_locality_predicate() {
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
        let mut req = request();
        req.endpoint.dry_multiplier = Some(0.0);
        assert_eq!(sampler_plan(&req, thinking_off()).dry_multiplier, None);

        req.endpoint.dry_multiplier = Some(0.8);
        assert_eq!(sampler_plan(&req, thinking_off()).dry_multiplier, Some(0.8));
    }

    #[test]
    fn a_zero_presence_penalty_is_omitted_and_a_negative_one_is_sent() {
        let mut req = request();
        req.endpoint.presence_penalty = Some(0.0);
        assert_eq!(sampler_plan(&req, thinking_off()).penalty_present, None);

        req.endpoint.presence_penalty = Some(-0.5);
        assert_eq!(
            sampler_plan(&req, thinking_off()).penalty_present,
            Some(-0.5)
        );
    }

    #[test]
    fn the_configured_chain_order_reaches_the_sampler_verbatim() {
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

        // Uncapped and thinking-off both mean "build the sampler, do not limit it".
        for thinking in [
            Thinking {
                enabled: true,
                budget: None,
            },
            thinking_off(),
        ] {
            assert_eq!(sampler_plan(&req, thinking).reasoning_budget_tokens, -1);
        }
    }

    #[test]
    fn an_endpoint_with_nothing_set_produces_a_plan_with_nothing_set() {
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
        let mut req = request();
        req.endpoint.top_k = Some(u32::MAX);
        assert_eq!(sampler_plan(&req, thinking_off()).top_k, Some(i32::MAX));
    }
}
