//! The regression net for what dropping upstream's `llama-server` would cost.
//!
//! Three capabilities live in `common/chat.cpp` and in nothing else we link:
//! Jinja chat-template application, per-format tool-call extraction, and the
//! reasoning/`<think>` split. Losing any of them is silent, so these tests exist
//! to make the loss loud.
//!
//! # Two rules these tests follow, and why
//!
//! **Everything goes through the production path.** No test builds its own
//! parser params or reaches around the ABI. A test that constructs its own
//! `common_chat_parser_params` passes against a shim that never calls
//! `parser.load`, which is exactly the failure being guarded against (D-100).
//! Every parse below comes from a real `ChatTemplates::apply`.
//!
//! **They are proven by mutation, not by passing.** Each was checked to fail
//! when the thing it guards is removed; the table is in N1's status. The
//! mutations were hand-applied one at a time in the foreground and reverted
//! before the next, so there is no driver that could leave residue in the tree
//! (D-101).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::chat::{ChatInputs, ChatTemplates, ReasoningFormat};
use super::model::{self, Context, ContextOptions, Model, ModelOptions, Mtmd, MtmdOptions};
use super::sampler::{GrammarKind, SamplerChain, SamplerKind, SamplerSpec, SamplerTrigger};

/// The vendored llama.cpp tree, from `build.rs`. Always present when this
/// module compiles, because the same feature that compiles it builds llama.cpp.
fn vendor_dir() -> PathBuf {
    PathBuf::from(env!("LOCA_LLAMA_VENDOR_DIR"))
}

fn template(name: &str) -> String {
    let path = vendor_dir().join("models/templates").join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read bundled template {}: {e}", path.display()))
}

/// Loads the backend modules the desktop build needs before anything can run.
///
/// In the shared (desktop) build every backend including the CPU baseline is a
/// module loaded at runtime, so with none loaded there are zero devices and no
/// model will load. In the static (mobile) build they are linked in and this is
/// a no-op.
fn ensure_backends() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        model::backend_init();
        if cfg!(loca_llama_link = "shared") {
            let dir = backend_dir();
            let n = model::backend_load_dir(&dir)
                .unwrap_or_else(|e| panic!("loading backends from {dir}: {}", e.message));
            assert!(n > 0, "no backend modules found in {dir}; nothing can run");
        }
    });
}

/// Where to load backend modules from.
///
/// The directory holding this executable first, because that is the production
/// layout (D-106 ships the modules beside the binary) and because it is what a
/// moved or anchored install has. The build directory is the fallback for a
/// build whose staging did not run.
fn backend_dir() -> String {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if std::fs::read_dir(dir).is_ok_and(|mut e| {
                e.any(|f| f.is_ok_and(|f| f.file_name().to_string_lossy().starts_with("libggml-")))
            }) {
                return dir.to_string_lossy().into_owned();
            }
        }
    }
    env!("LOCA_LLAMA_BACKEND_DIR").to_owned()
}

/// The optional test model. Kept out of git and out of the test suite's
/// network access: see the acquisition command in N1's status.
fn test_gguf() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models/test.gguf");
    p.is_file().then_some(p)
}

const NO_GGUF: &str = "SKIP: packages/loca-core-rs/models/test.gguf is absent, so the \
    linkage test did not run. See docs/rust-port/packages/N1/status.md for the one command \
    that fetches it. The two capability tests above do not need it and DID run.";

// ---------------------------------------------------------------------------
// T2: tool-call extraction
// ---------------------------------------------------------------------------

fn weather_tools() -> serde_json::Value {
    serde_json::json!([{
        "type": "function",
        "function": {
            "name": "get_weather",
            "description": "Get the current weather for a city",
            "parameters": {
                "type": "object",
                "properties": { "city": { "type": "string" } },
                "required": ["city"],
            },
        },
    }])
}

fn weather_messages() -> serde_json::Value {
    serde_json::json!([
        { "role": "system", "content": "You are a helpful assistant." },
        { "role": "user", "content": "What is the weather in Jakarta?" },
    ])
}

fn apply_with(template_file: &str, reasoning: ReasoningFormat) -> super::chat::ChatTurn {
    let tmpls = Arc::new(
        ChatTemplates::from_source(&template(template_file), "<s>", "</s>")
            .unwrap_or_else(|e| panic!("templates init for {template_file}: {}", e.message)),
    );
    let mut inputs = ChatInputs::new(weather_messages()).with_tools(weather_tools());
    inputs.reasoning_format = reasoning;
    tmpls
        .apply(&inputs)
        .unwrap_or_else(|e| panic!("apply for {template_file}: {}", e.message))
}

fn apply_with_tools(template_file: &str) -> super::chat::ChatTurn {
    apply_with(template_file, ReasoningFormat::Deepseek)
}

#[test]
fn applies_a_chat_template_with_the_tools_injected() {
    let turn = apply_with_tools("NousResearch-Hermes-2-Pro-Llama-3-8B-tool_use.jinja");
    let applied = turn.applied();

    assert!(!applied.prompt.is_empty(), "no prompt was rendered");
    assert!(
        applied.prompt.contains("get_weather"),
        "the tool definition never reached the prompt, so the model cannot call it",
    );
    assert!(
        applied.prompt.contains("What is the weather in Jakarta?"),
        "the user message was not rendered",
    );
}

#[test]
fn extracts_hermes_json_tool_calls() {
    let turn = apply_with_tools("NousResearch-Hermes-2-Pro-Llama-3-8B-tool_use.jinja");
    let output = concat!(
        "<tool_call>\n",
        r#"{"name": "get_weather", "arguments": {"city": "Jakarta"}}"#,
        "\n</tool_call>",
    );

    let msg = turn
        .parse_final(output)
        .unwrap_or_else(|e| panic!("parse: {}", e.message));

    assert_eq!(
        msg.tool_calls.len(),
        1,
        "expected exactly one tool call, got {msg:?}"
    );
    assert_eq!(msg.tool_calls[0].function.name, "get_weather");
    assert!(
        msg.tool_calls[0].function.arguments.contains("Jakarta"),
        "arguments lost the city: {:?}",
        msg.tool_calls[0].function.arguments,
    );
}

