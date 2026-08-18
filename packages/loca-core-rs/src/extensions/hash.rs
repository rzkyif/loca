//! The extension content hash: the trust anchor the drift gate rests on.
//!
//! Ports `packages/loca-core/src/extensions/hash.ts`.
//!
//! # Why the preimage is spelled out rather than described
//!
//! This digest is not a wire value, so nothing in the conformance suite would
//! notice it changing. What WOULD notice is every already-installed extension
//! on the first boot after cutover: a hash that differs from the pinned one by
//! a single byte flags the extension `drift`, which disables all of its tools
//! with no explanation the user can act on. So the algorithm below is a
//! byte-for-byte port and every deliberate choice in it is named at the site:
//!
//! 1. Walk the folder; include regular files only.
//! 2. Always exclude any `node_modules` directory and the ROOT `deno.lock`.
//! 3. If `<root>/.gitignore` exists, exclude what it matches. The `.gitignore`
//!    itself is hashed, so editing the ignore rules moves the digest too.
//! 4. Sort the included paths, in forward-slash form.
//! 5. SHA-256 over the concatenation of `rel \0 size \0 <bytes> \0` per file.
//!
//! `node_modules` and `deno.lock` are excluded because `deno install` writes
//! them AFTER the hash is pinned, and a later `deno run` can rewrite the lock;
//! including them would read benign install churn as tampering. The `deno.json`
//! they derive from IS hashed, so a tampered import map is still caught.
//!
//! # The two weaknesses this inherits, deliberately
//!
//! [`stat_signature`] is a cheap skip key for the per-call re-hash, and a
//! modification preserving the file count, the summed size and the newest mtime
//! defeats it until the next boot. That is a second lock on a door whose first
//! lock is the sandbox refusing an extension write access to its own install
//! directory (see [`super::sandbox`]); the cheaper strong signal (entry subtree
//! per call, whole tree at boot) is the known better answer and was deferred
//! deliberately rather than missed.
//!
//! Symlinks are the one shape that would be both unhashed and reachable, since
//! `import` follows one and a hash that skipped it would attest nothing about
//! its target. They are therefore REFUSED here rather than skipped, which is a
//! deviation from the specification and the only one in this file.

use std::io::Read;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use sha2::{Digest, Sha256};

use crate::error::{AppError, ErrorCode, Result};

const NODE_MODULES: &str = "node_modules";
const DENO_LOCK: &str = "deno.lock";

/// Read in chunks rather than whole files: an extension may legitimately ship a
/// large asset, and the digest is streaming anyway.
const READ_CHUNK: usize = 64 * 1024;

/// The sorted, forward-slash relative paths the hash covers.
///
/// Shared by [`hash_extension`] and [`stat_signature`] so the set that is
/// hashed and the set that is fingerprinted cannot diverge: two walks would let
/// a file be counted by one and hashed by the other.
pub fn included_files(root: &Path) -> Result<Vec<String>> {
    let ignore = load_gitignore(root);
    let mut files = Vec::new();
    walk(root, root, &ignore, &mut files)?;
    // JavaScript's `Array.prototype.sort` compares UTF-16 code units, and Rust's
    // `str` compares UTF-8 bytes. The two agree for everything in the BMP and
    // DISAGREE above it: an astral character encodes as a surrogate pair
    // starting at 0xD800 in UTF-16, so `emoji.txt` sorts BEFORE `\u{FFFD}.txt`
    // there and after it here. Sorting by code unit is what keeps a bundle
    // containing an emoji filename hashing to the same digest as before the
    // cutover; it is a one-line difference and an unrecoverable one to miss.
    files.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    Ok(files)
}

/// Lowercase hex SHA-256 over a buffer.
///
/// Used for the MANIFEST hash, which is a different thing from the content
/// hash above and is not a security gate: it is the cheap "did `loca.json`
/// change" heuristic rescan uses to decide whether to re-register a folder.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// The content hash: lowercase hex SHA-256 over the whole included set.
pub fn hash_extension(root: &Path) -> Result<String> {
    let files = included_files(root)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0u8; READ_CHUNK];

    for rel in &files {
        let abs = root.join(rel);
        // The size comes from a stat rather than from counting the bytes read,
        // exactly as the specification does it. A file that vanished between
        // the walk and here is SKIPPED rather than failing the hash: the next
        // call reads a different set and produces a different digest, which is
        // the drift signal doing its job.
        let Ok(meta) = std::fs::metadata(&abs) else {
            continue;
        };
        digest.update(format!("{rel}\0{}\0", meta.len()).as_bytes());

        let mut file = std::fs::File::open(&abs)?;
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
        }
        digest.update([0u8]);
    }
    Ok(hex::encode(digest.finalize()))
}

