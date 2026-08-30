//! The memories suite.
//!
//! The relevance arithmetic is tested against bit patterns from a frozen
//! fixture, never approximately: an approximate comparison passes on the
//! `f32`-accumulating implementation the fixture exists to catch. See
//! [`fixture`].

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use serde_json::{json, Value};
use tempfile::TempDir;

use crate::db::{migrate, Db};
use crate::paths::Paths;
use crate::secrets::Vault;
use crate::settings::Settings;
use crate::transport::frame_bus::FrameBus;

use super::store::MemoriesStore;
use crate::contract::api::MemoryKind;

// --- shared fixtures ---------------------------------------------------------

struct Env {
    _tmp: TempDir,
    paths: Arc<Paths>,
    db: Db,
    settings: Settings,
    vault: Vault,
    store: MemoriesStore,
}

fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::rooted(tmp.path());
    paths.ensure_dirs().unwrap();
    let paths = Arc::new(paths);
    let db = Db::open_in_memory().unwrap();
    db.call_blocking(|conn| migrate(conn)).unwrap();
    let frames = Arc::new(FrameBus::new());
    let settings = Settings::with_file(paths.settings_file(), db.clone(), Arc::clone(&frames));
    let vault = Vault::with_store(
        paths.secrets_enc_file(),
        Arc::new(crate::secrets::testing::MemoryStore::new()),
    );
    let store = MemoriesStore::new(Arc::clone(&paths), db.clone());
    Env {
        _tmp: tmp,
        paths,
        db,
        settings,
        vault,
        store,
    }
}

impl Env {
    fn memories_dir(&self) -> std::path::PathBuf {
        self.paths.memories_dir()
    }

    fn embedding(&self) -> super::embedding::Embedding {
        super::embedding::Embedding::unwired(self.settings.clone(), self.vault.clone())
    }
}

// --- the arithmetic fixture --------------------------------------------------

mod fixture {
    //! Bit-exact comparison against the frozen `testdata/relevance_vectors.json`.
    //!
    //! Two properties, and the second is what makes the first mean anything:
    //! every recorded case matches as IEEE-754 bits, AND the fixture still
    //! DISCRIMINATES, in that enough of its cases answer differently under an
    //! `f32` accumulator or a double-rounding normalisation that a regression
    //! could not slip through. The count is re-checked here so a hand-edited or
    //! partially regenerated file cannot quietly stop guarding anything.

    use super::*;
    use crate::memories::embedding::{l2_normalize, LocalEmbedder, EMBED_REPO};
    use crate::memories::relevance::{embed_source_hash, memory_embed_text, top_k_by_similarity};

    const VECTORS: &str = include_str!("testdata/relevance_vectors.json");

    fn doc() -> Value {
        serde_json::from_str(VECTORS).unwrap()
    }

    fn f32_of(value: &Value) -> f32 {
        let text = value.as_str().unwrap().trim_start_matches("0x");
        f32::from_bits(u32::from_str_radix(text, 16).unwrap())
    }

    fn f64_of(value: &Value) -> f64 {
        let text = value.as_str().unwrap().trim_start_matches("0x");
        f64::from_bits(u64::from_str_radix(text, 16).unwrap())
    }

    fn vector_of(value: &Value) -> Vec<f32> {
        value.as_array().unwrap().iter().map(f32_of).collect()
    }

    #[test]
    fn cosine_matches_the_typescript_bit_for_bit() {
        let doc = doc();
        let cases = doc["cosine"].as_array().unwrap();
        assert!(cases.len() >= 12, "the fixture lost cases");
        let mut discriminating = 0;
        for case in cases {
            let label = case["label"].as_str().unwrap();
            let a = vector_of(&case["a"]);
            let b = vector_of(&case["b"]);
            let want = f64_of(&case["expect"]);
            let got = crate::memories::relevance::cosine_normalized(&a, &b).unwrap();
            // Bits, not epsilon. An epsilon here would accept the f32
            // accumulator and the whole fixture would be decoration.
            assert_eq!(
                got.to_bits(),
                want.to_bits(),
                "{label}: got {got:e} ({:#x}), want {want:e} ({:#x})",
                got.to_bits(),
                want.to_bits(),
            );
            if case["discriminates"].as_bool().unwrap_or(false) {
                discriminating += 1;
            }
        }
        assert!(
            discriminating >= 4,
            "only {discriminating} cosine cases distinguish an f32 accumulator from an f64 one; \
             the fixture would pass on the broken implementation. Regenerate it with the emitter, \
             which refuses to write one this weak.",
        );
    }

    #[test]
    fn top_k_matches_the_typescript_including_tie_order() {
        for case in doc()["topK"].as_array().unwrap() {
            let label = case["label"].as_str().unwrap();
            let query = vector_of(&case["query"]);
            let items: Vec<(String, Vec<f32>)> = case["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| {
                    (
                        i["item"].as_str().unwrap().to_string(),
                        vector_of(&i["vector"]),
                    )
                })
                .collect();
            let top_k = case["topK"].as_u64().unwrap() as usize;
            let scored = top_k_by_similarity(&query, items, top_k).unwrap();

            let want_items: Vec<&str> = case["expectItems"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            let got_items: Vec<&str> = scored.iter().map(|s| s.item.as_str()).collect();
            assert_eq!(got_items, want_items, "{label}: wrong order");

            for (got, want) in scored.iter().zip(case["expectScores"].as_array().unwrap()) {
                assert_eq!(
                    got.score.to_bits(),
                    f64_of(want).to_bits(),
                    "{label}: score for {}",
                    got.item
                );
            }
        }
    }

    #[test]
    fn l2_normalize_matches_the_typescript_bit_for_bit() {
        let doc = doc();
        let cases = doc["normalize"].as_array().unwrap();
        let mut discriminating = 0;
        for case in cases {
            let label = case["label"].as_str().unwrap();
            let mut got = vector_of(&case["input"]);
            l2_normalize(&mut got);
            let want = vector_of(&case["expect"]);
            let got_bits: Vec<u32> = got.iter().map(|x| x.to_bits()).collect();
            let want_bits: Vec<u32> = want.iter().map(|x| x.to_bits()).collect();
            assert_eq!(got_bits, want_bits, "{label}");
            if case["discriminates"].as_bool().unwrap_or(false) {
                discriminating += 1;
            }
        }
        assert!(
            discriminating >= 3,
            "only {discriminating} normalisation cases distinguish one rounding from two",
        );
    }

    #[test]
    fn embed_source_hash_matches_the_typescript() {
        for case in doc()["embedSourceHash"].as_array().unwrap() {
            let model_id = case["modelId"].as_str().unwrap();
            let text = case["text"].as_str().unwrap();
            assert_eq!(
                embed_source_hash(model_id, text),
                case["expect"].as_str().unwrap(),
                "hash for {model_id:?} / {text:?}",
            );
        }
    }

    #[test]
    fn memory_embed_text_matches_the_typescript() {
        for case in doc()["memoryEmbedText"].as_array().unwrap() {
            assert_eq!(
                memory_embed_text(
                    case["title"].as_str().unwrap(),
                    case["summary"].as_str().unwrap()
                ),
                case["expect"].as_str().unwrap(),
            );
        }
    }

    /// The local model's identity is hashed into every stored
    /// `embedding_source_hash`, so a core that spells it differently reads a real
    /// install's whole vector store as stale and stops injecting memories, with
    /// no error anywhere. Pinned against the fixture's own value rather than a
    /// copy made here.
    #[test]
    fn the_local_embed_identity_is_the_string_the_typescript_emits() {
        let doc = doc();
        assert_eq!(
            EMBED_REPO,
            doc["defaultEmbedModelId"].as_str().unwrap(),
            "EMBED_REPO no longer matches what @loca/shared emits",
        );
        // The trait's default is the constant, so an implementation that does
        // not think about it cannot get it wrong.
        assert_eq!(
            crate::memories::embedding::NoLocalEmbedder.model_id(),
            EMBED_REPO,
        );
        // The composition is sha256 over `${modelId}\n${text}`, taken from the
        // case the emitter computed rather than re-derived here.
        let case = &doc["defaultIdentityHash"];
        assert_eq!(
            embed_source_hash(EMBED_REPO, case["text"].as_str().unwrap()),
            case["expect"].as_str().unwrap(),
        );
    }

    /// Change the identity by one character and every stored hash misses.
    #[test]
    fn a_different_identity_spelling_invalidates_every_stored_hash() {
        let text = memory_embed_text("Recipes", "Pasta recipes and cooking notes.");
        let right = embed_source_hash(EMBED_REPO, &text);
        for wrong in [
            "all-MiniLM-L6-v2-Q8_0.gguf",
            "@second-state/All-MiniLM-L6-v2-Embedding-GGUF",
            "@second-state/all-minilm-l6-v2-embedding-gguf/main",
            "local",
        ] {
            assert_ne!(
                embed_source_hash(wrong, &text),
                right,
                "{wrong} produced the same hash, which would hide the divergence",
            );
        }
    }
}

// --- relevance, beyond the fixture -------------------------------------------

mod relevance {
    use crate::memories::relevance::{
        cosine_normalized, tool_embed_text, tool_name_mentioned, top_k_by_similarity,
    };

    #[test]
    fn a_width_mismatch_is_an_error_rather_than_a_truncation() {
        let err = cosine_normalized(&[1.0, 0.0], &[1.0, 0.0, 0.0]).unwrap_err();
        assert!(
            err.message.contains("vector length mismatch"),
            "{}",
            err.message
        );
    }

    /// Why callers pre-filter by width: one stale vector otherwise takes the
    /// whole batch with it. Pinned so the pre-filter in `injection` cannot be
    /// removed as redundant.
    #[test]
    fn one_incomparable_vector_aborts_the_whole_batch() {
        let query = vec![1.0f32, 0.0];
        let items = vec![
            ("a".to_string(), vec![1.0f32, 0.0]),
            ("stale".to_string(), vec![0.0f32, 0.0, 0.0]),
            ("b".to_string(), vec![0.0f32, 1.0]),
        ];
        assert!(top_k_by_similarity(&query, items.clone(), 5).is_err());

        // Pre-filtered, the stale one reads as "not embedded" and the rest score.
        let filtered: Vec<_> = items
            .into_iter()
            .filter(|(_, v)| v.len() == query.len())
            .collect();
        let scored = top_k_by_similarity(&query, filtered, 5).unwrap();
        assert_eq!(scored.len(), 2);
        assert_eq!(scored[0].item, "a");
    }

    #[test]
    fn tool_embed_text_leads_with_the_humanised_name() {
        assert_eq!(
            tool_embed_text("web_search", "Search the web", &["find something"]),
            "web search\nSearch the web\nfind something",
        );
        assert_eq!(
            tool_embed_text("read-file", "Read a file", &[] as &[&str]),
            "read file\nRead a file\n",
        );
    }

    #[test]
    fn tool_name_mentioned_is_contiguous_and_word_bounded() {
        assert!(tool_name_mentioned(
            "please run web search now",
            "web_search"
        ));
        assert!(tool_name_mentioned("use WEB-SEARCH", "web_search"));
        assert!(!tool_name_mentioned("open the app store", "open_app"));
        assert!(!tool_name_mentioned("websearch", "web_search"));
        // Too short to be anything but noise.
        assert!(!tool_name_mentioned("i am a", "a_b"));
    }
}

