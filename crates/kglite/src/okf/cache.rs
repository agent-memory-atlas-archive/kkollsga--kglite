//! The graph a vault carries beside its notes: where it lives, what it stamps,
//! and opening a directory through it (VAULT.md §12).
//!
//! [`open`] is the one entry point a caller reaches for when it wants *this
//! directory as a graph* and does not care whether that costs a build: it
//! loads the cache, asks [`crate::okf::rebuild_if_changed`] whether the
//! directory has moved since, and saves back whatever it had to build. The
//! cache is a `.kgl` like any other, so a vault shipped with one is served
//! without a build on a machine that has never read it.
//!
//! **A cache problem never fails an open.** Every way the cache can go wrong —
//! absent, unreadable, written by another build of kglite, refused by the
//! format gate, un-writable, held by another process — is a cache *miss* or a
//! report warning, never an error: the caller asked for the graph, and the
//! directory can always answer that question the slow way.
//!
//! **Why the exclusion predicate exists.** The default cache sits at
//! `<root>/.kglite/graph.kgl`, inside the directory whose state the fingerprint
//! summarises — and `.kglite/` is a build input under the `obsidian` dialect
//! (`vault.yaml`, `skills/`, `recipes/`). Without [`is_cache_artifact`] the
//! save that writes the cache moves the fingerprint the cache was stamped
//! with, so the next open reads "changed", rebuilds, saves, and the cache can
//! never hit. The same predicate is what keeps a `--vault` server's own cache
//! write from looking like an edit to its watcher.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::graph::dir_graph::DirGraph;
use crate::graph::embedder::Embedder;
use crate::graph::io::open::GraphWriterLease;
use crate::okf::build::{build, BuildOutput};
use crate::okf::export::MANIFEST_FILE;
use crate::okf::fingerprint::{rebuild_if_changed, stamped_dialect};
use crate::okf::model::{BuildOptions, Dialect, RebuildOptions};
use crate::okf::vault_config::{config_dir, CONFIG_DIR};

/// The cache file's name inside `.kglite/` (VAULT.md §12).
pub const CACHE_FILE: &str = "graph.kgl";

/// Where [`CachePolicy::Default`] puts the cache for `root`.
pub fn cache_path(root: &Path) -> PathBuf {
    config_dir(root).join(CACHE_FILE)
}

/// Whether `rel_path` — a path relative to a vault root — names the vault's
/// own cache rather than one of its build inputs.
///
/// The set is `.kglite/graph.kgl` plus everything the save path and the writer
/// lease put *beside* it in the same directory: `<name>.lock` and
/// `<name>.lock-owner` (`graph::io::open`), and the in-flight
/// `<name>.tmp.<pid>.<nonce>` temp (`graph::io::file::save_temps`). The export
/// manifest joins them because it is written *by* kglite into the directory it
/// exported to, and is no more a build input than the cache is — it has
/// perturbed the fingerprint of any vault exported into itself since exports
/// existed.
///
/// Keyed on the whole relative path, not the file name: a `graph.kgl` the
/// author keeps anywhere else in the vault is their file — an attachment
/// candidate like any other — and a cache relocated out of `.kglite/` by
/// `CachePolicy::At` is excluded only by living outside the vault.
pub fn is_cache_artifact(rel_path: &Path) -> bool {
    let mut parts = rel_path.components();
    let Some(std::path::Component::Normal(first)) = parts.next() else {
        return false;
    };
    if first != std::ffi::OsStr::new(CONFIG_DIR) {
        return false;
    }
    let Some(std::path::Component::Normal(name)) = parts.next() else {
        return false;
    };
    if parts.next().is_some() {
        return false;
    }
    let Some(name) = name.to_str() else {
        return false;
    };
    name == CACHE_FILE
        || name == MANIFEST_FILE
        || name == format!("{CACHE_FILE}.lock")
        || name == format!("{CACHE_FILE}.lock-owner")
        || name.starts_with(&crate::graph::io::file::save_temp_prefix(Path::new(
            CACHE_FILE,
        )))
}

/// Where [`open`] keeps the graph it built.
#[derive(Debug, Clone)]
pub enum CachePolicy {
    /// `<root>/.kglite/graph.kgl` — the vault carries its own graph, so it can
    /// be shipped pre-built.
    Default,
    /// A caller-chosen path. Outside the vault it is invisible to the
    /// fingerprint and the watcher for free; inside one that is not
    /// `.kglite/graph.kgl` it is an ordinary file of that vault, which is a
    /// self-invalidating cache — [`is_cache_artifact`] excludes the default
    /// location and nothing else.
    At(PathBuf),
    /// No cache: [`open`] builds, and writes nothing.
    Disabled,
}

