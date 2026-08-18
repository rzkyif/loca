// The two capability flags the two cores legitimately disagree on.
//
// Differential mode compares every field neither side declared unstable, so
// ANY case whose response body includes `capabilities` has to say this. That is
// four files today, which is why it is one exported constant rather than four
// copies: the rule is subtle enough that a drifted copy would read as
// deliberate.
//
// The disagreement is real, not a port defect. The Deno core serves local
// inference and local speech through downloadable sidecars (llama-server,
// loca-core-speech) and answers true. The Rust core links both engines in at
// compile time behind `local-inference` / `local-speech`, both default OFF for
// reasons that are not going to change (CI builds from a fresh checkout with
// `vendor/` gitignored, and sherpa-onnx-sys downloads a prebuilt archive), so a
// default build answers false. A suite that must be green on two
// implementations can only assert their intersection, which is the same ruling
// `d15-speech.ts` already carries for `{"speed": "fast"}`.
//
// The other five flags are NOT here and must not be added: both targets under
// test are desktop service cores, so all five are exactly `true` on both, and
// masking them would throw away the only differential coverage the block has.
export const UNSTABLE_CAPABILITIES = {
  localInference: "<bool>",
  localSpeech: "<bool>",
} as const;
