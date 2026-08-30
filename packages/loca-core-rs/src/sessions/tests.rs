//! The sessions suite.
//!
//! Two halves: the store's behaviour, and the on-disk parity surface asserted
//! against a committed fixture tree.
//!
//! Every test builds its own store over its own tempdir, so they run
//! concurrently: `Paths` and the store are threaded values, not process globals.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Map, Value};
use tempfile::TempDir;

use crate::error::ErrorCode;
use crate::paths::Paths;

use super::attachments::{attachment_id_from_path, sanitize_filename};
use super::doc::StoredMessage;
use super::store::{CreateSessionInput, NewAttachment, SessionsStore};
use super::summary::content_to_text;

const ALICE: &str = "client-alice";
const BOB: &str = "client-bob";

fn store() -> (TempDir, SessionsStore) {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::rooted(tmp.path());
    paths.ensure_dirs().unwrap();
    (tmp, SessionsStore::new(Arc::new(paths)))
}

fn message(role: &str, content: &str) -> StoredMessage {
    let Value::Object(map) = json!({ "role": role, "content": content }) else {
        unreachable!()
    };
    StoredMessage::new(map)
}

fn user(content: &str) -> StoredMessage {
    message("user", content)
}

async fn new_session(store: &SessionsStore, owner: &str) -> String {
    store
        .create(CreateSessionInput {
            owner_client_id: owner.to_string(),
            ..Default::default()
        })
        .await
        .unwrap()
        .id
}

async fn roles_of(store: &SessionsStore, session_id: &str) -> Vec<String> {
    store
        .list_messages(session_id)
        .await
        .iter()
        .map(|m| m.role().unwrap_or_default().to_string())
        .collect()
}

async fn texts_of(store: &SessionsStore, session_id: &str) -> Vec<String> {
    store
        .list_messages(session_id)
        .await
        .iter()
        .map(|m| content_to_text(m.content()))
        .collect()
}

// --- sessions ----------------------------------------------------------------

#[tokio::test]
async fn create_returns_a_session_with_an_id_timestamps_and_a_default_title() {
    let (_tmp, store) = store();
    let session = store
        .create(CreateSessionInput {
            owner_client_id: ALICE.to_string(),
            ..Default::default()
        })
        .await
        .unwrap();

    assert!(!session.id.is_empty());
    assert_eq!(session.title, "");
    assert_eq!(session.owner_client_id, ALICE);
    // Epoch millis, not seconds and not zero. `chrono` and `SystemTime` both
    // make seconds the path of least resistance, and a seconds-based timestamp
    // would sort and render wrongly everywhere without failing anything.
    assert!(session.created_at_ms > 1_600_000_000_000);
    assert_eq!(session.created_at_ms, session.updated_at_ms);
    assert_eq!(session.temporary, None);
}

#[tokio::test]
async fn list_returns_only_the_callers_sessions_newest_first() {
    let (_tmp, store) = store();
    let first = new_session(&store, ALICE).await;
    let second = new_session(&store, ALICE).await;
    let theirs = new_session(&store, BOB).await;

    // Touch the older one so the ordering assertion is about `updatedAtMs`
    // rather than about creation order.
    store.patch_title(ALICE, &first, "touched").await.unwrap();

    let ids: Vec<String> = store
        .list(ALICE)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.id)
        .collect();
    assert_eq!(ids, vec![first, second]);
    assert!(!ids.contains(&theirs));
}

#[tokio::test]
async fn a_non_owner_cannot_read_a_session_and_cannot_tell_it_apart_from_a_missing_one() {
    let (_tmp, store) = store();
    let id = new_session(&store, ALICE).await;

    let denied = store.get_or_throw(BOB, &id).await.unwrap_err();
    let absent = store
        .get_or_throw(BOB, "no-such-session")
        .await
        .unwrap_err();
    assert_eq!(denied.code, ErrorCode::SessionNotFound);
    assert_eq!(absent.code, ErrorCode::SessionNotFound);
    assert!(store.get_or_throw(ALICE, &id).await.is_ok());
}

#[tokio::test]
async fn patch_title_persists_and_bumps_updated_at() {
    let (_tmp, store) = store();
    let id = new_session(&store, ALICE).await;
    let before = store.get_or_throw(ALICE, &id).await.unwrap();

    // The clock has millisecond resolution, so without this the bump is not
    // observable and the assertion below would pass on a store that never wrote.
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    store.patch_title(ALICE, &id, "renamed").await.unwrap();

    let after = store.get_or_throw(ALICE, &id).await.unwrap();
    assert_eq!(after.title, "renamed");
    assert!(after.updated_at_ms > before.updated_at_ms);
    assert_eq!(after.created_at_ms, before.created_at_ms);
    assert_eq!(store.list(ALICE).await.unwrap()[0].title, "renamed");
}

#[tokio::test]
async fn patch_title_from_a_non_owner_is_refused() {
    let (_tmp, store) = store();
    let id = new_session(&store, ALICE).await;
    let err = store.patch_title(BOB, &id, "hijacked").await.unwrap_err();
    assert_eq!(err.code, ErrorCode::SessionNotFound);
    assert_eq!(store.get_or_throw(ALICE, &id).await.unwrap().title, "");
}

#[tokio::test]
async fn delete_removes_the_session_and_rejects_every_later_read() {
    let (_tmp, store) = store();
    let id = new_session(&store, ALICE).await;
    store.append_message(&id, user("hi")).await.unwrap();

    store.delete(ALICE, &id).await.unwrap();

    assert_eq!(
        store.get_or_throw(ALICE, &id).await.unwrap_err().code,
        ErrorCode::SessionNotFound
    );
    // Deleting a session takes its messages with it: the transcript lives inside
    // the same file, so nothing can outlive it.
    assert!(store.list_messages(&id).await.is_empty());
    assert!(store.list(ALICE).await.unwrap().is_empty());
}

#[tokio::test]
async fn delete_from_a_non_owner_is_refused_and_leaves_the_session_intact() {
    let (_tmp, store) = store();
    let id = new_session(&store, ALICE).await;
    let err = store.delete(BOB, &id).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::SessionNotFound);
    assert!(store.get_or_throw(ALICE, &id).await.is_ok());
}

#[tokio::test]
async fn set_token_usage_is_owner_agnostic_and_a_missing_session_is_a_no_op() {
    let (_tmp, store) = store();
    let id = new_session(&store, ALICE).await;
    let usage = crate::contract::api::TokenUsage {
        prompt: 10,
        completion: 5,
        total: 15,
    };

    store.set_token_usage(&id, Some(usage)).await.unwrap();
    let session = store.get_or_throw(ALICE, &id).await.unwrap();
    assert_eq!(session.token_usage.unwrap().total, 15);

    // The stream reporting usage can outlive the session it was reporting for,
    // so a missing session must not raise.
    store.set_token_usage("gone", None).await.unwrap();
}

// --- temporary sessions ------------------------------------------------------

#[tokio::test]
async fn a_temporary_session_is_memory_only_and_excluded_from_every_listing() {
    let (tmp, store) = store();
    let session = store
        .create(CreateSessionInput {
            owner_client_id: ALICE.to_string(),
            title: Some("scratch".into()),
            temporary: true,
        })
        .await
        .unwrap();
    assert_eq!(session.temporary, Some(true));

    // Readable by id, and it takes messages like any other session.
    store
        .append_message(&session.id, user("secret"))
        .await
        .unwrap();
    assert_eq!(texts_of(&store, &session.id).await, vec!["secret"]);
    assert!(store.get_or_throw(ALICE, &session.id).await.is_ok());

    // Absent from both listings, and absent from disk.
    assert!(store.list(ALICE).await.unwrap().is_empty());
    assert!(store.list_all().await.is_empty());
    assert!(!tmp.path().join("sessions").join(&session.id).exists());
}

