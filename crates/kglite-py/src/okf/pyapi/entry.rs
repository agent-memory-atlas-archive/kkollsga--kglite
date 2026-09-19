//! Public Python functions for OKF ingestion: `build` and the cache-aware
//! `open`, `validate`, `source`, the lifecycle pair `fingerprint` /
//! `rebuild_if_changed`, and the vault writer, `export`.

use pyo3::prelude::*;
use std::path::PathBuf;

use super::report::{ExportReport, VaultReport};
use crate::graph::KnowledgeGraph;
use crate::okf::{BuildOptions, CachePolicy, Dialect, RebuildOptions};

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
    ///
    /// `default` is the dialect an omitted `dialect=` keyword picks, and it is
    /// **not** the same for every entry point: `validate` is the vault checker
    /// and defaults to `obsidian`, matching `kglite okf check`, while `build`
    /// and `fingerprint` keep `okf` for the bundle callers that have always
    /// relied on it — a path carries no stamp to read a better answer from.
    /// `rebuild_if_changed` does, so it takes no default at all and goes
    /// through `rebuild_options` below. An unrecognised *name* still falls
    /// back to `okf`, as `Dialect::parse` defines.
    fn options(self, default: Dialect) -> BuildOptions {
        let rebuild = self.rebuild_options();
        rebuild.resolve(rebuild.dialect.unwrap_or(default))
    }

    /// The same keywords with the dialect left open, for the one entry point
    /// that can answer it from the graph: an omitted `dialect=` on a rebuild
    /// means "as this graph was built", not "as an OKF bundle".
    fn rebuild_options(self) -> RebuildOptions {
        RebuildOptions {
            dialect: self
                .dialect
                .as_deref()
                .map(|name| Dialect::parse(Some(name))),
            require_frontmatter: self.require_frontmatter,
            respect_skip: self.respect_skip,
            skip_dirs: self.skip_dirs.unwrap_or_default(),
            with_body: self.with_body,
            ..RebuildOptions::default()
        }
    }
}

/// Build a KnowledgeGraph from an OKF bundle directory.
///
/// The `dialect` picks the conventions: `"okf"` (this function's default),
/// `"loose"`, or `"obsidian"` for the vault format specified in VAULT.md —
/// which is what `validate` defaults to instead. Defaults for
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
    .options(Dialect::Okf);
    py.detach(|| crate::okf::build(&path, &opts))
        .map(|out| KnowledgeGraph::from_arc(out.graph))
        .map_err(pyo3::exceptions::PyRuntimeError::new_err)
}

/// Open a directory as a graph, through a cached `.kgl` beside it.
///
/// Loads the cache, rebuilds only what the directory changed, and writes the
/// result back — the one call for "give me this vault as a graph" when the
/// caller does not want to decide whether that costs a build. `cache=False`
/// switches the cache off; a path relocates it. See the stub for the full
/// contract.
#[pyfunction]
#[pyo3(signature = (path, *, cache=None, dialect=None, embedder=None, require_frontmatter=None, respect_skip=true, skip_dirs=None, with_body=None))]
// One parameter per Python keyword: the stub mirrors this signature verbatim.
#[allow(clippy::too_many_arguments)]
pub fn open(
    py: Python<'_>,
    path: PathBuf,
    cache: Option<Bound<'_, PyAny>>,
    dialect: Option<String>,
    embedder: Option<Py<PyAny>>,
    require_frontmatter: Option<bool>,
    respect_skip: bool,
    skip_dirs: Option<Vec<String>>,
    with_body: Option<bool>,
) -> PyResult<KnowledgeGraph> {
    let policy = cache_policy(cache.as_ref())?;
    let opts = Keywords {
        dialect,
        require_frontmatter,
        respect_skip,
        skip_dirs,
        with_body,
    }
    .rebuild_options();
    let model = match embedder {
        Some(model) => Some(std::sync::Arc::new(
            crate::graph::embedder::py_adapter::PyEmbedderAdapter::new(py, model)?,
        ) as std::sync::Arc<dyn kglite_core::api::Embedder>),
        None => None,
    };
    let opened = py
        .detach(|| crate::okf::open(&path, &opts, model.as_deref(), policy))
        .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;
    let mut graph = KnowledgeGraph::from_arc(opened.into_graph());
    // The graph keeps the model that embedded it, for the same reason
    // `rebuild_if_changed`'s result does: a caller should not have to
    // re-register one to carry on where they left off.
    if let Some(model) = model {
        graph.set_embedder_native(model);
    }
    Ok(graph)
}

