//! Public Python functions for OKF ingestion: `build`, `validate`, `source`
//! and the vault writer, `export`.

use pyo3::prelude::*;
use std::path::PathBuf;

use super::report::{ExportReport, VaultReport};
use crate::graph::KnowledgeGraph;
use crate::okf::{BuildOptions, Dialect};

/// The keywords `build` and `validate` share, as one value.
///
/// `validate` is `build` with the graph thrown away (VAULT.md §9), so the two
/// must read a vault exactly alike; a second copy of this mapping is a place
/// for them to drift.
struct Keywords {
    dialect: Option<String>,
    require_frontmatter: Option<bool>,
    respect_skip: bool,
    skip_dirs: Option<Vec<String>>,
    with_body: Option<bool>,
}

impl Keywords {
    /// Built from the dialect rather than as a struct literal, so a new
    /// dialect-carried default reaches the wheel without a code change here.
    /// `None` for the two dialect-defaulted flags means "keep what the profile
    /// chose"; passing either explicitly overrides it in both directions.
    fn options(self) -> BuildOptions {
        let mut opts = BuildOptions::for_dialect(Dialect::parse(self.dialect.as_deref()));
        if let Some(v) = self.require_frontmatter {
            opts.require_frontmatter = v;
        }
        opts.respect_skip = self.respect_skip;
        opts.skip_dirs = self.skip_dirs.unwrap_or_default();
        if let Some(v) = self.with_body {
            opts.with_body = v;
        }
        opts
    }
}

/// Build a KnowledgeGraph from an OKF bundle directory.
///
/// The `dialect` picks the conventions: `"okf"` (default), `"loose"`, or
/// `"obsidian"` for the vault format specified in VAULT.md. Defaults for
/// `require_frontmatter` and `with_body` come from the dialect when they are
/// left unset; every other keyword is dialect-independent. See the stub for
/// the full contract.
#[pyfunction]
#[pyo3(signature = (path, *, dialect=None, require_frontmatter=None, respect_skip=true, skip_dirs=None, with_body=None))]
pub fn build(
    py: Python<'_>,
    path: PathBuf,
    dialect: Option<String>,
    require_frontmatter: Option<bool>,
    respect_skip: bool,
    skip_dirs: Option<Vec<String>>,
    with_body: Option<bool>,
) -> PyResult<KnowledgeGraph> {
    let opts = Keywords {
        dialect,
        require_frontmatter,
        respect_skip,
        skip_dirs,
        with_body,
    }
    .options();
    py.detach(|| crate::okf::build(&path, &opts))
        .map(|out| KnowledgeGraph::from_arc(out.graph))
        .map_err(pyo3::exceptions::PyRuntimeError::new_err)
}

/// Check a vault and return the build report without keeping the graph.
///
/// Runs the same read `build` runs, so what it reports is what a build would
/// do. `strict` decides the report's `ok` only, never what it found. See the
/// stub for the full contract.
#[pyfunction]
#[pyo3(signature = (path, *, dialect=None, strict=false, require_frontmatter=None, respect_skip=true, skip_dirs=None, with_body=None))]
// One parameter per Python keyword: the stub mirrors this signature verbatim.
#[allow(clippy::too_many_arguments)]
pub fn validate(
    py: Python<'_>,
    path: PathBuf,
    dialect: Option<String>,
    strict: bool,
    require_frontmatter: Option<bool>,
    respect_skip: bool,
    skip_dirs: Option<Vec<String>>,
    with_body: Option<bool>,
) -> PyResult<VaultReport> {
    let opts = Keywords {
        dialect,
        require_frontmatter,
        respect_skip,
        skip_dirs,
        with_body,
    }
    .options();
    py.detach(|| crate::okf::validate(&path, &opts))
        .map(|report| VaultReport::new(report, strict))
        .map_err(pyo3::exceptions::PyRuntimeError::new_err)
}

/// Read a concept's markdown body on demand (frontmatter stripped).
///
/// Pairs with partial ingestion: the graph stores each concept's `file_path`;
/// pass that path (joined with the bundle root) here to fetch the prose when an
/// agent has narrowed to a single concept.
#[pyfunction]
pub fn source(path: PathBuf) -> PyResult<String> {
    crate::okf::read_body(&path).map_err(pyo3::exceptions::PyRuntimeError::new_err)
}

/// Write a graph out as an Obsidian vault (VAULT.md §10).
///
/// Only files a previous export wrote are replaced or removed; anything else
/// in the directory is refused and reported unless `force` is set. See the
/// stub for the full contract.
#[pyfunction]
#[pyo3(signature = (graph, path, *, force=false, source_root=None))]
pub fn export(
    py: Python<'_>,
    graph: &KnowledgeGraph,
    path: PathBuf,
    force: bool,
    source_root: Option<PathBuf>,
) -> PyResult<ExportReport> {
    let opts = crate::okf::ExportOptions {
        force,
        source_root,
        ..crate::okf::ExportOptions::default()
    };
    let inner = graph.inner.clone();
    py.detach(|| crate::okf::export(&inner, &path, &opts))
        .map(ExportReport::new)
        .map_err(pyo3::exceptions::PyRuntimeError::new_err)
}