impl CachePolicy {
    /// The file this policy reads and writes, if it has one.
    pub fn path_for(&self, root: &Path) -> Option<PathBuf> {
        match self {
            CachePolicy::Default => Some(cache_path(root)),
            CachePolicy::At(path) => Some(path.clone()),
            CachePolicy::Disabled => None,
        }
    }
}

/// What [`open`] found: a graph the cache already held, or one it had to build.
///
/// The distinction is the report. A build produces a [`crate::okf::BuildReport`]
/// — file counts, parse errors, embed targets — and a cache hit produces none,
/// because nothing was read beyond the `stat` pass that proved the cache
/// current. A caller that renders a report renders it for `Rebuilt` and says
/// "loaded from cache" for `Loaded`; one that only wants the graph calls
/// [`Opened::into_graph`].
pub enum Opened {
    /// The cache was current: this is the graph it held, unchanged.
    Loaded(Arc<DirGraph>),
    /// The directory was read. The cache has been refreshed from this graph
    /// unless the policy is [`CachePolicy::Disabled`] or the write was
    /// skipped, in which case the report carries a warning saying so.
    ///
    /// Boxed because a `BuildReport` is an order of magnitude larger than an
    /// `Arc`, and an enum sized for its largest variant would make every
    /// cache hit — the common case — carry the build's footprint.
    Rebuilt(Box<BuildOutput>),
}

impl Opened {
    /// The graph, whichever way it was obtained.
    pub fn graph(&self) -> &Arc<DirGraph> {
        match self {
            Opened::Loaded(graph) => graph,
            Opened::Rebuilt(out) => &out.graph,
        }
    }

    /// The graph, consuming the verdict.
    pub fn into_graph(self) -> Arc<DirGraph> {
        match self {
            Opened::Loaded(graph) => graph,
            Opened::Rebuilt(out) => out.graph,
        }
    }

    /// What the build saw, or `None` on a cache hit — there was no build.
    pub fn report(&self) -> Option<&crate::okf::BuildReport> {
        match self {
            Opened::Loaded(_) => None,
            Opened::Rebuilt(out) => Some(&out.report),
        }
    }

    /// Whether the directory was read.
    pub fn was_rebuilt(&self) -> bool {
        matches!(self, Opened::Rebuilt(_))
    }
}

/// The version of kglite that stamps a graph it builds — what
/// `DirGraph::source_build_version` records and what [`open`] compares a cache
/// against.
///
/// Build semantics are not part of the fingerprint: an untouched vault built
/// by 0.17.10 and opened by 0.17.11 fingerprints identically, and the 0.17.11
/// graph is a different shape (chunk splitting and mask offsets moved). The
/// fingerprint answers "did the directory change"; this answers "would this
/// build read it the same way", and a cache needs both.
pub fn build_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// A canonical rendering of the non-dialect knobs a build read with — what
/// `DirGraph::source_options` records.
///
/// The dialect is stamped separately and compared separately; these are the
/// four `BuildOptions` fields a caller can set independently of it, and a
/// cache built with different ones is a different graph of the same directory
/// that the fingerprint cannot tell apart. `skip_dirs` is sorted because the
/// order the caller listed them in is not part of what the build did.
///
/// `BuildOptions::profile` is deliberately *not* here. A vault's own
/// `.kglite/vault.yaml` rewrites the profile, and it is a fingerprint input,
/// so the case that matters is covered; what is left is a Rust caller passing
/// two different `Profile` values for one directory, which no binding, the
/// CLI or the MCP server can do. Embedder identity is unstamped for the same
/// reason it is unstamped everywhere else — see VAULT.md §12.
pub fn options_stamp(opts: &BuildOptions) -> String {
    let mut skip_dirs: Vec<&str> = opts.skip_dirs.iter().map(String::as_str).collect();
    skip_dirs.sort_unstable();
    format!(
        "require_frontmatter={};respect_skip={};with_body={};skip_dirs={}",
        opts.require_frontmatter,
        opts.respect_skip,
        opts.with_body,
        skip_dirs.join(",")
    )
}