#[test]
fn extracts_qwen_xml_tool_calls_through_the_same_path() {
    // Qwen3.5 does not use Hermes JSON; its template specifies a nested XML
    // form. Two families, two wire syntaxes, one code path: this is the
    // concrete reason `common/` is linked instead of a libllama-only crate,
    // which would have to re-derive the syntax per family.
    let turn = apply_with_tools("Qwen3.5-4B.jinja");
    let output = concat!(
        "The user is asking about weather. I should call the tool.\n</think>\n\n",
        "<tool_call>\n<function=get_weather>\n<parameter=city>\nJakarta\n",
        "</parameter>\n</function>\n</tool_call>",
    );

    let msg = turn
        .parse_final(output)
        .unwrap_or_else(|e| panic!("parse: {}", e.message));

    assert_eq!(
        msg.tool_calls.len(),
        1,
        "expected exactly one tool call, got {msg:?}"
    );
    assert_eq!(msg.tool_calls[0].function.name, "get_weather");
    assert!(
        msg.tool_calls[0].function.arguments.contains("Jakarta"),
        "{msg:?}"
    );
}

// ---------------------------------------------------------------------------
// T3: reasoning split
// ---------------------------------------------------------------------------

#[test]
fn splits_reasoning_out_of_the_content() {
    let turn = apply_with_tools("Qwen3.5-4B.jinja");
    // The generation prompt already opened `<think>`, so a real model resumes
    // inside it and closes with `</think>`.
    let output = "The user wants weather. I should answer directly.\n</think>\n\nIt is 31 degrees.";

    let msg = turn
        .parse_final(output)
        .unwrap_or_else(|e| panic!("parse: {}", e.message));

    assert!(
        msg.reasoning_content.contains("answer directly"),
        "the thought did not land in reasoning_content: {msg:?}",
    );
    assert_eq!(
        msg.content.trim(),
        "It is 31 degrees.",
        "post-think content was not preserved"
    );
    assert!(
        !msg.content.contains("<think>"),
        "think tags leaked into content: {msg:?}"
    );
    assert!(
        !msg.content.contains("answer directly"),
        "the thought also leaked into content, so a user would see it: {msg:?}",
    );
}

#[test]
fn reasoning_stays_inline_when_the_split_is_off_at_apply_time() {
    // The negative half. Without it, a shim that hardcoded the split would pass
    // the test above while ignoring the caller entirely.
    //
    // Note WHERE the control is: this varies the APPLY-time format, because the
    // parse-time one is inert at this pin. That is not a detail of the test, it
    // is why `parse_final` takes no format argument.
    let output = "The user wants weather. I should answer directly.\n</think>\n\nIt is 31 degrees.";

    let msg = apply_with("Qwen3.5-4B.jinja", ReasoningFormat::None)
        .parse_final(output)
        .unwrap_or_else(|e| panic!("parse: {}", e.message));

    assert!(
        msg.reasoning_content.is_empty(),
        "reasoning was split out despite apply-time ReasoningFormat::None: {msg:?}",
    );
    assert!(
        msg.content.contains("<think>"),
        "with the split off the tags must survive in content: {msg:?}",
    );
}

// ---------------------------------------------------------------------------
// T4: the trap
// ---------------------------------------------------------------------------

#[test]
fn a_tool_capable_turn_carries_a_peg_parser_into_the_parse() {
    // The diagnosis rather than the symptom, and it took a review to get it
    // pointed at the right side.
    //
    // `common_chat_parser_params`' converting constructor copies only `format`
    // and `generation_prompt`; the PEG parser must be revived with
    // `parser.load(params.parser)` on the PARSE side. Without that, parsing
    // degrades to content-only, logs at DEBUG, and RETURNS SUCCESS: no
    // tool_calls, no reasoning_content, nothing red.
    //
    // This test used to assert `has_parser()`, which is the APPLY side. That is
    // a different property: it stayed green under BOTH trap mutations (deleting
    // the load, and deleting the load together with its loud guard), so the one
    // test named for the trap was insensitive to it. `apply` producing a parser
    // is necessary but says nothing about whether `parse` receives it.
    //
    // `parser_survives_revival` runs the same `revive_parser` helper that
    // `loca_chat_parse` runs, so there is one line to delete and it breaks both.
    // A probe carrying its own copy of the revival would stay green after the
    // real one was removed, which is the D-100 shape.
    for file in [
        "NousResearch-Hermes-2-Pro-Llama-3-8B-tool_use.jinja",
        "Qwen3.5-4B.jinja",
    ] {
        let turn = apply_with_tools(file);
        assert!(
            turn.has_parser(),
            "{file}: apply produced no PEG parser at all",
        );
        assert!(
            turn.parser_survives_revival(),
            "{file}: the PEG parser did not survive into the parser params that \
             parse actually uses, so tool calls and reasoning would be silently \
             dropped while every parse still reported success",
        );
    }
}

// ---------------------------------------------------------------------------
// D-108: no call path from our code to llama.cpp's HTTP client
// ---------------------------------------------------------------------------

