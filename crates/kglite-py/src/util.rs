//! Cross-cutting helpers for the PyO3 boundary.
//!
//! [`EnterKg`] is the single chokepoint for "release the GIL, run a
//! blocking chunk of engine work, observe Ctrl-C, and map any
//! [`KgError`] back to the typed Python exception". Before this existed,
//! every `#[pymethods]` body open-coded
//! `py.detach(move || ...).map_err(crate::error_py::kg_to_pyerr)`, which
//! made it easy to forget the GIL release or the error mapping and left
//! no single place to add cancellation.
//!
//! Mirrors the role polars' `EnterPolarsExt::enter_polars`
//! (`crates/polars-python/src/utils.rs`) plays in that binding.
//!
//! ## Ctrl-C / interruptible queries
//!
//! While the closure runs (GIL released), a SIGINT handler is armed that
//! flips a process-global `AtomicBool`. The engine polls
//! that flag at the same checkpoints it polls the query deadline (see
//! `ExecuteOptions::cancel`), so a long `MATCH`/scan aborts promptly with
//! `KgError::Cancelled`, which maps to Python's `KeyboardInterrupt`. Outside a
//! call the handler forwards SIGINT to the disposition it replaced (normally
//! Python's own handler).
//!
//! **Scope:** this targets the interactive single-query case
//! (notebook / REPL on the main thread). The flag and handler are
//! process-global, so under concurrent multi-thread `Session` serving a
//! Ctrl-C cancels whichever query last entered `enter_kg`; servers run
//! their own signal model and don't use this path. The handler is
//! Unix-only — on other platforms the deadline still bounds queries but
//! Ctrl-C is a no-op mid-query.

use pyo3::marker::Ungil;
use pyo3::prelude::*;

use crate::error::KgError;
use crate::error_py::kg_to_pyerr;
use kglite_core::api::session::CancelToken;

/// GIL-release + cancellation + error-mapping helper on [`Python`].
///
/// Wrap any blocking engine call so the GIL is released for its
/// duration, Ctrl-C is observable, and a returned [`KgError`] becomes
/// the most specific `kglite.*` Python exception.
pub(crate) trait EnterKg {
    /// Release the GIL, arm the SIGINT handler, run `f`
    /// (handing it the cooperative-cancel flag to thread into
    /// `ExecuteOptions::cancel`), then map `Err(e)` through
    /// [`kg_to_pyerr`]. The closure receives `None` for the cancel flag
    /// on platforms without the handler, in which case it should still
    /// pass it straight through — the engine treats `None` as
    /// "never cancelled".
    fn enter_kg<T, E, F>(self, f: F) -> PyResult<T>
    where
        F: Ungil + Send + FnOnce(Option<CancelToken>) -> Result<T, E>,
        T: Ungil + Send,
        E: Ungil + Send + Into<KgError>;
}

impl EnterKg for Python<'_> {
    #[inline]
    fn enter_kg<T, E, F>(self, f: F) -> PyResult<T>
    where
        F: Ungil + Send + FnOnce(Option<CancelToken>) -> Result<T, E>,
        T: Ungil + Send,
        E: Ungil + Send + Into<KgError>,
    {
        let _guard = sigint::arm();
        let cancel = sigint::cancel_flag();
        // GIL released for the whole engine call; the SIGINT handler
        // (armed by `_guard`) can flip `cancel` from signal context.
        let r = self.detach(move || f(cancel));
        // `_guard` disarms on drop, after the GIL is reacquired here.
        r.map_err(|e| kg_to_pyerr(e.into()))
    }
}

