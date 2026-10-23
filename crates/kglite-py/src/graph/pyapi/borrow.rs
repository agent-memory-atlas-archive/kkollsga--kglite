//! Waiting borrows of a live `KnowledgeGraph`.
//!
//! A `#[pyclass]` with `&self` / `&mut self` methods is guarded by PyO3's
//! `RefCell`-style flag, and the generated glue raises `Already borrowed` the
//! moment a second thread touches the graph mid-write. Every
//! `KnowledgeGraph` method therefore takes `slf: &Bound<'_, Self>` and borrows
//! through [`borrow_waiting`] / [`borrow_mut_waiting`], which wait for the
//! holder instead.

use crate::graph::KnowledgeGraph;
use pyo3::prelude::*;
use std::cell::RefCell;
use std::ops::{Deref, DerefMut};
use std::time::Duration;

/// Error for a borrow conflict that waiting cannot resolve: a callback (an
/// embedder, a progress hook, a finaliser) re-entering the graph whose
/// operation is running it, which would wait on itself. A second thread never
/// sees it: it waits.
///
/// Kept in the Python wrapper (not core `KgError`): the borrow guard is a
/// PyO3/GIL concern, so a non-Python binding never raises it. Raised as a
/// `RuntimeError`, which `PyBorrowError` also is.
pub(crate) fn concurrent_access_pyerr() -> PyErr {
    PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(
        "KnowledgeGraph accessed concurrently: a callback re-entered the graph \
         whose operation is running it, which would wait on itself. Do not call the graph \
         from inside `embed()` or another callback it runs. To query a graph from other \
         threads while it works, use a Session: `g.session()` returns a thread-safe handle \
         whose `cypher()` reads run lock-free and `execute()` writes serialize; \
         `g.freeze()` gives a lock-free read-only snapshot. See docs/concepts/concurrency.md.",
    )
}

thread_local! {
    /// Borrows this thread holds right now: (cell address, exclusive).
    static HELD: RefCell<Vec<(usize, bool)>> = const { RefCell::new(Vec::new()) };
}

/// Registers a borrow in [`HELD`] for as long as it lives.
struct HeldMark;

impl HeldMark {
    fn enter(cell: usize, exclusive: bool) -> Self {
        HELD.with(|h| h.borrow_mut().push((cell, exclusive)));
        Self
    }
}

impl Drop for HeldMark {
    fn drop(&mut self) {
        HELD.with(|h| {
            h.borrow_mut().pop();
        });
    }
}

/// True when waiting for `cell` would wait on this thread itself: it already
/// holds a borrow that conflicts with the one requested.
fn this_thread_blocks(cell: usize, exclusive: bool) -> bool {
    HELD.with(|h| {
        h.borrow()
            .iter()
            .any(|&(c, held_exclusive)| c == cell && (exclusive || held_exclusive))
    })
}

/// Retry `attempt` until it yields, sleeping with the GIL released between
/// tries (a waiter that kept the GIL would starve the writer it waits on) and
/// polling for signals so Ctrl-C ends the wait.
fn wait_for_borrow<T>(
    py: Python<'_>,
    cell: usize,
    exclusive: bool,
    mut attempt: impl FnMut() -> Option<T>,
) -> PyResult<T> {
    let mut pause = Duration::from_micros(20);
    loop {
        if let Some(guard) = attempt() {
            return Ok(guard);
        }
        if this_thread_blocks(cell, exclusive)
            || crate::graph::embedder::py_adapter::in_embedder_callback()
        {
            return Err(concurrent_access_pyerr());
        }
        py.detach(|| std::thread::sleep(pause));
        py.check_signals()?;
        pause = (pause * 2).min(Duration::from_millis(1));
    }
}

/// A shared borrow of the graph, from [`borrow_waiting`].
pub(crate) struct KgRef<'py> {
    inner: PyRef<'py, KnowledgeGraph>,
    _mark: HeldMark,
}

impl Deref for KgRef<'_> {
    type Target = KnowledgeGraph;
    fn deref(&self) -> &KnowledgeGraph {
        &self.inner
    }
}

/// An exclusive borrow of the graph, from [`borrow_mut_waiting`].
pub(crate) struct KgRefMut<'py> {
    inner: PyRefMut<'py, KnowledgeGraph>,
    _mark: HeldMark,
}

impl Deref for KgRefMut<'_> {
    type Target = KnowledgeGraph;
    fn deref(&self) -> &KnowledgeGraph {
        &self.inner
    }
}

impl DerefMut for KgRefMut<'_> {
    fn deref_mut(&mut self) -> &mut KnowledgeGraph {
        self.inner.deref_mut()
    }
}

/// Shared borrow of the graph, waiting out a writer on another thread.
pub(crate) fn borrow_waiting<'py>(slf: &Bound<'py, KnowledgeGraph>) -> PyResult<KgRef<'py>> {
    let cell = slf.as_ptr() as usize;
    let inner = wait_for_borrow(slf.py(), cell, false, || slf.try_borrow().ok())?;
    Ok(KgRef {
        inner,
        _mark: HeldMark::enter(cell, false),
    })
}

/// Exclusive borrow of the graph, waiting out any other holder.
pub(crate) fn borrow_mut_waiting<'py>(slf: &Bound<'py, KnowledgeGraph>) -> PyResult<KgRefMut<'py>> {
    let cell = slf.as_ptr() as usize;
    let inner = wait_for_borrow(slf.py(), cell, true, || slf.try_borrow_mut().ok())?;
    Ok(KgRefMut {
        inner,
        _mark: HeldMark::enter(cell, true),
    })
}