/// Finds the compiled shim object, wherever `cc` put it.
fn shim_object() -> Option<PathBuf> {
    fn walk(dir: &Path, out: &mut Option<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.file_name().is_some_and(|n| {
                // `cc` prefixes the object with a build hash
                // (`70ca879c342373fc-shim.o`), so this matches the suffix.
                // Matching the bare name found nothing and turned this whole
                // test into a silent skip; it took a mutation to notice.
                let n = n.to_string_lossy();
                n.ends_with("shim.o") || n.ends_with("shim.obj")
            }) {
                *out = Some(path);
            }
        }
    }
    let mut found = None;
    walk(Path::new(env!("LOCA_LLAMA_SHIM_OBJ_DIR")), &mut found);
    found
}

#[test]
fn the_shim_never_calls_llama_cpps_http_client_or_subprocess_spawner() {
    // `llama-common` carries an HTTP downloader (cpp-httplib) and a subprocess
    // spawner in the SAME cmake target as chat.cpp. `LLAMA_OPENSSL=OFF` and
    // `LLAMA_SUBPROCESS=OFF` reduce them but do not remove them: in the desktop
    // build the shared library legitimately exports 766 httplib symbols,
    // because a shared library ships its whole target and nothing is
    // dead-stripped across that boundary.
    //
    // So the property that actually matters is not "no such symbols exist" but
    // "our code never reaches them", and that is checkable: the shim is the
    // only thing we compile against llama.cpp's headers, so an undefined
    // reference in its object file is the complete set of what we call.
    //
    // This protects an absolute rule (no non-consented network after install),
    // which is why it is a test rather than a paragraph.
    // NOT a skip. The object is always present when this module compiles, since
    // the same feature builds it, so "not found" means the audit is silently not
    // running rather than that it is inapplicable. That is precisely the state
    // this test spent its first life in.
    let obj = shim_object().unwrap_or_else(|| {
        panic!(
            "shim object not found under {}; the no-call-path audit cannot run, \
             which means it is guarding nothing",
            env!("LOCA_LLAMA_SHIM_OBJ_DIR"),
        )
    });

    let Ok(out) = std::process::Command::new("nm")
        .arg("-u")
        .arg(&obj)
        .output()
    else {
        eprintln!("SKIP: `nm` is unavailable, so the no-call-path audit did not run");
        return;
    };
    if !out.status.success() {
        eprintln!("SKIP: `nm` failed on {}", obj.display());
        return;
    }

    let stdout = String::from_utf8_lossy(&out.stdout);
    let symbols: Vec<String> = stdout
        .lines()
        .map(|l| l.trim().trim_start_matches('_').to_owned())
        .filter(|l| !l.is_empty())
        .collect();
    assert!(
        !symbols.is_empty(),
        "`nm -u` listed no undefined symbols at all, so this audit proves nothing",
    );

    // C++ names are mangled, so a family name appears as a substring of the
    // mangled symbol; substring is the right test for these.
    for family in [
        "httplib",
        "common_download",
        "curl_easy",
        "ssl_ctx",
        "subproc",
    ] {
        let hit = symbols.iter().find(|s| s.to_lowercase().contains(family));
        assert!(
            hit.is_none(),
            "the shim references `{family}` (as `{}`), so our code has a call path \
             into llama.cpp's network or subprocess machinery. That breaches the \
             no-non-consented-network rule; remove the call rather than the assertion.",
            hit.map_or("", String::as_str),
        );
    }

    // D-107, all four doors.
    //
    // Two of these are ggml's loaders. The other two are llama.cpp convenience
    // functions that call `ggml_backend_load_all()` THEMSELVES whenever the
    // registry is empty (src/llama.cpp:101-113, 131-133), which is exactly the
    // state at startup. Referencing any of the four hands the working-directory
    // scan back to libllama, which is what the first version of this shim did by
    // calling `llama_backend_init()`: the poison covered this file, the call
    // reached the same code one level down, and nothing said so.
    //
    // `llama_supports_gpu_offload` and `llama_supports_rpc` are the two most
    // likely to be added later by someone answering a reasonable question
    // ("does this machine have a GPU?"). They are cheap-looking and they are not.
    //
    // Matched EXACTLY rather than as a substring: `ggml_backend_load_all` is a
    // prefix of `ggml_backend_load_all_from_path`, and `ggml_backend_load` is a
    // prefix of both while being the safe per-path call the shim DOES use. A
    // substring test here fails on correct code, which is how this assertion was
    // first written.
    for banned in [
        "ggml_backend_load_all",
        "ggml_backend_load_all_from_path",
        "llama_backend_init",
        "llama_supports_gpu_offload",
        "llama_supports_rpc",
    ] {
        assert!(
            !symbols.iter().any(|s| s == banned),
            "the shim references `{banned}`. Directly or via libllama, that reaches \
             ggml_backend_load_all, which searches the process working directory and \
             dlopens anything matching libggml-*.so it finds there, and which also \
             dlopens whatever GGML_BACKEND_PATH names. Load backends with \
             loca_backend_load or loca_backend_load_dir, from a path we computed.",
        );
    }

    // The positive half: the safe per-path loader IS referenced. Without this the
    // assertions above are satisfied by a shim that loads no backends at all,
    // which would pass while nothing worked.
    assert!(
        symbols.iter().any(|s| s == "ggml_backend_load"),
        "the shim references no backend loader at all, so this audit is passing \
         vacuously",
    );
}

// ---------------------------------------------------------------------------
// T1: the linkage test
// ---------------------------------------------------------------------------

