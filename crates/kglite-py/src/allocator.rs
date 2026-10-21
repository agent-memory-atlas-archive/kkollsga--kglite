//! Process-global allocator maintenance — `kglite.trim_memory()`.
//!
//! Wrapper-specific by construction: the `#[global_allocator]` in `lib.rs`
//! belongs to *this* extension module, not to the `kglite` engine crate (which
//! sets no allocator and inherits whatever its host chose). A sibling binding
//! that does not link mimalloc has nothing to trim. `kglite::api::memory` owns
//! only the registration seam the MCP server calls into; what a release does
//! lives here. The Python seam calls (load, save, embedding
//! import/remove/replace) live in the `pyapi` methods: only the binding knows
//! which allocator it runs on.

use pyo3::prelude::*;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

/// Seam collects performed so far; the observable behind `_seam_trim_count`.
static SEAM_TRIMS: AtomicU64 = AtomicU64::new(0);

// mimalloc's maintenance entry point. `libmimalloc-sys` compiles the whole v2
// library into this extension but binds only the allocation functions, so the
// call is declared here. `mi_collect` is `mi_decl_export`ed in `mimalloc.h`
// and defined in the same translation unit (`src/static.c`) as `mi_malloc`, so
// the object file is unconditionally linked; naming it is what stops the
// linker dead-stripping it out of the final extension.
extern "C" {
    /// `void mi_collect(bool force)`.
    fn mi_collect(force: bool);
    /// `long mi_option_get(mi_option_t option)`.
    fn mi_option_get(option: i32) -> std::os::raw::c_long;
    /// `void mi_option_set(mi_option_t option, long value)`.
    fn mi_option_set(option: i32, value: std::os::raw::c_long);
}

/// `mi_option_purge_delay` in the vendored mimalloc v2 `mi_option_e`: between
/// `eager_commit_delay` (14) and `use_numa_nodes` (16), the two neighbours
/// `libmimalloc-sys` itself exports.
const MI_OPTION_PURGE_DELAY: i32 = 15;

/// Open [`kglite_core::api::memory::ReleaseScope`]s, and the purge delay to
/// restore when the last one closes.
static OPEN_SCOPES: AtomicU64 = AtomicU64::new(0);
static SAVED_PURGE_DELAY: AtomicI64 = AtomicI64::new(0);

/// Register the server-side release hooks with the engine.
///
/// A scope sets mimalloc's purge delay to 0 for its duration, so the load's
/// transient buffers are returned to the OS as they are freed, and ends with a
/// forced collect. The collect alone is not enough: with the default delay,
/// blocks freed during a load are queued for a purge ten milliseconds out and
/// keep the process footprint high (`phys_footprint` after serving a 195 MB
/// graph: 700-730 MB, 253 MB with a collect at the end, 230 MB with the
/// delay at 0 during the load). The delay is not lowered process-wide:
/// purge-on-free regressed in-memory benchmark cells by up to 145 % (release
/// build, `tests/benchmarks/test_bench_core.py`).
pub(crate) fn register_release_hooks() {
    kglite_core::api::memory::set_release_hooks(kglite_core::api::memory::ReleaseHooks {
        enter: begin_scope,
        leave: end_scope,
    });
}

fn begin_scope() {
    if OPEN_SCOPES.fetch_add(1, Ordering::SeqCst) == 0 {
        // SAFETY: `mi_option_get` / `mi_option_set` take plain integers and
        // are the allocator's public option API, safe from any thread.
        unsafe {
            SAVED_PURGE_DELAY.store(
                mi_option_get(MI_OPTION_PURGE_DELAY) as i64,
                Ordering::SeqCst,
            );
            mi_option_set(MI_OPTION_PURGE_DELAY, 0);
        }
    }
}

fn end_scope() {
    // SAFETY: as in `begin_scope`; `mi_collect` is the allocator's public
    // maintenance entry point, callable from any thread.
    unsafe {
        if OPEN_SCOPES.fetch_sub(1, Ordering::SeqCst) == 1 {
            mi_option_set(
                MI_OPTION_PURGE_DELAY,
                SAVED_PURGE_DELAY.load(Ordering::SeqCst) as _,
            );
        }
        mi_collect(true);
    }
}

/// Return allocator-retained memory to the operating system.
///
/// Opt-in everywhere except the seams where retention is large, certain and
/// not reusable: the end of `load()`, of `KnowledgeGraph.save()`, and of every
/// call that replaces or removes an embedding store (a freed 50k x 1024 store
/// otherwise stays resident: 212 -> 17 MB once trimmed). mimalloc
/// keeps a freed workload's pages so the next one can reuse them without a
/// syscall, which is what makes repeated queries fast, so a forced collect at
/// other seams (graph drop, every query) would trade that away. Measured on a
/// release build at 1M Product + 160k Value + 8M edges (macOS
/// `phys_footprint`): `load()` leaves 633 MB against 364 MB after a trim, and
/// `save()` leaves 787 MB against 521 MB (about 270 MB each). The collect costs
/// about 2 ms against 515 ms (load) and 380 ms (save), and neither seam has
/// pages worth keeping: the load's transient decode buffers and the save's
/// serialisation buffers are not reused by the queries that follow. The same
/// lever stays exported for a host that knows where its other peaks end.
#[pyfunction]
pub(crate) fn trim_memory(py: Python<'_>) {
    collect(py);
}

/// Collect at a seam where the freed pages are known not to be reused.
pub(crate) fn release_after_seam(py: Python<'_>) {
    SEAM_TRIMS.fetch_add(1, Ordering::Relaxed);
    collect(py);
}

/// Number of seam collects so far; a test hook, not an API.
#[pyfunction]
pub(crate) fn _seam_trim_count() -> u64 {
    SEAM_TRIMS.load(Ordering::Relaxed)
}

fn collect(py: Python<'_>) {
    // SAFETY: `mi_collect` takes no pointers, is the allocator's own public
    // maintenance entry point, and is safe to call from any thread at any
    // time (mimalloc handles being called by a thread that does not own the
    // heap). The GIL is released because a forced collect walks every page
    // heap and every arena, which takes milliseconds after a large peak.
    py.detach(|| unsafe { mi_collect(true) });
}