#[tokio::test]
async fn sweep_client_temporary_drops_only_the_callers_other_temporary_sessions() {
    let (_tmp, store) = store();
    let temporary = |owner: &'static str| {
        let store = store.clone();
        async move {
            store
                .create(CreateSessionInput {
                    owner_client_id: owner.to_string(),
                    temporary: true,
                    ..Default::default()
                })
                .await
                .unwrap()
                .id
        }
    };
    let keep = temporary(ALICE).await;
    let stale = temporary(ALICE).await;
    let theirs = temporary(BOB).await;
    let persistent = new_session(&store, ALICE).await;

    store.sweep_client_temporary(ALICE, Some(&keep)).await;

    assert!(store.get_or_throw(ALICE, &keep).await.is_ok());
    assert!(store.get_or_throw(BOB, &theirs).await.is_ok());
    assert!(store.get_or_throw(ALICE, &persistent).await.is_ok());
    assert_eq!(
        store.get_or_throw(ALICE, &stale).await.unwrap_err().code,
        ErrorCode::SessionNotFound
    );
}

// --- messages ----------------------------------------------------------------

#[tokio::test]
async fn append_and_list_round_trip_in_order_with_assigned_ords() {
    let (_tmp, store) = store();
    let id = new_session(&store, ALICE).await;

    let first = store.append_message(&id, user("one")).await.unwrap();
    let second = store
        .append_message(&id, message("assistant", "two"))
        .await
        .unwrap();

    assert_eq!(first.ord, 0);
    assert_eq!(second.ord, 1);
    assert_ne!(first.id, second.id);

    let messages = store.list_messages(&id).await;
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].id(), Some(first.id.as_str()));
    assert_eq!(messages[1].role(), Some("assistant"));
    // The store fills in what the client omitted.
    assert!(messages[0].created_at_ms().unwrap() > 1_600_000_000_000);
}

#[tokio::test]
async fn appending_to_a_missing_session_is_session_not_found() {
    let (_tmp, store) = store();
    let err = store.append_message("nope", user("hi")).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::SessionNotFound);
}

#[tokio::test]
async fn insert_message_after_splices_mid_history_and_renumbers() {
    let (_tmp, store) = store();
    let id = new_session(&store, ALICE).await;
    let a = store.append_message(&id, user("a")).await.unwrap();
    store
        .append_message(&id, message("assistant", "b"))
        .await
        .unwrap();
    store.append_message(&id, user("c")).await.unwrap();

    let inserted = store
        .insert_message_after(&id, message("assistant", "spliced"), Some(&a.id))
        .await
        .unwrap();
    assert_eq!(inserted.ord, 1);

    assert_eq!(texts_of(&store, &id).await, vec!["a", "spliced", "b", "c"]);
    // A mid-history splice shifts everything after it, so the whole document is
    // renumbered and ord equals the array index again.
    let ords: Vec<i64> = store
        .list_messages(&id)
        .await
        .iter()
        .map(|m| m.ord().unwrap())
        .collect();
    assert_eq!(ords, vec![0, 1, 2, 3]);
}

#[tokio::test]
async fn insert_message_after_appends_for_a_null_unknown_or_tail_anchor() {
    for anchor in ["none", "unknown", "tail"] {
        let (_tmp, store) = store();
        let id = new_session(&store, ALICE).await;
        let first = store.append_message(&id, user("a")).await.unwrap();
        let tail = store.append_message(&id, user("b")).await.unwrap();

        let after = match anchor {
            "none" => None,
            "unknown" => Some("no-such-message"),
            _ => Some(tail.id.as_str()),
        };
        let inserted = store
            .insert_message_after(&id, message("assistant", "appended"), after)
            .await
            .unwrap();

        assert_eq!(inserted.ord, 2, "anchor {anchor}");
        assert_eq!(
            texts_of(&store, &id).await,
            vec!["a", "b", "appended"],
            "anchor {anchor}",
        );
        // A tail insert shifts nothing, so the existing ords are untouched.
        assert_eq!(
            store.list_messages(&id).await[0].id(),
            Some(first.id.as_str())
        );
    }
}

#[tokio::test]
async fn a_tail_insert_after_a_delete_does_not_reuse_an_ord() {
    let (_tmp, store) = store();
    let id = new_session(&store, ALICE).await;
    store.append_message(&id, user("a")).await.unwrap();
    let second = store.append_message(&id, user("b")).await.unwrap();
    let third = store.append_message(&id, user("c")).await.unwrap();

    store.delete_message(&id, &second.id).await.unwrap();
    // Two messages remain, with ords 0 and 2. A length-based ord would assign 2
    // again and collide with the surviving one; max plus one assigns 3.
    let appended = store.append_message(&id, user("d")).await.unwrap();
    assert_eq!(appended.ord, 3);
    assert_eq!(
        store.get_message(&id, &third.id).await.unwrap().ord(),
        Some(2)
    );
}

#[tokio::test]
async fn delete_turn_removes_everything_between_the_anchor_and_the_next_user_message() {
    let (_tmp, store) = store();
    let id = new_session(&store, ALICE).await;
    let anchor = store.append_message(&id, user("question")).await.unwrap();
    let reasoning = store
        .append_message(&id, message("reasoning", "thinking"))
        .await
        .unwrap();
    let answer = store
        .append_message(&id, message("assistant", "answer"))
        .await
        .unwrap();
    store
        .append_message(&id, user("next question"))
        .await
        .unwrap();
    store
        .append_message(&id, message("assistant", "next answer"))
        .await
        .unwrap();

    let removed = store.delete_turn(&id, &anchor.id).await.unwrap();
    assert_eq!(removed, vec![reasoning.id, answer.id]);
    assert_eq!(
        texts_of(&store, &id).await,
        vec!["question", "next question", "next answer"]
    );
    let ords: Vec<i64> = store
        .list_messages(&id)
        .await
        .iter()
        .map(|m| m.ord().unwrap())
        .collect();
    assert_eq!(ords, vec![0, 1, 2]);
}

#[tokio::test]
async fn delete_turn_on_the_newest_an_empty_or_an_unknown_anchor_removes_nothing() {
    let (_tmp, store) = store();
    let id = new_session(&store, ALICE).await;
    let first = store.append_message(&id, user("one")).await.unwrap();
    let answer = store
        .append_message(&id, message("assistant", "answer"))
        .await
        .unwrap();
    let newest = store.append_message(&id, user("two")).await.unwrap();

    // The newest turn has nothing after it.
    assert!(store.delete_turn(&id, &newest.id).await.unwrap().is_empty());
    // An anchor immediately followed by a user message has an empty turn.
    assert!(store.delete_turn(&id, &answer.id).await.unwrap().is_empty());
    // An unknown anchor is a no-op rather than an error.
    assert!(store.delete_turn(&id, "nope").await.unwrap().is_empty());

    assert_eq!(store.list_messages(&id).await.len(), 3);
    assert_eq!(
        store.list_messages(&id).await[0].id(),
        Some(first.id.as_str())
    );
}

#[tokio::test]
async fn get_and_delete_of_a_missing_message_are_message_not_found() {
    let (_tmp, store) = store();
    let id = new_session(&store, ALICE).await;

    for code in [
        store.get_message(&id, "nope").await.unwrap_err().code,
        store.delete_message(&id, "nope").await.unwrap_err().code,
    ] {
        // Distinct from `session_not_found`: the session was found and the
        // message was not, and the two are separate published codes.
        assert_eq!(code, ErrorCode::MessageNotFound);
    }
}

// --- patching a message ------------------------------------------------------

fn patch(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn patch_message_applies_the_change_and_pins_id_role_and_ord() {
    let (_tmp, store) = store();
    let id = new_session(&store, ALICE).await;
    store.append_message(&id, user("first")).await.unwrap();
    let target = store.append_message(&id, user("before")).await.unwrap();

    let result = store
        .patch_message(&id, &target.id, &patch(json!({ "content": "after" })))
        .await
        .unwrap();
    assert_eq!(result.id, target.id);
    assert_eq!(result.ord, 1);

    let patched = store.get_message(&id, &target.id).await.unwrap();
    assert_eq!(content_to_text(patched.content()), "after");
    assert_eq!(patched.role(), Some("user"));
    assert_eq!(patched.ord(), Some(1));
}

#[tokio::test]
async fn a_patch_cannot_re_key_re_role_or_reorder_a_message() {
    let (_tmp, store) = store();
    let id = new_session(&store, ALICE).await;
    let target = store.append_message(&id, user("hello")).await.unwrap();

    // `role` is refused outright: merging one would let a patch turn a user
    // message into an assistant message.
    let err = store
        .patch_message(&id, &target.id, &patch(json!({ "role": "assistant" })))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::ValidationError);

    // `id` and `ord` are accepted in the body and then ignored.
    store
        .patch_message(
            &id,
            &target.id,
            &patch(json!({ "id": "hijacked", "ord": 99, "content": "changed" })),
        )
        .await
        .unwrap();
    let patched = store.get_message(&id, &target.id).await.unwrap();
    assert_eq!(patched.id(), Some(target.id.as_str()));
    assert_eq!(patched.ord(), Some(0));
    assert_eq!(content_to_text(patched.content()), "changed");
}