#[test]
fn greedy_decoding_is_deterministic() {
    // Guards the half of the linkage the chat tests never touch: that libllama
    // and ggml actually execute, that decode produces logits, and that the
    // sampler chain runs. Greedy so the assertion is exact rather than
    // probabilistic.
    ensure_backends();

    let Some(path) = test_gguf() else {
        eprintln!("{NO_GGUF}");
        return;
    };

    let model = Arc::new(
        Model::load(&path.to_string_lossy(), &ModelOptions::default())
            .unwrap_or_else(|e| panic!("model load: {}", e.message)),
    );
    let n_vocab = model.n_vocab() as usize;
    assert!(n_vocab > 0, "model reports an empty vocabulary");

    let mut ctx = Context::new(
        Arc::clone(&model),
        &ContextOptions {
            n_ctx: 256,
            n_batch: 256,
            n_threads: 2,
            flash_attn: false,
            ..ContextOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("context: {}", e.message));

    let mut sampler = SamplerChain::new(Arc::clone(&model), &SamplerSpec::greedy())
        .unwrap_or_else(|e| panic!("sampler: {}", e.message));

    let run = |ctx: &mut Context, sampler: &mut SamplerChain| -> Vec<i32> {
        ctx.reset();
        sampler.reset();
        assert_eq!(ctx.n_past(), 0, "reset must empty the context");

        let prompt = model
            .tokenize("Once upon a time", true, false)
            .expect("tokenize");
        assert!(!prompt.is_empty(), "prompt tokenized to nothing");
        ctx.decode(&prompt).expect("decode prompt");
        assert_eq!(
            ctx.n_past(),
            prompt.len() as i32,
            "n_past must report what the context actually holds",
        );

        let mut produced = Vec::new();
        for _ in 0..16 {
            let logits = ctx.logits(-1).expect("logits");
            assert_eq!(
                logits.len(),
                n_vocab,
                "the logits slice must be exactly one row of the vocabulary",
            );
            let token = sampler.sample(ctx, -1).expect("sample");
            if model.is_eog(token) {
                break;
            }
            // Exactly one accept per sampled token. `common_sampler_sample` does
            // not accept, so this is the single advance of the grammar stack,
            // the reasoning budget and the penalty ring.
            sampler.accept(token, true);
            produced.push(token);
            ctx.decode(&[token]).expect("decode token");
        }
        produced
    };

    let first = run(&mut ctx, &mut sampler);
    assert!(
        !first.is_empty(),
        "greedy decoding produced no tokens at all"
    );

    // Determinism is asserted against a second run rather than a hardcoded
    // token list, so the test is meaningful for whatever GGUF is present rather
    // than only for the one it was written against.
    let second = run(&mut ctx, &mut sampler);
    assert_eq!(
        first, second,
        "greedy decoding is not deterministic across runs"
    );

    let text: Vec<u8> = first
        .iter()
        .flat_map(|&t| model.token_bytes(t, false).expect("token bytes"))
        .collect();
    assert!(!text.is_empty(), "detokenizing produced no bytes");
}

#[test]
fn a_malformed_grammar_is_rejected_rather_than_accepted_and_then_fatal() {
    // The one class of bug that must never come back silently: a crash reachable
    // from a safe Rust API with safe inputs.
    //
    // The hand-rolled chain this replaced added `llama_sampler_init_grammar`'s
    // return value to the chain without checking it. That function returns null
    // for a grammar it cannot parse, `llama_sampler_chain_add` stores the null,
    // and `llama_sampler_chain_apply` dereferences it, so `new` returned `Ok`
    // and the FIRST sampled token killed the process (SIGSEGV, confirmed).
    //
    // The assertion is deliberately about the OBSERVABLE property rather than
    // about how the chain is built, so it keeps holding across implementations:
    // a grammar llama.cpp cannot parse must come back as an error from `new`.
    ensure_backends();
    let Some(path) = test_gguf() else {
        eprintln!("{NO_GGUF}");
        return;
    };
    let model = Arc::new(
        Model::load(&path.to_string_lossy(), &ModelOptions::default())
            .unwrap_or_else(|e| panic!("model load: {}", e.message)),
    );

    let spec = SamplerSpec {
        grammar: Some("root ::= ( unterminated".to_owned()),
        ..SamplerSpec::default()
    };
    let err = match SamplerChain::new(Arc::clone(&model), &spec) {
        Err(e) => e,
        Ok(mut sampler) => {
            // Do not sample here. If the chain really does contain a null the
            // process dies and takes the whole test binary with it, which
            // reports as a signal rather than as this assertion. Failing loudly
            // on the accepted handle is the diagnosis; the crash is the symptom.
            let _ = &mut sampler;
            panic!(
                "a malformed grammar was ACCEPTED. If the sampler chain holds a null \
                 sampler, the next sample() is a SIGSEGV reachable from safe Rust.",
            );
        }
    };
    assert!(
        err.message.to_lowercase().contains("grammar"),
        "the error should name the grammar as the cause: {}",
        err.message,
    );
}

#[test]
fn every_sampler_name_is_one_llama_cpp_accepts() {
    // The defending test for D-157, and the shape of it is the finding.
    //
    // `SamplerKind`'s names used to come from `#[serde(rename_all =
    // "snake_case")]`, a DERIVATION RULE standing in for llama.cpp's LOOKUP
    // TABLE. It agreed on ten of eleven names and diverged on the one llama.cpp
    // abbreviated: typical-p is `typ_p`, and `typical_p` is not canonical, not
    // one of the kebab/no-dash aliases derived from a canonical name, and not
    // one of the three misc aliases. The shim rejects unknown names by design,
    // so that variant made every chain containing it a FAILED TURN, and nine
    // catalog models ship it as their default sampler order.
    //
    // The test that was supposed to guard this asserted that the string
    // `"typical_p"` appeared in the serialized spec. It did. A string assertion
    // about a string cannot see what the string means to its consumer, so it
    // passed for as long as it existed. This one hands each name to the real
    // mapper and lets llama.cpp answer; asserting a different literal instead
    // would be the same defect with a better constant.
    //
    // One chain per name rather than one chain of all eleven, so a failure
    // names the offender instead of reporting that a chain did not build.
    ensure_backends();
    let Some(path) = test_gguf() else {
        eprintln!("{NO_GGUF}");
        return;
    };
    let model = Arc::new(
        Model::load(&path.to_string_lossy(), &ModelOptions::default())
            .unwrap_or_else(|e| panic!("model load: {}", e.message)),
    );

    for kind in SamplerKind::ALL {
        let spec = SamplerSpec {
            samplers: Some(vec![kind.as_str().to_owned()]),
            ..SamplerSpec::default()
        };
        if let Err(e) = SamplerChain::new(Arc::clone(&model), &spec) {
            panic!(
                "llama.cpp does not accept {:?} as `{}`: {}",
                kind,
                kind.as_str(),
                e.message,
            );
        }
    }
}

#[test]
fn every_sampler_chain_the_shipped_catalog_declares_is_one_llama_cpp_accepts() {
    // The other half of D-157, and the half that decides its severity. The
    // enum's wrong name was not merely reachable by a user typing: the shipped
    // catalog declares `typ_p` in the default sampler order of nine models, so
    // the failure was the DEFAULT configuration on those models rather than an
    // edge case, and every turn on one of them would have been rejected.
    //
    // The fixture is the real artifact rather than a list written from the code,
    // which is the one check that catches an assumption the code was built on:
    // a hand-written list would contain the names this crate already expects.
    // Reading the catalog also means a chain added to it later is defended
    // without anyone remembering to extend a test.
    ensure_backends();
    let Some(path) = test_gguf() else {
        eprintln!("{NO_GGUF}");
        return;
    };
    let model = Arc::new(
        Model::load(&path.to_string_lossy(), &ModelOptions::default())
            .unwrap_or_else(|e| panic!("model load: {}", e.message)),
    );

    const CATALOG: &str = include_str!("../../models/catalog.dev.json");
    let catalog: serde_json::Value =
        serde_json::from_str(CATALOG).unwrap_or_else(|e| panic!("catalog parses: {e}"));

    let mut chains: Vec<Vec<String>> = Vec::new();
    collect_sampler_chains(&catalog, &mut chains);
    assert!(
        !chains.is_empty(),
        "found no sampler chain in the catalog, so this test proves nothing: \
         the key moved and the walk needs updating",
    );

    for chain in chains {
        let spec = SamplerSpec {
            samplers: Some(chain.clone()),
            ..SamplerSpec::default()
        };
        if let Err(e) = SamplerChain::new(Arc::clone(&model), &spec) {
            panic!(
                "the catalog ships a chain llama.cpp rejects: {chain:?}: {}",
                e.message,
            );
        }
    }
}

/// Every `samplers` array anywhere in the catalog, wherever the key sits.
///
/// Walked structurally rather than by path, so a catalog reshuffle moves the
/// chains without silently emptying the fixture. The empty-result assertion at
/// the call site is what stops that being a green vacuum.
fn collect_sampler_chains(value: &serde_json::Value, out: &mut Vec<Vec<String>>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                if key == "samplers" {
                    if let Some(names) = child.as_array() {
                        let names: Vec<String> = names
                            .iter()
                            .filter_map(|n| n.as_str().map(str::to_owned))
                            .collect();
                        if !names.is_empty() && !out.contains(&names) {
                            out.push(names);
                        }
                    }
                }
                collect_sampler_chains(child, out);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_sampler_chains(item, out);
            }
        }
        _ => {}
    }
}

