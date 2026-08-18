//! D13-WIN: can a directory be renamed while a DLL loaded from inside it is
//! mapped into the running process?
//!
//! # Why this file exists
//!
//! D-106 turns self-update from "swap a binary" into "swap a directory": with
//! `GGML_BACKEND_DL`, `libggml`, `libllama` and the backend modules ship beside
//! the core binary and a mixed set is the "starts and behaves unpredictably"
//! state. The whole Windows design rests on one known fact, that a running
//! `.exe` can be RENAMED even though it cannot be OVERWRITTEN. Whether that
//! extends to renaming the PARENT DIRECTORY of a mapped image is a different
//! question with a different answer, and nobody in this fleet knows it.
//!
//! Three worlds follow from the answer, and they cost different amounts:
//!
//! | result                          | consequence for D13                                                                          |
//! | ------------------------------- | -------------------------------------------------------------------------------------------- |
//! | directory rename OK             | the Windows swap stays directory-granular; nothing else changes                               |
//! | directory fails, file rename OK | Windows falls back to per-file renames and LOSES cross-file atomicity, which is D-106's state |
//! | both fail                       | the Windows swap needs a reboot-time mechanism (`MoveFileEx` `MOVEFILE_DELAY_UNTIL_REBOOT`)   |
//!
//! # What this test does NOT do
//!
//! It does not assert the answer. Asserting an outcome nobody has observed is
//! how a spike returns the assumption it was written to test. The two legs are
//! RECORDED and printed; only the instrument's own controls are asserted.
//!
//! # Reading the result
//!
//! Grep a `windows-latest` CI log for `D13-WIN SPIKE`. The block is written
//! straight to the process stdout HANDLE rather than through `println!`,
//! because libtest captures the print macros and CI runs `cargo test` without
//! `--nocapture`, so a passing test's `println!` never reaches the log. The
//! `VERDICT:` line names the world; the `INSTRUMENT:` line says whether the
//! verdict may be believed.
//!
//! # Proving the DLL is mapped at the moment of the rename (the whole point)
//!
//! A test that loads a DLL and drops it, or that never maps the image at all,
//! answers the easy question and reports a false OK, and a false OK here is a
//! corrupted install on a user's machine. Five independent facts are checked,
//! and the last two are re-checked AFTER the rename attempt:
//!
//! 1. `LoadLibraryW` on the full path returns a non-null `HMODULE`.
//! 2. The probe's file name is UNIQUE to this run (pid plus nanos plus a
//!    counter). This matters more than it looks: `LoadLibraryW` returns the
//!    handle of an ALREADY-LOADED module whose base name matches, so copying
//!    `winhttp.dll` under its own name could hand back the System32 mapping and
//!    the rename would then succeed for a completely unrelated reason.
//! 3. `GetModuleHandleW(<unique base name>)` returns the same handle, so the
//!    loader's module list holds an entry for OUR file.
//! 4. `GetModuleFileNameW` reports a path that is not under `\System32\` and
//!    ends in our unique name.
//! 5. The two bytes at the module's base address read `MZ` through a volatile
//!    read, which touches the mapped image itself rather than any bookkeeping
//!    about it. An unmapped base would fault instead of returning.
//!
//! # Proving the instrument can fail, and can pass (D-146)
//!
//! D-146's rule: a verification instrument is itself a claim about the artifact
//! and needs its own check, proven able to FAIL on a known-bad input and PASS
//! on a known-good one. A gate proven only to fail is indistinguishable from a
//! gate that always fails.
//!
//! - **known-good, asserted to succeed:** renaming an ordinary directory in the
//!   same temp root. If this fails, directory rename is unavailable here for
//!   reasons that have nothing to do with mapped images and the whole run is
//!   meaningless.
//! - **known-bad, asserted to fail:** `remove_file` on the mapped DLL. A loaded
//!   DLL cannot be deleted on Windows. This is the same object as the question,
//!   at the same instant, so it proves the mapping is imposing a real
//!   filesystem restriction right then rather than merely existing in a handle
//!   somewhere.
//! - **sensitivity, recorded not asserted:** renaming a directory holding a
//!   file opened with `dwShareMode = 0`. This is not a fact this fleet has
//!   observed, so it is reported rather than asserted. It calibrates the OK
//!   path: if a directory rename cannot be blocked even by an exclusively-open
//!   file, then "directory rename OK" says much less than it appears to.
//!
//! The report is emitted BEFORE the assertions run, so a red run still carries
//! the full block into the log.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::ffi::{c_void, OsStr};
use std::fs;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};

