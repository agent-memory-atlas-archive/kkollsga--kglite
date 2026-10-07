//! The process-wide writer slot behind `--write-concurrency queue`.
//!
//! A write-mode Bolt transaction takes the slot at BEGIN and holds it until
//! its `TxState` is dropped, so writers run one at a time on the latest graph
//! and never conflict at COMMIT (SQLite's `BEGIN IMMEDIATE`). The permit is an
//! RAII value inside `TxState`: every path that discards a transaction —
//! commit (success or failure), rollback, RESET, connection close, reaping —
//! releases the slot by dropping it, so no path can leak it.
//!
//! Waiters are served FIFO (tokio's semaphore is fair). A holder that goes
//! quiet is reclaimed by the *waiters*, not by a background task: while a
//! writer is queued it polls the holder's activity record and, past the idle
//! timeout, asks the backend to discard the holder's transaction. An idle
//! holder nobody is waiting on is never disturbed.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// How write transactions are admitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WriteConcurrency {
    /// One writer at a time, admitted at BEGIN; COMMIT cannot conflict.
    Queue,
    /// Every write transaction runs on its BEGIN snapshot and a stale one is
    /// refused at COMMIT with a retriable conflict.
    Optimistic,
}

/// Admission settings. `None` for a timeout means no timeout.
#[derive(Clone, Copy, Debug)]
pub(crate) struct WriterConfig {
    pub(crate) mode: WriteConcurrency,
    /// Longest a BEGIN waits for the slot.
    pub(crate) wait_timeout: Option<Duration>,
    /// Inactivity after which a queued writer may reclaim the slot.
    pub(crate) idle_timeout: Option<Duration>,
}

impl WriterConfig {
    pub(crate) const DEFAULT_WAIT: Duration = Duration::from_secs(20);
    pub(crate) const DEFAULT_IDLE: Duration = Duration::from_secs(10);
}

impl Default for WriterConfig {
    fn default() -> Self {
        Self {
            mode: WriteConcurrency::Queue,
            wait_timeout: Some(Self::DEFAULT_WAIT),
            idle_timeout: Some(Self::DEFAULT_IDLE),
        }
    }
}

/// How often a queued writer looks at the holder's activity.
const REAP_POLL: Duration = Duration::from_millis(50);

/// Last-activity record of the transaction holding the slot.
pub(crate) struct HolderActivity {
    handle: String,
    epoch: Instant,
    last_touch_ms: AtomicU64,
    in_flight: AtomicUsize,
}

impl HolderActivity {
    fn touch(&self) {
        let ms = self.epoch.elapsed().as_millis() as u64;
        self.last_touch_ms.store(ms, Ordering::Relaxed);
    }

    fn idle_for(&self) -> Duration {
        let now = self.epoch.elapsed().as_millis() as u64;
        Duration::from_millis(now.saturating_sub(self.last_touch_ms.load(Ordering::Relaxed)))
    }

    /// Marks a query as running on the transaction; it counts as activity on
    /// both edges, and the holder is never reaped while one is in flight.
    pub(crate) fn begin_query(self: &Arc<Self>) -> QueryGuard {
        self.in_flight.fetch_add(1, Ordering::AcqRel);
        self.touch();
        QueryGuard(Arc::clone(self))
    }
}

/// Ends the in-flight mark taken by [`HolderActivity::begin_query`].
pub(crate) struct QueryGuard(Arc<HolderActivity>);