/// `cache=` as the three policies it spells.
///
/// `None` (and an omitted keyword) is the vault's own `.kglite/graph.kgl`;
/// `False` is no cache at all; anything else is a path, which is why `True`
/// has to be answered before the path extraction — `PathBuf` would reject it
/// with a type error that says nothing about caches.
fn cache_policy(cache: Option<&Bound<'_, PyAny>>) -> PyResult<CachePolicy> {
    let Some(value) = cache.filter(|value| !value.is_none()) else {
        return Ok(CachePolicy::Default);
    };
    if let Ok(flag) = value.extract::<bool>() {
        return Ok(if flag {
            CachePolicy::Default
        } else {
            CachePolicy::Disabled
        });
    }
    Ok(CachePolicy::At(value.extract::<PathBuf>()?))
}

/// Check a vault and return the build report without keeping the graph.
///
/// Runs the same read `build` runs, so what it reports is what a build would
/// do, and reads a vault by default (`dialect="obsidian"`, as `kglite okf
/// check` does) where `build` defaults to `okf`. `strict` decides the report's
/// `ok` only, never what it found. See the stub for the full contract.
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
    .options(Dialect::Obsidian);
    py.detach(|| crate::okf::validate(&path, &opts))
        .map(|report| VaultReport::new(report, strict))
        .map_err(pyo3::exceptions::PyRuntimeError::new_err)
}

/// A stable 64-bit summary of what a build of this directory would read.
///
/// Same directory, same keywords, same number — across processes and machines.
/// See the stub for the full contract.
#[pyfunction]
#[pyo3(signature = (path, *, dialect=None, require_frontmatter=None, respect_skip=true, skip_dirs=None, with_body=None))]
pub fn fingerprint(
    py: Python<'_>,
    path: PathBuf,
    dialect: Option<String>,
    require_frontmatter: Option<bool>,
    respect_skip: bool,
    skip_dirs: Option<Vec<String>>,
    with_body: Option<bool>,
) -> PyResult<u64> {
    let opts = Keywords {
        dialect,
        require_frontmatter,
        respect_skip,
        skip_dirs,
        with_body,
    }
    .options(Dialect::Okf);
    py.detach(|| crate::okf::fingerprint(&path, &opts))
        .map_err(pyo3::exceptions::PyRuntimeError::new_err)
}

/// Rebuild a graph from the directory it was built from, if that directory has
/// changed since.
///
/// Returns `None` when it has not — nothing beyond a `stat` pass is read — and
/// a new graph otherwise. An omitted dialect is the one the graph's build
/// stamped. See the stub for the full contract.
#[pyfunction]
#[pyo3(signature = (graph, *, dialect=None, embedder=None, require_frontmatter=None, respect_skip=true, skip_dirs=None, with_body=None))]
// One parameter per Python keyword: the stub mirrors this signature verbatim.
#[allow(clippy::too_many_arguments)]
pub fn rebuild_if_changed(
    py: Python<'_>,
    graph: &KnowledgeGraph,
    dialect: Option<String>,
    embedder: Option<Py<PyAny>>,
    require_frontmatter: Option<bool>,
    respect_skip: bool,
    skip_dirs: Option<Vec<String>>,
    with_body: Option<bool>,
) -> PyResult<Option<KnowledgeGraph>> {
    let opts = Keywords {
        dialect,
        require_frontmatter,
        respect_skip,
        skip_dirs,
        with_body,
    }
    .rebuild_options();
    // An explicit `embedder=` is wrapped like `set_embedder`'s; otherwise the
    // graph's own bound model is used, so a caller who has already registered
    // one does not register it twice.
    let model = match embedder {
        Some(model) => Some(std::sync::Arc::new(
            crate::graph::embedder::py_adapter::PyEmbedderAdapter::new(py, model)?,
        ) as std::sync::Arc<dyn kglite_core::api::Embedder>),
        None => graph.embedder().map(std::sync::Arc::clone),
    };
    let inner = graph.inner.clone();
    let rebuilt = py
        .detach(|| crate::okf::rebuild_if_changed(&inner, &opts, model.as_deref()))
        .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;
    Ok(rebuilt.map(|out| {
        let mut fresh = KnowledgeGraph::from_arc(out.graph);
        // The rebuilt graph is the same vault, so it keeps the model that was
        // embedding it — a caller should not have to re-register one to carry
        // on where they left off.
        if let Some(model) = model {
            fresh.set_embedder_native(model);
        }
        fresh
    }))
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
#[pyo3(signature = (graph, path, *, force=false, source_root=None, edge_tables=None))]
pub fn export(
    py: Python<'_>,
    graph: &KnowledgeGraph,
    path: PathBuf,
    force: bool,
    source_root: Option<PathBuf>,
    edge_tables: Option<std::collections::BTreeMap<String, String>>,
) -> PyResult<ExportReport> {
    let opts = crate::okf::ExportOptions {
        force,
        source_root,
        edge_tables: edge_tables.unwrap_or_default(),
        ..crate::okf::ExportOptions::default()
    };
    let inner = graph.inner.clone();
    py.detach(|| crate::okf::export(&inner, &path, &opts))
        .map(ExportReport::new)
        .map_err(pyo3::exceptions::PyRuntimeError::new_err)
}
