//! The allocator-release seam: a host that owns a global allocator registers
//! how to hand freed pages back to the OS, and a long-lived embedder (the MCP
//! server) wraps each graph load or swap in a [`ReleaseScope`].
//!
//! The engine sets no `#[global_allocator]`, so it cannot collect anything
//! itself; only the binding knows which allocator it runs on. With no hooks
//! registered (a plain cargo binary on the system allocator) a scope does
//! nothing.

use std::sync::OnceLock;

/// What a host does at the two ends of a [`ReleaseScope`]. Scopes may nest and
/// overlap across threads, so both functions must tolerate that.
#[derive(Clone, Copy)]
pub struct ReleaseHooks {
    /// A load or swap is starting: free memory should go back to the OS as it
    /// is freed, not on the allocator's delayed schedule.
    pub enter: fn(),
    /// The load or swap is done and its predecessor dropped: return whatever
    /// the allocator still holds.
    pub leave: fn(),
}

static HOOKS: OnceLock<ReleaseHooks> = OnceLock::new();

/// Register the host's hooks. The first registration wins; returns `false`
/// when hooks were already set.
pub fn set_release_hooks(hooks: ReleaseHooks) -> bool {
    HOOKS.set(hooks).is_ok()
}

/// Brackets a graph load or swap. Create it before the load starts and let it
/// drop after the outgoing graph is freed; the drop runs on every exit path,
/// error returns included. A hook may take milliseconds, so scope a load or
/// swap, never a query.
#[must_use = "the scope releases memory when it is dropped"]
pub struct ReleaseScope(());

impl ReleaseScope {
    pub fn enter() -> Self {
        if let Some(hooks) = HOOKS.get() {
            (hooks.enter)();
        }
        ReleaseScope(())
    }
}

impl Drop for ReleaseScope {
    fn drop(&mut self) {
        if let Some(hooks) = HOOKS.get() {
            (hooks.leave)();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static ENTERS: AtomicUsize = AtomicUsize::new(0);
    static LEAVES: AtomicUsize = AtomicUsize::new(0);

    fn enter() {
        ENTERS.fetch_add(1, Ordering::SeqCst);
    }
    fn leave() {
        LEAVES.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn scopes_are_no_ops_until_hooks_are_set_then_bracket_each_scope_and_run_on_unwind() {
        drop(ReleaseScope::enter());
        assert_eq!(ENTERS.load(Ordering::SeqCst), 0);
        assert_eq!(LEAVES.load(Ordering::SeqCst), 0);

        assert!(set_release_hooks(ReleaseHooks { enter, leave }));
        assert!(!set_release_hooks(ReleaseHooks { enter, leave }));

        drop(ReleaseScope::enter());
        assert_eq!(
            (ENTERS.load(Ordering::SeqCst), LEAVES.load(Ordering::SeqCst)),
            (1, 1)
        );

        let unwound = std::panic::catch_unwind(|| {
            let _scope = ReleaseScope::enter();
            panic!("load failed");
        });
        assert!(unwound.is_err());
        assert_eq!(
            (ENTERS.load(Ordering::SeqCst), LEAVES.load(Ordering::SeqCst)),
            (2, 2)
        );
    }
}