#[tokio::test]
async fn a_patch_carrying_a_field_of_another_role_is_refused() {
    let (_tmp, store) = store();
    let id = new_session(&store, ALICE).await;
    let target = store.append_message(&id, user("hello")).await.unwrap();

    // The guard comes from the generated contract type's `deny_unknown_fields`
    // rather than from anything restated in this module.
    for body in [
        json!({ "toolCalls": [] }),
        json!({ "interrupted": true }),
        json!({ "callId": "c1" }),
        // The right field, the wrong type.
        json!({ "content": 42 }),
    ] {
        let err = store
            .patch_message(&id, &target.id, &patch(body.clone()))
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ValidationError, "accepted {body}");
    }

    // A field that does belong to a user message still works, so the guard is
    // discriminating rather than refusing everything.
    store
        .patch_message(&id, &target.id, &patch(json!({ "automated": true })))
        .await
        .unwrap();
}

// --- attachments -------------------------------------------------------------

#[tokio::test]
async fn recording_an_attachment_does_not_bump_updated_at() {
    let (_tmp, store) = store();
    let id = new_session(&store, ALICE).await;
    let before = store.get_or_throw(ALICE, &id).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;

    let record = store
        .record_attachment(
            &id,
            NewAttachment {
                message_id: "m1".into(),
                filename: "photo.png".into(),
                mime: Some("image/png".into()),
                size_bytes: 3,
                abs_path: "/tmp/photo.png".into(),
            },
        )
        .await
        .unwrap();

    assert_eq!(record.session_id, id);
    assert_eq!(record.filename, "photo.png");
    assert!(record.created_at_ms > 1_600_000_000_000);
    // Uploading a file is not a conversation event, so it must not reorder the
    // session list.
    let after = store.get_or_throw(ALICE, &id).await.unwrap();
    assert_eq!(after.updated_at_ms, before.updated_at_ms);

    let fetched = store.get_attachment(&id, &record.id).await.unwrap();
    assert_eq!(fetched.abs_path, "/tmp/photo.png");
    assert_eq!(
        store.get_attachment(&id, "nope").await.unwrap_err().code,
        ErrorCode::NotFound
    );
}

#[test]
fn sanitize_filename_holds_every_rule() {
    // Each case is here because deleting the rule that handles it changes the
    // answer, which is what makes this a guard rather than a description.
    for (input, want) in [
        ("photo.png", "photo.png"),
        // Path separators, and the traversal they would otherwise carry. The
        // leading `..` goes to the hidden-file rule after the separators become
        // underscores, which is why only the second pair survives.
        ("../../etc/passwd", "_.._etc_passwd"),
        ("a\\b.txt", "a_b.txt"),
        // Windows-forbidden and shell glyphs.
        ("re:port*?\"<>|.txt", "re_port______.txt"),
        // Control characters.
        ("a\u{0}b\u{1f}c", "a_b_c"),
        // Leading dots (a hidden file) and trailing dots and spaces, which
        // Windows silently drops.
        (".hidden", "hidden"),
        ("name.  ", "name"),
        // Reserved basenames, with and without an extension.
        ("con", "_con"),
        ("CON.txt", "_CON.txt"),
        ("lpt9.log", "_lpt9.log"),
        // Not reserved: the rule matches the whole stem, not a prefix.
        ("console.log", "console.log"),
        // Everything stripped.
        ("...", "file"),
        ("", "file"),
    ] {
        assert_eq!(sanitize_filename(input), want, "input {input:?}");
    }
}

#[test]
fn sanitize_filename_caps_the_length_and_keeps_a_short_extension() {
    let long = "a".repeat(500);
    assert_eq!(sanitize_filename(&long).len(), 200);

    let with_ext = format!("{}.png", "a".repeat(500));
    let capped = sanitize_filename(&with_ext);
    assert_eq!(capped.len(), 200);
    assert!(capped.ends_with(".png"));

    // An "extension" longer than the cap is not one, so it is not preserved.
    let long_ext = format!("{}.{}", "a".repeat(300), "b".repeat(40));
    assert_eq!(sanitize_filename(&long_ext).len(), 200);
}

#[test]
fn an_attachment_id_is_pulled_out_of_the_stored_url_or_the_attachment_is_skipped() {
    assert_eq!(
        attachment_id_from_path("https://host:1/api/v1/sessions/s1/attachments/a1"),
        Some("a1")
    );
    assert_eq!(attachment_id_from_path("/attachments/a1?v=2"), Some("a1"));
    // A path that does not match is skipped rather than failing the turn.
    for path in [
        "",
        "/api/v1/sessions/s1",
        "/attachments/",
        "/attachments/a/b",
    ] {
        assert_eq!(attachment_id_from_path(path), None, "path {path:?}");
    }
}

// --- the storage view --------------------------------------------------------

#[tokio::test]
async fn list_all_spans_clients_with_sizes_and_delete_by_id_is_owner_agnostic() {
    let (tmp, store) = store();
    let mine = new_session(&store, ALICE).await;
    let theirs = new_session(&store, BOB).await;

    // Real bytes on disk, so the reported footprint is measured rather than
    // assumed. `session.json` plus the attachments directory, and nothing else.
    let attachments = tmp.path().join("sessions").join(&mine).join("attachments");
    std::fs::create_dir_all(&attachments).unwrap();
    std::fs::write(attachments.join("a1_photo.png"), vec![7u8; 1024]).unwrap();

    let all = store.list_all().await;
    assert_eq!(all.len(), 2);
    let entry = all.iter().find(|e| e.id == mine).unwrap();
    assert_eq!(entry.owner_client_id, ALICE);
    assert!(entry.size_bytes > 1024, "attachments were not counted");
    let other = all.iter().find(|e| e.id == theirs).unwrap();
    assert!(other.size_bytes > 0 && other.size_bytes < 1024);

    let deleted = store.delete_by_id(&theirs).await.unwrap();
    assert_eq!(deleted.owner_client_id, BOB);
    assert_eq!(store.list_all().await.len(), 1);
    assert!(store.delete_by_id("no-such-session").await.is_none());
}

// --- resilience --------------------------------------------------------------

#[tokio::test]
async fn concurrent_appends_on_one_session_never_lose_an_update() {
    let (_tmp, store) = store();
    let id = new_session(&store, ALICE).await;

    let mut tasks = Vec::new();
    for i in 0..24 {
        let store = store.clone();
        let id = id.clone();
        tasks.push(tokio::spawn(async move {
            store.append_message(&id, user(&format!("m{i}"))).await
        }));
    }
    for task in tasks {
        task.await.unwrap().unwrap();
    }

    // Every append landed, and every ord is distinct: the read-modify-write
    // cycles were serialized rather than interleaved.
    let messages = store.list_messages(&id).await;
    assert_eq!(messages.len(), 24);
    let mut ords: Vec<i64> = messages.iter().map(|m| m.ord().unwrap()).collect();
    ords.sort_unstable();
    assert_eq!(ords, (0..24).collect::<Vec<_>>());
}

