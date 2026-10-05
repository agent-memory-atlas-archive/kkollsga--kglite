//! A test-only clock for the executor's deadline: lets a test say "the
//! deadline passes at the Nth periodic poll" and count the polls a statement
//! makes, so deadline behaviour is pinned by poll position rather than by
//! wall-clock ratios, which machine load moves.

use std::cell::Cell;

thread_local! {
    static POLLS: Cell<usize> = const { Cell::new(0) };
    static PASSES_AT_POLL: Cell<Option<usize>> = const { Cell::new(None) };
    static PASSED: Cell<bool> = const { Cell::new(false) };
}

/// Count one periodic poll; the deadline passes when the armed poll is reached.
pub(super) fn note_poll() {
    let poll = POLLS.with(|p| {
        let n = p.get();
        p.set(n + 1);
        n
    });
    if PASSES_AT_POLL.with(Cell::get) == Some(poll) {
        PASSED.with(|p| p.set(true));
    }
}

/// Whether the armed poll has been reached on this thread.
pub(super) fn deadline_passed() -> bool {
    PASSED.with(Cell::get)
}

/// Reset the poll count and make the deadline pass at zero-based poll
/// `poll`, or never with `None`.
pub(super) fn arm(poll: Option<usize>) {
    POLLS.with(|p| p.set(0));
    PASSES_AT_POLL.with(|p| p.set(poll));
    PASSED.with(|p| p.set(false));
}

/// Periodic polls made since the last [`arm`].
pub(super) fn polls() -> usize {
    POLLS.with(Cell::get)
}