// --- SKILL.md ----------------------------------------------------------------

mod skill_frontmatter {
    use crate::memories::skill::parse_skill;

    #[test]
    fn a_file_without_frontmatter_is_all_body() {
        let parts = parse_skill("# Steps\ndo the thing");
        assert_eq!(parts.description, "");
        assert!(parts.suggested_tools.is_empty());
        assert_eq!(parts.body, "# Steps\ndo the thing");
    }

    #[test]
    fn description_and_an_inline_tool_array() {
        let parts = parse_skill(
            "---\ndescription: Files a bug report\nsuggested-tools: [web_search, write_memory]\n---\n# Steps\ndo the thing",
        );
        assert_eq!(parts.description, "Files a bug report");
        assert_eq!(parts.suggested_tools, ["web_search", "write_memory"]);
        assert_eq!(parts.body, "# Steps\ndo the thing");
    }

    #[test]
    fn a_block_list_of_tools_ends_at_the_first_non_item() {
        let parts = parse_skill(
            "---\nsuggested-tools:\n  - web_search\n\n  - write_memory\nname: ignored\n---\nbody",
        );
        assert_eq!(parts.suggested_tools, ["web_search", "write_memory"]);
        assert_eq!(parts.body, "body");
    }

    /// Anything that is not one of the two keys is tolerated, so a community
    /// `SKILL.md` drops in unchanged.
    #[test]
    fn unknown_keys_are_ignored_rather_than_rejected() {
        let parts = parse_skill(
            "---\nname: File Bug\nlicense: MIT\ndescription: \"Files a bug: quickly\"\n---\nbody",
        );
        assert_eq!(parts.description, "Files a bug: quickly");
        assert_eq!(parts.body, "body");
    }

    #[test]
    fn crlf_and_the_separating_blank_line_are_handled() {
        let parts = parse_skill("---\r\ndescription: Windows\r\n---\r\n\r\nbody\r\nmore");
        assert_eq!(parts.description, "Windows");
        assert_eq!(parts.body, "body\r\nmore");
    }

    /// The block is lazy: it ends at the FIRST closing delimiter, so a `---`
    /// inside the body does not swallow it.
    #[test]
    fn the_block_ends_at_the_first_delimiter() {
        let parts = parse_skill("---\ndescription: One\n---\nbody\n---\nnot frontmatter");
        assert_eq!(parts.description, "One");
        assert_eq!(parts.body, "body\n---\nnot frontmatter");
    }

    /// An opening delimiter followed immediately by another is not a block: the
    /// lazy group needs the newline before the closer.
    #[test]
    fn an_empty_pair_of_delimiters_is_not_frontmatter() {
        let parts = parse_skill("---\n---\nbody");
        assert_eq!(parts.body, "---\n---\nbody");
    }

    /// The closing `---` does NOT have to end its line: the trailing `\r?\n?` is
    /// optional, so `----`, `--- ` and `---trailing` all close the block and
    /// their extra characters head the body. The same parser runs in the
    /// client's memory editor, so a core that rejected these would seed no
    /// summary for a file the client shows as having one.
    #[test]
    fn a_closing_delimiter_need_not_end_its_line() {
        for (input, body) in [
            ("---\ndescription: hi\n----\nbody", "-\nbody"),
            ("---\ndescription: hi\n--- \nbody", " \nbody"),
            ("---\ndescription: hi\n---trailing\nbody", "trailing\nbody"),
            ("---\r\ndescription: hi\r\n----\r\nbody", "-\r\nbody"),
        ] {
            let parts = parse_skill(input);
            assert_eq!(parts.description, "hi", "{input:?}");
            assert_eq!(parts.body, body, "{input:?}");
        }
    }

    /// `\r?\n?` takes a lone `\r` too, so a classic-Mac line ending after the
    /// delimiter is consumed rather than left at the head of the body.
    #[test]
    fn a_lone_carriage_return_after_the_delimiter_is_consumed() {
        let parts = parse_skill("---\ndescription: hi\n---\rbody");
        assert_eq!(parts.description, "hi");
        assert_eq!(parts.body, "body");
    }

    #[test]
    fn quotes_are_stripped_one_from_each_end() {
        let parts =
            parse_skill("---\ndescription: \"quoted\"\nsuggested-tools: ['a', \"b\"]\n---\nx");
        assert_eq!(parts.description, "quoted");
        assert_eq!(parts.suggested_tools, ["a", "b"]);
    }
}

// --- naming rules ------------------------------------------------------------

mod content_rules {
    use crate::memories::content::{check_skill_file_name, pretty_title, slugify};

    #[test]
    fn slugify_matches_the_typescript_rules() {
        assert_eq!(slugify("Meeting Notes: Q3!"), "meeting-notes-q3");
        assert_eq!(slugify("  leading and trailing  "), "leading-and-trailing");
        assert_eq!(slugify("!!!"), "memory");
        assert_eq!(slugify(""), "memory");
        assert_eq!(slugify("Ünïcödé"), "n-c-d");
        assert_eq!(slugify("Already-Slugged-2"), "already-slugged-2");
        // Cut to 64, and the cut happens after the trim, so a trailing dash can
        // survive it.
        assert_eq!(slugify(&"a".repeat(100)).len(), 64);
    }

    #[test]
    fn pretty_title_takes_the_last_segment() {
        assert_eq!(pretty_title("skills/file-bug"), "File Bug");
        assert_eq!(pretty_title("knowledge/release-notes.md"), "Release Notes");
        assert_eq!(pretty_title("deep/nested/some_thing.MD"), "Some Thing");
        assert_eq!(pretty_title("trailing/slash/"), "Slash");
        assert_eq!(pretty_title(""), "Memory");
    }

    /// The guard is a name-shape check rather than a canonicalisation, and
    /// `SKILL.md` is refused in any casing: on a case-insensitive filesystem a
    /// bundled `skill.md` would otherwise overwrite the instruction body.
    #[test]
    fn the_skill_file_guard_refuses_traversal_and_the_body_itself() {
        assert!(check_skill_file_name("template.md").is_ok());
        for bad in [
            "",
            "   ",
            "SKILL.md",
            "skill.md",
            "SkIlL.Md",
            "../escape",
            "sub/file.md",
            "sub\\file.md",
            "..",
        ] {
            assert!(
                check_skill_file_name(bad).is_err(),
                "{bad:?} was accepted as a bundled file name"
            );
        }
    }
}

// --- the store ---------------------------------------------------------------

mod store {
    use super::*;

    #[tokio::test]
    async fn create_get_list_and_a_case_insensitive_title_conflict() {
        let env = env();
        let doc = env
            .store
            .create(
                MemoryKind::Knowledge,
                "Meeting Notes: Q3!",
                "# Notes\nhello",
            )
            .await
            .unwrap();
        assert_eq!(doc.filename, "meeting-notes-q3.md");
        assert_eq!(doc.content, "# Notes\nhello");
        assert_eq!(doc.provider, super::super::store::USER_MEMORY_PROVIDER);
        assert!(doc.enabled);
        assert_eq!(doc.summary_stale, Some(true));

        let titles: Vec<String> = env
            .store
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.title)
            .collect();
        assert_eq!(titles, ["Meeting Notes: Q3!"]);

        assert_eq!(
            env.store
                .get_by_title("MEETING NOTES: Q3!")
                .await
                .unwrap()
                .unwrap()
                .id,
            doc.id
        );

        // Tools address memories by title, so "Notes" beside "notes" would be
        // two rows they cannot tell apart.
        for clash in ["Meeting Notes: Q3!", "meeting notes: q3!"] {
            let err = env
                .store
                .create(MemoryKind::Knowledge, clash, "again")
                .await
                .unwrap_err();
            assert!(err.message.contains("already exists"), "{}", err.message);
        }

