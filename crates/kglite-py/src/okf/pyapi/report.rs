//! `VaultReport` — the build report as a Python value (VAULT.md §9).

use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::okf::BuildReport;

/// What a build or check saw: counts, errors, warnings, and the one verdict.
///
/// Frozen: a report describes a build that already happened, and a caller that
/// could edit it would be reporting something no build produced. `strict` is
/// carried alongside because it decides `ok`, not what was found.
#[pyclass(frozen, module = "kglite.okf")]
pub struct VaultReport {
    report: BuildReport,
    strict: bool,
}

impl VaultReport {
    pub fn new(report: BuildReport, strict: bool) -> Self {
        Self { report, strict }
    }
}

#[pymethods]
impl VaultReport {
    /// Findings that make the vault fail the spec, in the order the build found them.
    #[getter]
    fn errors(&self) -> Vec<String> {
        self.report.errors.clone()
    }

    /// Findings that are legitimate in a real vault but worth seeing.
    #[getter]
    fn warnings(&self) -> Vec<String> {
        self.report.warnings.clone()
    }

    /// What the build counted, as a dict.
    #[getter]
    fn counts<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        let report = &self.report;
        dict.set_item("files_scanned", report.files_scanned)?;
        dict.set_item("concepts", report.concepts)?;
        dict.set_item("nodes_by_label", report.nodes_by_label.clone())?;
        dict.set_item("edges_by_type", report.edges_by_type.clone())?;
        dict.set_item("dangling", report.dangling)?;
        dict.set_item("folder_notes", report.folder_notes)?;
        dict.set_item("missing_attachments", report.missing_attachments)?;
        dict.set_item("ambiguous_attachments", report.ambiguous_attachments)?;
        dict.set_item("indexes_declared", report.indexes_declared)?;
        dict.set_item("text_indexes_built", report.text_indexes_built)?;
        dict.set_item("skills_imported", report.skills_imported)?;
        dict.set_item("recipes_imported", report.recipes_imported)?;
        dict.set_item("embed_targets", report.embed_targets.clone())?;
        Ok(dict)
    }

    /// Whether the vault passed — no errors, and no warnings under `strict`.
    #[getter]
    fn ok(&self) -> bool {
        self.report.is_ok(self.strict)
    }

    fn __str__(&self) -> String {
        self.report.render()
    }

    fn __repr__(&self) -> String {
        format!(
            "<VaultReport ok={} errors={} warnings={}>",
            if self.ok() { "True" } else { "False" },
            self.report.errors.len(),
            self.report.warnings.len()
        )
    }
}
