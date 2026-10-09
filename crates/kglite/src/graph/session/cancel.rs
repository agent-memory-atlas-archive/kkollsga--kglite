//! Per-query cooperative cancellation.
//!
//! A [`CancelToken`] is a cloneable handle on one query's cancel flag. The
//! caller keeps a clone, hands another to [`ExecuteOptions::cancel`], and
//! calls [`CancelToken::cancel`] from any thread; the running query aborts
//! with [`KgError::Cancelled`] at its next deadline checkpoint.
//!
//! The engine's hot loops poll a plain `&'static AtomicBool` (one relaxed
//! load through a pointer, `Copy` into rayon closures). To keep that cost
//! without a lifetime parameter on every executor type, a token's flag is
//! drawn from a process-wide pool of leaked slots and returned (reset) to the
//! pool when the last clone drops. Memory is bounded by the peak number of
//! simultaneously live tokens, not by the number of queries.
//!
//! Contract: keep the token alive until the execute call returns. Dropping
//! the last clone earlier recycles the slot, and a later token could then
//! cancel the still-running query.
//!
//! [`ExecuteOptions::cancel`]: super::ExecuteOptions::cancel
//! [`KgError::Cancelled`]: crate::error::KgError::Cancelled

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

static FREE_SLOTS: Mutex<Vec<&'static AtomicBool>> = Mutex::new(Vec::new());

struct Lease {
    flag: &'static AtomicBool,
    /// `true` when the slot came from [`FREE_SLOTS`] and goes back on drop;
    /// `false` for a caller-owned static ([`CancelToken::from_static`]).
    pooled: bool,
}

impl Drop for Lease {
    fn drop(&mut self) {
        if self.pooled {
            self.flag.store(false, Ordering::Relaxed);
            FREE_SLOTS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(self.flag);
        }
    }
}

/// Shareable cancel handle for one query. `Send + Sync`; clones share the flag.
#[derive(Clone)]
pub struct CancelToken(Arc<Lease>);

impl CancelToken {
    /// A fresh, un-cancelled token.
    pub fn new() -> Self {
        let recycled = FREE_SLOTS.lock().unwrap_or_else(|e| e.into_inner()).pop();
        let flag = recycled.unwrap_or_else(|| Box::leak(Box::new(AtomicBool::new(false))));
        Self(Arc::new(Lease { flag, pooled: true }))
    }

    /// Wrap a caller-owned static flag, for signal handlers that cannot
    /// capture state (the Python wheel's SIGINT handler, the CLI REPL). The
    /// caller owns resetting it between runs.
    pub fn from_static(flag: &'static AtomicBool) -> Self {
        Self(Arc::new(Lease {
            flag,
            pooled: false,
        }))
    }

    /// Request cancellation. Async-signal-safe (one atomic store).
    pub fn cancel(&self) {
        self.0.flag.store(true, Ordering::SeqCst);
    }

    /// Whether [`Self::cancel`] has been called.
    pub fn is_cancelled(&self) -> bool {
        self.0.flag.load(Ordering::Relaxed)
    }

    /// The polled flag, for the executor's hot loops.
    #[inline]
    pub(crate) fn flag(&self) -> &'static AtomicBool {
        self.0.flag
    }
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for CancelToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancelToken")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_independent_and_clones_share() {
        let a = CancelToken::new();
        let b = CancelToken::new();
        let a2 = a.clone();
        a.cancel();
        assert!(a2.is_cancelled());
        assert!(!b.is_cancelled());
    }

    #[test]
    fn recycled_slot_starts_clear() {
        let a = CancelToken::new();
        a.cancel();
        drop(a);
        assert!(!CancelToken::new().is_cancelled());
    }
}