        // Same slug, different title: the filename gets a numeric suffix.
        let sibling = env
            .store
            .create(MemoryKind::Knowledge, "Meeting Notes Q3", "x")
            .await
            .unwrap();
        assert_eq!(sibling.filename, "meeting-notes-q3-2.md");
    }

    /// SQLite's `NOCASE` is ASCII-only, and a Unicode-aware comparison would be
    /// an "improvement" that merges two titles differing only in an accent's
    /// case, which must stay two rows.
    #[tokio::test]
    async fn title_collision_is_ascii_case_folding_only() {
        let env = env();
        env.store
            .create(MemoryKind::Knowledge, "Ärger", "a")
            .await
            .unwrap();
        // Would collide under a Unicode lowercase comparison; does not under
        // NOCASE.
        let second = env.store.create(MemoryKind::Knowledge, "ärger", "b").await;
        assert!(
            second.is_ok(),
            "a Unicode-aware title comparison crept in: {:?}",
            second.err()
        );
    }

    #[tokio::test]
    async fn a_filename_taken_on_disk_but_not_in_the_database_is_skipped() {
        let env = env();
        // A stray file with no row, which is what an interrupted create leaves.
        std::fs::write(env.memories_dir().join("stray.md"), "orphan").unwrap();
        let doc = env
            .store
            .create(MemoryKind::Knowledge, "Stray", "mine")
            .await
            .unwrap();
        assert_eq!(doc.filename, "stray-2.md");
        assert_eq!(
            std::fs::read_to_string(env.memories_dir().join("stray.md")).unwrap(),
            "orphan",
            "the stray file was overwritten",
        );
    }

    #[tokio::test]
    async fn skills_are_folders_with_frontmatter_and_bundled_files() {
        let env = env();
        let body = "---\ndescription: Files a bug report\nsuggested-tools: [web_search, write_memory]\n---\n# Steps\ndo the thing";
        let skill = env
            .store
            .create(MemoryKind::Skill, "File Bug", body)
            .await
            .unwrap();
        assert_eq!(skill.kind, MemoryKind::Skill);
        assert_eq!(skill.filename, "file-bug");
        assert_eq!(skill.content, body);
        // The frontmatter seeds the summary, and it is not stale: it was derived
        // from exactly this content.
        assert_eq!(skill.summary.as_deref(), Some("Files a bug report"));
        assert_eq!(skill.summary_stale, Some(false));
        assert_eq!(
            skill.suggested_tools.as_deref(),
            Some(&["web_search".to_string(), "write_memory".to_string()][..])
        );
        assert_eq!(skill.files.as_deref(), Some(&[][..]));

        std::fs::write(
            env.memories_dir().join("file-bug").join("template.md"),
            "bug template",
        )
        .unwrap();
        assert_eq!(
            env.store.get(&skill.id).await.unwrap().files,
            Some(vec!["template.md".to_string()])
        );
        assert_eq!(
            env.store.get_file(&skill.id, "template.md").await.unwrap(),
            "bug template"
        );
        assert!(env.store.get_file(&skill.id, "../secret").await.is_err());
    }

    /// A skill declaring no tools reports the field ABSENT, not empty; the
    /// client renders the two differently.
    #[tokio::test]
    async fn a_skill_without_tools_omits_the_field_entirely() {
        let env = env();
        let skill = env
            .store
            .create(MemoryKind::Skill, "Bare", "# Steps")
            .await
            .unwrap();
        assert_eq!(skill.suggested_tools, None);
        let json = serde_json::to_value(&skill).unwrap();
        assert!(json.get("suggestedTools").is_none());
        // Files, by contrast, is present and empty for a skill with none.
        assert_eq!(json["files"], json!([]));
    }

    #[tokio::test]
    async fn write_and_delete_manage_a_skills_bundled_files() {
        let env = env();
        let skill = env
            .store
            .create(MemoryKind::Skill, "Web Research", "# Steps\nsearch")
            .await
            .unwrap();
        let id = &skill.id;
        env.store
            .write_file(id, "checklist.md", "1. cross-check")
            .await
            .unwrap();
        assert_eq!(
            env.store.get(id).await.unwrap().files,
            Some(vec!["checklist.md".to_string()])
        );
        env.store
            .write_file(id, "checklist.md", "1. updated")
            .await
            .unwrap();
        assert_eq!(
            env.store.get_file(id, "checklist.md").await.unwrap(),
            "1. updated"
        );

        for bad in ["SKILL.md", "skill.md", "../escape", "sub/file.md", "  "] {
            assert!(env.store.write_file(id, bad, "x").await.is_err(), "{bad:?}");
        }

        env.store.delete_file(id, "checklist.md").await.unwrap();
        assert_eq!(env.store.get(id).await.unwrap().files, Some(vec![]));
        // Deleting it again is not silently fine: the row is gone from disk.
        assert!(env.store.delete_file(id, "checklist.md").await.is_err());

        // Knowledge has no bundled files at all.
        let note = env
            .store
            .create(MemoryKind::Knowledge, "Note", "body")
            .await
            .unwrap();
        assert!(env.store.write_file(&note.id, "x.md", "y").await.is_err());
    }

    #[tokio::test]
    async fn summary_staleness_follows_the_pinned_hash() {
        let env = env();
        let doc = env
            .store
            .create(MemoryKind::Knowledge, "Pinned", "body")
            .await
            .unwrap();
        let id = &doc.id;
        assert_eq!(env.store.get(id).await.unwrap().summary_stale, Some(true));

        env.store
            .set_summary(id, "a summary", &doc.content_hash)
            .await
            .unwrap();
        assert_eq!(env.store.get(id).await.unwrap().summary_stale, Some(false));

        env.store.replace_content(id, "new body").await.unwrap();
        assert_eq!(env.store.get(id).await.unwrap().summary_stale, Some(true));
    }

    #[tokio::test]
    async fn edit_content_requires_exactly_one_match() {
        let env = env();
        let doc = env
            .store
            .create(
                MemoryKind::Knowledge,
                "Todo",
                "- alpha\n- beta\n- alpha tail",
            )
            .await
            .unwrap();
        let id = &doc.id;
        let edit = env
            .store
            .edit_content(id, "- beta", "- gamma")
            .await
            .unwrap();
        assert_eq!(edit.before, "- alpha\n- beta\n- alpha tail");
        assert_eq!(edit.after, "- alpha\n- gamma\n- alpha tail");
        assert_eq!(edit.memory.content, edit.after);

        let missing = env
            .store
            .edit_content(id, "missing", "x")
            .await
            .unwrap_err();
        assert!(missing.message.contains("not found"), "{}", missing.message);
        let twice = env
            .store
            .edit_content(id, "- alpha", "x")
            .await
            .unwrap_err();
        assert!(
            twice.message.contains("more than once"),
            "{}",
            twice.message
        );
        assert!(env.store.edit_content(id, "", "x").await.is_err());
    }

    #[tokio::test]
    async fn a_vanished_file_surfaces_as_not_found_rather_than_empty_content() {
        let env = env();
        let doc = env
            .store
            .create(MemoryKind::Knowledge, "Ghost", "body")
            .await
            .unwrap();
        std::fs::remove_file(env.memories_dir().join(&doc.filename)).unwrap();
        let err = env.store.get(&doc.id).await.unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::NotFound);
        assert!(err.message.contains("missing on disk"), "{}", err.message);
    }

    #[tokio::test]
    async fn rename_keeps_the_file_and_delete_removes_it() {
        let env = env();
        let doc = env
            .store
            .create(MemoryKind::Knowledge, "Old Title", "body")
            .await
            .unwrap();
        let renamed = env.store.rename(&doc.id, "New Title").await.unwrap();
        assert_eq!(renamed.title, "New Title");
        assert_eq!(renamed.filename, doc.filename);
        // Renaming to the title it already has is a no-op, not a conflict.
        assert_eq!(
            env.store.rename(&doc.id, "New Title").await.unwrap().title,
            "New Title"
        );

        env.store.delete(&doc.id).await.unwrap();
        assert!(env.store.list().await.unwrap().is_empty());
        assert!(!env.memories_dir().join(&doc.filename).exists());
    }

    #[tokio::test]
    async fn renaming_onto_another_memorys_title_is_a_conflict() {
        let env = env();
        env.store
            .create(MemoryKind::Knowledge, "Taken", "a")
            .await
            .unwrap();
        let other = env
            .store
            .create(MemoryKind::Knowledge, "Free", "b")
            .await
            .unwrap();
        let err = env.store.rename(&other.id, "taken").await.unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::Conflict);
    }

    #[tokio::test]
    async fn enable_and_disable_toggle_participation() {
        let env = env();
        let doc = env
            .store
            .create(MemoryKind::Knowledge, "Toggle", "body")
            .await
            .unwrap();
        assert!(doc.enabled);
        assert!(!env.store.set_enabled(&doc.id, false).await.unwrap().enabled);
        assert!(!env.store.get(&doc.id).await.unwrap().enabled);
    }

    #[tokio::test]
    async fn rescan_adds_removes_and_flags_changed_files() {
        let env = env();
        let kept = env
            .store
            .create(MemoryKind::Knowledge, "Kept", "kept body")
            .await
            .unwrap();
        let dropped = env
            .store
            .create(MemoryKind::Knowledge, "Dropped", "dropped body")
            .await
            .unwrap();
        let dir = env.memories_dir();
        std::fs::write(dir.join("handwritten.md"), "external content").unwrap();
        std::fs::remove_file(dir.join(&dropped.filename)).unwrap();
        std::fs::write(dir.join(&kept.filename), "kept body v2").unwrap();

        let result = env.store.rescan().await.unwrap();
        assert_eq!(result.added, 1);
        assert_eq!(result.removed, 1);
        assert_eq!(result.changed, 1);

        let titles: Vec<String> = env
            .store
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.title)
            .collect();
        assert!(titles.contains(&"handwritten".to_string()));
        assert!(!titles.contains(&"Dropped".to_string()));
        assert_eq!(
            env.store.get(&kept.id).await.unwrap().content,
            "kept body v2"
        );

        // The changed hash invalidates any pinned summary.
        let state = env
            .store
            .list_index_states()
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.id == kept.id)
            .unwrap();
        assert_ne!(
            state.summary_source_hash.as_deref(),
            Some(state.content_hash.as_str())
        );
    }

    #[tokio::test]
    async fn rescan_on_an_untouched_tree_reports_nothing() {
        let env = env();
        assert_eq!(env.store.rescan().await.unwrap().added, 0);
        env.store
            .create(MemoryKind::Knowledge, "Settled", "body")
            .await
            .unwrap();
        let second = env.store.rescan().await.unwrap();
        assert_eq!((second.added, second.removed, second.changed), (0, 0, 0));
    }

    #[tokio::test]
    async fn rescan_adopts_a_hand_placed_skill_folder() {
        let env = env();
        let dir = env.memories_dir().join("hand-made");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            "---\ndescription: By hand\n---\nsteps",
        )
        .unwrap();
        assert_eq!(env.store.rescan().await.unwrap().added, 1);
        let adopted = &env.store.list().await.unwrap()[0];
        assert_eq!(adopted.kind, MemoryKind::Skill);
        assert_eq!(adopted.title, "hand-made");
        assert_eq!(adopted.summary.as_deref(), Some("By hand"));
    }

    /// A folder with no `SKILL.md` is not a memory, and a rescan must not adopt
    /// one or trip over it.
    #[tokio::test]
    async fn rescan_ignores_a_folder_without_a_skill_body() {
        let env = env();
        std::fs::create_dir_all(env.memories_dir().join("not-a-skill")).unwrap();
        assert_eq!(env.store.rescan().await.unwrap().added, 0);
        assert!(env.store.list().await.unwrap().is_empty());
    }

    /// A name whose kind flipped is re-adopted in the SAME pass: the row is
    /// dropped and the disk entry left for the adoption loop, so one rescan
    /// reports `{added: 1, removed: 1}`. Consuming the entry while dropping the
    /// row would lose the addition until a second rescan.
    #[tokio::test]
    async fn rescan_readopts_a_name_whose_kind_flipped_in_one_pass() {
        let env = env();
        let before = env
            .store
            .create(MemoryKind::Knowledge, "Notes", "as knowledge")
            .await
            .unwrap();
        assert_eq!(before.filename, "notes.md");

        // The same name is now a skill folder: `notes.md/SKILL.md`.
        let file = env.memories_dir().join("notes.md");
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir_all(&file).unwrap();
        std::fs::write(
            file.join("SKILL.md"),
            "---\ndescription: now a skill\n---\nsteps",
        )
        .unwrap();

        let counts = env.store.rescan().await.unwrap();
        assert_eq!(
            (counts.added, counts.removed, counts.changed),
            (1, 1, 0),
            "the flipped entry was consumed by the removal instead of being re-adopted"
        );
        let rows = env.store.list().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, MemoryKind::Skill);
        assert_eq!(rows[0].filename, "notes.md");
        assert_eq!(rows[0].summary.as_deref(), Some("now a skill"));
        assert_ne!(rows[0].id, before.id, "the row should be a new one");
    }

    /// A duplicate stem gets a suffixed title rather than failing the whole
    /// rescan on the `UNIQUE` constraint.
    #[tokio::test]
    async fn rescan_deduplicates_an_adopted_title() {
        let env = env();
        // A memory whose TITLE is "notes" but whose filename is not notes.md, so
        // the hand-placed file below is a new file rather than an edit of this
        // one. A rename keeps the on-disk name, which is what makes the two
        // diverge.
        let existing = env
            .store
            .create(MemoryKind::Knowledge, "temp", "in the app")
            .await
            .unwrap();
        env.store.rename(&existing.id, "notes").await.unwrap();
        std::fs::write(env.memories_dir().join("notes.md"), "by hand").unwrap();
        assert_eq!(env.store.rescan().await.unwrap().added, 1);
        let titles: Vec<String> = env
            .store
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.title)
            .collect();
        assert!(titles.contains(&"notes (2)".to_string()), "{titles:?}");
    }

    #[tokio::test]
    async fn extension_memories_are_read_only_disabled_and_gated() {
        let env = env();
        env.store
            .create(MemoryKind::Knowledge, "Mine", "body")
            .await
            .unwrap();

        let base = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(base.path().join("ext-a")).unwrap();
        std::fs::write(base.path().join("ext-a").join("guide.md"), "# Guide").unwrap();
        env.store
            .register_extension_memories(
                "ext-a",
                base.path().to_path_buf(),
                &[super::super::store::MemoryDecl {
                    kind: MemoryKind::Knowledge,
                    path: "guide.md".to_string(),
                }],
            )
            .await
            .unwrap();

        let mut titles: Vec<String> = env
            .store
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.title)
            .collect();
        titles.sort();
        assert_eq!(titles, ["Guide", "Mine"]);

        let guide = env
            .store
            .list()
            .await
            .unwrap()
            .into_iter()
            .find(|m| m.title == "Guide")
            .unwrap();
        assert_eq!(guide.provider, "ext-a");
        // Opt-in: an extension's memory arrives disabled.
        assert!(!guide.enabled);
        // Read-only, but silenceable.
        assert!(env.store.replace_content(&guide.id, "x").await.is_err());
        assert!(env.store.delete(&guide.id).await.is_err());
        assert!(env.store.set_enabled(&guide.id, true).await.is_ok());
        // Its content still resolves, through the registered base directory.
        assert_eq!(env.store.get(&guide.id).await.unwrap().content, "# Guide");

        // The gate hides it without dropping the row.
        //
        // This drives a FAKE, so it passes whether or not anything installs a
        // gate in production. The test that closes that gap is
        // `extensions::tests::an_uninstalled_extensions_memories_leave_every_listing`.
        struct Gate;
        #[async_trait::async_trait]
        impl super::super::store::ProviderGate for Gate {
            async fn active_providers(
                &self,
            ) -> crate::error::Result<std::collections::HashSet<String>> {
                Ok(["ext-b".to_string()].into_iter().collect())
            }
        }
        env.store.set_provider_gate(Some(Arc::new(Gate)));
        let visible: Vec<String> = env
            .store
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.title)
            .collect();
        assert_eq!(visible, ["Mine"]);
        assert_eq!(env.store.all_rows().await.unwrap().len(), 2);

        env.store.set_provider_gate(None);
        assert_eq!(env.store.list().await.unwrap().len(), 2);

        env.store.remove_extension_memories("ext-a").await.unwrap();
        assert_eq!(env.store.list().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_declared_memory_that_escapes_the_install_dir_is_skipped() {
        let env = env();
        let base = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(base.path().join("ext-a")).unwrap();
        std::fs::write(base.path().join("outside.md"), "secret").unwrap();
        env.store
            .register_extension_memories(
                "ext-a",
                base.path().to_path_buf(),
                &[
                    super::super::store::MemoryDecl {
                        kind: MemoryKind::Knowledge,
                        path: "../outside.md".to_string(),
                    },
                    super::super::store::MemoryDecl {
                        kind: MemoryKind::Knowledge,
                        path: "missing.md".to_string(),
                    },
                ],
            )
            .await
            .unwrap();
        // The escaping one was refused and the missing one was skipped rather
        // than failing the install, so nothing was registered at all.
        assert!(env.store.list().await.unwrap().is_empty());
    }

    /// Re-registering the same declarations leaves the rows alone, so a
    /// reinstall does not churn ids or lose the user's enabled toggle.
    #[tokio::test]
    async fn re_registering_preserves_the_existing_rows() {
        let env = env();
        let base = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(base.path().join("ext-a")).unwrap();
        std::fs::write(base.path().join("ext-a").join("guide.md"), "# Guide").unwrap();
        let decls = [super::super::store::MemoryDecl {
            kind: MemoryKind::Knowledge,
            path: "guide.md".to_string(),
        }];
        env.store
            .register_extension_memories("ext-a", base.path().to_path_buf(), &decls)
            .await
            .unwrap();
        let first = env.store.list().await.unwrap()[0].clone();
        env.store.set_enabled(&first.id, true).await.unwrap();

        env.store
            .register_extension_memories("ext-a", base.path().to_path_buf(), &decls)
            .await
            .unwrap();
        let second = env.store.list().await.unwrap()[0].clone();
        assert_eq!(second.id, first.id, "the row was recreated");
        assert!(second.enabled, "the user's toggle was lost");
    }

    #[tokio::test]
    async fn embeddings_round_trip_through_the_blob_column() {
        let env = env();
        let doc = env
            .store
            .create(MemoryKind::Knowledge, "Vectors", "body")
            .await
            .unwrap();
        let vector = vec![0.6f32, -0.8, 0.0, 1.0e-38];
        env.store
            .set_embedding(&doc.id, &vector, "hash-1")
            .await
            .unwrap();
        let loaded = env.store.load_all_embeddings().await.unwrap();
        assert_eq!(loaded.len(), 1);
        let (id, stored) = &loaded[0];
        assert_eq!(id, &doc.id);
        // Bit-exact: a byte-order or width mistake would still round-trip
        // "approximately" and score subtly wrong forever.
        let got: Vec<u32> = stored.vector.iter().map(|x| x.to_bits()).collect();
        let want: Vec<u32> = vector.iter().map(|x| x.to_bits()).collect();
        assert_eq!(got, want);
        assert_eq!(stored.source_hash.as_deref(), Some("hash-1"));
    }

    /// The stored blob is little-endian, which is what an existing install has
    /// on disk. Pinned against the raw bytes rather than against a round trip,
    /// because a round trip through a matching pair of mistakes still passes.
    #[tokio::test]
    async fn the_embedding_blob_is_little_endian() {
        let env = env();
        let doc = env
            .store
            .create(MemoryKind::Knowledge, "Bytes", "body")
            .await
            .unwrap();
        env.store
            .set_embedding(&doc.id, &[1.0f32], "h")
            .await
            .unwrap();
        let blob: Vec<u8> = env
            .db
            .call_blocking(|conn| {
                Ok(conn.query_row("SELECT embedding FROM memories", [], |r| r.get(0))?)
            })
            .unwrap();
        assert_eq!(blob, vec![0x00, 0x00, 0x80, 0x3f]);
    }

    /// A truncated blob loses one row rather than the whole load.
    #[tokio::test]
    async fn a_malformed_embedding_blob_is_skipped_not_fatal() {
        let env = env();
        let good = env
            .store
            .create(MemoryKind::Knowledge, "Good", "a")
            .await
            .unwrap();
        let bad = env
            .store
            .create(MemoryKind::Knowledge, "Bad", "b")
            .await
            .unwrap();
        env.store
            .set_embedding(&good.id, &[1.0, 0.0], "h")
            .await
            .unwrap();
        env.store
            .set_embedding(&bad.id, &[1.0, 0.0], "h")
            .await
            .unwrap();
        // Claim a width the blob cannot back.
        let id = bad.id.clone();
        env.db
            .call_blocking(move |conn| {
                conn.execute(
                    "UPDATE memories SET embedding_dim = 64 WHERE id = ?1",
                    [&id],
                )?;
                Ok(())
            })
            .unwrap();
        let loaded = env.store.load_all_embeddings().await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].0, good.id);
    }

    #[tokio::test]
    async fn index_states_report_both_pins() {
        let env = env();
        let doc = env
            .store
            .create(MemoryKind::Knowledge, "Indexed", "body")
            .await
            .unwrap();
        let state = &env.store.list_index_states().await.unwrap()[0];
        assert_eq!(state.content_hash, doc.content_hash);
        assert_eq!(state.summary_source_hash, None);
        assert_eq!(state.embedding_source_hash, None);

        env.store
            .set_summary(&doc.id, "s", &doc.content_hash)
            .await
            .unwrap();
        env.store.set_embedding(&doc.id, &[1.0], "e").await.unwrap();
        let state = &env.store.list_index_states().await.unwrap()[0];
        assert_eq!(
            state.summary_source_hash.as_deref(),
            Some(doc.content_hash.as_str())
        );
        assert_eq!(state.embedding_source_hash.as_deref(), Some("e"));
    }

    /// The content hash is `sha256` of exactly the bytes on disk.
    #[tokio::test]
    async fn the_content_hash_is_sha256_of_the_content() {
        let env = env();
        let doc = env
            .store
            .create(MemoryKind::Knowledge, "Hashed", "hello")
            .await
            .unwrap();
        assert_eq!(
            doc.content_hash,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
        );
    }

    /// Concurrent edits do not interleave. Without the mutation lock the two
    /// read-modify-write cycles race and one edit is lost.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_edits_are_serialised() {
        let env = env();
        let doc = env
            .store
            .create(MemoryKind::Knowledge, "Race", "line\n")
            .await
            .unwrap();
        let mut tasks = Vec::new();
        for i in 0..8 {
            let store = env.store.clone();
            let id = doc.id.clone();
            tasks.push(tokio::spawn(async move {
                store
                    .edit_content(&id, "line\n", &format!("line\nedit {i}\n"))
                    .await
            }));
        }
        let mut applied = 0;
        for task in tasks {
            if task.await.unwrap().is_ok() {
                applied += 1;
            }
        }
        // Each edit either applied cleanly or found its anchor already consumed;
        // what must never happen is a torn file.
        let content = env.store.get(&doc.id).await.unwrap().content;
        assert_eq!(
            content.lines().filter(|l| l.starts_with("edit")).count(),
            applied
        );
        assert!(content.starts_with("line\n"));
    }
}