#[tokio::test]
async fn a_corrupt_session_json_is_skipped_rather_than_failing_every_other_session() {
    let (tmp, store) = store();
    let good = new_session(&store, ALICE).await;
    let corrupt = new_session(&store, ALICE).await;
    std::fs::write(
        tmp.path()
            .join("sessions")
            .join(&corrupt)
            .join("session.json"),
        b"{ truncated",
    )
    .unwrap();
    // The sidecar would otherwise still answer for it, and the point here is the
    // full-document path.
    std::fs::remove_file(tmp.path().join("sessions").join(&corrupt).join("meta.json")).unwrap();

    // A truncated file must not poison any of the three walks.
    let listed: Vec<String> = store
        .list(ALICE)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.id)
        .collect();
    assert_eq!(listed, vec![good.clone()]);
    assert_eq!(store.list_all().await.len(), 1);
    store.sweep_orphaned_dirs().await;

    // And the corrupt session is still on disk: a corrupt file is a real session
    // that reads skip, not an orphan to delete.
    assert!(tmp.path().join("sessions").join(&corrupt).exists());
    assert!(store.get_or_throw(ALICE, &good).await.is_ok());
}

#[tokio::test]
async fn a_missing_or_truncated_meta_makes_list_fall_back_and_backfill() {
    let (tmp, store) = store();
    let id = new_session(&store, ALICE).await;
    store.patch_title(ALICE, &id, "real title").await.unwrap();
    store
        .append_message(&id, user("hello there"))
        .await
        .unwrap();
    let meta = tmp.path().join("sessions").join(&id).join("meta.json");

    // A truncated write is not the same as unparseable JSON: a meta with a valid
    // id but no `updatedAtMs` would sort as garbage. Every list-relevant field is
    // required, and each case drops exactly one from an otherwise complete
    // sidecar; the stale title is what a trusted partial meta would serve.
    let complete = json!({
        "id": &id,
        "ownerClientId": ALICE,
        "title": "STALE",
        "createdAtMs": 1,
        "updatedAtMs": 2,
        "messageCount": 99,
        "summary": [],
    });
    let mut broken: Vec<String> = Vec::new();
    for field in [
        "id",
        "ownerClientId",
        "title",
        "createdAtMs",
        "updatedAtMs",
        "messageCount",
    ] {
        let mut partial = complete.clone();
        partial.as_object_mut().unwrap().remove(field);
        broken.push(partial.to_string());
    }
    // Wrong types, and not JSON at all.
    broken.push(
        json!({ "id": &id, "ownerClientId": ALICE, "title": "STALE",
        "createdAtMs": "nope", "updatedAtMs": 2, "messageCount": 99 })
        .to_string(),
    );
    broken.push("{ truncated".to_string());
    broken.push(String::new());

    for meta_text in &broken {
        std::fs::write(&meta, meta_text).unwrap();
        let listed = store.list(ALICE).await.unwrap();
        assert_eq!(listed.len(), 1, "meta {meta_text}");
        assert_eq!(listed[0].title, "real title", "meta {meta_text}");
        assert_eq!(listed[0].message_count, 1, "meta {meta_text}");
        // Backfilled, so the next call is fast again.
        let rewritten: Value = serde_json::from_slice(&std::fs::read(&meta).unwrap()).unwrap();
        assert_eq!(rewritten["title"], "real title");
        assert_eq!(rewritten["messageCount"], 1);
    }

    // `summary` is the one optional field: it is derived, and an older sidecar
    // that predates it must not force a full re-read of every transcript.
    let mut without_summary = complete.clone();
    without_summary["title"] = json!("from the sidecar");
    without_summary["messageCount"] = json!(1);
    without_summary.as_object_mut().unwrap().remove("summary");
    std::fs::write(&meta, without_summary.to_string()).unwrap();
    assert_eq!(
        store.list(ALICE).await.unwrap()[0].title,
        "from the sidecar"
    );

    std::fs::remove_file(&meta).unwrap();
    assert_eq!(store.list(ALICE).await.unwrap()[0].title, "real title");
    assert!(meta.exists());
}

#[tokio::test]
async fn the_summary_carries_the_opening_exchange_within_its_budget() {
    let (_tmp, store) = store();
    let id = new_session(&store, ALICE).await;
    store
        .append_message(&id, message("system", "ignored"))
        .await
        .unwrap();
    store
        .append_message(&id, user("  what  is\n up? "))
        .await
        .unwrap();
    store
        .append_message(&id, message("reasoning", "ignored"))
        .await
        .unwrap();
    store
        .append_message(&id, message("assistant", "not much"))
        .await
        .unwrap();

    let summary = &store.list(ALICE).await.unwrap()[0].summary;
    assert_eq!(summary.len(), 2);
    // Only user and assistant contribute, whitespace is collapsed, and the
    // assistant is labelled "agent" because the client supplies the real name.
    assert_eq!(summary[0].text, "what is up?");
    assert_eq!(summary[0].role, crate::contract::api::SummaryPartRole::User);
    assert_eq!(
        summary[1].role,
        crate::contract::api::SummaryPartRole::Agent
    );
}

#[tokio::test]
async fn sweep_orphaned_dirs_removes_attachment_only_dirs_and_keeps_real_sessions() {
    let (tmp, store) = store();
    let real = new_session(&store, ALICE).await;

    // What an unclean shutdown leaves behind: a temporary session's attachments
    // reached disk while its document lived only in memory.
    let orphan = tmp
        .path()
        .join("sessions")
        .join("01ORPHANORPHANORPHANORPHAN");
    std::fs::create_dir_all(orphan.join("attachments")).unwrap();
    std::fs::write(orphan.join("attachments").join("a1_x.png"), b"bytes").unwrap();

    store.sweep_orphaned_dirs().await;

    assert!(!orphan.exists());
    assert!(tmp.path().join("sessions").join(&real).exists());
    assert!(store.get_or_throw(ALICE, &real).await.is_ok());
}

#[tokio::test]
async fn a_session_id_that_is_not_a_path_segment_reads_as_absent() {
    let (tmp, store) = store();
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(
        outside.join("session.json"),
        serde_json::to_vec(&json!({
            "id": "outside",
            "ownerClientId": ALICE,
            "title": "not reachable",
            "createdAtMs": 1,
            "updatedAtMs": 1,
            "messages": [],
            "attachments": [],
        }))
        .unwrap(),
    )
    .unwrap();

    // Ids arrive back as URL path parameters and axum percent-decodes those, so
    // without the segment check this would resolve outside `sessions/`. Absent
    // rather than a distinct error, matching the posture owner scoping takes.
    for id in ["../outside", "..", ".", "", "a/b", "a\\b"] {
        assert_eq!(
            store.get_or_throw(ALICE, id).await.unwrap_err().code,
            ErrorCode::SessionNotFound,
            "id {id:?}",
        );
    }
    assert!(outside.join("session.json").exists());
}

// --- on-disk parity, against a committed fixture tree ------------------------

#[derive(serde::Deserialize)]
struct Fixture {
    #[serde(rename = "aliceClientId")]
    alice: String,
    #[serde(rename = "bobClientId")]
    bob: String,
    #[serde(rename = "mainSessionId")]
    main: String,
    #[serde(rename = "otherSessionId")]
    other: String,
    #[serde(rename = "mainMessageCount")]
    main_message_count: usize,
    #[serde(rename = "mainRoles")]
    main_roles: Vec<String>,
}

fn fixture() -> Fixture {
    serde_json::from_str(include_str!("testdata/deno-sessions.json")).unwrap()
}

/// A store over a private copy of the fixture tree, so a test may mutate it.
fn fixture_store() -> (TempDir, SessionsStore) {
    let (tmp, store) = store();
    let from = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/sessions/testdata/deno-sessions");
    copy_dir(&from, &tmp.path().join("sessions"));
    (tmp, store)
}