impl Drop for QueryGuard {
    fn drop(&mut self) {
        self.0.touch();
        self.0.in_flight.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Ownership of the slot. Dropping it frees the slot for the next waiter.
pub(crate) struct WriterPermit {
    // Field order matters: `Drop for WriterPermit` clears the holder record
    // first, then this permit releases the semaphore.
    _permit: OwnedSemaphorePermit,
    activity: Arc<HolderActivity>,
    slot: Arc<WriterSlot>,
}

impl WriterPermit {
    pub(crate) fn activity(&self) -> &Arc<HolderActivity> {
        &self.activity
    }
}

impl Drop for WriterPermit {
    fn drop(&mut self) {
        let mut holder = self.slot.holder.lock().unwrap_or_else(|p| p.into_inner());
        if holder
            .as_ref()
            .is_some_and(|h| Arc::ptr_eq(h, &self.activity))
        {
            *holder = None;
        }
    }
}

/// Why a BEGIN did not get the slot.
#[derive(Debug)]
pub(crate) struct WaitTimedOut {
    pub(crate) waited: Duration,
}

pub(crate) struct WriterSlot {
    semaphore: Arc<Semaphore>,
    holder: Mutex<Option<Arc<HolderActivity>>>,
    config: WriterConfig,
}

impl WriterSlot {
    pub(crate) fn new(config: WriterConfig) -> Arc<Self> {
        Arc::new(Self {
            semaphore: Arc::new(Semaphore::new(1)),
            holder: Mutex::new(None),
            config,
        })
    }

    pub(crate) fn config(&self) -> &WriterConfig {
        &self.config
    }

    /// The holder's transaction handle when it has been idle past the
    /// timeout with no query running.
    fn reclaimable_holder(&self) -> Option<String> {
        let idle = self.config.idle_timeout?;
        let holder = self.holder.lock().unwrap_or_else(|p| p.into_inner());
        let h = holder.as_ref()?;
        (h.in_flight.load(Ordering::Acquire) == 0 && h.idle_for() > idle).then(|| h.handle.clone())
    }

    /// Wait for the slot on behalf of transaction `handle`.
    ///
    /// `reclaim` is called with the holder's handle when the holder has gone
    /// idle past the timeout; it must discard that transaction (dropping its
    /// permit), after which this call acquires the slot.
    pub(crate) async fn acquire(
        self: &Arc<Self>,
        handle: &str,
        reclaim: impl Fn(&str),
    ) -> Result<WriterPermit, WaitTimedOut> {
        let started = Instant::now();
        let deadline = self.config.wait_timeout.map(|t| started + t);
        let acquire = Arc::clone(&self.semaphore).acquire_owned();
        tokio::pin!(acquire);
        let permit = loop {
            let next_poll = tokio::time::sleep(REAP_POLL);
            tokio::select! {
                // Prefer the permit over the timeout when both are ready.
                biased;
                got = &mut acquire => {
                    break got.expect("the writer semaphore is never closed");
                }
                _ = next_poll => {
                    if let Some(h) = self.reclaimable_holder() {
                        reclaim(&h);
                    }
                    if deadline.is_some_and(|d| Instant::now() >= d) {
                        return Err(WaitTimedOut { waited: started.elapsed() });
                    }
                }
            }
        };
        let activity = Arc::new(HolderActivity {
            handle: handle.to_string(),
            epoch: Instant::now(),
            last_touch_ms: AtomicU64::new(0),
            in_flight: AtomicUsize::new(0),
        });
        *self.holder.lock().unwrap_or_else(|p| p.into_inner()) = Some(Arc::clone(&activity));
        Ok(WriterPermit {
            _permit: permit,
            activity,
            slot: Arc::clone(self),
        })
    }
}

/// Handles whose transaction was discarded by an idle reclaim, newest last.
///
/// Lets the client's next message on a reaped handle get a specific error
/// instead of "unknown transaction". Bounded: an entry only matters until the
/// client's next message, and a client that never sends one costs one slot.
#[derive(Default)]
pub(crate) struct ReapedHandles(Mutex<std::collections::VecDeque<String>>);

impl ReapedHandles {
    const CAP: usize = 256;

    pub(crate) fn record(&self, handle: &str) {
        let mut q = self.0.lock().unwrap_or_else(|p| p.into_inner());
        if q.len() == Self::CAP {
            q.pop_front();
        }
        q.push_back(handle.to_string());
    }

    pub(crate) fn contains(&self, handle: &str) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .any(|h| h == handle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(wait_ms: u64, idle_ms: u64) -> WriterConfig {
        WriterConfig {
            mode: WriteConcurrency::Queue,
            wait_timeout: (wait_ms > 0).then(|| Duration::from_millis(wait_ms)),
            idle_timeout: (idle_ms > 0).then(|| Duration::from_millis(idle_ms)),
        }
    }

    #[tokio::test]
    async fn waiters_are_served_in_arrival_order() {
        let slot = WriterSlot::new(cfg(0, 0));
        let first = slot.acquire("tx-0", |_| {}).await.unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut tasks = Vec::new();
        for i in 1..=4 {
            let (slot, order) = (Arc::clone(&slot), Arc::clone(&order));
            tasks.push(tokio::spawn(async move {
                let p = slot.acquire(&format!("tx-{i}"), |_| {}).await.unwrap();
                order.lock().unwrap().push(i);
                tokio::time::sleep(Duration::from_millis(5)).await;
                drop(p);
            }));
            // Stagger so arrival order is unambiguous.
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        drop(first);
        for t in tasks {
            t.await.unwrap();
        }
        assert_eq!(*order.lock().unwrap(), vec![1, 2, 3, 4]);
    }

    #[tokio::test]
    async fn wait_times_out_while_the_holder_is_active() {
        let slot = WriterSlot::new(cfg(150, 0));
        let _held = slot.acquire("tx-0", |_| {}).await.unwrap();
        let err = slot
            .acquire("tx-1", |_| {})
            .await
            .err()
            .expect("must time out");
        assert!(err.waited >= Duration::from_millis(150));
    }

    #[tokio::test]
    async fn dropping_the_permit_frees_the_slot_and_clears_the_holder() {
        let slot = WriterSlot::new(cfg(150, 0));
        let held = slot.acquire("tx-0", |_| {}).await.unwrap();
        assert!(slot.holder.lock().unwrap().is_some());
        drop(held);
        assert!(slot.holder.lock().unwrap().is_none());
        slot.acquire("tx-1", |_| {}).await.expect("slot is free");
    }

    #[tokio::test]
    async fn a_cancelled_wait_leaves_the_slot_usable() {
        let slot = WriterSlot::new(cfg(0, 0));
        let held = slot.acquire("tx-0", |_| {}).await.unwrap();
        let s2 = Arc::clone(&slot);
        let waiter = tokio::spawn(async move { s2.acquire("tx-1", |_| {}).await.map(|_| ()) });
        tokio::time::sleep(Duration::from_millis(30)).await;
        waiter.abort();
        let _ = waiter.await;
        drop(held);
        slot.acquire("tx-2", |_| {})
            .await
            .expect("slot free after an aborted waiter");
    }

    #[tokio::test]
    async fn an_idle_holder_is_reclaimed_by_a_waiter_but_a_busy_one_is_not() {
        let slot = WriterSlot::new(cfg(0, 100));
        let held = Arc::new(Mutex::new(Some(
            slot.acquire("tx-0", |_| {}).await.unwrap(),
        )));
        // A query in flight protects the holder however long it runs.
        let guard = held
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .activity()
            .begin_query();
        let reclaimed = Arc::new(Mutex::new(Vec::new()));
        let (s2, h2, r2) = (Arc::clone(&slot), Arc::clone(&held), Arc::clone(&reclaimed));
        let waiter = tokio::spawn(async move {
            s2.acquire("tx-1", |h| {
                r2.lock().unwrap().push(h.to_string());
                h2.lock().unwrap().take(); // the backend dropping the TxState
            })
            .await
            .map(|_| ())
        });
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(
            reclaimed.lock().unwrap().is_empty(),
            "reaped a holder mid-query"
        );
        drop(guard);
        waiter
            .await
            .unwrap()
            .expect("waiter acquires after the reclaim");
        assert_eq!(*reclaimed.lock().unwrap(), vec!["tx-0".to_string()]);
    }

    #[test]
    fn reaped_handles_are_bounded_and_ordered() {
        let r = ReapedHandles::default();
        for i in 0..(ReapedHandles::CAP + 10) {
            r.record(&format!("tx-{i}"));
        }
        assert!(!r.contains("tx-0"));
        assert!(r.contains(&format!("tx-{}", ReapedHandles::CAP + 9)));
    }
}