/// Open `root` as a graph, through `cache`.
///
/// The fast path is a cache that is already current: the file is loaded and
/// [`crate::okf::rebuild_if_changed`] confirms the directory has not moved,
/// which costs one `stat` per input file and no parsing at all. Anything else
/// reads the directory, and the result is written back to the cache.
///
/// **What counts as a miss.** Any failure to load the file — absent, truncated,
/// a format this build refuses, over the load-memory ceiling — plus four
/// stamp mismatches the fingerprint cannot see: a different `source_root` (the
/// vault was moved or copied, and `rebuild_if_changed` would fingerprint the
/// *stamped* root rather than this one), a different `source_build_version`
/// (this build of kglite reads the same directory into a different graph), a
/// different `source_options`, and a dialect that is absent or contradicts the
/// one asked for. A miss is silent: it is the ordinary state of a directory
/// that has never been opened.
///
/// **What a cache failure costs.** Nothing but the cache. The write takes the
/// graph's writer lease with a zero timeout, so a second process opening the
/// same vault skips its own write rather than racing for the rename; an
/// unwritable directory, a full volume or a lost race each leave a warning in
/// the returned report and the graph the caller asked for.
pub fn open(
    root: &Path,
    opts: &RebuildOptions,
    embedder: Option<&dyn Embedder>,
    cache: CachePolicy,
) -> Result<Opened, String> {
    let Some(cache_path) = cache.path_for(root) else {
        return Ok(Opened::Rebuilt(Box::new(build_fresh(root, opts)?)));
    };
    if let Some(loaded) = load_usable_cache(&cache_path, root, opts) {
        match rebuild_if_changed(&loaded, opts, embedder)? {
            None => return Ok(Opened::Loaded(loaded)),
            Some(out) => return Ok(Opened::Rebuilt(Box::new(write_back(out, &cache_path)))),
        }
    }
    Ok(Opened::Rebuilt(Box::new(write_back(
        build_fresh(root, opts)?,
        &cache_path,
    ))))
}

/// Build `root` with the dialect `opts` names, or `okf` when it names none —
/// the same default [`crate::okf::rebuild_if_changed`] falls back to for a
/// graph with no stamp.
fn build_fresh(root: &Path, opts: &RebuildOptions) -> Result<BuildOutput, String> {
    build(root, &opts.resolve(opts.dialect.unwrap_or(Dialect::Okf)))
}

/// The cache at `path`, if it is a cache of *this* directory as *this* build
/// would read it. Every rejection is a miss, so nothing here returns an error.
fn load_usable_cache(path: &Path, root: &Path, opts: &RebuildOptions) -> Option<Arc<DirGraph>> {
    let loaded = crate::graph::io::file::load_file(&path.to_string_lossy()).ok()?;
    let canonical = root.canonicalize().ok()?;
    if loaded.source_root.as_deref() != Some(canonical.to_string_lossy().as_ref()) {
        return None;
    }
    if loaded.source_build_version.as_deref() != Some(build_version()) {
        return None;
    }
    // An unknown stamped dialect is an error to `stamped_dialect` and a miss
    // here: a cache written by a build that knows a dialect this one does not
    // is exactly a cache this build cannot use.
    let dialect = stamped_dialect(&loaded).ok().flatten()?;
    if opts.dialect.is_some_and(|asked| asked != dialect) {
        return None;
    }
    if loaded.source_options.as_deref() != Some(options_stamp(&opts.resolve(dialect)).as_str()) {
        return None;
    }
    Some(loaded)
}

/// Save `out.graph` to `path`, turning every way that can fail into a warning
/// on `out.report`. Returns the same output either way — the caller asked for
/// a graph, and it has one.
fn write_back(mut out: BuildOutput, path: &Path) -> BuildOutput {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        if let Err(error) = std::fs::create_dir_all(parent) {
            return warn_unwritten(out, path, &error);
        }
    }
    // Zero timeout: a second process opening the same vault at the same moment
    // wants its graph, not a queue. Skipping the write costs that process one
    // rebuild next time; waiting for the lease costs it a rebuild's worth of
    // wall clock now, every time.
    let _lease = match GraphWriterLease::acquire_ex(path, Duration::ZERO) {
        Ok(lease) => lease,
        // Contention and a failure to take the lock at all are different
        // stories for the operator reading the warning: one is another
        // process doing the same work, the other is the directory.
        Err(refusal) if refusal.error.kind() == std::io::ErrorKind::WouldBlock => {
            out.report.warnings.push(format!(
                "the graph cache at `{}` is held by another process, so it was not \
                 rewritten ({})",
                path.display(),
                refusal.error
            ));
            return out;
        }
        Err(refusal) => return warn_unwritten(out, path, &refusal.error),
    };
    if let Err(error) = crate::graph::io::file::save_graph(&mut out.graph, &path.to_string_lossy())
    {
        return warn_unwritten(out, path, &error);
    }
    out
}

/// The warning every "the cache did not get written" path leaves — one
/// wording, so an operator seeing it once recognises it from any cause.
fn warn_unwritten(mut out: BuildOutput, path: &Path, error: &dyn std::fmt::Display) -> BuildOutput {
    out.report.warnings.push(format!(
        "the graph cache at `{}` was not written ({error}); this open rebuilt the \
         directory and the next one will too",
        path.display()
    ));
    out
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;
