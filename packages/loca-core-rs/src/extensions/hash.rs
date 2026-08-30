//! The extension content hash: the trust anchor the drift gate rests on.
//!
//! Byte-for-byte parity matters: a digest that differs from the pinned one by a
//! single byte flags every installed extension `drift` on first boot. The
//! preimage: walk the folder (regular files only), exclude any `node_modules`
//! directory and the ROOT `deno.lock` (both written by `deno install` AFTER the
//! hash is pinned), apply `<root>/.gitignore` (itself hashed), sort paths in
//! forward-slash form, then SHA-256 over `rel \0 size \0 <bytes> \0` per file.
//!
//! Symlinks are REFUSED rather than skipped: `import` follows one, and a hash
//! that skipped it would attest nothing about its target.

use std::io::Read;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use sha2::{Digest, Sha256};

use crate::error::{AppError, ErrorCode, Result};

const NODE_MODULES: &str = "node_modules";
const DENO_LOCK: &str = "deno.lock";
const READ_CHUNK: usize = 64 * 1024;

/// The sorted, forward-slash relative paths the hash covers. Shared by
/// [`hash_extension`] and [`stat_signature`] so the two sets cannot diverge.
pub fn included_files(root: &Path) -> Result<Vec<String>> {
    let ignore = load_gitignore(root);
    let mut files = Vec::new();
    walk(root, root, &ignore, &mut files)?;
    // Sorted by UTF-16 code unit, as JavaScript's `Array.sort` does: UTF-8 byte
    // order disagrees above the BMP, and a bundle with an emoji filename would
    // hash differently.
    files.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    Ok(files)
}

pub use crate::files::sha256_hex;

/// The content hash: lowercase hex SHA-256 over the whole included set.
pub fn hash_extension(root: &Path) -> Result<String> {
    let files = included_files(root)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0u8; READ_CHUNK];

    for rel in &files {
        let abs = root.join(rel);
        // Size from a stat, not from the bytes read. A file that vanished
        // between the walk and here is skipped: the digest moves, which is the
        // drift signal doing its job.
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

/// A stat-only fingerprint of the hashed set: `count:totalSize:newestMtime`,
/// the per-call re-hash skip key. Mtime alone is forgeable by a tool with write
/// access to its own folder; count and summed size make that far harder.
/// Unreadable answers `0:0:0`, a key that matches nothing.
pub fn stat_signature(root: &Path) -> String {
    let Ok(files) = included_files(root) else {
        return "0:0:0".to_string();
    };
    let mut count: u64 = 0;
    let mut total: u64 = 0;
    let mut newest: i64 = 0;
    for rel in &files {
        let Ok(meta) = std::fs::metadata(root.join(rel)) else {
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

/// Milliseconds since the Unix epoch, negative before it.
fn epoch_ms(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(since) => i64::try_from(since.as_millis()).unwrap_or(i64::MAX),
        Err(before) => -i64::try_from(before.duration().as_millis()).unwrap_or(i64::MAX),
    }
}

/// The root `.gitignore`, or an empty matcher. A malformed ignore file must not
/// make an extension unhashable.
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

        // `file_type` comes from the directory entry and does NOT follow the link.
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
        // Anything else (fifo, socket, device node) is skipped.
    }
    Ok(())
}

/// The path of `full` relative to `root`, always forward-slashed: the separator
/// is part of the hashed preimage, so a Windows core must agree with a macOS one.
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

    /// The fixture tree. Written out rather than checked in: a committed
    /// `.gitignore` would make git ignore the fixture's own files. The expected
    /// digest was produced by the reference implementation over exactly these
    /// entries, so changing one without regenerating the digest fails.
    const FIXTURE: &[(&str, &[u8])] = &[
        ("loca.json", b"{\"id\":\"fixture\",\"version\":\"1.0.0\"}\n"),
        ("deno.json", b"{\"exports\":\"./main.ts\"}\n"),
        ("deno.lock", b"{\"version\":\"4\"}\n"),
        ("nested/deno.lock", b"nested lock is hashed\n"),
        ("main.ts", b"export const answer = 42;\n"),
        ("nested/deep/util.ts", b"export const x = 1;\n"),
        ("empty.txt", b""),
        ("assets/blob.bin", &[0x00, 0xff, 0xfe, 0x41, 0x0a]),
        (".gitignore", b"ignored.txt\nbuild/\n*.log\n"),
        ("ignored.txt", b"never hashed\n"),
        ("build/output.js", b"never hashed either\n"),
        ("noisy.log", b"nor this\n"),
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

    /// Produced by the reference implementation, not by this one. Never update
    /// it to whatever the Rust currently answers: that turns a parity test into
    /// a change detector.
    const ORACLE_DIGEST: &str = "5a2cd5741bd33e8c9f8e6746c7b7f219d059d1f79d42e6902322600fb88c7129";

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
    fn the_included_set_is_the_shipped_one() {
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

    /// Without the length prefix `a`="xy",`b`="" and `a`="x",`b`="y" feed the
    /// hasher the same stream.
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

        fs::write(root.join("ignored.txt"), b"different\n").unwrap();
        fs::write(root.join("build/output.js"), b"different\n").unwrap();
        fs::write(root.join("noisy.log"), b"different\n").unwrap();
        assert_eq!(hash_extension(&root).unwrap(), before);

        // Changing the rules must move the digest, or an attacker could hide a
        // file by adding a line to the ignore file.
        fs::write(root.join(".gitignore"), b"ignored.txt\nbuild/\n").unwrap();
        assert_ne!(hash_extension(&root).unwrap(), before);
    }

    #[test]
    fn a_symlink_anywhere_in_the_tree_is_refused_rather_than_skipped() {
        #[cfg(unix)]
        {
            let (_dir, root) = fixture();
            std::os::unix::fs::symlink(root.join("main.ts"), root.join("alias.ts")).unwrap();
            let err = hash_extension(&root).unwrap_err();
            assert_eq!(err.code, ErrorCode::InvalidLocaJson);
            assert!(err.message.contains("alias.ts"), "{}", err.message);

            fs::remove_file(root.join("alias.ts")).unwrap();
            assert!(hash_extension(&root).is_ok());
            std::os::unix::fs::symlink(root.join("main.ts"), root.join("nested/alias.ts")).unwrap();
            assert_eq!(
                hash_extension(&root).unwrap_err().code,
                ErrorCode::InvalidLocaJson
            );
        }
    }

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

    /// Each term pinned with the other two held constant. Holding the mtime
    /// constant means forging it, which is exactly the attack the signature
    /// exists to resist.
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

        // SIZE alone.
        fs::write(root.join("a.txt"), b"01234567890123456789").unwrap();
        forge("a.txt", pinned);
        assert_ne!(
            stat_signature(root),
            baseline,
            "a size change was invisible"
        );

        // COUNT alone: two files summing to the original size.
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

    /// Tested on the function because a unix path never contains a backslash
    /// by accident, so the normalisation is unobservable on CI.
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
        let signature = stat_signature(Path::new("/definitely/not/a/real/path"));
        assert_eq!(signature, "0:0:0");
    }

    #[test]
    fn an_empty_folder_hashes_to_the_empty_digest() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            hash_extension(dir.path()).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    /// UTF-16 and UTF-8 order disagree only above the BMP: an astral character
    /// sorts before U+FFFD in UTF-16 and after it in UTF-8.
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
