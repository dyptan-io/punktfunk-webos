//! Shared `dlopen`/`dlsym` plumbing for the vendor libraries this layer resolves at runtime
//! rather than linking (`docs/NOTES.md` for why). Originated in `ndl::ffi`, which is still the
//! reference reading for the pattern.
use std::ffi::{c_void, CStr};
use std::sync::OnceLock;

use anyhow::{bail, Context, Result};

/// An open handle to a vendor library. Never closed — the resolved function pointers outlive it by
/// design, as a `DT_NEEDED` load would have.
pub(crate) struct Lib {
    handle: *mut c_void,
    name: &'static CStr,
}

impl Lib {
    /// `RTLD_GLOBAL` matches what a `DT_NEEDED` load would have given the process.
    pub(crate) fn open(name: &'static CStr) -> Result<Self> {
        // SAFETY: `name` is a NUL-terminated `CStr`.
        let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_LAZY | libc::RTLD_GLOBAL) };
        if handle.is_null() {
            bail!("dlopen({name:?}) failed — not available on this device");
        }
        Ok(Self { handle, name })
    }

    /// One symbol, or an error naming it — *which* symbol is missing is often what identifies the
    /// flavour of library this device has. `T` must be a function-pointer type.
    pub(crate) fn sym<T: Sized>(&self, name: &CStr) -> Result<T> {
        // SAFETY: `self.handle` is a live `dlopen` handle and `name` NUL-terminated.
        let ptr = unsafe { libc::dlsym(self.handle, name.as_ptr()) };
        if ptr.is_null() {
            bail!("{:?} is missing symbol {name:?}", self.name);
        }
        debug_assert_eq!(size_of::<T>(), size_of::<*mut c_void>(), "T must be a function pointer");
        // SAFETY: `T` is a function-pointer type and `ptr` is non-null and dlsym-verified.
        Ok(unsafe { std::mem::transmute_copy(&ptr) })
    }
}

/// Resolve a table once, caching the outcome **including the failure**: a symbol missing from
/// this device's library won't appear on a retry. Text rather than `anyhow::Error` because
/// errors aren't `Clone` and callers only print it.
pub(crate) fn cached<T: 'static>(
    cache: &'static OnceLock<std::result::Result<T, String>>,
    lib: &'static CStr,
    build: impl FnOnce(&Lib) -> Result<T>,
) -> Result<&'static T> {
    cache
        .get_or_init(|| Lib::open(lib).and_then(|lib| build(&lib)).map_err(|e| format!("{e:#}")))
        .as_ref()
        .map_err(|e| anyhow::Error::msg(e.clone()))
        .with_context(|| lib.to_string_lossy().into_owned())
}
