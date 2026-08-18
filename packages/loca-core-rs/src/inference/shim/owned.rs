//! The allocation-ownership discipline for the C ABI.
//!
//! Two rules, both enforced by types rather than by review:
//!
//! 1. A `char *` the shim returned was allocated with `malloc` and must be
//!    freed with `loca_string_free`. [`ShimString`] is the only thing that ever
//!    holds one, and it frees on drop. Note what is NOT used anywhere here:
//!    `CString::from_raw`, which would hand a `malloc` block to Rust's
//!    allocator. That happens to work on some platforms and corrupts the heap
//!    on others, which is the worst kind of bug to ship.
//!
//! 2. `loca_last_error` points into a thread-local the next call overwrites, so
//!    it is copied immediately on the failure path and the pointer is never
//!    stored. [`last_error`] is the only reader.

use std::ffi::{c_char, CStr, CString};
use std::ptr::NonNull;

use crate::error::{internal, AppError};

use super::ffi;

/// A `char *` owned by us and freed by the shim's allocator on drop.
pub struct ShimString(NonNull<c_char>);

impl ShimString {
    /// Takes ownership of a pointer the shim returned.
    ///
    /// Returns `None` for null, which every fallible shim function uses to
    /// signal failure, so the caller pairs this with [`last_error`].
    ///
    /// # Safety
    ///
    /// `ptr` must be null, or a pointer returned by this ABI and not yet freed.
    /// Ownership transfers here; the caller must not use or free it after.
    pub unsafe fn from_raw(ptr: *mut c_char) -> Option<Self> {
        NonNull::new(ptr).map(Self)
    }

    /// The contents as an owned `String`, copied out of the C allocation.
    pub fn to_str(&self) -> Result<String, AppError> {
        // SAFETY: the pointer is non-null by construction and was produced by
        // `dup_cstr`, which always NUL-terminates. It stays valid for the life
        // of `self` because only `Drop` frees it.
        let bytes = unsafe { CStr::from_ptr(self.0.as_ptr()) };
        bytes
            .to_str()
            .map(str::to_owned)
            .map_err(|e| internal(format!("shim returned invalid utf-8: {e}")))
    }
}

impl Drop for ShimString {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from the shim's `malloc` and is freed exactly
        // once, here, because `ShimString` is not `Copy` or `Clone` and nothing
        // else holds the pointer.
        unsafe { ffi::loca_string_free(self.0.as_ptr()) }
    }
}

/// The last error reported on this thread, copied out immediately.
///
/// Call this only after a shim call reported failure; at any other time the
/// buffer holds whatever the previous failure left there.
pub fn last_error(context: &str) -> AppError {
    // SAFETY: `loca_last_error` never returns null (it returns `c_str()` on a
    // thread_local `std::string`), and the copy happens before any other shim
    // call on this thread can overwrite it.
    let msg = unsafe {
        CStr::from_ptr(ffi::loca_last_error())
            .to_string_lossy()
            .into_owned()
    };
    if msg.is_empty() {
        internal(format!(
            "{context}: llama.cpp shim failed without a message"
        ))
    } else {
        internal(format!("{context}: {msg}"))
    }
}

/// A NUL-terminated copy of `s`, for passing in.
///
/// Rejects interior NULs rather than truncating: a silently truncated prompt or
/// template would be a very confusing failure much later.
pub fn c_string(s: &str, what: &str) -> Result<CString, AppError> {
    CString::new(s).map_err(|_| internal(format!("{what} contains an interior NUL byte")))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn c_string_rejects_interior_nul() {
        assert!(c_string("ok", "x").is_ok());
        let err = c_string("bad\0here", "prompt").unwrap_err();
        assert!(err.message.contains("interior NUL"), "{}", err.message);
    }
}
