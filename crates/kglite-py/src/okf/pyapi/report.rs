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

/// What an export wrote, left alone, deleted and refused (VAULT.md §10).
///
/// Frozen for the same reason [`VaultReport`] is: it describes an export that
/// already happened.
#[pyclass(frozen, module = "kglite.okf")]
pub struct ExportReport {
    report: crate::okf::ExportReport,
}

impl ExportReport {
    pub fn new(report: crate::okf::ExportReport) -> Self {
        Self { report }
    }
}

#[pymethods]
impl ExportReport {
    /// Files created or replaced.
    #[getter]
    fn files_written(&self) -> usize {
        self.report.files_written
    }

    /// Files already byte-identical to what the export would write, so left alone.
    #[getter]
    fn files_unchanged(&self) -> usize {
        self.report.files_unchanged
    }

    /// Manifest-owned files whose node is gone from the graph, removed.
    #[getter]
    fn files_deleted(&self) -> usize {
        self.report.files_deleted
    }

    /// Writes and deletions declined for safety.
    #[getter]
    fn files_refused(&self) -> usize {
        self.report.files_refused
    }

    /// One line per refusal, in path order.
    #[getter]
    fn refusals(&self) -> Vec<String> {
        self.report.refusals.clone()
    }

    /// Edge properties dropped — frontmatter lists carry targets, not properties.
    #[getter]
    fn edge_properties_dropped(&self) -> usize {
        self.report.edge_properties_dropped
    }

    /// Attachment files copied in from the source root.
    #[getter]
    fn attachments_copied(&self) -> usize {
        self.report.attachments_copied
    }

    /// Attachment nodes whose bytes could not be copied.
    #[getter]
    fn attachments_unresolved(&self) -> usize {
        self.report.attachments_unresolved
    }

    /// Skill files written under `.kglite/skills/`.
    #[getter]
    fn skills_written(&self) -> usize {
        self.report.skills_written
    }

    /// Recipe files written under `.kglite/recipes/`.
    #[getter]
    fn recipes_written(&self) -> usize {
        self.report.recipes_written
    }

    /// One line per declared edge table the export could not write as asked.
    #[getter]
    fn warnings(&self) -> Vec<String> {
        self.report.warnings.clone()
    }

    /// Whether the export wrote everything it wanted to — no refusals.
    #[getter]
    fn ok(&self) -> bool {
        self.report.files_refused == 0
    }

    fn __str__(&self) -> String {
        self.report.render()
    }

    fn __repr__(&self) -> String {
        format!(
            "<ExportReport written={} unchanged={} deleted={} refused={}>",
            self.report.files_written,
            self.report.files_unchanged,
            self.report.files_deleted,
            self.report.files_refused
        )
    }
}
