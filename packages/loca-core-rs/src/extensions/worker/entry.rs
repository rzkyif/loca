//! Which file inside an extension the worker imports.
//!
//! Ports `packages/loca-core/src/extensions/worker-entry-resolution.ts`.

use std::path::{Path, PathBuf};

use crate::contain::is_within;

/// Resolve the module the worker imports, in the order Deno itself would.
///
/// `deno.json`'s `exports`, then `package.json`'s `main`, then `index.ts`. The
/// containment check is the point: a manifest that names `../../evil.ts` must
/// not aim the worker outside the directory whose content hash was verified.
///
/// Lexical containment, matching the sandbox's self-write refusal, and with the
/// same accepted residual: a symlink inside the folder pointing out. D12's Q2
/// closes that from the other side by refusing to install an extension that
/// contains one.
pub fn resolve_entry_path(folder: &Path) -> PathBuf {
    for candidate in [
        json_string_field(&folder.join("deno.json"), "exports"),
        json_string_field(&folder.join("package.json"), "main"),
    ]
    .into_iter()
    .flatten()
    {
        let resolved = folder.join(&candidate);
        if is_within(folder, &resolved) {
            return resolved;
        }
        tracing::warn!(
            target: "loca::extensions",
            "ignoring an entry point that points outside the extension: {candidate}",
        );
    }
    folder.join("index.ts")
}

/// Read one top-level string field out of a JSON file.
///
/// `exports` may also be an object in the general Deno vocabulary; only the
/// string form names a single entry module, and anything else falls through to
/// the next candidate rather than being guessed at.
fn json_string_field(path: &Path, field: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let found = value.get(field)?.as_str()?.to_string();
    (!found.is_empty()).then_some(found)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, contents: &str) {
        std::fs::write(dir.join(name), contents).unwrap();
    }

    #[test]
    fn deno_json_exports_wins_then_package_main_then_index() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        assert_eq!(resolve_entry_path(dir), dir.join("index.ts"));

        write(dir, "package.json", r#"{"main":"lib/main.js"}"#);
        assert_eq!(resolve_entry_path(dir), dir.join("lib/main.js"));

        write(dir, "deno.json", r#"{"exports":"./mod.ts"}"#);
        assert_eq!(resolve_entry_path(dir), dir.join("./mod.ts"));
    }

    #[test]
    fn an_entry_pointing_outside_the_extension_is_refused() {
        // The manifest is author-supplied and the folder is what the content
        // hash attests. An entry escaping it runs code the hash never saw.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write(dir, "deno.json", r#"{"exports":"../../evil.ts"}"#);
        let resolved = resolve_entry_path(dir);
        assert!(
            is_within(dir, &resolved),
            "the worker was aimed outside the extension: {}",
            resolved.display()
        );
        // It falls through to the next candidate rather than failing hard,
        // matching the TypeScript.
        assert_eq!(resolved, dir.join("index.ts"));
    }

    #[test]
    fn a_traversal_that_lands_back_inside_is_allowed() {
        // `sub/../mod.ts` really is inside, and refusing it would be a lexical
        // check pretending to be a security one.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write(dir, "deno.json", r#"{"exports":"sub/../mod.ts"}"#);
        assert_eq!(resolve_entry_path(dir), dir.join("sub/../mod.ts"));
    }

    #[test]
    fn a_malformed_or_non_string_field_falls_through() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write(dir, "deno.json", "{not json");
        write(dir, "package.json", r#"{"main":{"import":"./x.js"}}"#);
        assert_eq!(resolve_entry_path(dir), dir.join("index.ts"));

        // An empty string is not an entry point either: joining it would
        // resolve to the folder itself, which the worker cannot import.
        write(dir, "deno.json", r#"{"exports":""}"#);
        assert_eq!(resolve_entry_path(dir), dir.join("index.ts"));
    }
}