// --- injection ---------------------------------------------------------------

mod injection {
    use super::*;
    use crate::memories::injection::{
        has_memories, memory_token_blocks, relevant_memories, relevant_memories_block,
        MemoryTokenBudget,
    };
    use crate::memories::relevance::memory_embed_text;

    /// Score and format in one call.
    async fn prompt(
        env: &Env,
        query: Option<&str>,
        vector: Option<&[f32]>,
        max: usize,
        skills: bool,
    ) -> Option<String> {
        let docs = relevant_memories(&env.store, &env.embedding(), query, vector, max, skills)
            .await
            .unwrap();
        relevant_memories_block(&docs)
    }

    #[tokio::test]
    async fn ranks_by_similarity_and_applies_the_floor() {
        let env = env();
        let embedding = env.embedding();
        let recipes = env
            .store
            .create(MemoryKind::Knowledge, "Recipes", "pasta things")
            .await
            .unwrap();
        let summary = "Pasta recipes and cooking notes.";
        env.store
            .set_summary(&recipes.id, summary, &recipes.content_hash)
            .await
            .unwrap();
        // A summarised memory must carry the hash the indexer would compute, or
        // the prompt-time freshness guard drops it as stale.
        env.store
            .set_embedding(
                &recipes.id,
                &[1.0, 0.0],
                &embedding.source_hash(&memory_embed_text("Recipes", summary)),
            )
            .await
            .unwrap();
        // No summary, so only the width guard applies to this one.
        let taxes = env
            .store
            .create(MemoryKind::Knowledge, "Taxes", "tax things")
            .await
            .unwrap();
        env.store
            .set_embedding(&taxes.id, &[0.0, 1.0], "h2")
            .await
            .unwrap();

        let block = prompt(&env, Some("dinner ideas"), Some(&[1.0, 0.0]), 3, true)
            .await
            .unwrap();
        assert!(block.contains("Recipes: Pasta recipes and cooking notes."));
        assert!(
            !block.contains("Taxes"),
            "an orthogonal memory cleared the floor"
        );
        assert!(block.contains("read_memory"));

        // maxRelevant caps the list even when both clear the floor.
        let capped = prompt(&env, Some("anything"), Some(&[0.8, 0.7]), 1, true)
            .await
            .unwrap();
        // One bullet: every entry is preceded by a newline, header included.
        assert_eq!(capped.matches("\n- ").count(), 1);
    }

