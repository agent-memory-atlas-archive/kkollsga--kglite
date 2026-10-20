//! Process-global allocator maintenance — `kglite.trim_memory()`.
//!
//! Wrapper-specific by construction: the `#[global_allocator]` in `lib.rs`
//! belongs to *this* extension module, not to the `kglite` engine crate (which
//! sets no allocator and inherits whatever its host chose). A sibling binding
//! that does not link mimalloc has nothing to trim, so there is nothing here
//! for `kglite::api` to own. The seam calls (load, save, embedding
//! import/remove/replace) therefore live in the `pyapi` methods too: only the
//! binding knows which allocator it runs on.

use pyo3::prelude::*;
use std::sync::atomic::{AtomicU64, Ordering};

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