#[test]
fn an_unknown_sampler_name_is_an_error_rather_than_a_shorter_chain() {
    // llama.cpp's own name mapper LOG_WRN's an unrecognised name and drops it.
    // `llm.samplers` is a user setting, so upstream's behaviour would turn a
    // typo into a chain that is quietly missing a sampler and a generation that
    // is quietly different. The shim compares the counts and names the offender.
    //
    // Serialized by hand because `SamplerKind` cannot express a bad name, which
    // is the point: the typo can only arrive from a config string.
    ensure_backends();
    let Some(path) = test_gguf() else {
        eprintln!("{NO_GGUF}");
        return;
    };
    let model = Arc::new(
        Model::load(&path.to_string_lossy(), &ModelOptions::default())
            .unwrap_or_else(|e| panic!("model load: {}", e.message)),
    );

    let err = SamplerChain::new_from_json(
        Arc::clone(&model),
        r#"{"samplers":["top_k","tpo_p","temperature"]}"#,
    )
    .err()
    .unwrap_or_else(|| panic!("a misspelled sampler name was accepted"));
    assert!(
        err.message.contains("tpo_p"),
        "the error must name the offending sampler: {}",
        err.message,
    );
}

#[test]
fn the_configured_sampler_order_reaches_llama_cpp() {
    // The ORDER is a user setting and the whole reason the spec carries a list.
    // Nothing else about a built chain is observable without a statistical test,
    // so this asserts against llama.cpp's own rendering of the chain it built.
    ensure_backends();
    let Some(path) = test_gguf() else {
        eprintln!("{NO_GGUF}");
        return;
    };
    let model = Arc::new(
        Model::load(&path.to_string_lossy(), &ModelOptions::default())
            .unwrap_or_else(|e| panic!("model load: {}", e.message)),
    );

    let forward = SamplerChain::new(
        Arc::clone(&model),
        &SamplerSpec {
            samplers: Some(vec![
                SamplerKind::TopK.as_str().to_owned(),
                SamplerKind::TopP.as_str().to_owned(),
            ]),
            ..SamplerSpec::default()
        },
    )
    .unwrap_or_else(|e| panic!("sampler: {}", e.message))
    .chain_description()
    .unwrap_or_else(|e| panic!("chain: {}", e.message));

    let reversed = SamplerChain::new(
        Arc::clone(&model),
        &SamplerSpec {
            samplers: Some(vec![
                SamplerKind::TopP.as_str().to_owned(),
                SamplerKind::TopK.as_str().to_owned(),
            ]),
            ..SamplerSpec::default()
        },
    )
    .unwrap_or_else(|e| panic!("sampler: {}", e.message))
    .chain_description()
    .unwrap_or_else(|e| panic!("chain: {}", e.message));

    assert_ne!(
        forward, reversed,
        "reversing the configured order produced an identical chain, so the \
         order is being ignored: {forward}",
    );
    let k = forward
        .find("top-k")
        .unwrap_or_else(|| panic!("no top-k in {forward}"));
    let p = forward
        .find("top-p")
        .unwrap_or_else(|| panic!("no top-p in {forward}"));
    assert!(k < p, "top-k must precede top-p as configured: {forward}");
}