    #[tokio::test]
    async fn a_stale_source_hash_is_skipped_after_an_embed_model_switch() {
        let env = env();
        let embedding = env.embedding();
        let fresh = env
            .store
            .create(MemoryKind::Knowledge, "Fresh", "fresh body")
            .await
            .unwrap();
        env.store
            .set_summary(&fresh.id, "Fresh summary.", &fresh.content_hash)
            .await
            .unwrap();
        env.store
            .set_embedding(
                &fresh.id,
                &[1.0, 0.0],
                &embedding.source_hash(&memory_embed_text("Fresh", "Fresh summary.")),
            )
            .await
            .unwrap();
        // Same width as the query, so it clears the length guard, but its hash
        // is from another model and the cosine would be meaningless.
        let stale = env
            .store
            .create(MemoryKind::Knowledge, "Stale", "stale body")
            .await
            .unwrap();
        env.store
            .set_summary(&stale.id, "Stale summary.", &stale.content_hash)
            .await
            .unwrap();
        env.store
            .set_embedding(&stale.id, &[1.0, 0.0], "hash-from-another-model")
            .await
            .unwrap();

        let block = prompt(&env, Some("anything"), Some(&[1.0, 0.0]), 5, true)
            .await
            .unwrap();
        assert!(block.contains("Fresh: Fresh summary."));
        assert!(!block.contains("Stale"));
    }

    /// The width guard, which is the one that decides whether ONE stale vector
    /// costs the turn its memories or costs only itself.
    #[tokio::test]
    async fn a_stale_width_costs_only_itself() {
        let env = env();
        let good = env
            .store
            .create(MemoryKind::Knowledge, "Good", "a")
            .await
            .unwrap();
        env.store
            .set_embedding(&good.id, &[1.0, 0.0], "h")
            .await
            .unwrap();
        let wide = env
            .store
            .create(MemoryKind::Knowledge, "Wide", "b")
            .await
            .unwrap();
        env.store
            .set_embedding(&wide.id, &[1.0; 1536], "h")
            .await
            .unwrap();

        let block = prompt(&env, Some("q"), Some(&[1.0, 0.0]), 5, true).await;
        assert!(
            block.unwrap().contains("Good"),
            "the stale-width vector took the batch with it"
        );
    }

    #[tokio::test]
    async fn nothing_indexed_or_no_query_yields_nothing() {
        let env = env();
        assert!(prompt(&env, Some("query"), Some(&[1.0]), 3, true)
            .await
            .is_none());
        env.store
            .create(MemoryKind::Knowledge, "Doc", "body")
            .await
            .unwrap();
        assert!(prompt(&env, None, Some(&[1.0]), 3, true).await.is_none());
        assert!(prompt(&env, Some(""), Some(&[1.0]), 3, true)
            .await
            .is_none());
    }

    /// With no query vector supplied and no embedding source configured, the
    /// turn degrades quietly rather than erroring: that is the state of every
    /// core before the embed model is downloaded.
    #[tokio::test]
    async fn no_embedding_source_degrades_to_nothing() {
        let env = env();
        let doc = env
            .store
            .create(MemoryKind::Knowledge, "Doc", "body")
            .await
            .unwrap();
        env.store.set_embedding(&doc.id, &[1.0], "h").await.unwrap();
        assert!(prompt(&env, Some("query"), None, 3, true).await.is_none());
    }

    #[tokio::test]
    async fn skills_are_excluded_unless_auto_relevancy_is_on() {
        let env = env();
        let embedding = env.embedding();
        let skill = env
            .store
            .create(MemoryKind::Skill, "Deploy", "# Steps")
            .await
            .unwrap();
        env.store
            .set_summary(&skill.id, "How to deploy.", &skill.content_hash)
            .await
            .unwrap();
        env.store
            .set_embedding(
                &skill.id,
                &[1.0, 0.0],
                &embedding.source_hash(&memory_embed_text("Deploy", "How to deploy.")),
            )
            .await
            .unwrap();

        assert!(prompt(&env, Some("q"), Some(&[1.0, 0.0]), 5, false)
            .await
            .is_none());
        let block = prompt(&env, Some("q"), Some(&[1.0, 0.0]), 5, true)
            .await
            .unwrap();
        assert!(block.contains("Skills that may apply"));
        assert!(block.contains("follow them"));
    }

    #[tokio::test]
    async fn a_disabled_memory_never_reaches_the_prompt() {
        let env = env();
        let doc = env
            .store
            .create(MemoryKind::Knowledge, "Off", "body")
            .await
            .unwrap();
        env.store
            .set_embedding(&doc.id, &[1.0, 0.0], "h")
            .await
            .unwrap();
        env.store.set_enabled(&doc.id, false).await.unwrap();
        assert!(prompt(&env, Some("q"), Some(&[1.0, 0.0]), 5, true)
            .await
            .is_none());
    }

    #[tokio::test]
    async fn an_unsummarised_memory_is_listed_with_a_placeholder() {
        let env = env();
        let doc = env
            .store
            .create(MemoryKind::Knowledge, "Raw", "body")
            .await
            .unwrap();
        env.store
            .set_embedding(&doc.id, &[1.0, 0.0], "h")
            .await
            .unwrap();
        let block = prompt(&env, Some("q"), Some(&[1.0, 0.0]), 5, true)
            .await
            .unwrap();
        assert!(block.contains("Raw: (not summarized yet)"), "{block}");
    }

    #[tokio::test]
    async fn has_memories_is_false_on_an_empty_store() {
        let env = env();
        assert!(!has_memories(&env.store).await);
        env.store
            .create(MemoryKind::Knowledge, "One", "body")
            .await
            .unwrap();
        assert!(has_memories(&env.store).await);
    }

    // --- token expansion -----------------------------------------------------

