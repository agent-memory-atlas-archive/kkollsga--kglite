//! PyO3 entry points for OKF ingestion.

pub mod entry;
pub mod report;

use pyo3::prelude::*;

/// Register the `_kglite_okf` native submodule on the parent `kglite` module.
/// Called from `src/lib.rs`. The Python-facing package `kglite.okf`
/// (`kglite/okf/__init__.py`) re-exports from here.
pub fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let m = PyModule::new(py, "_kglite_okf")?;
    m.add_function(wrap_pyfunction!(entry::build, &m)?)?;
    m.add_function(wrap_pyfunction!(entry::validate, &m)?)?;
    m.add_function(wrap_pyfunction!(entry::source, &m)?)?;
    m.add_function(wrap_pyfunction!(entry::fingerprint, &m)?)?;
    m.add_function(wrap_pyfunction!(entry::rebuild_if_changed, &m)?)?;
    m.add_function(wrap_pyfunction!(entry::export, &m)?)?;
    m.add_class::<report::VaultReport>()?;
    m.add_class::<report::ExportReport>()?;
    parent.add_submodule(&m)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("kglite._kglite_okf", &m)?;
    Ok(())
}