#[test]
fn a_lazy_tool_call_grammar_does_not_constrain_from_the_first_token() {
    // The capability a hand-rolled chain could not express, and the reason it
    // mattered: the old shim called the EAGER `llama_sampler_init_grammar`, so a
    // tool-capable template constrained generation from token one and the model
    // was forced to emit a tool call on EVERY turn, whether the user asked for
    // one or not.
    //
    // Asserted structurally rather than by generating: a lazy grammar builds a
    // chain that carries its triggers, and the eager one cannot.
    ensure_backends();
    let Some(path) = test_gguf() else {
        eprintln!("{NO_GGUF}");
        return;
    };
    let model = Arc::new(
        Model::load(&path.to_string_lossy(), &ModelOptions::default())
            .unwrap_or_else(|e| panic!("model load: {}", e.message)),
    );
    let turn = apply_with_tools("Qwen3.5-4B.jinja");
    let applied = turn.applied();
    assert!(
        !applied.grammar.is_empty(),
        "the tool-capable template produced no grammar at all",
    );

    let spec = SamplerSpec {
        grammar: Some(applied.grammar.clone()),
        grammar_kind: Some(GrammarKind::ToolCalls),
        grammar_lazy: applied.grammar_lazy,
        grammar_triggers: Some(
            applied
                .grammar_triggers
                .iter()
                .map(|t| SamplerTrigger {
                    kind: t.kind,
                    value: t.value.clone(),
                    token: t.token,
                })
                .collect(),
        ),
        preserved_tokens: Some(applied.preserved_tokens.clone()),
        generation_prompt: Some(applied.generation_prompt.clone()),
        ..SamplerSpec::default()
    };

    // The load-bearing assertion is simply that this SUCCEEDS with the real
    // template's grammar, triggers, preserved tokens and generation prompt all
    // carried through. The hand-rolled chain ignored every one of those four.
    let sampler = SamplerChain::new(Arc::clone(&model), &spec)
        .unwrap_or_else(|e| panic!("lazy tool-call sampler: {}", e.message));
    let chain = sampler
        .chain_description()
        .unwrap_or_else(|e| panic!("chain: {}", e.message));
    assert!(
        !chain.is_empty(),
        "a grammar-constrained sampler described an empty chain"
    );
}

#[test]
fn a_backend_is_registered_before_anything_tries_to_infer() {
    // In the desktop build zero devices is the difference between "slow" and
    // "cannot run at all", and it happens whenever the backend modules are not
    // beside the binary. Worth its own assertion so that failure is named
    // rather than surfacing as a confusing model-load error.
    ensure_backends();
    let devices = model::devices().unwrap_or_else(|e| panic!("devices: {}", e.message));
    assert!(
        !devices.is_empty(),
        "no compute devices registered; inference cannot run"
    );
}

// ---------------------------------------------------------------------------
// D-116: the install must still run after it has been moved
// ---------------------------------------------------------------------------

/// The name of the test the relocation check re-runs in the moved copy. It has
/// to be one that genuinely loads llama.cpp, or the child would prove only that
/// a binary can start.
const RELOCATED_CHILD_TEST: &str =
    "inference::shim::tests::a_tool_capable_turn_carries_a_peg_parser_into_the_parse";

#[test]
#[cfg(loca_llama_link = "shared")]
fn the_linked_tree_still_runs_after_being_moved() {
    // Self-update anchors the previous install by MOVING the directory aside,
    // so the thing rollback falls back to is a relocated copy of exactly this
    // layout. An absolute rpath would make that copy fail to load, and the
    // failure would appear for the first time during a real rollback on a real
    // machine (D-116).
    //
    // A build that runs in place never exercises the move, so this test does
    // the move: it copies the executable and every runtime library to a fresh
    // directory and runs the copy from there. Nothing absolute is emitted into
    // the rpath, so the child can only resolve its libraries through
    // `@loader_path`/`$ORIGIN`, which is the mechanism being checked.
    if std::env::var_os("LOCA_RELOCATION_CHILD").is_some() {
        return; // Guard against a child recursing into this test.
    }

    let Ok(exe) = std::env::current_exe() else {
        panic!("cannot locate the test executable");
    };
    let Some(src_dir) = exe.parent() else {
        panic!("test executable has no parent directory");
    };

    // The load-bearing assertion, and it has to come first.
    //
    // Moving the tree and running it is necessary but NOT sufficient on its
    // own: the original build directory still exists on this machine, so a
    // binary with an absolute rpath resolves through it and the relocated run
    // passes anyway. That was confirmed by mutation, not assumed. Only reading
    // the load commands distinguishes the two.
    assert_rpaths_are_relative(&exe);

    let moved = std::env::temp_dir().join(format!("loca-relocated-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&moved);
    std::fs::create_dir_all(&moved).expect("create the relocated directory");

    // Every runtime library, plus the executable itself. Deliberately NOT a
    // symlink: a symlink would leave the loader resolving through the original
    // absolute path and prove nothing.
    let mut copied = 0usize;
    for entry in std::fs::read_dir(src_dir)
        .expect("read the artifact directory")
        .flatten()
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name();
        let n = name.to_string_lossy();
        if n.contains(".dylib") || n.contains(".so") || n.ends_with(".dll") {
            std::fs::copy(&path, moved.join(&name)).expect("copy a runtime library");
            copied += 1;
        }
    }
    assert!(
        copied > 0,
        "no runtime libraries were staged beside the test binary"
    );

    let moved_exe = moved.join(exe.file_name().unwrap_or_default());
    std::fs::copy(&exe, &moved_exe).expect("copy the executable");

    let out = std::process::Command::new(&moved_exe)
        .args(["--exact", RELOCATED_CHILD_TEST, "--nocapture"])
        .env("LOCA_RELOCATION_CHILD", "1")
        // Cleared so a stray loader path on the parent's environment cannot
        // rescue the child and mask an absolute rpath.
        .env_remove("DYLD_LIBRARY_PATH")
        .env_remove("DYLD_FALLBACK_LIBRARY_PATH")
        .env_remove("LD_LIBRARY_PATH")
        .current_dir(&moved)
        .output()
        .expect("run the relocated executable");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&moved);

    assert!(
        out.status.success(),
        "the relocated install failed to run, so a rollback to an anchored tree \
         would fail too. This is what an absolute rpath looks like.\n\
         status: {}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status,
    );
    assert!(
        stdout.contains("1 passed"),
        "the relocated run did not report the child test passing:\n{stdout}",
    );
}