/// A stat-only fingerprint of the hashed set: `count:totalSize:newestMtime`.
///
/// Used as the per-call re-hash skip key. Mtime alone would not do: a tool with
/// write access to its own folder could reset it to the previously-verified
/// value, and matching the count and the summed size as well is far harder to
/// forge while actually changing content.
///
/// Unreadable answers `0:0:0` rather than failing, because this is a cache key
/// and a key that cannot be computed simply must not match anything.
pub fn stat_signature(root: &Path) -> String {
    let Ok(files) = included_files(root) else {
        return "0:0:0".to_string();
    };
    let mut count: u64 = 0;
    let mut total: u64 = 0;
    let mut newest: i64 = 0;
    for rel in &files {
        let Ok(meta) = std::fs::metadata(root.join(rel)) else {
            // Vanished mid-walk. Ignored, as in the specification.
            continue;
        };
        count += 1;
        total += meta.len();
        if let Ok(modified) = meta.modified() {
            newest = newest.max(epoch_ms(modified));
        }
    }
    format!("{count}:{total}:{newest}")
}

/// Milliseconds since the Unix epoch, negative before it, which is what
/// `Date.prototype.getTime` answers.
fn epoch_ms(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(since) => i64::try_from(since.as_millis()).unwrap_or(i64::MAX),
        Err(before) => -i64::try_from(before.duration().as_millis()).unwrap_or(i64::MAX),
    }
}

/// The root `.gitignore`, or an empty matcher when there is none.
///
/// ripgrep's implementation rather than a hand-rolled glob pass: getting
/// gitignore precedence wrong changes which files are hashed, which changes the
/// digest, which drifts every installed extension at once. A build error is
/// swallowed for the same reason the specification wraps the read in a
/// try/catch: a malformed ignore file must not make an extension unhashable.
fn load_gitignore(root: &Path) -> Gitignore {
    let mut builder = GitignoreBuilder::new(root);
    let _ = builder.add(root.join(".gitignore"));
    builder.build().unwrap_or_else(|_| Gitignore::empty())
}

fn walk(root: &Path, dir: &Path, ignore: &Gitignore, out: &mut Vec<String>) -> Result<()> {
    let entries = std::fs::read_dir(dir)?;
    for entry in entries {
        let entry = entry?;
        let full = entry.path();
        let file_type = entry.file_type()?;
        let rel = relative_slashed(root, &full);

        // `file_type` here comes from the directory entry and does NOT follow
        // the link, which is the whole reason this check can exist.
        if file_type.is_symlink() {
            return Err(AppError::new(
                ErrorCode::InvalidLocaJson,
                format!(
                    "extension contains a symbolic link ({rel}); \
                     symlinks cannot be content-verified and are not allowed"
                ),
            ));
        }

        if file_type.is_dir() {
            if entry.file_name() == NODE_MODULES {
                continue;
            }
            if ignore.matched(&full, true).is_ignore() {
                continue;
            }
            walk(root, &full, ignore, out)?;
        } else if file_type.is_file() {
            if rel == DENO_LOCK {
                continue;
            }
            if ignore.matched(&full, false).is_ignore() {
                continue;
            }
            out.push(rel);
        }
        // Anything else (a fifo, a socket, a device node) is skipped, as in the
        // specification: only files and directories are traversed.
    }
    Ok(())
}