/// Scoped SIGINT (Ctrl-C) handling. On Unix, an `enter_kg` call arms a
/// process-wide handler that flips a flag the engine polls; outside any call
/// the handler forwards the signal to the disposition it replaced (normally
/// Python's own), so Ctrl-C behaves exactly as if it were not installed.
/// On other platforms every operation is a no-op.
///
/// One `sigaction` per call, not two: the handler stays installed after the
/// call returns. Each call re-arms it with a single `sigaction`, whose
/// returned old disposition shows whether someone else (`signal.signal`) has
/// taken SIGINT since the last call; that disposition becomes the new
/// forward target.
#[cfg(unix)]
mod sigint {
    use kglite_core::api::session::CancelToken;
    use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};

    /// Process-global cancel flag. The engine polls it via
    /// `ExecuteOptions::cancel`; the signal handler sets it.
    static QUERY_CANCEL: AtomicBool = AtomicBool::new(false);

    /// Number of `enter_kg` calls currently running. While non-zero the
    /// handler cancels; at zero it forwards.
    static ACTIVE: AtomicUsize = AtomicUsize::new(0);

    /// `Some(&QUERY_CANCEL)` — handed to `ExecuteOptions::cancel`.
    pub(super) fn cancel_flag() -> Option<CancelToken> {
        Some(CancelToken::from_static(&QUERY_CANCEL))
    }

    /// The SIGINT disposition our handler replaced. Leaked on change (once
    /// per foreign install, not per call) so the signal handler reads one
    /// pointer, never a torn pair.
    struct Forward {
        action: libc::sigaction,
    }

    static FORWARD: AtomicPtr<Forward> = AtomicPtr::new(std::ptr::null_mut());

    /// Async-signal-safe: atomics, plus a call into the previous handler.
    extern "C" fn handle_sigint(
        sig: libc::c_int,
        info: *mut libc::siginfo_t,
        ctx: *mut libc::c_void,
    ) {
        if ACTIVE.load(Ordering::SeqCst) > 0 {
            QUERY_CANCEL.store(true, Ordering::SeqCst);
            return;
        }
        let fwd = FORWARD.load(Ordering::SeqCst);
        if fwd.is_null() {
            return;
        }
        // SAFETY: `fwd` is a leaked, never-freed `Forward`.
        let prev = unsafe { &(*fwd).action };
        let h = prev.sa_sigaction;
        if h == libc::SIG_IGN {
            return;
        }
        if h == libc::SIG_DFL {
            // SAFETY: restoring the default disposition and re-raising are
            // async-signal-safe; the process terminates as it would have.
            unsafe {
                libc::signal(sig, libc::SIG_DFL);
                libc::raise(sig);
            }
            return;
        }
        if prev.sa_flags & libc::SA_SIGINFO != 0 {
            // SAFETY: the previous owner registered this as a 3-argument handler.
            let f: extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void) =
                unsafe { std::mem::transmute(h) };
            f(sig, info, ctx);
        } else {
            // SAFETY: registered without SA_SIGINFO, so a 1-argument handler.
            let f: extern "C" fn(libc::c_int) = unsafe { std::mem::transmute(h) };
            f(sig);
        }
    }

    pub(super) struct Guard;

    /// Arm the handler for one call (refcounted across concurrent/nested
    /// calls) and clear any stale cancel flag when the first call enters.
    /// Returns a guard that disarms on drop.
    pub(super) fn arm() -> Guard {
        if ACTIVE.fetch_add(1, Ordering::SeqCst) == 0 {
            QUERY_CANCEL.store(false, Ordering::SeqCst);
        }
        // SAFETY: zeroed sigaction is a valid empty mask; the handler field
        // is set below. `sigaction` is the POSIX install call.
        let mut new: libc::sigaction = unsafe { std::mem::zeroed() };
        let handler =
            handle_sigint as extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void);
        new.sa_sigaction = handler as libc::sighandler_t;
        new.sa_flags = libc::SA_SIGINFO;
        let mut prev: libc::sigaction = unsafe { std::mem::zeroed() };
        unsafe {
            libc::sigaction(libc::SIGINT, &new, &mut prev);
        }
        let ours = handler as libc::sighandler_t;
        if prev.sa_sigaction != ours {
            let boxed = Box::into_raw(Box::new(Forward { action: prev }));
            FORWARD.store(boxed, Ordering::SeqCst);
        }
        Guard
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            ACTIVE.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

#[cfg(not(unix))]
mod sigint {
    use kglite_core::api::session::CancelToken;

    pub(super) fn cancel_flag() -> Option<CancelToken> {
        None
    }

    pub(super) struct Guard;

    pub(super) fn arm() -> Guard {
        Guard
    }
}
