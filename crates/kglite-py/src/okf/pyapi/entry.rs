//! Public Python functions for OKF ingestion: `build`.

use pyo3::prelude::*;
use std::path::PathBuf;

use crate::graph::KnowledgeGraph;
use crate::okf::{BuildOptions, Dialect};

/// Build a KnowledgeGraph from an OKF bundle directory.
///
/// The `dialect` picks the conventions: `"okf"` (default), `"loose"`, or
/// `"obsidian"` for the vault format specified in VAULT.md. Defaults for
/// `require_frontmatter` and `with_body` come from the dialect when they are
/// left unset; every other keyword is dialect-independent. See the stub for
/// the full contract.
#[pyfunction]
#[pyo3(signature = (path, *, dialect=None, require_frontmatter=None, respect_skip=true, skip_dirs=None, with_body=None, embed=false))]
#[allow(clippy::too_many_arguments)]
pub fn build(
    py: Python<'_>,
    path: PathBuf,
    dialect: Option<String>,
    require_frontmatter: Option<bool>,
    respect_skip: bool,
    skip_dirs: Option<Vec<String>>,
    with_body: Option<bool>,
    embed: bool,
) -> PyResult<KnowledgeGraph> {
    // Built from the dialect rather than as a struct literal, so a new
    // dialect-carried default reaches the wheel without a code change here.
    // `None` for the two dialect-defaulted flags means "keep what the profile
    // chose"; passing either explicitly overrides it in both directions.
    let mut opts = BuildOptions::for_dialect(Dialect::parse(dialect.as_deref()));
    if let Some(v) = require_frontmatter {
        opts.require_frontmatter = v;
    }
    opts.respect_skip = respect_skip;
    opts.skip_dirs = skip_dirs.unwrap_or_default();
    if let Some(v) = with_body {
        opts.with_body = v;
    }
    opts.embed = embed;
    py.detach(|| crate::okf::build(&path, &opts))
        .map(|out| KnowledgeGraph::from_arc(out.graph))
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