fn copy_dir(from: &Path, to: &PathBuf) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[tokio::test]
async fn an_existing_install_is_read_as_is() {
    let f = fixture();
    let (_tmp, store) = fixture_store();

    // Alice's two sessions, and not Bob's.
    let listed = store.list(&f.alice).await.unwrap();
    assert_eq!(listed.len(), 2);
    assert!(listed.iter().any(|e| e.id == f.main));
    assert!(!listed.iter().any(|e| e.id == f.other));
    assert_eq!(store.list(&f.bob).await.unwrap().len(), 1);

    let session = store.get_or_throw(&f.alice, &f.main).await.unwrap();
    assert_eq!(session.title, "Parity fixture");
    assert_eq!(session.owner_client_id, f.alice);
    assert!(session.created_at_ms > 1_600_000_000_000);
    // Written by a later `setTokenUsage`, so it also proves the optional field
    // survives the round trip rather than being dropped as unknown.
    let usage = session.token_usage.unwrap();
    assert_eq!(
        (usage.prompt, usage.completion, usage.total),
        (120, 45, 165)
    );

    // Every role the union carries, in order, including the four the LLM never
    // sees (`tool_filter`, `memory_filter`, `display`, `error`).
    assert_eq!(roles_of(&store, &f.main).await, f.main_roles);
    assert_eq!(
        store.list_messages(&f.main).await.len(),
        f.main_message_count
    );

    // The multipart user message keeps its non-text parts, which a naive
    // string-typed content field would have dropped.
    let multipart = &store.list_messages(&f.main).await[1];
    assert_eq!(content_to_text(multipart.content()), "And in this photo?");
    let parts = multipart.content().unwrap().as_array().unwrap();
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[1]["type"], "image_file");

    // The attachment record, and the bytes beside it.
    let attachments = store.list_all().await;
    let main = attachments.iter().find(|e| e.id == f.main).unwrap();
    assert!(main.size_bytes > 64);
}

#[tokio::test]
async fn a_document_written_by_the_other_core_round_trips_without_losing_a_field() {
    let f = fixture();
    let (tmp, store) = fixture_store();
    let file = tmp
        .path()
        .join("sessions")
        .join(&f.main)
        .join("session.json");
    let before: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();

    // A mutation that touches one field forces a full rewrite through this
    // crate's serializer, which is the only way to see what it would drop.
    store
        .patch_title(&f.alice, &f.main, "renamed")
        .await
        .unwrap();

    let after: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    let mut want = before;
    // The two fields the mutation was for.
    want["title"] = json!("renamed");
    want["updatedAtMs"] = after["updatedAtMs"].clone();

    // Every other field, at every depth, must come back exactly, including the
    // messages whose shape this build does not model.
    assert_eq!(numbers_as_written(&after), numbers_as_written(&want));
}

/// Renders an integral float as an integer, on both sides of the comparison
/// above.
///
/// Two differences are tolerated, and only two, both about how a value is
/// spelled rather than what it is: key order and whitespace, which fall out of
/// comparing parsed values rather than bytes, and `45` against `45.0`, which is
/// this function (token counts are `number` on the wire, so `f64` here, and
/// `serde_json` writes an integral `f64` with its trailing zero).
///
/// Do not widen this, and do not "upgrade" the comparison to raw bytes: the
/// first would stop it catching a dropped field, the second would fail on a
/// difference nobody can observe.
fn numbers_as_written(value: &Value) -> Value {
    match value {
        Value::Number(n) => match n.as_f64() {
            Some(f) if f.fract() == 0.0 && f.abs() < 9e15 => json!(f as i64),
            _ => value.clone(),
        },
        Value::Array(items) => Value::Array(items.iter().map(numbers_as_written).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), numbers_as_written(v)))
                .collect(),
        ),
        _ => value.clone(),
    }
}

#[tokio::test]
async fn a_message_whose_role_this_build_cannot_represent_survives_a_read_and_a_rewrite() {
    let f = fixture();
    let (tmp, store) = fixture_store();
    let file = tmp
        .path()
        .join("sessions")
        .join(&f.main)
        .join("session.json");

    // A role from a build that is not this one. The generated `Message` enum is
    // closed over nine roles with `deny_unknown_fields`, so typing the stored
    // messages with it would fail the whole document parse, and the read path
    // treats a failed parse as an absent session: this one message would erase
    // the entire transcript from every listing.
    let mut doc: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    doc["messages"].as_array_mut().unwrap().push(json!({
        "role": "from_a_future_build",
        "content": "hello",
        "id": "01FUTURE",
        "ord": 99,
        "createdAtMs": 1786000000000i64,
        "unknownField": { "nested": true },
    }));
    std::fs::write(&file, serde_json::to_vec_pretty(&doc).unwrap()).unwrap();

    // Read: the session is still there, and so is every other message.
    assert!(store.get_or_throw(&f.alice, &f.main).await.is_ok());
    let messages = store.list_messages(&f.main).await;
    assert_eq!(messages.len(), f.main_message_count + 1);
    assert_eq!(messages.last().unwrap().role(), Some("from_a_future_build"));

    // Rewrite: the unknown message comes back byte for byte, extra field
    // included.
    store.append_message(&f.main, user("after")).await.unwrap();

    // And it is counted rather than silently dropped. Asserted after a mutation
    // because the edit above went straight to disk, and the list reads the
    // sidecar, which only a write through the store refreshes.
    assert_eq!(
        store
            .list(&f.alice)
            .await
            .unwrap()
            .iter()
            .find(|e| e.id == f.main)
            .unwrap()
            .message_count,
        (f.main_message_count + 2) as i64,
    );
    let reread: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    let unknown = &reread["messages"].as_array().unwrap()[f.main_message_count];
    assert_eq!(unknown["role"], "from_a_future_build");
    assert_eq!(unknown["unknownField"]["nested"], true);
}

#[tokio::test]
async fn a_message_this_build_cannot_represent_still_cannot_be_patched() {
    let f = fixture();
    let (tmp, store) = fixture_store();
    let file = tmp
        .path()
        .join("sessions")
        .join(&f.main)
        .join("session.json");
    let mut doc: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    doc["messages"].as_array_mut().unwrap().push(json!({
        "role": "from_a_future_build",
        "content": "hello",
        "id": "01FUTURE",
        "ord": 99,
        "createdAtMs": 1786000000000i64,
    }));
    std::fs::write(&file, serde_json::to_vec_pretty(&doc).unwrap()).unwrap();

    // Tolerated on read, refused on write. There is no schema to validate the
    // patch against, and guessing one would be how a malformed message gets
    // written back.
    let err = store
        .patch_message(&f.main, "01FUTURE", &patch(json!({ "content": "x" })))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::ValidationError);
}

// --- routes ------------------------------------------------------------------
//
// Driven through the assembled router with `tower::oneshot`, so the auth layer,
// the extractors and the generated inbound types are all in the path.

mod routes {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use axum::response::Response;
    use futures_util::stream::{self, BoxStream};
    use futures_util::StreamExt;
    use serde_json::{json, Value};
    use tempfile::TempDir;
    use tokio::sync::oneshot;
    use tower::ServiceExt;

    use super::*;
    use crate::sessions::chat_engine::{ChatEngine, OneShotDelta, OneShotRequest};
    use crate::sessions::routes;
    use crate::transport::testing::{context_with, StubAuth};

    const TOKEN: &str = "alice-token";
    const OTHER_TOKEN: &str = "bob-token";

    /// A chat engine the test scripts, standing in for D4.
    #[derive(Default)]
    struct ScriptedChat {
        deltas: Vec<OneShotDelta>,
        fail_with: Option<&'static str>,
        /// Held open so a test can keep one stream in flight while it issues a
        /// second request.
        gate: Mutex<Option<oneshot::Receiver<()>>>,
        titles_regenerated: AtomicUsize,
        last_history_len: AtomicUsize,
    }

    impl ChatEngine for ScriptedChat {
        fn one_shot(
            &self,
            req: OneShotRequest,
        ) -> BoxStream<'static, crate::error::Result<OneShotDelta>> {
            self.last_history_len
                .store(req.history.len(), Ordering::SeqCst);
            let gate = self.gate.lock().unwrap().take();
            let deltas: Vec<crate::error::Result<OneShotDelta>> = match self.fail_with {
                Some(message) => vec![Err(crate::error::internal(message))],
                None => self.deltas.iter().cloned().map(Ok).collect(),
            };
            Box::pin(async_stream(gate, deltas))
        }

