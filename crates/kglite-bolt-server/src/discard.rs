//! DISCARD support around `boltr` 0.2.0.
//!
//! `boltr` answers DISCARD with an empty SUCCESS: it ignores `n` and drops the
//! result summary (counters, bookmark, `t_last`, `has_more`), which the
//! official drivers send on `result.consume()`. The connection's two halves
//! fix it between them:
//!
//! - [`GuardedReader`](crate::guard::GuardedReader) rewrites an incoming
//!   DISCARD's signature to PULL, so `boltr` streams `n` records and answers
//!   with the real summary, and records the request here.
//! - [`CoalescingWriter`](crate::coalesce::CoalescingWriter) drops the RECORD
//!   messages that answer a request recorded as a DISCARD and passes every
//!   other message, the summary included, through unchanged.
//!
//! The halves meet in [`DiscardTracker`]: one entry per request that gets a
//! response, in arrival order, popped when the response's closing message
//! (anything that is not a RECORD) is written. `boltr` answers every request
//! except GOODBYE with exactly one closing message, so the front entry always
//! describes the response being written.

use std::collections::VecDeque;
use std::sync::Mutex;

/// Bolt request signatures this module cares about.
pub const SIG_GOODBYE: u8 = 0x02;
pub const SIG_DISCARD: u8 = 0x2F;
pub const SIG_PULL: u8 = 0x3F;

#[derive(Default)]
pub struct DiscardTracker {
    /// `true` for a request that was a DISCARD, oldest first.
    pending: Mutex<VecDeque<bool>>,
}

impl DiscardTracker {
    pub fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self::default())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<bool>> {
        self.pending.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// A complete request message arrived. GOODBYE gets no response.
    pub fn request_arrived(&self, signature: u8, was_discard: bool) {
        if signature != SIG_GOODBYE {
            self.lock().push_back(was_discard);
        }
    }

    /// The response being written belongs to a DISCARD.
    pub fn answering_discard(&self) -> bool {
        self.lock().front().copied().unwrap_or(false)
    }

    /// A closing message (SUCCESS, FAILURE, IGNORED) was written.
    pub fn response_closed(&self) {
        self.lock().pop_front();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_are_consumed_in_arrival_order() {
        let t = DiscardTracker::new();
        t.request_arrived(0x10, false);
        t.request_arrived(SIG_PULL, true);
        t.request_arrived(SIG_GOODBYE, false);
        assert!(!t.answering_discard());
        t.response_closed();
        assert!(t.answering_discard());
        t.response_closed();
        assert!(!t.answering_discard(), "GOODBYE queued nothing");
    }
}