/// The path of `full` relative to `root`, always forward-slashed.
///
/// Forward slashes on every platform because the separator is part of the
/// hashed preimage: a Windows core writing `a\b.ts` would compute a different
/// digest for the same bundle than the macOS core that pinned it.
fn relative_slashed(root: &Path, full: &Path) -> String {
    full.strip_prefix(root)
        .unwrap_or(full)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    use std::fs;
    use std::path::PathBuf;

    /// The fixture tree, as `(relative path, contents)`.
    ///
    /// Written out rather than checked in for two reasons that are both
    /// practical: a committed `.gitignore` inside a fixture would make git
    /// ignore the very files the fixture needs, and a committed
    /// `node_modules/` confuses every tool in this repo.
    ///
    /// **This list is the oracle's input too.** The expected digest below was
    /// produced by running the shipping TypeScript `hashExtension` over a tree
    /// built from exactly these entries, so changing one without regenerating
    /// the digest is a test that fails for the right reason.
    const FIXTURE: &[(&str, &[u8])] = &[
        ("loca.json", b"{\"id\":\"fixture\",\"version\":\"1.0.0\"}\n"),
        ("deno.json", b"{\"exports\":\"./main.ts\"}\n"),
        // Excluded: the ROOT lock only.
        ("deno.lock", b"{\"version\":\"4\"}\n"),
        // Included: a nested lock is not the root one.
        ("nested/deno.lock", b"nested lock is hashed\n"),
        ("main.ts", b"export const answer = 42;\n"),
        ("nested/deep/util.ts", b"export const x = 1;\n"),
        // An empty file still contributes its name and a zero size.
        ("empty.txt", b""),
        // Bytes that are not valid UTF-8, to pin that the hash is over bytes.
        ("assets/blob.bin", &[0x00, 0xff, 0xfe, 0x41, 0x0a]),
        // The ignore rules are themselves content.
        (".gitignore", b"ignored.txt\nbuild/\n*.log\n"),
        ("ignored.txt", b"never hashed\n"),
        ("build/output.js", b"never hashed either\n"),
        ("noisy.log", b"nor this\n"),
        // Excluded at any depth.
        ("node_modules/dep/index.js", b"module.exports = 1;\n"),
        ("nested/node_modules/dep/index.js", b"module.exports = 2;\n"),
    ];

    fn build_fixture(root: &Path) {
        for (rel, contents) in FIXTURE {
            let path = root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, contents).unwrap();
        }
    }

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        build_fixture(&root);
        (dir, root)
    }

    /// Produced by the shipping TypeScript implementation, not by this one.
    ///
    /// Regenerate with a script that imports
    /// `packages/loca-core/src/extensions/hash.ts`, writes the FIXTURE tree,
    /// and prints `await hashExtension(dir)`. If this constant is ever updated
    /// to whatever the Rust currently answers, the test stops being a parity
    /// test and starts being a change detector, and the cutover drift it exists
    /// to prevent goes unnoticed.
    const ORACLE_DIGEST: &str = "5a2cd5741bd33e8c9f8e6746c7b7f219d059d1f79d42e6902322600fb88c7129";

    /// The included set, as the oracle's `listIncludedFiles` reports it.
    ///
    /// Pinned separately from the digest so a divergence says WHICH file moved
    /// rather than only that some byte differed.
    const ORACLE_FILES: &[&str] = &[
        ".gitignore",
        "assets/blob.bin",
        "deno.json",
        "empty.txt",
        "loca.json",
        "main.ts",
        "nested/deep/util.ts",
        "nested/deno.lock",
    ];

    #[test]
    fn the_included_set_matches_the_oracles() {
        let (_dir, root) = fixture();
        assert_eq!(included_files(&root).unwrap(), ORACLE_FILES);
    }

    #[test]
    fn the_digest_matches_the_shipping_implementation_byte_for_byte() {
        let (_dir, root) = fixture();
        assert_eq!(hash_extension(&root).unwrap(), ORACLE_DIGEST);
    }

    #[test]
    fn the_digest_is_lowercase_hex_sha256() {
        let (_dir, root) = fixture();
        let digest = hash_extension(&root).unwrap();
        assert_eq!(digest.len(), 64);
        assert!(digest
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)));
    }

    #[test]
    fn two_identical_trees_hash_identically() {
        let (_a, root_a) = fixture();
        let (_b, root_b) = fixture();
        assert_eq!(
            hash_extension(&root_a).unwrap(),
            hash_extension(&root_b).unwrap()
        );
    }

    #[test]
    fn changing_a_files_content_moves_the_digest() {
        let (_dir, root) = fixture();
        let before = hash_extension(&root).unwrap();
        fs::write(root.join("main.ts"), b"export const answer = 43;\n").unwrap();
        assert_ne!(hash_extension(&root).unwrap(), before);
    }

    /// The preimage carries the size, so two files whose bytes could be split
    /// differently between them must not collide.
    ///
    /// Without the length prefix `a` = "xy", `b` = "" and `a` = "x", `b` = "y"
    /// feed the hasher the same stream.
    #[test]
    fn the_length_prefix_stops_a_boundary_collision() {
        let one = tempfile::tempdir().unwrap();
        fs::write(one.path().join("a"), b"xy").unwrap();
        fs::write(one.path().join("b"), b"").unwrap();

        let two = tempfile::tempdir().unwrap();
        fs::write(two.path().join("a"), b"x").unwrap();
        fs::write(two.path().join("b"), b"y").unwrap();

        assert_ne!(
            hash_extension(one.path()).unwrap(),
            hash_extension(two.path()).unwrap()
        );
    }

    /// Renaming a file with the same content must move the digest, which is
    /// what hashing the path rather than only the bytes buys.
    #[test]
    fn a_rename_moves_the_digest() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.ts"), b"same").unwrap();
        let before = hash_extension(dir.path()).unwrap();
        fs::rename(dir.path().join("a.ts"), dir.path().join("b.ts")).unwrap();
        assert_ne!(hash_extension(dir.path()).unwrap(), before);
    }

    #[test]
    fn node_modules_is_excluded_at_every_depth() {
        // `deno install` writes it AFTER the hash is pinned, so hashing it
        // would flag every dependency-carrying extension as drifted the moment
        // its dependencies were installed.
        let (_dir, root) = fixture();
        let before = hash_extension(&root).unwrap();
        fs::create_dir_all(root.join("node_modules/new-dep")).unwrap();
        fs::write(root.join("node_modules/new-dep/index.js"), b"x").unwrap();
        fs::create_dir_all(root.join("nested/node_modules/other")).unwrap();
        fs::write(root.join("nested/node_modules/other/index.js"), b"y").unwrap();
        assert_eq!(hash_extension(&root).unwrap(), before);
    }

    #[test]
    fn the_root_deno_lock_is_excluded_and_a_nested_one_is_not() {
        // The root lock is rewritten by `deno install` and by a later
        // `deno run`; a nested one is ordinary content the author shipped.
        let (_dir, root) = fixture();
        let before = hash_extension(&root).unwrap();
        fs::write(root.join("deno.lock"), b"{\"version\":\"5\"}\n").unwrap();
        assert_eq!(hash_extension(&root).unwrap(), before);

        fs::write(root.join("nested/deno.lock"), b"changed\n").unwrap();
        assert_ne!(hash_extension(&root).unwrap(), before);
    }

    #[test]
    fn gitignored_paths_are_excluded_but_the_gitignore_itself_is_hashed() {
        let (_dir, root) = fixture();
        let before = hash_extension(&root).unwrap();

        // Matched by name, by directory rule, and by glob: all three invisible.
        fs::write(root.join("ignored.txt"), b"different\n").unwrap();
        fs::write(root.join("build/output.js"), b"different\n").unwrap();
        fs::write(root.join("noisy.log"), b"different\n").unwrap();
        assert_eq!(hash_extension(&root).unwrap(), before);

        // The rules themselves are content: changing them changes what is
        // hashed, so the digest MUST move or an attacker could hide a file by
        // adding a line to the ignore file.
        fs::write(root.join(".gitignore"), b"ignored.txt\nbuild/\n").unwrap();
        assert_ne!(hash_extension(&root).unwrap(), before);
    }

    #[test]
    fn a_symlink_anywhere_in_the_tree_is_refused_rather_than_skipped() {
        // The one deviation from the specification, and the reason for it: a
        // skipped symlink is unhashed, while `import` follows it happily, so
        // the one shape the gate cannot see is also the one shape that runs.
        #[cfg(unix)]
        {
            let (_dir, root) = fixture();
            std::os::unix::fs::symlink(root.join("main.ts"), root.join("alias.ts")).unwrap();
            let err = hash_extension(&root).unwrap_err();
            assert_eq!(err.code, ErrorCode::InvalidLocaJson);
            assert!(err.message.contains("alias.ts"), "{}", err.message);

            // And nested, so the refusal is not only a root-level check.
            fs::remove_file(root.join("alias.ts")).unwrap();
            assert!(hash_extension(&root).is_ok());
            std::os::unix::fs::symlink(root.join("main.ts"), root.join("nested/alias.ts")).unwrap();
            assert_eq!(
                hash_extension(&root).unwrap_err().code,
                ErrorCode::InvalidLocaJson
            );
        }
    }

    /// A symlinked DIRECTORY is refused too, which is the case that would
    /// otherwise let a whole subtree in unhashed.
    #[test]
    #[cfg(unix)]
    fn a_symlinked_directory_is_refused_as_well() {
        let (_dir, root) = fixture();
        std::os::unix::fs::symlink(root.join("nested"), root.join("alias")).unwrap();
        assert_eq!(
            hash_extension(&root).unwrap_err().code,
            ErrorCode::InvalidLocaJson
        );
    }

    #[test]
    fn the_stat_signature_is_the_three_terms_it_says_it_is() {
        let (_dir, root) = fixture();
        let signature = stat_signature(&root);
        let parts: Vec<&str> = signature.split(':').collect();
        assert_eq!(parts.len(), 3, "{signature}");
        assert_eq!(parts[0], ORACLE_FILES.len().to_string());
    }

    /// Each of the three terms, pinned with the other two held constant.
    ///
    /// Holding the mtime constant means forging it, which is not a test trick:
    /// it is exactly the attack this signature exists to resist. Mtime alone
    /// was defeated by a tool resetting its own files' clock after editing
    /// them, so the test plays that attacker and checks the other two terms
    /// still move.
    ///
    /// Two mutations survived the earlier version of this test, both for one
    /// reason: it changed a file, which moves the count AND the size AND the
    /// mtime together, so any single term carrying the whole signal looked
    /// identical to all three working.
    #[test]
    fn each_term_of_the_stat_signature_is_load_bearing_on_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("a.txt"), b"0123456789").unwrap();
        let pinned = fs::metadata(root.join("a.txt"))
            .unwrap()
            .modified()
            .unwrap();
        let forge = |name: &str, at: SystemTime| {
            let file = fs::File::options()
                .write(true)
                .open(root.join(name))
                .unwrap();
            file.set_times(fs::FileTimes::new().set_accessed(at).set_modified(at))
                .unwrap();
        };
        let baseline = stat_signature(root);

        // SIZE alone: still one file, mtime forged back to the original.
        fs::write(root.join("a.txt"), b"01234567890123456789").unwrap();
        forge("a.txt", pinned);
        assert_ne!(
            stat_signature(root),
            baseline,
            "a size change was invisible"
        );

        // COUNT alone: two files summing to the original size, same mtime.
        fs::write(root.join("a.txt"), b"01234").unwrap();
        fs::write(root.join("b.txt"), b"56789").unwrap();
        forge("a.txt", pinned);
        forge("b.txt", pinned);
        assert_ne!(
            stat_signature(root),
            baseline,
            "a file count change was invisible"
        );

        // MTIME alone: back to exactly the original tree, then touched.
        fs::remove_file(root.join("b.txt")).unwrap();
        fs::write(root.join("a.txt"), b"0123456789").unwrap();
        forge("a.txt", pinned);
        assert_eq!(
            stat_signature(root),
            baseline,
            "the fixture did not restore, so the two assertions above proved nothing"
        );
        forge("a.txt", pinned + std::time::Duration::from_secs(60));
        assert_ne!(
            stat_signature(root),
            baseline,
            "an in-place edit that preserved the size was invisible"
        );
    }

    /// The separator is part of the hashed preimage, so it is normalised rather
    /// than left native.
    ///
    /// Tested on the function rather than through the filesystem, because a
    /// macOS or Linux path never contains a backslash by accident and the
    /// normalisation is therefore unobservable on the only two platforms with
    /// CI. A Windows core that skipped it would compute a different digest for
    /// the same bundle than the machine that pinned it, and every extension
    /// would read as drifted on that host alone.
    ///
    /// A unix file whose NAME contains a backslash is rewritten too. That is
    /// not something introduced here: `hash.ts:135` does the same `replaceAll`,
    /// so reproducing it is the parity this file exists for.
    #[test]
    fn a_relative_path_is_forward_slashed_whatever_the_host_uses() {
        assert_eq!(
            relative_slashed(Path::new("/root"), Path::new("/root/a\\b.txt")),
            "a/b.txt"
        );
        assert_eq!(
            relative_slashed(Path::new("/root"), Path::new("/root/nested/x.ts")),
            "nested/x.ts"
        );
    }

    #[test]
    fn an_unreadable_root_signs_as_zero_rather_than_matching_anything() {
        // A key that cannot be computed must not compare equal to a previously
        // stored one, or an extension whose folder vanished would skip its
        // re-hash forever.
        let signature = stat_signature(Path::new("/definitely/not/a/real/path"));
        assert_eq!(signature, "0:0:0");
    }

    #[test]
    fn an_empty_folder_hashes_to_the_empty_digest() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            hash_extension(dir.path()).unwrap(),
            // SHA-256 of nothing at all.
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    /// Sorting is by UTF-16 code unit, as JavaScript's `Array.sort` does it.
    ///
    /// The two orders disagree only above the BMP, so this is the only shape
    /// that can catch a `sort()` here: an astral character encodes as a
    /// surrogate pair starting at 0xD800 in UTF-16 and as bytes starting at
    /// 0xF0 in UTF-8, so it sorts before U+FFFD there and after it here.
    #[test]
    fn filenames_sort_the_way_the_specification_sorted_them() {
        let dir = tempfile::tempdir().unwrap();
        let astral = "\u{1F600}.txt";
        let bmp = "\u{FFFD}.txt";
        fs::write(dir.path().join(astral), b"a").unwrap();
        fs::write(dir.path().join(bmp), b"b").unwrap();

        let files = included_files(dir.path()).unwrap();
        assert_eq!(
            files,
            vec![astral.to_string(), bmp.to_string()],
            "sorted by UTF-8 bytes rather than by UTF-16 code units"
        );
    }
}