        fn regenerate_title(&self, _session_id: String, _owner_client_id: String) {
            self.titles_regenerated.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Yields `items` once `gate` (if any) resolves, so a test can hold one
    /// stream open while it issues another request.
    fn async_stream(
        gate: Option<oneshot::Receiver<()>>,
        items: Vec<crate::error::Result<OneShotDelta>>,
    ) -> impl futures_util::Stream<Item = crate::error::Result<OneShotDelta>> + Send {
        stream::once(async move {
            if let Some(gate) = gate {
                let _ = gate.await;
            }
            stream::iter(items)
        })
        .flatten()
    }

    struct Harness {
        _tmp: TempDir,
        router: axum::Router,
        store: SessionsStore,
        chat: Arc<ScriptedChat>,
    }

    fn harness_with(chat: ScriptedChat) -> Harness {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::rooted(tmp.path());
        paths.ensure_dirs().unwrap();
        let paths = Arc::new(paths);

        let auth = Arc::new(
            StubAuth::new()
                .with_client(TOKEN, ALICE)
                .with_client(OTHER_TOKEN, BOB),
        );
        let mut ctx = context_with(auth);
        ctx.paths = Arc::clone(&paths);

        let store = SessionsStore::new(paths);
        let chat = Arc::new(chat);
        let router = routes::routes(
            &ctx,
            store.clone(),
            Arc::clone(&chat) as Arc<dyn ChatEngine>,
        );
        Harness {
            _tmp: tmp,
            router,
            store,
            chat,
        }
    }

    fn harness() -> Harness {
        harness_with(ScriptedChat::default())
    }

    impl Harness {
        async fn send(&self, req: Request<Body>) -> Response {
            self.router.clone().oneshot(req).await.unwrap()
        }

        async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
            self.call_as(TOKEN, method, path, body).await
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

        async fn call_as(
            &self,
            token: &str,
            method: &str,
            path: &str,
            body: Option<Value>,
        ) -> (StatusCode, Value) {
            let mut builder = Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", format!("Bearer {token}"));
            let request = match body {
                Some(value) => builder
                    .header("content-type", "application/json")
                    .body(Body::from(value.to_string()))
                    .unwrap(),
                None => {
                    builder = builder.header("content-type", "application/json");
                    builder.body(Body::empty()).unwrap()
                }
            };
            let response = self.send(request).await;
            let status = response.status();
            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let value = if bytes.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&bytes).unwrap_or(Value::Null)
            };
            (status, value)
        }