/// Reads `binary`'s runtime search paths and asserts every one is relative to
/// the loading image.
///
/// An absolute rpath survives in place and dies on relocation, which is the
/// D-116 failure. This is the only check that separates the two while the
/// original build directory still exists, so it is the primary guard and the
/// move-and-run above is the corroboration.
#[cfg(loca_llama_link = "shared")]
fn assert_rpaths_are_relative(binary: &Path) {
    let relative = |p: &str| {
        p.starts_with("@loader_path")
            || p.starts_with("@executable_path")
            || p.starts_with("$ORIGIN")
    };

    let paths: Vec<String> = if cfg!(target_os = "macos") {
        let Ok(out) = std::process::Command::new("otool")
            .arg("-l")
            .arg(binary)
            .output()
        else {
            eprintln!("SKIP: `otool` unavailable, cannot inspect rpaths");
            return;
        };
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let lines: Vec<&str> = text.lines().collect();
        lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.trim() == "cmd LC_RPATH")
            .filter_map(|(i, _)| {
                lines[i..].iter().take(4).find_map(|l| {
                    l.trim()
                        .strip_prefix("path ")
                        .map(|p| p.split(" (offset").next().unwrap_or(p).trim().to_owned())
                })
            })
            .collect()
    } else {
        // ELF. Two readers, because the exact spelling differs between tools and
        // between binutils versions, and a parser that silently produces an
        // empty list would turn this assertion into a confusing failure about
        // "no rpath at all" rather than about the tool.
        //
        //   readelf -d:  0x...1d (RUNPATH)  Library runpath: [$ORIGIN:$ORIGIN/../lib]
        //   objdump -p:  RUNPATH              $ORIGIN:$ORIGIN/../lib
        //
        // Both forms are reduced to the colon-separated value.
        let read = |cmd: &str, arg: &str| -> Option<Vec<String>> {
            let out = std::process::Command::new(cmd)
                .arg(arg)
                .arg(binary)
                .output()
                .ok()?;
            if !out.status.success() {
                return None;
            }
            let text = String::from_utf8_lossy(&out.stdout).into_owned();
            let found: Vec<String> = text
                .lines()
                .filter(|l| l.contains("RUNPATH") || l.contains("RPATH"))
                .filter_map(|l| {
                    // Prefer the bracketed readelf form; otherwise take
                    // everything after the last run of whitespace.
                    if let (Some(a), Some(b)) = (l.find('['), l.rfind(']')) {
                        if a < b {
                            return Some(l[a + 1..b].to_owned());
                        }
                    }
                    l.split_whitespace().last().map(str::to_owned)
                })
                .flat_map(|v| {
                    v.split(':')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .filter(|s| *s != "RUNPATH" && *s != "RPATH")
                .collect();
            (!found.is_empty()).then_some(found)
        };

        // Neither tool producing a dynamic-section entry falls through to the
        // empty vec, which the `!paths.is_empty()` assertion below rejects. That
        // is deliberate rather than a skip: a binary with genuinely no RUNPATH is
        // exactly the failure this test exists for, and silently passing when the
        // reader found nothing is how a guard stops guarding.
        read("readelf", "-d")
            .or_else(|| read("objdump", "-p"))
            .unwrap_or_default()
    };

    assert!(
        !paths.is_empty(),
        "the binary declares no rpath at all, so it cannot find its llama.cpp \
         libraries in an installed layout",
    );
    for p in &paths {
        assert!(
            relative(p),
            "absolute rpath `{p}`. Self-update anchors the old install by moving \
             the directory aside, and an absolute rpath makes that anchored tree \
             unloadable, so rollback fails exactly when it is needed (D-116). Use \
             @loader_path or $ORIGIN.",
        );
    }
}

// ---------------------------------------------------------------------------
// T5: the embedding path
// ---------------------------------------------------------------------------

#[test]
fn embedding_produces_a_stable_vector_of_the_models_width() {
    // Guards the embedding half of the linkage, which nothing else touches: an
    // embedding context is a DIFFERENT llama.cpp configuration from a
    // generation one (`embeddings = true`, a pooling type, no KV cache in play)
    // and it reaches `llama_get_embeddings_seq`, which the decode path never
    // calls.
    //
    // Run against the tiny generative test model rather than MiniLM, and that
    // is fine for what this asserts: llama.cpp pools hidden states the same way
    // whatever produced them, so this exercises the whole shim path (context
    // creation with pooling, the all-token output mask, the pooled read, the
    // copy out) without a 35 MB fixture in the tree. What it deliberately does
    // NOT claim is that the vectors are semantically good; that is the model's
    // business, not the shim's.
    ensure_backends();

    let Some(path) = test_gguf() else {
        eprintln!("{NO_GGUF}");
        return;
    };

    let model = Arc::new(
        Model::load(&path.to_string_lossy(), &ModelOptions::default())
            .unwrap_or_else(|e| panic!("model load: {}", e.message)),
    );
    let n_embd = model.n_embd();
    assert!(n_embd > 0, "model reports no embedding width");

    let mut ctx = Context::new(
        Arc::clone(&model),
        &ContextOptions {
            n_ctx: 128,
            n_batch: 128,
            n_threads: 2,
            embeddings: true,
            pooling_type: "mean".to_string(),
            ..ContextOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("embedding context: {}", e.message));

    let tokens = model
        .tokenize("the quick brown fox", true, false)
        .unwrap_or_else(|e| panic!("tokenize: {}", e.message));
    assert!(!tokens.is_empty(), "the fixture tokenized to nothing");

    let first = ctx
        .embed(&tokens)
        .unwrap_or_else(|e| panic!("embed: {}", e.message));

    assert_eq!(
        first.len(),
        n_embd as usize,
        "the shim wrote a vector of the wrong width",
    );
    // Not all zeros. This is what catches a context that ran but pooled nothing,
    // which is the shape the `llama_encode` version of this shim produced: an
    // apparently successful call returning a buffer the graph never wrote.
    assert!(
        first.iter().any(|v| *v != 0.0 && v.is_finite()),
        "the embedding is all zeros or non-finite, so the pooled read returned a buffer \
         nothing wrote: {:?}",
        &first[..first.len().min(8)],
    );

    // Same input, same vector. The shim clears the context per call, so this is
    // also what would catch that clear going missing: without it the second call
    // pools the concatenation of both inputs and drifts.
    let second = ctx
        .embed(&tokens)
        .unwrap_or_else(|e| panic!("second embed: {}", e.message));
    assert_eq!(
        first, second,
        "embedding the same text twice through one context gave different vectors, so state \
         is leaking between calls and every memory after the first is embedded against the \
         wrong sequence",
    );

    // A different input must not give the same vector, which is what separates
    // "pooling works" from "a constant is being returned".
    let other = model
        .tokenize("entirely unrelated subject matter", true, false)
        .unwrap_or_else(|e| panic!("tokenize: {}", e.message));
    let other = ctx
        .embed(&other)
        .unwrap_or_else(|e| panic!("embed other: {}", e.message));
    assert_ne!(
        first, other,
        "two different texts embedded identically, so the vector does not depend on the input",
    );
}

#[test]
fn a_generation_context_refuses_to_embed_rather_than_answering_wrongly() {
    // The converse, and it is the one that matters for correctness rather than
    // for liveness. A context built WITHOUT `embeddings` has no pooled output,
    // and the failure mode to avoid is returning whatever happens to be in the
    // buffer: a plausible vector, silently wrong, feeding a cosine similarity
    // that still looks like a similarity.
    ensure_backends();

    let Some(path) = test_gguf() else {
        eprintln!("{NO_GGUF}");
        return;
    };

    let model = Arc::new(
        Model::load(&path.to_string_lossy(), &ModelOptions::default())
            .unwrap_or_else(|e| panic!("model load: {}", e.message)),
    );
    let mut ctx = Context::new(Arc::clone(&model), &ContextOptions::default())
        .unwrap_or_else(|e| panic!("context: {}", e.message));

    let tokens = model
        .tokenize("the quick brown fox", true, false)
        .unwrap_or_else(|e| panic!("tokenize: {}", e.message));

    assert!(
        ctx.embed(&tokens).is_err(),
        "a generation context produced an embedding, so nothing distinguishes a real pooled \
         vector from an unwritten buffer",
    );
}

// ---------------------------------------------------------------------------
// T6: the vision (libmtmd) linkage
// ---------------------------------------------------------------------------

#[test]
fn loading_a_language_model_as_a_projector_fails_instead_of_crashing() {
    // Guards that `libmtmd` is LINKED and reachable across the ABI at all, which
    // nothing else here touches: the shim linked `libllama` + `ggml` + `common`
    // and no mtmd for the whole port, which is why a local model could not see
    // an image while the catalog advertised `vision`.
    //
    // The observable is a clean failure rather than a success, because the only
    // GGUF in the tree is a tiny LANGUAGE model and a projector it is not. That
    // still exercises everything that matters here: the symbol resolves, the
    // call crosses the boundary, mtmd inspects the file, and the refusal comes
    // back as an error message rather than an abort. A missing `libmtmd` would
    // not reach this point at all, it would fail to link.
    ensure_backends();

    let Some(path) = test_gguf() else {
        eprintln!("{NO_GGUF}");
        return;
    };

    let model = Arc::new(
        Model::load(&path.to_string_lossy(), &ModelOptions::default())
            .unwrap_or_else(|e| panic!("model load: {}", e.message)),
    );

    let loaded = Mtmd::load(
        Arc::clone(&model),
        &path.to_string_lossy(),
        &MtmdOptions {
            n_threads: 2,
            use_gpu: false,
        },
    );

    let err = match loaded {
        Ok(_) => panic!(
            "a language model loaded as a multimodal projector, so nothing distinguishes a real \
             projector from any other GGUF"
        ),
        Err(err) => err,
    };
    // The path, because a two-file model gives a user two things to check and
    // "failed to load" names neither of them.
    assert!(
        err.message.contains("projector") && err.message.contains("test.gguf"),
        "the failure must name what failed and which file: {}",
        err.message,
    );
}