    async fn blocks(env: &Env, text: &str) -> Option<String> {
        memory_token_blocks(&env.store, text, &mut MemoryTokenBudget::new())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn tokens_match_filename_stems_dedupe_and_ignore_unknowns() {
        let env = env();
        env.store
            .create(MemoryKind::Knowledge, "Meeting Notes", "agenda body")
            .await
            .unwrap();
        let out = blocks(
            &env,
            "see @meeting-notes and again @meeting-notes plus @unknown-token",
        )
        .await
        .unwrap();
        assert!(out.contains("[Knowledge @meeting-notes \"Meeting Notes\""));
        assert!(out.contains("agenda body"));
        // Injected content is fenced and flagged as untrusted data.
        assert!(out.contains("--- BEGIN KNOWLEDGE ---"));
        assert_eq!(out.matches("[Knowledge ").count(), 1);
        assert!(!out.contains("unknown-token"));

        // Email-like text must not trigger: the token cannot follow a word char.
        assert!(blocks(&env, "mail me at notes@meeting-notes.com")
            .await
            .is_none());
        assert!(blocks(&env, "no tokens here").await.is_none());
    }

    #[tokio::test]
    async fn a_quoted_token_addresses_a_stem_with_spaces() {
        let env = env();
        std::fs::write(env.memories_dir().join("My Notes.md"), "spaced body").unwrap();
        env.store.rescan().await.unwrap();

        let out = blocks(&env, "look at @\"my notes\" please").await.unwrap();
        assert!(out.contains("spaced body"));
        assert!(out.contains("[Knowledge @\"my notes\""));
        // The bare form cannot match a spaced stem.
        assert!(blocks(&env, "look at @my please").await.is_none());
    }

    #[tokio::test]
    async fn a_bare_token_may_contain_dots_and_drops_trailing_punctuation() {
        let env = env();
        std::fs::write(env.memories_dir().join("node.js.md"), "dotted body").unwrap();
        env.store.rescan().await.unwrap();
        let out = blocks(&env, "see @node.js.").await.unwrap();
        assert!(out.contains("dotted body"));
        assert!(out.contains("[Knowledge @node.js \"node.js\""), "{out}");
    }

    /// The prefix class is ASCII: written with Rust's Unicode-aware `\w` the
    /// pattern stops matching after an accented letter and this returns nothing.
    #[tokio::test]
    async fn a_token_after_an_accented_letter_still_matches() {
        let env = env();
        env.store
            .create(MemoryKind::Knowledge, "Notes", "body")
            .await
            .unwrap();
        // The accented letter must be IMMEDIATELY before the trigger: with a
        // space between them the prefix class matches either way.
        let out = blocks(&env, "café@notes").await;
        assert!(out.is_some(), "the Unicode \\w class crept back in");
        // The ASCII word char it is contrasted with still blocks the match, so
        // this is not just "everything matches now".
        assert!(blocks(&env, "cafe@notes").await.is_none());
    }

    /// `#` and `/` resolve too, because the user picks their own trigger symbol,
    /// but the whole pass is gated on the message containing an `@`. That reads
    /// like a bug and is the specified behaviour; changing it would start
    /// expanding messages the shipping core leaves alone.
    #[tokio::test]
    async fn other_trigger_symbols_resolve_only_when_an_at_is_present() {
        let env = env();
        env.store
            .create(MemoryKind::Knowledge, "Notes", "body")
            .await
            .unwrap();
        assert!(blocks(&env, "see #notes").await.is_none());
        let out = blocks(&env, "see #notes and @notes").await.unwrap();
        assert!(out.contains("[Knowledge #notes"), "{out}");
        // Deduped by stem across symbols: the second mention is a reference.
        assert_eq!(out.matches("--- BEGIN KNOWLEDGE ---").count(), 1);
    }

    #[tokio::test]
    async fn a_second_mention_across_messages_is_a_reference_not_a_copy() {
        let env = env();
        env.store
            .create(MemoryKind::Knowledge, "Notes", "body")
            .await
            .unwrap();
        let mut budget = MemoryTokenBudget::new();
        let first = memory_token_blocks(&env.store, "@notes", &mut budget)
            .await
            .unwrap()
            .unwrap();
        assert!(first.contains("--- BEGIN KNOWLEDGE ---"));
        let second = memory_token_blocks(&env.store, "@notes again", &mut budget)
            .await
            .unwrap()
            .unwrap();
        assert!(second.contains("is included earlier in this conversation"));
        assert!(!second.contains("--- BEGIN KNOWLEDGE ---"));
    }

    #[tokio::test]
    async fn a_claimed_stem_is_left_to_its_other_provider() {
        let env = env();
        env.store
            .create(MemoryKind::Knowledge, "Notes", "body")
            .await
            .unwrap();
        let mut budget = MemoryTokenBudget::new();
        budget.claimed.insert("notes".to_string());
        assert!(memory_token_blocks(&env.store, "@notes", &mut budget)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn the_request_budget_truncates_and_then_omits() {
        let env = env();
        // Two large memories: the first is truncated, the second is omitted.
        let big = "x".repeat(200_000);
        env.store
            .create(MemoryKind::Knowledge, "One", &big)
            .await
            .unwrap();
        env.store
            .create(MemoryKind::Knowledge, "Two", &big)
            .await
            .unwrap();
        let mut budget = MemoryTokenBudget::new();
        let first = memory_token_blocks(&env.store, "@one", &mut budget)
            .await
            .unwrap()
            .unwrap();
        assert!(
            first.contains("[truncated]"),
            "the per-memory cap did not apply"
        );
        // 64k of the 256k budget went on the first; three more exhaust it.
        for _ in 0..3 {
            env.store
                .create(
                    MemoryKind::Knowledge,
                    &format!("Filler{}", ulid::Ulid::new()),
                    &big,
                )
                .await
                .unwrap();
        }
        let mut drain = MemoryTokenBudget::new();
        for name in ["@one", "@two"] {
            let _ = memory_token_blocks(&env.store, name, &mut drain)
                .await
                .unwrap();
        }
        let ids: Vec<String> = env
            .store
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|m| format!("@{}", m.filename.trim_end_matches(".md")))
            .collect();
        let mut out = String::new();
        for id in &ids {
            if let Some(block) = memory_token_blocks(&env.store, id, &mut drain)
                .await
                .unwrap()
            {
                out.push_str(&block);
            }
        }
        assert!(
            out.contains("omitted: too much referenced content"),
            "{}",
            &out[..200.min(out.len())]
        );
    }

    /// The budget counts UTF-16 code units, as JavaScript's `String.length`
    /// does, so an emoji costs two and a cut lands in the same place.
    #[tokio::test]
    async fn truncation_counts_utf16_code_units() {
        let env = env();
        // 40k astral characters is 80k UTF-16 units, past the 64k per-memory cap.
        let body = "\u{1F600}".repeat(40_000);
        env.store
            .create(MemoryKind::Knowledge, "Emoji", &body)
            .await
            .unwrap();
        let out = blocks(&env, "@emoji").await.unwrap();
        assert!(out.contains("[truncated]"));
        let injected: String = out.chars().filter(|c| *c == '\u{1F600}').collect();
        // 32,000 emoji is exactly 64,000 UTF-16 units: counting chars instead
        // would have injected 64,000 of them.
        assert_eq!(injected.chars().count(), 32_000);
    }

    #[tokio::test]
    async fn a_skill_block_notes_its_tools_and_bundled_files() {
        let env = env();
        let skill = env
            .store
            .create(
                MemoryKind::Skill,
                "Deploy",
                "---\nsuggested-tools: [run_command]\n---\nsteps",
            )
            .await
            .unwrap();
        env.store
            .write_file(&skill.id, "checklist.md", "x")
            .await
            .unwrap();
        let out = blocks(&env, "@deploy").await.unwrap();
        assert!(out.contains("[Skill @deploy \"Deploy\""));
        assert!(out.contains("--- BEGIN SKILL ---"));
        assert!(out.contains("Tools this skill may use: run_command."));
        assert!(out.contains("Bundled reference files (read with read_skill_file): checklist.md."));
    }

    #[tokio::test]
    async fn a_disabled_memory_is_not_expandable() {
        let env = env();
        let doc = env
            .store
            .create(MemoryKind::Knowledge, "Off", "body")
            .await
            .unwrap();
        env.store.set_enabled(&doc.id, false).await.unwrap();
        assert!(blocks(&env, "@off").await.is_none());
    }

    /// The floor is inclusive, and it is 0.25. A memory sitting exactly ON the
    /// boundary is the only case that sees both the constant's value and the
    /// `>=`. The vector makes the cosine exactly representable: the query is the
    /// first basis vector, so the score is the memory's first component, and
    /// 0.25 is exact in binary. No epsilon needed.
    #[tokio::test]
    async fn a_memory_exactly_at_the_floor_is_kept() {
        let env = env();
        let embedding = env.embedding();
        let edge = env
            .store
            .create(MemoryKind::Knowledge, "Edge", "right on the line")
            .await
            .unwrap();
        let summary = "A memory sitting exactly on the similarity floor.";
        env.store
            .set_summary(&edge.id, summary, &edge.content_hash)
            .await
            .unwrap();
        // Unit length, so the stored vector is used as-is: 0.25^2 + 0.9375 = 1.
        let at_floor = [0.25_f32, 0.937_5_f32.sqrt()];
        env.store
            .set_embedding(
                &edge.id,
                &at_floor,
                &embedding.source_hash(&memory_embed_text("Edge", summary)),
            )
            .await
            .unwrap();

        let block = prompt(&env, Some("anything"), Some(&[1.0, 0.0]), 3, true)
            .await
            .expect("a memory at the floor is still a relevant memory");
        assert!(
            block.contains("Edge:"),
            "a memory exactly at the floor was dropped, so the floor is either \
             exclusive or set above 0.25: {block}"
        );
    }

    /// The total budget is 256k, which is exactly four memories at the 64k
    /// per-memory cap. Landing the boundary exactly on a memory count is what
    /// makes the number itself observable: at 256k the fourth still fits and the
    /// fifth cannot, and at 128k the third already cannot.
    #[tokio::test]
    async fn the_request_budget_is_exactly_four_capped_memories() {
        let env = env();
        let big = "x".repeat(200_000);
        let mut names = Vec::new();
        for i in 0..5 {
            let title = format!("Budget{i}");
            env.store
                .create(MemoryKind::Knowledge, &title, &big)
                .await
                .unwrap();
            names.push(format!("@budget{i}"));
        }

        let mut budget = MemoryTokenBudget::new();
        let mut expanded = 0;
        for name in &names {
            let block = memory_token_blocks(&env.store, name, &mut budget)
                .await
                .unwrap()
                .unwrap_or_default();
            if block.contains("[truncated]") {
                expanded += 1;
            }
        }
        assert_eq!(
            expanded, 4,
            "the request budget fits a different number of capped memories than \
             four, so either the 256k total or the 64k per-memory cap moved"
        );
    }
}

// --- the embedding service ---------------------------------------------------

mod embedding {
    use super::*;
    use crate::memories::embedding::{Embedding, LocalEmbedder, NoLocalEmbedder};
    use async_trait::async_trait;
    use axum::routing::post;
    use axum::Json as AxumJson;
    use serde_json::Value as JsonValue;
    use std::sync::Mutex;

    /// A local engine under the test's control.
    struct FakeLocal {
        available: bool,
        model_id: String,
        vectors: Vec<Vec<f32>>,
    }

    #[async_trait]
    impl LocalEmbedder for FakeLocal {
        fn model_id(&self) -> &str {
            &self.model_id
        }
        async fn is_available(&self) -> bool {
            self.available
        }
        async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, crate::error::AppError> {
            Ok(self.vectors.clone())
        }
    }

    /// An OpenAI-shaped `/embeddings` server, recording what it was sent.
    struct FakeServer {
        base_url: String,
        seen: Arc<Mutex<Vec<(String, JsonValue)>>>,
    }

    async fn serve(response: JsonValue, status: u16) -> FakeServer {
        let seen: Arc<Mutex<Vec<(String, JsonValue)>>> = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&seen);
        let app = axum::Router::new().route(
            "/v1/embeddings",
            post(move |headers: axum::http::HeaderMap, body: String| {
                let recorder = Arc::clone(&recorder);
                let response = response.clone();
                async move {
                    let auth = headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    let parsed: JsonValue = serde_json::from_str(&body).unwrap_or(JsonValue::Null);
                    recorder.lock().unwrap().push((auth, parsed));
                    (
                        axum::http::StatusCode::from_u16(status).unwrap(),
                        AxumJson(response),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        FakeServer {
            base_url: format!("http://127.0.0.1:{}/v1", addr.port()),
            seen,
        }
    }

    fn data(vectors: &[&[f64]]) -> JsonValue {
        json!({
            "data": vectors
                .iter()
                .enumerate()
                .map(|(index, embedding)| json!({ "index": index, "embedding": embedding }))
                .collect::<Vec<_>>()
        })
    }

    async fn configure_external(env: &Env, base_url: &str, model: &str) {
        let mut patch = crate::settings::SettingsRecord::new();
        patch.insert("llm.provider".into(), json!("external"));
        patch.insert("llm.external.baseUrl".into(), json!(base_url));
        patch.insert("llm.external.apiKey".into(), json!("sk-test-123"));
        patch.insert("llm.external.embedModel".into(), json!(model));
        env.settings.patch(patch).await.unwrap();
    }

    #[tokio::test]
    async fn the_external_path_sends_the_model_and_bearer_and_normalises_the_reply() {
        let env = env();
        // A 5-wide vector: the external model's dimension, not the local 384.
        let server = serve(data(&[&[3.0, 4.0, 0.0, 0.0, 0.0]]), 200).await;
        configure_external(&env, &server.base_url, "text-embedding-3-small").await;
        let embedding = env.embedding();
        embedding.start().await.unwrap();

        let vectors = embedding.embed(&["hello".to_string()]).await.unwrap();
        assert_eq!(vectors.len(), 1);
        assert_eq!(vectors[0].len(), 5);
        // 3-4-0-0-0 normalises to 0.6, 0.8, 0, 0, 0.
        assert_eq!(vectors[0][0].to_bits(), 0.6f32.to_bits());
        assert_eq!(vectors[0][1].to_bits(), 0.8f32.to_bits());

        let seen = server.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, "Bearer sk-test-123");
        assert_eq!(seen[0].1["model"], "text-embedding-3-small");
        assert_eq!(seen[0].1["input"], json!(["hello"]));

        // The identity follows the configured Relevance Model, so a switch
        // re-embeds rather than comparing incompatible vectors.
        assert_eq!(embedding.active_model_id(), "text-embedding-3-small");
    }

    #[tokio::test]
    async fn a_trailing_slash_on_the_base_url_does_not_double_up() {
        let env = env();
        let server = serve(data(&[&[1.0, 0.0]]), 200).await;
        configure_external(&env, &format!("{}/", server.base_url), "m").await;
        let embedding = env.embedding();
        embedding.start().await.unwrap();
        assert!(embedding.embed(&["x".to_string()]).await.is_ok());
    }

    #[tokio::test]
    async fn vectors_are_reassembled_by_index_not_by_arrival() {
        let env = env();
        let out_of_order = json!({
            "data": [
                { "index": 1, "embedding": [0.0, 1.0] },
                { "index": 0, "embedding": [1.0, 0.0] },
            ]
        });
        let server = serve(out_of_order, 200).await;
        configure_external(&env, &server.base_url, "m").await;
        let embedding = env.embedding();
        embedding.start().await.unwrap();
        let vectors = embedding
            .embed(&["first".to_string(), "second".to_string()])
            .await
            .unwrap();
        assert_eq!(vectors[0][0].to_bits(), 1.0f32.to_bits());
        assert_eq!(vectors[1][1].to_bits(), 1.0f32.to_bits());
    }

    #[tokio::test]
    async fn a_short_batch_or_an_inconsistent_width_is_a_provider_error() {
        let env = env();
        let server = serve(data(&[&[1.0, 0.0]]), 200).await;
        configure_external(&env, &server.base_url, "m").await;
        let embedding = env.embedding();
        embedding.start().await.unwrap();
        let err = embedding
            .embed(&["one".to_string(), "two".to_string()])
            .await
            .unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::ProviderError);
        assert!(
            err.message.contains("1 vectors for 2 inputs"),
            "{}",
            err.message
        );

        let ragged = serve(data(&[&[1.0, 0.0], &[1.0]]), 200).await;
        configure_external(&env, &ragged.base_url, "m").await;
        let embedding = env.embedding();
        embedding.start().await.unwrap();
        let err = embedding
            .embed(&["one".to_string(), "two".to_string()])
            .await
            .unwrap_err();
        assert!(
            err.message.contains("inconsistent vector width"),
            "{}",
            err.message
        );
    }

    #[tokio::test]
    async fn a_non_2xx_is_a_provider_error_and_an_unreachable_host_is_unavailable() {
        let env = env();
        let server = serve(json!({"error": "nope"}), 404).await;
        configure_external(&env, &server.base_url, "m").await;
        let embedding = env.embedding();
        embedding.start().await.unwrap();
        let err = embedding.embed(&["x".to_string()]).await.unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::ProviderError);
        assert!(err.message.contains("HTTP 404"), "{}", err.message);

        // Nothing listening: a different code, because the caller's recovery
        // differs.
        configure_external(&env, "http://127.0.0.1:1/v1", "m").await;
        let embedding = env.embedding();
        embedding.start().await.unwrap();
        let err = embedding.embed(&["x".to_string()]).await.unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::ServerUnavailable);
    }

    #[tokio::test]
    async fn external_mode_without_a_relevance_model_is_unavailable() {
        let env = env();
        configure_external(&env, "https://api.example.com/v1", "").await;
        let embedding = env.embedding();
        embedding.start().await.unwrap();
        assert!(!embedding.is_ready().await);
        let err = embedding.embed(&["x".to_string()]).await.unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::ServerUnavailable);
        assert!(err.message.contains("not available"), "{}", err.message);
        // And the identity falls back to the local model's.
        assert_eq!(
            embedding.active_model_id(),
            crate::memories::embedding::EMBED_REPO
        );
    }

    #[tokio::test]
    async fn an_external_model_with_no_key_and_a_remote_host_is_unavailable() {
        let env = env();
        let mut patch = crate::settings::SettingsRecord::new();
        patch.insert("llm.provider".into(), json!("external"));
        patch.insert(
            "llm.external.baseUrl".into(),
            json!("https://api.example.com/v1"),
        );
        patch.insert("llm.external.embedModel".into(), json!("m"));
        env.settings.patch(patch).await.unwrap();
        let embedding = env.embedding();
        embedding.start().await.unwrap();
        assert!(!embedding.is_ready().await);
    }

    #[tokio::test]
    async fn with_no_local_engine_embeddings_are_unavailable_but_the_identity_stands() {
        let env = env();
        let embedding = env.embedding();
        embedding.start().await.unwrap();
        assert!(!embedding.is_ready().await);
        assert_eq!(
            embedding.active_model_id(),
            crate::memories::embedding::EMBED_REPO,
            "the identity must not depend on whether the model is downloaded",
        );
        assert!(NoLocalEmbedder.is_available().await.eq(&false));
    }

    #[tokio::test]
    async fn a_local_engine_is_used_when_available_and_its_output_is_normalised() {
        let env = env();
        let embedding = Embedding::new(
            env.settings.clone(),
            env.vault.clone(),
            Arc::new(FakeLocal {
                available: true,
                model_id: crate::memories::embedding::EMBED_REPO.to_string(),
                // Deliberately unnormalised: normalising is the caller's job.
                vectors: vec![vec![3.0, 4.0]],
            }),
        );
        embedding.start().await.unwrap();
        assert!(embedding.is_ready().await);
        let vectors = embedding.embed(&["x".to_string()]).await.unwrap();
        assert_eq!(vectors[0][0].to_bits(), 0.6f32.to_bits());
        assert_eq!(vectors[0][1].to_bits(), 0.8f32.to_bits());
    }

    /// An engine hosting a different model reports a different identity and every
    /// hash moves with it, which is what makes a model switch re-embed instead of
    /// mis-scoring.
    #[tokio::test]
    async fn the_local_identity_flows_into_the_staleness_hash() {
        let env = env();
        let embedding = Embedding::new(
            env.settings.clone(),
            env.vault.clone(),
            Arc::new(FakeLocal {
                available: true,
                model_id: "some-other-model".to_string(),
                vectors: vec![vec![1.0]],
            }),
        );
        embedding.start().await.unwrap();
        assert_eq!(embedding.active_model_id(), "some-other-model");
        assert_ne!(
            embedding.source_hash("text"),
            crate::memories::relevance::embed_source_hash(
                crate::memories::embedding::EMBED_REPO,
                "text"
            ),
        );
    }

    #[tokio::test]
    async fn an_empty_batch_never_reaches_an_endpoint() {
        let env = env();
        let embedding = env.embedding();
        assert!(embedding.embed(&[]).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn embed_with_hash_pairs_the_vector_with_the_active_identity() {
        let env = env();
        let server = serve(data(&[&[1.0, 0.0]]), 200).await;
        configure_external(&env, &server.base_url, "m").await;
        let embedding = env.embedding();
        embedding.start().await.unwrap();
        let result = embedding
            .embed_with_hash("some text")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.source_hash, embedding.source_hash("some text"));
        assert_eq!(result.vector.len(), 2);
    }

    /// The identity tracks a settings change, which is what makes a switch
    /// between two external models invalidate the stored vectors.
    #[tokio::test]
    async fn the_identity_follows_a_settings_change() {
        let env = env();
        let embedding = env.embedding();
        embedding.start().await.unwrap();
        assert_eq!(
            embedding.active_model_id(),
            crate::memories::embedding::EMBED_REPO
        );
        configure_external(&env, "https://api.example.com/v1", "text-embedding-3-large").await;
        // The watch task is asynchronous; give it the runtime for a moment.
        for _ in 0..50 {
            if embedding.active_model_id() == "text-embedding-3-large" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(embedding.active_model_id(), "text-embedding-3-large");
    }
}

// --- the indexer -------------------------------------------------------------

mod indexer {
    use super::*;
    use crate::memories::embedding::{Embedding, LocalEmbedder};
    use crate::memories::indexer::{
        index_memory, MemoryIndexer, NoIndexer, NoSummaries, SummaryGenerator, SummaryRequest,
    };
    use async_trait::async_trait;
    use std::sync::Mutex;

    struct ScriptedSummaries {
        answer: Option<String>,
        seen: Mutex<Vec<SummaryRequest>>,
    }

    #[async_trait]
    impl SummaryGenerator for ScriptedSummaries {
        async fn generate(
            &self,
            request: SummaryRequest,
        ) -> Result<Option<String>, crate::error::AppError> {
            self.seen.lock().unwrap().push(request);
            Ok(self.answer.clone())
        }
    }

    struct FailingSummaries;

    #[async_trait]
    impl SummaryGenerator for FailingSummaries {
        async fn generate(
            &self,
            _request: SummaryRequest,
        ) -> Result<Option<String>, crate::error::AppError> {
            Err(crate::error::internal("the model is not loaded"))
        }
    }

    struct FakeLocal(Vec<f32>);

    #[async_trait]
    impl LocalEmbedder for FakeLocal {
        async fn is_available(&self) -> bool {
            true
        }
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, crate::error::AppError> {
            Ok(texts.iter().map(|_| self.0.clone()).collect())
        }
    }

    fn embedder(env: &Env) -> Embedding {
        Embedding::new(
            env.settings.clone(),
            env.vault.clone(),
            Arc::new(FakeLocal(vec![0.6, 0.8])),
        )
    }

    #[tokio::test]
    async fn a_pass_summarises_then_embeds_and_is_a_no_op_when_fresh() {
        let env = env();
        let doc = env
            .store
            .create(MemoryKind::Knowledge, "Recipes", "pasta things")
            .await
            .unwrap();
        let summaries: Arc<dyn SummaryGenerator> = Arc::new(ScriptedSummaries {
            answer: Some("Pasta notes.".to_string()),
            seen: Mutex::new(Vec::new()),
        });
        let embedding = embedder(&env);

        let first = index_memory(&env.store, &embedding, &summaries, &env.settings, &doc.id)
            .await
            .unwrap();
        assert!(first.summarized && first.embedded);

        let indexed = env.store.get(&doc.id).await.unwrap();
        assert_eq!(indexed.summary.as_deref(), Some("Pasta notes."));
        assert_eq!(indexed.summary_stale, Some(false));

        // The embedding was derived from title + summary, under the active
        // identity, so the second pass finds both fresh and does nothing.
        let second = index_memory(&env.store, &embedding, &summaries, &env.settings, &doc.id)
            .await
            .unwrap();
        assert_eq!(second, Default::default());
    }

    #[tokio::test]
    async fn the_summary_request_carries_the_memory_specific_shape() {
        let env = env();
        let doc = env
            .store
            .create(MemoryKind::Knowledge, "Recipes", &"x".repeat(20_000))
            .await
            .unwrap();
        let scripted = Arc::new(ScriptedSummaries {
            answer: None,
            seen: Mutex::new(Vec::new()),
        });
        let summaries: Arc<dyn SummaryGenerator> = scripted.clone();
        index_memory(
            &env.store,
            &embedder(&env),
            &summaries,
            &env.settings,
            &doc.id,
        )
        .await
        .unwrap();
        let seen = scripted.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        let request = &seen[0];
        assert!(request
            .user_message
            .starts_with("Memory title: Recipes\n\n"));
        // Capped, so one huge memory cannot blow up the request.
        assert!(
            request.user_message.len() < 9_000,
            "{}",
            request.user_message.len()
        );
        assert_eq!(request.temperature, 0.0);
        assert_eq!(request.max_tokens, 160);
        assert_eq!(
            request.thinking_budget_key,
            "prompts.memorySummaryThinkingBudget"
        );
        // The default prompt comes from the settings schema rather than a copy.
        assert!(
            request.system_prompt.contains("memory summarizer"),
            "{}",
            request.system_prompt
        );
    }

    #[tokio::test]
    async fn a_failed_summary_is_warned_and_the_embedding_still_lands() {
        let env = env();
        let doc = env
            .store
            .create(MemoryKind::Knowledge, "Doc", "body")
            .await
            .unwrap();
        let summaries: Arc<dyn SummaryGenerator> = Arc::new(FailingSummaries);
        let outcome = index_memory(
            &env.store,
            &embedder(&env),
            &summaries,
            &env.settings,
            &doc.id,
        )
        .await
        .unwrap();
        assert!(!outcome.summarized);
        assert!(
            outcome.embedded,
            "a failed summary took the embedding with it"
        );
        assert!(env.store.get(&doc.id).await.unwrap().summary.is_none());
    }

    /// With no LLM configured the embedding step still runs, which is the
    /// degraded behaviour that matters: relevance works before summaries do.
    #[tokio::test]
    async fn no_summary_generator_still_embeds_from_the_content_head() {
        let env = env();
        let doc = env
            .store
            .create(MemoryKind::Knowledge, "Doc", "body")
            .await
            .unwrap();
        let summaries: Arc<dyn SummaryGenerator> = Arc::new(NoSummaries);
        let embedding = embedder(&env);
        let outcome = index_memory(&env.store, &embedding, &summaries, &env.settings, &doc.id)
            .await
            .unwrap();
        assert!(outcome.embedded);
        let state = &env.store.list_index_states().await.unwrap()[0];
        assert_eq!(
            state.embedding_source_hash.as_deref(),
            Some(
                embedding
                    .source_hash(&crate::memories::relevance::memory_embed_text(
                        "Doc", "body"
                    ))
                    .as_str()
            ),
        );
    }

    #[tokio::test]
    async fn with_no_embedding_source_the_pass_asks_to_be_re_armed() {
        let env = env();
        let doc = env
            .store
            .create(MemoryKind::Knowledge, "Doc", "body")
            .await
            .unwrap();
        let summaries: Arc<dyn SummaryGenerator> = Arc::new(NoSummaries);
        // The unwired embedding: no local engine, no external model.
        let outcome = index_memory(
            &env.store,
            &env.embedding(),
            &summaries,
            &env.settings,
            &doc.id,
        )
        .await
        .unwrap();
        assert!(!outcome.embedded);
        assert!(
            outcome.embedding_deferred,
            "the pass gave up instead of asking to be re-armed",
        );
    }

    #[tokio::test]
    async fn a_memory_deleted_while_queued_is_a_no_op() {
        let env = env();
        let summaries: Arc<dyn SummaryGenerator> = Arc::new(NoSummaries);
        let outcome = index_memory(
            &env.store,
            &embedder(&env),
            &summaries,
            &env.settings,
            "01ZZZZZZZZZZZZZZZZZZZZZZZZ",
        )
        .await
        .unwrap();
        assert_eq!(outcome, Default::default());
    }

    #[test]
    fn the_no_op_indexer_accepts_both_shapes_of_request() {
        NoIndexer.schedule(None);
        NoIndexer.schedule(Some("id"));
    }
}

// --- the route slice ---------------------------------------------------------

mod routes {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tower::ServiceExt;

    use crate::memories::indexer::MemoryIndexer;
    use crate::transport::testing::{context_with, StubAuth};

    const TOKEN: &str = "alice-token";

    #[derive(Default)]
    struct CountingIndexer {
        all: AtomicUsize,
        one: AtomicUsize,
    }

    impl MemoryIndexer for CountingIndexer {
        fn schedule(&self, memory_id: Option<&str>) {
            match memory_id {
                Some(_) => self.one.fetch_add(1, Ordering::SeqCst),
                None => self.all.fetch_add(1, Ordering::SeqCst),
            };
        }
    }

    struct Harness {
        env: Env,
        router: axum::Router,
        indexer: Arc<CountingIndexer>,
    }

    fn harness() -> Harness {
        let env = env();
        let auth = Arc::new(StubAuth::new().with_client(TOKEN, "alice"));
        let mut ctx = context_with(auth);
        ctx.paths = Arc::clone(&env.paths);
        ctx.db = env.db.clone();
        let indexer = Arc::new(CountingIndexer::default());
        let router = crate::memories::routes::routes(
            &ctx,
            env.store.clone(),
            Arc::clone(&indexer) as Arc<dyn MemoryIndexer>,
        );
        Harness {
            env,
            router,
            indexer,
        }
    }

    impl Harness {
        async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
            self.call_as(Some(TOKEN), method, path, body).await
        }

        async fn call_as(
            &self,
            token: Option<&str>,
            method: &str,
            path: &str,
            body: Option<Value>,
        ) -> (StatusCode, Value) {
            let mut builder = Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json");
            if let Some(token) = token {
                builder = builder.header("authorization", format!("Bearer {token}"));
            }
            let request = match body {
                Some(value) => builder.body(Body::from(value.to_string())).unwrap(),
                None => builder.body(Body::empty()).unwrap(),
            };
            let response = self.router.clone().oneshot(request).await.unwrap();
            let status = response.status();
            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let value = if bytes.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&bytes).unwrap_or(Value::Null)
            };
            (status, value)
        }

        async fn get(&self, path: &str) -> (StatusCode, Value) {
            self.call("GET", path, None).await
        }

        async fn del(&self, path: &str) -> (StatusCode, Value) {
            self.call("DELETE", path, None).await
        }

        async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
            self.call("POST", path, Some(body)).await
        }