        async fn new_session(&self) -> String {
            let (status, body) = self.post("/api/v1/sessions", json!({})).await;
            assert_eq!(status, StatusCode::OK);
            body["id"].as_str().unwrap().to_string()
        }
    }

    // --- lifecycle -----------------------------------------------------------

    #[tokio::test]
    async fn the_session_lifecycle_round_trips_over_http() {
        let h = harness();

        let (status, created) = h
            .post("/api/v1/sessions", json!({ "title": "first" }))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(created["title"], "first");
        assert_eq!(created["ownerClientId"], ALICE);
        let id = created["id"].as_str().unwrap().to_string();

        let (status, patched) = h
            .patch(
                &format!("/api/v1/sessions/{id}"),
                json!({ "title": "second" }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        // A narrow response: the id and the new title, nothing else.
        assert_eq!(patched, json!({ "id": id, "title": "second" }));

        let (status, fetched) = h.get(&format!("/api/v1/sessions/{id}")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(fetched["title"], "second");
        assert_eq!(fetched["messages"], json!([]));

        let (status, _) = h.del(&format!("/api/v1/sessions/{id}")).await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, body) = h.get(&format!("/api/v1/sessions/{id}")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "session_not_found");
    }

    #[tokio::test]
    async fn the_listing_is_a_bare_array() {
        let h = harness();
        h.new_session().await;
        let (status, body) = h.get("/api/v1/sessions").await;
        assert_eq!(status, StatusCode::OK);
        // No envelope. A `{ items: [...] }` assumption would be wrong here.
        assert!(body.is_array(), "expected a bare array, got {body}");
        assert_eq!(body.as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn every_route_requires_a_bearer() {
        let h = harness();
        let id = h.new_session().await;
        for (method, path) in [
            ("GET", "/api/v1/sessions".to_string()),
            ("POST", "/api/v1/sessions".to_string()),
            ("GET", format!("/api/v1/sessions/{id}")),
            ("PATCH", format!("/api/v1/sessions/{id}")),
            ("DELETE", format!("/api/v1/sessions/{id}")),
            ("POST", format!("/api/v1/sessions/{id}/regenerate-title")),
            ("POST", format!("/api/v1/sessions/{id}/messages")),
            ("PATCH", format!("/api/v1/sessions/{id}/messages/m1")),
            ("DELETE", format!("/api/v1/sessions/{id}/messages/m1")),
            ("POST", format!("/api/v1/sessions/{id}/attachments")),
            ("GET", format!("/api/v1/sessions/{id}/attachments/a1")),
            ("POST", format!("/api/v1/sessions/{id}/chat")),
        ] {
            let response = h
                .send(
                    Request::builder()
                        .method(method)
                        .uri(&path)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await;
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {path} is not behind the auth layer",
            );
        }
    }

    #[tokio::test]
    async fn one_client_cannot_see_or_touch_another_clients_session() {
        let h = harness();
        let id = h.new_session().await;

        let (status, body) = h
            .call_as(OTHER_TOKEN, "GET", &format!("/api/v1/sessions/{id}"), None)
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        // Not `forbidden`: a wrong owner and a wrong id are indistinguishable.
        assert_eq!(body["error"]["code"], "session_not_found");

        let (status, _) = h
            .call_as(
                OTHER_TOKEN,
                "PATCH",
                &format!("/api/v1/sessions/{id}"),
                Some(json!({ "title": "hijacked" })),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (_, listed) = h
            .call_as(OTHER_TOKEN, "GET", "/api/v1/sessions", None)
            .await;
        assert_eq!(listed, json!([]));
    }

    #[tokio::test]
    async fn a_malformed_create_or_patch_body_is_a_validation_error() {
        let h = harness();
        let id = h.new_session().await;

        for (method, path, body) in [
            // The contract says `title?: string`; a number is not one.
            (
                "POST",
                "/api/v1/sessions".to_string(),
                Some(json!({ "title": 42 })),
            ),
            (
                "POST",
                "/api/v1/sessions".to_string(),
                Some(json!({ "temporary": "yes" })),
            ),
            // An absent body at all.
            ("POST", "/api/v1/sessions".to_string(), None),
            // PATCH requires the title the contract types as optional.
            ("PATCH", format!("/api/v1/sessions/{id}"), Some(json!({}))),
            (
                "PATCH",
                format!("/api/v1/sessions/{id}"),
                Some(json!({ "title": null })),
            ),
        ] {
            let (status, response) = h.call(method, &path, body.clone()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{method} {path} {body:?}");
            assert_eq!(response["error"]["code"], "validation_error");
        }
    }

    #[tokio::test]
    async fn a_temporary_session_is_reachable_by_id_and_absent_from_the_listing() {
        let h = harness();
        let (_, created) = h
            .post("/api/v1/sessions", json!({ "temporary": true }))
            .await;
        assert_eq!(created["temporary"], true);
        let id = created["id"].as_str().unwrap();

        let (status, _) = h.get(&format!("/api/v1/sessions/{id}")).await;
        assert_eq!(status, StatusCode::OK);
        let (_, listed) = h.get("/api/v1/sessions").await;
        assert_eq!(listed, json!([]));
    }

    // --- messages ------------------------------------------------------------

    #[tokio::test]
    async fn messages_append_patch_and_delete_over_http() {
        let h = harness();
        let id = h.new_session().await;

        let (status, appended) = h
            .post(
                &format!("/api/v1/sessions/{id}/messages"),
                json!({ "role": "user", "content": "hello" }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        // `ord` is declared `number` on the wire, so it is `f64` here and
        // serializes as `0.0`, which `Value::as_i64` refuses. Both spellings
        // parse to the same JavaScript number, so the normalizer settles it,
        // the same way the on-disk round-trip test does.
        assert_eq!(super::numbers_as_written(&appended["ord"]), json!(0));
        let msg_id = appended["id"].as_str().unwrap().to_string();

        let (status, patched) = h
            .patch(
                &format!("/api/v1/sessions/{id}/messages/{msg_id}"),
                json!({ "content": "edited" }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        // Through the normalizer for the same reason the round-trip test uses
        // it: `ord` is declared `number` on the wire, so it is `f64` here and
        // serializes as `0.0`. Both parse to the same JavaScript number.
        assert_eq!(
            super::numbers_as_written(&patched),
            json!({ "id": msg_id, "ord": 0 }),
        );

        let (_, fetched) = h.get(&format!("/api/v1/sessions/{id}")).await;
        assert_eq!(fetched["messages"][0]["content"], "edited");
        assert_eq!(fetched["messages"][0]["role"], "user");

        let (status, _) = h
            .del(&format!("/api/v1/sessions/{id}/messages/{msg_id}"))
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, fetched) = h.get(&format!("/api/v1/sessions/{id}")).await;
        assert_eq!(fetched["messages"], json!([]));
    }

    #[tokio::test]
    async fn an_invalid_message_body_is_refused_by_the_generated_type() {
        let h = harness();
        let id = h.new_session().await;
        let path = format!("/api/v1/sessions/{id}/messages");

        for body in [
            // No role at all, and a role outside the union.
            json!({ "content": "hi" }),
            json!({ "role": "wizard", "content": "hi" }),
            // A field that belongs to another role.
            json!({ "role": "user", "content": "hi", "toolCalls": [] }),
            // The right role, a missing required field.
            json!({ "role": "tool", "callId": "c1" }),
            // A malformed multipart content part.
            json!({ "role": "user", "content": [{ "type": "text" }] }),
        ] {
            let (status, response) = h.post(&path, body.clone()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "accepted {body}");
            assert_eq!(response["error"]["code"], "validation_error");
        }
    }

    #[tokio::test]
    async fn patching_a_message_with_another_roles_field_is_refused() {
        let h = harness();
        let id = h.new_session().await;
        let (_, appended) = h
            .post(
                &format!("/api/v1/sessions/{id}/messages"),
                json!({ "role": "user", "content": "hello" }),
            )
            .await;
        let msg_id = appended["id"].as_str().unwrap();
        let path = format!("/api/v1/sessions/{id}/messages/{msg_id}");

        // The guard the per-role patch schemas exist for, reached here through
        // the generated contract type rather than through a restatement of it.
        for body in [json!({ "toolCalls": [] }), json!({ "role": "assistant" })] {
            let (status, response) = h.patch(&path, body.clone()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "accepted {body}");
            assert_eq!(response["error"]["code"], "validation_error");
        }

        let (status, body) = h
            .patch(
                &format!("/api/v1/sessions/{id}/messages/no-such-message"),
                json!({ "content": "x" }),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "message_not_found");
    }

    // --- attachments ---------------------------------------------------------

    const BOUNDARY: &str = "----locatestboundary";

    /// One multipart part: field name, filename (a file part has one), content
    /// type, bytes.
    type Part<'a> = (&'a str, Option<&'a str>, Option<&'a str>, &'a [u8]);

    fn multipart(parts: &[Part<'_>]) -> Vec<u8> {
        let mut body = Vec::new();
        for (name, filename, content_type, bytes) in parts {
            body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
            let disposition = match filename {
                Some(f) => {
                    format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"{f}\"\r\n")
                }
                None => format!("Content-Disposition: form-data; name=\"{name}\"\r\n"),
            };
            body.extend_from_slice(disposition.as_bytes());
            if let Some(ct) = content_type {
                body.extend_from_slice(format!("Content-Type: {ct}\r\n").as_bytes());
            }
            body.extend_from_slice(b"\r\n");
            body.extend_from_slice(bytes);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
        body
    }

    impl Harness {
        async fn upload(&self, session_id: &str, parts: &[Part<'_>]) -> (StatusCode, Value) {
            let response = self
                .send(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/api/v1/sessions/{session_id}/attachments"))
                        .header("authorization", format!("Bearer {TOKEN}"))
                        .header(
                            "content-type",
                            format!("multipart/form-data; boundary={BOUNDARY}"),
                        )
                        .body(Body::from(multipart(parts)))
                        .unwrap(),
                )
                .await;
            let status = response.status();
            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            (
                status,
                serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            )
        }
    }

    #[tokio::test]
    async fn an_attachment_uploads_and_downloads_with_its_content_type() {
        let h = harness();
        let id = h.new_session().await;
        let png = [0x89u8, 0x50, 0x4e, 0x47, 1, 2, 3];

        let (status, uploaded) = h
            .upload(
                &id,
                &[
                    ("messageId", None, None, b"m1"),
                    ("file", Some("photo.png"), Some("image/png"), &png),
                ],
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(uploaded["filename"], "photo.png");
        let attachment_id = uploaded["id"].as_str().unwrap().to_string();
        let abs_path = uploaded["absPath"].as_str().unwrap();
        // Written under the session's own attachments directory, behind an id
        // prefix. That prefix is a SEPARATE id from the record's, in the ported
        // core as in this one: the upload route mints one to make the filename
        // unique and the store mints the record's own. Nothing ever parses the
        // prefix (`attachment_id_from_path` reads the URL, which carries the
        // record id), so the two never need to agree.
        let file_name = abs_path.rsplit('/').next().unwrap();
        let (prefix, rest) = file_name.split_once('_').unwrap();
        assert_eq!(rest, "photo.png");
        assert_eq!(prefix.len(), 26, "the prefix is not a ULID: {prefix}");
        assert!(abs_path.contains(&format!("/sessions/{id}/attachments/")));
        assert!(std::path::Path::new(abs_path).is_file());

        let response = h
            .send(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/v1/sessions/{id}/attachments/{attachment_id}"))
                    .header("authorization", format!("Bearer {TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "image/png");
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(bytes.to_vec(), png);

        // The record is on the session, and reachable by id.
        let record = h.store.get_attachment(&id, &attachment_id).await.unwrap();
        assert_eq!(record.size_bytes, png.len() as u64);
        assert_eq!(record.message_id, "m1");
    }

    #[tokio::test]
    async fn an_uploaded_filename_cannot_escape_the_attachments_directory() {
        let h = harness();
        let id = h.new_session().await;
        let (status, uploaded) = h
            .upload(
                &id,
                &[
                    ("messageId", None, None, b"m1"),
                    ("file", Some("../../etc/passwd"), None, b"x"),
                ],
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(uploaded["filename"], "_.._etc_passwd");
        let abs_path = std::path::Path::new(uploaded["absPath"].as_str().unwrap());
        // The dots survive as literal characters, which is fine and is what the
        // TypeScript produces too. What matters is that they are one path
        // segment: the file resolves inside the attachments directory rather
        // than above it, which the canonical parent is the only honest way to
        // assert.
        assert!(abs_path.is_file());
        let attachments = h._tmp.path().join("sessions").join(&id).join("attachments");
        assert_eq!(
            abs_path.parent().unwrap().canonicalize().unwrap(),
            attachments.canonicalize().unwrap(),
        );
        assert!(!std::path::Path::new(&format!("{}/etc/passwd", h._tmp.path().display())).exists());
    }

    #[tokio::test]
    async fn an_upload_missing_either_part_is_a_validation_error() {
        let h = harness();
        let id = h.new_session().await;
        for parts in [
            vec![("file", Some("a.png"), None, b"x".as_slice())],
            vec![("messageId", None, None, b"m1".as_slice())],
            vec![],
        ] {
            let (status, body) = h.upload(&id, &parts).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(body["error"]["code"], "validation_error");
        }
    }

    #[tokio::test]
    async fn a_recorded_mime_that_is_not_a_legal_header_falls_back_rather_than_failing() {
        let h = harness();
        let id = h.new_session().await;
        let file = h._tmp.path().join("hostile.bin");
        std::fs::write(&file, b"payload").unwrap();

        // Recorded directly rather than uploaded: multer rejects a malformed
        // part header before the route ever sees it, so an upload cannot
        // produce this. A record already on disk can, and that is the case that
        // matters, because `mime` reaches a response header and a stored
        // newline would be a header injection.
        let record = h
            .store
            .record_attachment(
                &id,
                NewAttachment {
                    message_id: "m1".into(),
                    filename: "hostile.bin".into(),
                    mime: Some("text/plain\r\nX-Injected: yes".into()),
                    size_bytes: 7,
                    abs_path: file.to_string_lossy().into_owned(),
                },
            )
            .await
            .unwrap();

        let response = h
            .send(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/v1/sessions/{id}/attachments/{}", record.id))
                    .header("authorization", format!("Bearer {TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        // Served at the safe default rather than failing the download, and the
        // smuggled header is nowhere.
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()["content-type"],
            "application/octet-stream"
        );
        assert!(response.headers().get("x-injected").is_none());
    }

    #[tokio::test]
    async fn a_missing_attachment_is_not_found() {
        let h = harness();
        let id = h.new_session().await;
        let (status, body) = h
            .get(&format!("/api/v1/sessions/{id}/attachments/nope"))
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "not_found");
    }

    // --- title regeneration --------------------------------------------------

    #[tokio::test]
    async fn regenerate_title_hands_off_to_the_chat_engine_and_returns_204() {
        let h = harness();
        let id = h.new_session().await;

        let (status, _) = h
            .post_none(&format!("/api/v1/sessions/{id}/regenerate-title"))
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(h.chat.titles_regenerated.load(Ordering::SeqCst), 1);

        // An unknown session never reaches the engine.
        let (status, _) = h.post_none("/api/v1/sessions/nope/regenerate-title").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(h.chat.titles_regenerated.load(Ordering::SeqCst), 1);
    }

    // --- the SSE chat fallback -----------------------------------------------

    fn delta(text: &str) -> OneShotDelta {
        OneShotDelta {
            content_delta: Some(text.to_string()),
            usage: None,
        }
    }

    /// The `data:` payloads of an SSE body, in order.
    async fn sse_payloads(response: Response) -> Vec<Value> {
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        String::from_utf8(bytes.to_vec())
            .unwrap()
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .map(|json| serde_json::from_str(json).unwrap())
            .collect()
    }

    impl Harness {
        fn chat_request(&self, session_id: &str, content: &str) -> Request<Body> {
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/sessions/{session_id}/chat"))
                .header("authorization", format!("Bearer {TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "content": content }).to_string()))
                .unwrap()
        }
    }

    #[tokio::test]
    async fn the_sse_fallback_streams_deltas_then_done_and_persists_both_turns() {
        let usage = crate::contract::api::TokenUsage {
            prompt: 7,
            completion: 3,
            total: 10,
        };
        let h = harness_with(ScriptedChat {
            deltas: vec![
                delta("Hello"),
                // A chunk carrying only usage produces no output line.
                OneShotDelta {
                    content_delta: None,
                    usage: Some(usage),
                },
                delta(" world"),
            ],
            ..Default::default()
        });
        let id = h.new_session().await;

        let response = h.send(h.chat_request(&id, "hi there")).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        assert_eq!(response.headers()["cache-control"], "no-cache");

        assert_eq!(
            sse_payloads(response).await,
            vec![
                json!({ "contentDelta": "Hello" }),
                json!({ "contentDelta": " world" }),
                json!({ "done": true, "usage": { "prompt": 7, "completion": 3, "total": 10 } }),
            ],
        );

        // The user message was persisted before the engine ran, so the engine
        // saw it, and the assistant turn was persisted after.
        assert_eq!(h.chat.last_history_len.load(Ordering::SeqCst), 1);
        let messages = h.store.list_messages(&id).await;
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role(), Some("user"));
        assert_eq!(content_to_text(messages[0].content()), "hi there");
        assert_eq!(messages[1].role(), Some("assistant"));
        assert_eq!(content_to_text(messages[1].content()), "Hello world");
    }

    #[tokio::test]
    async fn an_sse_failure_is_a_terminal_error_line_and_persists_no_assistant_turn() {
        let h = harness_with(ScriptedChat {
            fail_with: Some("upstream said: Bearer sk-abcdefghijklmnopqrstuvwxyz012345"),
            ..Default::default()
        });
        let id = h.new_session().await;

        let response = h.send(h.chat_request(&id, "hi")).await;
        assert_eq!(response.status(), StatusCode::OK);
        let payloads = sse_payloads(response).await;
        assert_eq!(payloads.len(), 1);
        let message = payloads[0]["error"].as_str().unwrap();
        // Scrubbed on its way out: a provider error can echo a URL with a token.
        assert!(
            !message.contains("sk-abcdefghijklmnopqrstuvwxyz012345"),
            "a token reached the wire: {message}",
        );

        // The user message stays (the client can replay from history); nothing
        // is invented for the assistant.
        let messages = h.store.list_messages(&id).await;
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role(), Some("user"));
    }

    #[tokio::test]
    async fn an_empty_or_malformed_chat_body_is_refused_before_anything_is_persisted() {
        let h = harness();
        let id = h.new_session().await;
        let path = format!("/api/v1/sessions/{id}/chat");

        for body in [json!({ "content": "" }), json!({}), json!({ "content": 4 })] {
            let (status, response) = h.post(&path, body.clone()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "accepted {body}");
            assert_eq!(response["error"]["code"], "validation_error");
        }
        assert!(h.store.list_messages(&id).await.is_empty());
    }

    #[tokio::test]
    async fn a_second_stream_on_one_session_is_refused_while_the_first_is_in_flight() {
        let (release, gate) = oneshot::channel();
        let h = harness_with(ScriptedChat {
            deltas: vec![delta("answer")],
            gate: Mutex::new(Some(gate)),
            ..Default::default()
        });
        let id = h.new_session().await;

        // Held, not drained: the body is what owns the session's slot.
        let first = h.send(h.chat_request(&id, "one")).await;
        assert_eq!(first.status(), StatusCode::OK);

        let (status, body) = h
            .post(
                &format!("/api/v1/sessions/{id}/chat"),
                json!({ "content": "two" }),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"]["code"], "conflict");
        // Refused before anything was persisted, so the two turns cannot
        // interleave in the transcript.
        assert_eq!(h.store.list_messages(&id).await.len(), 1);

        // A different session is unaffected: the guard is per session.
        let other = h.new_session().await;
        let (status, _) = h
            .post(
                &format!("/api/v1/sessions/{other}/chat"),
                json!({ "content": "x" }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);

        let _ = release.send(());
        let payloads = sse_payloads(first).await;
        assert_eq!(payloads[0], json!({ "contentDelta": "answer" }));

        // The slot is released once the stream ends, so the session is usable
        // again.
        let (status, _) = h
            .post(
                &format!("/api/v1/sessions/{id}/chat"),
                json!({ "content": "three" }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn a_client_that_disconnects_mid_stream_releases_the_session_slot() {
        let (release, gate) = oneshot::channel();
        let h = harness_with(ScriptedChat {
            deltas: vec![delta("answer")],
            gate: Mutex::new(Some(gate)),
            ..Default::default()
        });
        let id = h.new_session().await;

        let first = h.send(h.chat_request(&id, "one")).await;
        assert_eq!(first.status(), StatusCode::OK);
        // What a disconnect is: the response body is dropped without ever being
        // read. There is no completion callback to run, so only the guard's
        // `Drop` can release the slot.
        drop(first);
        let _ = release.send(());

        let (status, _) = h
            .post(
                &format!("/api/v1/sessions/{id}/chat"),
                json!({ "content": "two" }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "the slot was never released");
    }

    #[tokio::test]
    async fn chat_on_a_missing_or_someone_elses_session_never_reaches_the_engine() {
        let h = harness();
        let id = h.new_session().await;

        let (status, _) = h
            .post("/api/v1/sessions/nope/chat", json!({ "content": "hi" }))
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, body) = h
            .call_as(
                OTHER_TOKEN,
                "POST",
                &format!("/api/v1/sessions/{id}/chat"),
                Some(json!({ "content": "hi" })),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "session_not_found");
        assert!(h.store.list_messages(&id).await.is_empty());
    }
}