type Hmodule = *mut c_void;
type Handle = *mut c_void;

/// `(DWORD)-11`, the console stdout handle.
const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF5;
const GENERIC_READ: u32 = 0x8000_0000;
const OPEN_EXISTING: u32 = 3;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;

// Declared here rather than taken from `windows-sys`, whose feature list in
// `Cargo.toml` covers only Foundation, Security, JobObjects and Threading.
// Widening it would be a change to an orchestrator-owned file for a test.
#[allow(non_snake_case)]
#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryW(name: *const u16) -> Hmodule;
    fn FreeLibrary(module: Hmodule) -> i32;
    fn GetModuleFileNameW(module: Hmodule, buf: *mut u16, size: u32) -> u32;
    fn GetModuleHandleW(name: *const u16) -> Hmodule;
    fn GetStdHandle(which: u32) -> Handle;
    fn WriteFile(
        handle: Handle,
        buf: *const u8,
        len: u32,
        written: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        security: *mut c_void,
        disposition: u32,
        flags: u32,
        template: Handle,
    ) -> Handle;
    fn CloseHandle(handle: Handle) -> i32;
}

/// Real DLLs with no unusual load behaviour, tried in order. Any one of them
/// serves; the file is only ever a carrier for a mapped image.
const CANDIDATES: [&str; 5] = [
    "winhttp.dll",
    "version.dll",
    "wtsapi32.dll",
    "dnsapi.dll",
    "netapi32.dll",
];

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

fn system32() -> PathBuf {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
    PathBuf::from(root).join("System32")
}

/// Unique per call, per process, per run. Point 2 of the mapping proof depends
/// on this: a base name that collides with an already-loaded module makes
/// `LoadLibraryW` hand back that module's handle instead of mapping our file.
fn unique_name(tag: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("loca_d13_{tag}_{}_{nanos}_{seq}.dll", std::process::id())
}

struct Probe {
    dll: PathBuf,
    module: Hmodule,
    source: String,
}

fn load_probe(dir: &Path, tag: &str) -> Result<Probe, String> {
    let mut attempts = String::new();
    for candidate in CANDIDATES {
        let source = system32().join(candidate);
        if !source.is_file() {
            attempts.push_str(&format!("  {candidate}: not present\n"));
            continue;
        }
        let dll = dir.join(unique_name(tag));
        if let Err(err) = fs::copy(&source, &dll) {
            attempts.push_str(&format!("  {candidate}: copy failed ({err})\n"));
            continue;
        }
        let path = wide(dll.as_os_str());
        let module = unsafe { LoadLibraryW(path.as_ptr()) };
        if !module.is_null() {
            return Ok(Probe {
                dll,
                module,
                source: source.display().to_string(),
            });
        }
        attempts.push_str(&format!(
            "  {candidate}: LoadLibraryW failed ({})\n",
            io::Error::last_os_error()
        ));
        let _ = fs::remove_file(&dll);
    }
    Err(attempts)
}

fn loader_path(module: Hmodule) -> String {
    let mut buf = vec![0u16; 32768];
    let len = unsafe { GetModuleFileNameW(module, buf.as_mut_ptr(), buf.len() as u32) };
    String::from_utf16_lossy(&buf[..len as usize])
}

/// Reads the first two bytes of the mapped image. This dereferences the mapping
/// itself, so it cannot be satisfied by stale loader bookkeeping.
fn image_header(module: Hmodule) -> [u8; 2] {
    let base = module.cast::<u8>();
    unsafe { [ptr::read_volatile(base), ptr::read_volatile(base.add(1))] }
}

fn outcome(result: &io::Result<()>) -> String {
    match result {
        Ok(()) => "OK".to_string(),
        Err(err) => format!(
            "FAILED (os error {}: {err})",
            err.raw_os_error().unwrap_or(-1)
        ),
    }
}

/// Writes to the process stdout HANDLE, which libtest's capture does not
/// intercept, so a PASSING test still puts its verdict in the CI log. The print
/// macros go through `std::io::stdout`, which libtest redirects per thread.
fn emit(report: &str) {
    let bytes = report.as_bytes();
    let mut written: u32 = 0;
    unsafe {
        let handle = GetStdHandle(STD_OUTPUT_HANDLE);
        WriteFile(
            handle,
            bytes.as_ptr(),
            bytes.len() as u32,
            &mut written,
            ptr::null_mut(),
        );
    }
    // Belt and braces: visible under `--nocapture` and in any harness that does
    // not capture, and harmless where the handle write already worked.
    print!("{report}");
}

