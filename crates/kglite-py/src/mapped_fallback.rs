//! The `MappedFallbackWarning` category and the announcement of what a save
//! wrote without a mapped form.

use kglite_core::api::DirGraph;
use pyo3::prelude::*;

pyo3::create_exception!(
    kglite,
    MappedFallbackWarning,
    pyo3::exceptions::PyUserWarning,
    "A save wrote part of the graph (its topology, an id or type index, a column) without a mapped form, so a mapped load of the file holds it on the heap. The message names each part and the reason; `graph_info()['mapped_fallbacks']` lists them after a load."
);

/// Announce the notice the last save parked on `graph`, if any, through
/// `warnings.warn`. The save has already happened, so a failure to warn (a
/// filter that raises) is dropped.
pub(crate) fn announce(py: Python<'_>, graph: &DirGraph) {
    if let Some(message) = graph.take_save_notice() {
        if let Ok(text) = std::ffi::CString::new(message) {
            let _ = PyErr::warn(
                py,
                py.get_type::<MappedFallbackWarning>().as_any(),
                &text,
                1,
            );
        }
    }
}