        async fn post_none(&self, path: &str) -> (StatusCode, Value) {
            self.call("POST", path, None).await
        }

        async fn patch(&self, path: &str, body: Value) -> (StatusCode, Value) {
            self.call("PATCH", path, Some(body)).await
        }

        async fn put(&self, path: &str, body: Value) -> (StatusCode, Value) {
            self.call("PUT", path, Some(body)).await
        }

        async fn create(&self, title: &str) -> Value {
            let (status, body) = self
                .post("/api/v1/memories", json!({ "title": title }))
                .await;
            assert_eq!(status, StatusCode::CREATED);
            body
        }
    }

    #[tokio::test]
    async fn create_lists_gets_patches_and_deletes() {
        let h = harness();
        let created = h.create("Notes").await;
        let id = created["id"].as_str().unwrap().to_string();
        assert_eq!(created["kind"], "knowledge");
        assert_eq!(created["filename"], "notes.md");
        assert_eq!(created["summaryStale"], json!(true));
        assert_eq!(created["provider"], "user");
        assert_eq!(created["content"], "");
        assert_eq!(h.indexer.one.load(Ordering::SeqCst), 1);

        let (status, list) = h.get("/api/v1/memories").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(list["memories"].as_array().unwrap().len(), 1);
        // The listing carries metadata only.
        assert!(list["memories"][0].get("content").is_none());

        let (status, got) = h.get(&format!("/api/v1/memories/{id}")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(got["id"], json!(id));

        let (status, patched) = h
            .patch(
                &format!("/api/v1/memories/{id}"),
                json!({ "title": "Renamed", "content": "body", "enabled": false }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(patched["title"], "Renamed");
        assert_eq!(patched["content"], "body");
        assert_eq!(patched["enabled"], json!(false));

        // A delete answers `{ok: true}` with a 200, not a 204.
        let (status, deleted) = h.del(&format!("/api/v1/memories/{id}")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(deleted, json!({ "ok": true }));
        let (status, _) = h.get(&format!("/api/v1/memories/{id}")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn an_empty_patch_is_a_no_op_that_still_schedules() {
        let h = harness();
        let id = h.create("Notes").await["id"].as_str().unwrap().to_string();
        let before = h.indexer.one.load(Ordering::SeqCst);
        let (status, body) = h.patch(&format!("/api/v1/memories/{id}"), json!({})).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["title"], "Notes");
        assert_eq!(h.indexer.one.load(Ordering::SeqCst), before + 1);
    }

    /// The bodies are strict, so an unknown key is a 400 from the deserializer.
    #[tokio::test]
    async fn unknown_and_wrongly_typed_fields_are_rejected() {
        let h = harness();
        for body in [
            json!({ "title": "X", "nope": 1 }),
            json!({ "title": 42 }),
            json!({ "title": "X", "kind": "other" }),
            json!({}),
        ] {
            let (status, response) = h.post("/api/v1/memories", body.clone()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            assert_eq!(response["error"]["code"], "validation_error");
        }
        let id = h.create("Real").await["id"].as_str().unwrap().to_string();
        let (status, _) = h
            .patch(
                &format!("/api/v1/memories/{id}"),
                json!({ "unknown": true }),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn an_empty_title_is_rejected_by_the_store() {
        let h = harness();
        for title in ["", "   "] {
            let (status, body) = h.post("/api/v1/memories", json!({ "title": title })).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{title:?}");
            assert_eq!(body["error"]["code"], "validation_error");
        }
    }

    #[tokio::test]
    async fn a_duplicate_title_is_a_conflict() {
        let h = harness();
        h.create("Notes").await;
        let (status, body) = h
            .post("/api/v1/memories", json!({ "title": "notes" }))
            .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"]["code"], "conflict");
    }

    #[tokio::test]
    async fn the_file_routes_round_trip_and_guard_the_body() {
        let h = harness();
        let (status, skill) = h
            .post(
                "/api/v1/memories",
                json!({ "kind": "skill", "title": "Deploy", "content": "steps" }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED);
        let id = skill["id"].as_str().unwrap().to_string();
        assert_eq!(skill["files"], json!([]));

        let (status, ok) = h
            .put(
                &format!("/api/v1/memories/{id}/files/checklist.md"),
                json!({ "content": "1. check" }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ok, json!({ "ok": true }));

        let (status, file) = h
            .get(&format!("/api/v1/memories/{id}/files/checklist.md"))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(file, json!({ "content": "1. check" }));

        let (status, memory) = h.get(&format!("/api/v1/memories/{id}")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(memory["files"], json!(["checklist.md"]));

        // The instruction body is not reachable through the file API.
        let (status, _) = h
            .put(
                &format!("/api/v1/memories/{id}/files/SKILL.md"),
                json!({ "content": "x" }),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (status, _) = h
            .del(&format!("/api/v1/memories/{id}/files/checklist.md"))
            .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = h
            .get(&format!("/api/v1/memories/{id}/files/checklist.md"))
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn rescan_reports_counts_and_schedules_every_memory() {
        let h = harness();
        std::fs::write(h.env.memories_dir().join("dropped-in.md"), "hello").unwrap();
        let (status, body) = h.post_none("/api/v1/memories/rescan").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({ "added": 1, "removed": 0, "changed": 0 }));
        assert_eq!(h.indexer.all.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn reindex_checks_the_id_before_scheduling() {
        let h = harness();
        let id = h.create("Notes").await["id"].as_str().unwrap().to_string();
        let before = h.indexer.one.load(Ordering::SeqCst);

        let (status, body) = h.post_none(&format!("/api/v1/memories/{id}/reindex")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({ "ok": true }));
        assert_eq!(h.indexer.one.load(Ordering::SeqCst), before + 1);

        let (status, _) = h
            .post_none("/api/v1/memories/01ZZZZZZZZZZZZZZZZZZZZZZZZ/reindex")
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            h.indexer.one.load(Ordering::SeqCst),
            before + 1,
            "an unknown id was still scheduled",
        );
    }

    #[tokio::test]
    async fn every_route_requires_a_paired_client() {
        let h = harness();
        for (method, path) in [
            ("GET", "/api/v1/memories"),
            ("POST", "/api/v1/memories"),
            ("POST", "/api/v1/memories/rescan"),
            ("GET", "/api/v1/memories/some-id"),
            ("PATCH", "/api/v1/memories/some-id"),
            ("DELETE", "/api/v1/memories/some-id"),
            ("POST", "/api/v1/memories/some-id/reindex"),
            ("GET", "/api/v1/memories/some-id/files/x.md"),
            ("PUT", "/api/v1/memories/some-id/files/x.md"),
            ("DELETE", "/api/v1/memories/some-id/files/x.md"),
        ] {
            let (status, body) = h.call_as(None, method, path, Some(json!({}))).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
            assert_eq!(body["error"]["code"], "missing_token");
        }
    }

    #[tokio::test]
    async fn an_extension_memory_is_read_only_over_the_api() {
        let h = harness();
        let base = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(base.path().join("ext-a")).unwrap();
        std::fs::write(base.path().join("ext-a").join("guide.md"), "# Guide").unwrap();
        h.env
            .store
            .register_extension_memories(
                "ext-a",
                base.path().to_path_buf(),
                &[crate::memories::store::MemoryDecl {
                    kind: MemoryKind::Knowledge,
                    path: "guide.md".to_string(),
                }],
            )
            .await
            .unwrap();
        let id = h.env.store.list().await.unwrap()[0].id.clone();

        let (status, body) = h
            .patch(
                &format!("/api/v1/memories/{id}"),
                json!({ "content": "rewritten" }),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"]["code"], "conflict");

        // Silencing it is still allowed.
        let (status, _) = h
            .patch(
                &format!("/api/v1/memories/{id}"),
                json!({ "enabled": true }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
    }
}