/// Opens `file` with no sharing at all, so the returned handle is the most
/// exclusive claim on it Windows offers.
fn open_exclusive(file: &Path) -> Handle {
    let path = wide(file.as_os_str());
    unsafe {
        CreateFileW(
            path.as_ptr(),
            GENERIC_READ,
            0,
            ptr::null_mut(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            ptr::null_mut(),
        )
    }
}

#[test]
fn a_directory_holding_a_mapped_dll_is_renamed_or_reports_why_not() {
    let root = tempfile::TempDir::new().expect("temp root");
    let root = root.path();

    // ---------------------------------------------------------------- fixture A
    let a_live = root.join("a").join("live");
    fs::create_dir_all(&a_live).expect("fixture a");
    let probe = match load_probe(&a_live, "a") {
        Ok(probe) => probe,
        Err(attempts) => {
            emit(&format!(
                "\n=== D13-WIN SPIKE ===\nINSTRUMENT: INVALID, no probe DLL could be \
                 loaded, so nothing below was measured\n{attempts}=== END D13-WIN SPIKE ===\n"
            ));
            panic!("D13-WIN spike could not map any probe DLL; see the block above");
        }
    };
    let base_name = probe
        .dll
        .file_name()
        .and_then(|name| name.to_str())
        .expect("probe file name")
        .to_string();

    // The five mapping proofs, gathered before anything is renamed.
    let by_name = unsafe { GetModuleHandleW(wide(OsStr::new(&base_name)).as_ptr()) };
    let listed_as_ours = by_name == probe.module;
    let path_before = loader_path(probe.module);
    let lower = path_before.to_lowercase();
    let not_system32 = !lower.contains("\\system32\\");
    let is_our_file = lower.ends_with(&base_name.to_lowercase());
    let header_before = image_header(probe.module);
    let mapped_before = header_before == *b"MZ";

    // ------------------------------------------------- control: known-good pass
    let good_live = root.join("control-good").join("live");
    fs::create_dir_all(&good_live).expect("control-good");
    fs::write(good_live.join("ordinary.txt"), b"closed before the rename").expect("write");
    let good = fs::rename(&good_live, root.join("control-good").join("renamed"));

    // ------------------------------------------------- control: known-bad fail
    // A loaded DLL cannot be deleted on Windows. Same file, same instant as the
    // question below, so this is the mapping demonstrating that it bites.
    let deleted = fs::remove_file(&probe.dll);

    // ------------------------------------------- probe: is the rename sensitive
    let share_live = root.join("control-share").join("live");
    fs::create_dir_all(&share_live).expect("control-share");
    let held = share_live.join("held-open.bin");
    fs::write(&held, b"opened with dwShareMode = 0").expect("write");
    let exclusive = open_exclusive(&held);
    let invalid_handle: Handle = -1isize as Handle;
    let exclusive_open = !exclusive.is_null() && exclusive != invalid_handle;
    let sensitivity = fs::rename(&share_live, root.join("control-share").join("renamed"));
    if exclusive_open {
        unsafe { CloseHandle(exclusive) };
    }

    // ------------------------------------------------------------- THE QUESTION
    let a_renamed = root.join("a").join("old");
    let directory = fs::rename(&a_live, &a_renamed);

    // The mapping must still be live for the answer above to mean anything.
    let header_after = image_header(probe.module);
    let mapped_after = header_after == *b"MZ";
    let path_after = loader_path(probe.module);

    // ----------------------------------------------- fixture B: the file rename
    // Its own directory and its own module, so the two legs cannot interfere:
    // fixture A's directory may or may not have moved by now.
    let b_live = root.join("b").join("live");
    fs::create_dir_all(&b_live).expect("fixture b");
    let (file, b_module, b_mapped_after) = match load_probe(&b_live, "b") {
        Ok(second) => {
            let renamed = fs::rename(&second.dll, b_live.join("probe.old.dll"));
            let still = image_header(second.module) == *b"MZ";
            (Ok(renamed), Some(second.module), still)
        }
        Err(attempts) => (Err(attempts), None, false),
    };

    let directory_ok = directory.is_ok();
    let file_ok = matches!(&file, Ok(Ok(())));
    let verdict = match (directory_ok, file_ok) {
        (true, _) => {
            "WORLD 1 - DIRECTORY RENAME OK. The Windows swap stays directory-granular. \
             D13 needs no new design and the D-106 estimate stands at one run."
        }
        (false, true) => {
            "WORLD 2 - DIRECTORY RENAME FAILED, FILE RENAME OK. Windows must fall back to \
             per-file renames, which loses cross-file atomicity: a kill between two renames \
             leaves a NEW binary beside an OLD libllama.dll, D-106's unpredictable state. \
             Two runs plus a new crash-window case."
        }
        (false, false) => {
            "WORLD 3 - BOTH FAILED. The Windows swap cannot move a mapped image at all and \
             needs a reboot-time mechanism (MoveFileEx MOVEFILE_DELAY_UNTIL_REBOOT). A \
             different design."
        }
    };
    let instrument = if good.is_ok() && deleted.is_err() && mapped_before && mapped_after {
        "VALID (known-good directory rename passed, known-bad delete of the mapped DLL \
         failed, image mapped before AND after the question)"
    } else {
        "INVALID - DISREGARD THE VERDICT. One of the controls did not behave, so the \
         measurement is not evidence of anything."
    };

    let mut report = String::new();
    report.push_str("\n=== D13-WIN SPIKE: rename a directory holding a mapped DLL ===\n");
    report.push_str(&format!("probe source            : {}\n", probe.source));
    report.push_str(&format!(
        "probe copied to         : {}\n",
        probe.dll.display()
    ));
    report.push_str(&format!("HMODULE                 : {:p}\n", probe.module));
    report.push_str(&format!("loader reports          : {path_before}\n"));
    report.push_str(&format!(
        "MAPPING  unique name in the loader list : {}\n",
        if listed_as_ours { "yes" } else { "NO" }
    ));
    report.push_str(&format!(
        "MAPPING  loaded from our path, not System32 : {}\n",
        if not_system32 && is_our_file {
            "yes"
        } else {
            "NO"
        }
    ));
    report.push_str(&format!(
        "MAPPING  image header at base           : {:?} ({})\n",
        header_before,
        if mapped_before {
            "MZ, mapped"
        } else {
            "NOT MAPPED"
        }
    ));
    report.push_str(&format!(
        "CONTROL  known-good, rename an ordinary directory : {}\n",
        outcome(&good)
    ));
    report.push_str(&format!(
        "CONTROL  known-bad, delete the mapped DLL         : {}\n",
        outcome(&deleted)
    ));
    report.push_str(&format!(
        "PROBE    sensitivity, rename a directory holding a dwShareMode=0 handle : {} \
         (handle opened: {})\n",
        outcome(&sensitivity),
        if exclusive_open { "yes" } else { "no" }
    ));
    report.push_str(&format!(
        "RESULT   DIRECTORY RENAME with a mapped DLL inside : {}\n",
        outcome(&directory)
    ));
    report.push_str(&format!(
        "         image still mapped after that attempt     : {} (loader now reports {path_after})\n",
        if mapped_after { "yes, MZ" } else { "NO" }
    ));
    match &file {
        Ok(result) => report.push_str(&format!(
            "RESULT   FILE RENAME of a mapped DLL               : {} (image still mapped: {})\n",
            outcome(result),
            if b_mapped_after { "yes" } else { "NO" }
        )),
        Err(attempts) => report.push_str(&format!(
            "RESULT   FILE RENAME of a mapped DLL               : NOT MEASURED, fixture b \
             loaded nothing\n{attempts}"
        )),
    }
    report.push_str(&format!("INSTRUMENT: {instrument}\n"));
    report.push_str(&format!("VERDICT: {verdict}\n"));
    report.push_str("=== END D13-WIN SPIKE ===\n");

    unsafe { FreeLibrary(probe.module) };
    if let Some(module) = b_module {
        unsafe { FreeLibrary(module) };
    }

    // Emitted before any assertion, so a red run still carries the whole block.
    emit(&report);

    // Only the instrument is asserted. The two RESULT lines are the finding and
    // asserting either direction would be asserting the guess this spike exists
    // to replace.
    assert!(
        listed_as_ours,
        "GetModuleHandleW did not return the handle LoadLibraryW gave"
    );
    assert!(
        not_system32 && is_our_file,
        "the loader mapped some other file: {path_before}"
    );
    assert!(
        mapped_before,
        "the probe image was not mapped before the rename attempt"
    );
    assert!(
        mapped_after,
        "the probe image was unmapped before the rename attempt completed"
    );
    assert!(
        good.is_ok(),
        "known-good control failed: renaming an ordinary directory does not work here"
    );
    assert!(deleted.is_err(), "known-bad control passed: a mapped DLL was DELETED, so the mapping is imposing no restriction and no result here is evidence");
}
