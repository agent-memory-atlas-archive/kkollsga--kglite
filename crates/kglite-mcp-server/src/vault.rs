//! The first-party `--vault` producer: okf build hooks, the same-label
//! embedding carry, and the declared-target embedding pass.
//!
//! Every other producer-backed mode takes its [`WorkspaceGraphHooks`] from an
//! embedding binary through [`ServerExtensions::with_workspace_graph`]. Vault
//! mode does not: `--vault` is a mode of *this* binary, so the hooks are built
//! here and installed straight onto [`GraphState`]. That is not only a
//! packaging preference — it is what lets the build closure hold state the
//! public request type does not carry: the previous graph (for the embedding
//! carry) and the state's bound embedder (for the declared embed targets).
//!
//! [`ServerExtensions::with_workspace_graph`]: crate::ServerExtensions::with_workspace_graph

use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use kglite::api::embeddings::{embed_property, EmbedHooks, EmbedMode};
use kglite::api::{make_dir_graph_mut, DirGraph, Embedder};
use kglite::okf::{
    BuildOptions, BuildOutput, BuildReport, CachePolicy, Dialect, Opened, RebuildOptions,
};

use crate::tools::{
    read_lock, WorkspaceGraphHooks, WorkspaceGraphRelevance, WorkspaceGraphRequest,
    WorkspaceGraphResult,
};

/// Refusal for `--vault` on a binary that also injected workspace hooks.
///
/// Both would install a producer on the same [`GraphState`] slot, and the
/// silent resolution — whichever `boot_graph` wrote last — decides what the
/// server *is*. An embedder that wants its own producer runs `--watch`.
pub(crate) const VAULT_HOOKS_CONFLICT_MSG: &str =
    "--vault builds the graph with this binary's own vault producer, but this build also \
injects WorkspaceGraphHooks through ServerExtensions::with_workspace_graph. Only one producer \
can own the graph: run --watch DIR to use the injected producer, or drop the injection to use \
--vault.";

/// Options the vault producer builds with. One place, so the boot build and
/// every rebuild cannot drift apart.
pub(crate) fn vault_build_options() -> BuildOptions {
    BuildOptions::for_dialect(Dialect::Obsidian)
}

/// The same options in the shape the cache-aware boot takes, so the graph the
/// cache hands back is the graph a rebuild would produce. A vault's build has
/// no knobs beyond its dialect — `.kglite/vault.yaml` carries the rest — so
/// the two renderings are one value written twice.
pub(crate) fn vault_rebuild_options() -> RebuildOptions {
    RebuildOptions::for_dialect(Dialect::Obsidian)
}

/// Settle which producer boot installs, for any mode.
///
/// Vault mode builds its own and refuses to share; every other mode passes the
/// embedding binary's injection straight through. One function so the refusal
/// cannot be bypassed by a second construction site.
pub(crate) fn vault_producer(
    mode: &crate::cli::Mode,
    injected: Option<WorkspaceGraphHooks>,
    embedder: &Arc<RwLock<Option<Arc<dyn Embedder>>>>,
    cache: CachePolicy,
) -> Result<(Option<WorkspaceGraphHooks>, Option<VaultReportSlot>), String> {
    let crate::cli::Mode::Vault { dir } = mode else {
        return Ok((injected, None));
    };
    if injected.is_some() {
        return Err(VAULT_HOOKS_CONFLICT_MSG.to_string());
    }
    let root = dir.canonicalize().unwrap_or_else(|_| dir.clone());
    let (hooks, report) = vault_hooks(root, Arc::clone(embedder), cache);
    Ok((Some(hooks), Some(report)))
}

/// Build the hooks `--vault` installs on the graph state.
///
/// `embedder` is the state's own embedder slot rather than a resolved
/// embedder: it is read at *build* time, so a rebuild picks up whatever the
/// manifest bound, and a deployment with no `extensions.embedder` simply never
/// embeds. `trust.allow_embedder` needs no second check here — an embedder
/// cannot be constructed without it (`build_embedder_from_manifest`), so a
/// bound embedder *is* the operator's authorisation.
pub(crate) fn vault_hooks(
    root: PathBuf,
    embedder: Arc<RwLock<Option<Arc<dyn Embedder>>>>,
    cache: CachePolicy,
) -> (WorkspaceGraphHooks, VaultReportSlot) {
    // The graph the last successful build published, kept so the next one can
    // carry its vectors forward. `Mutex` rather than `RwLock`: the only access
    // is the build closure, which is serialised by the rebuild gate anyway.
    let previous: Arc<Mutex<Option<Arc<DirGraph>>>> = Arc::new(Mutex::new(None));
    let last_report: VaultReportSlot = Arc::new(Mutex::new(None));
    let report_slot = Arc::clone(&last_report);
    let relevance_root = root.clone();
    let hooks = WorkspaceGraphHooks {
        build: Box::new(move |request: WorkspaceGraphRequest| {
            let (built, report) = build_vault_graph(request.root(), &previous, &embedder, &cache)?;
            *previous.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&built));
            // `None` on a boot served straight from the cache: nothing was
            // read, so there is nothing a build report could describe.
            *report_slot.lock().unwrap_or_else(|e| e.into_inner()) = report;
            Ok(WorkspaceGraphResult::new(built))
        }),
        is_relevant: Box::new(move |change: WorkspaceGraphRelevance<'_>| {
            is_vault_path(&relevance_root, change.path())
        }),
    };
    (hooks, last_report)
}

/// Where the producer leaves the report of its most recent successful build,
/// so `rebuild_graph` can hand an agent the same text `kglite okf check`
/// prints. The build closure is the only writer; a failed build leaves the
/// previous report in place and reports its own error instead.
pub(crate) type VaultReportSlot = Arc<Mutex<Option<BuildReport>>>;

/// One vault build: okf, then the carry, then the declared embed targets.
///
/// **Boot goes through the cache, a rebuild does not.** With no previous
/// graph in hand, `okf::open` answers the question — load the vault's own
/// `.kglite/graph.kgl`, `stat` the directory against its stamp, and build
/// only if it moved — so a restart over an untouched vault costs a load
/// instead of a walk. Once the server holds a graph, the cache can only tell
/// it what it already knows, and consulting it would read the whole file back
/// to learn nothing; the rebuild builds and then *refreshes* the cache, so
/// the next boot starts from what the watcher last saw.
///
/// A build failure (a `vault.yaml` that will not parse, an unreadable root) is
/// returned as `Err` and reaches the caller through the workspace rebuild's
/// hot-fail path, which keeps serving the previous graph and surfaces the
/// message. It must never degrade to an empty graph: that answers every query
/// with "no results", which reads as data rather than as a broken vault. A
/// cache that cannot be read or written is never one of those failures.
fn build_vault_graph(
    root: &Path,
    previous: &Mutex<Option<Arc<DirGraph>>>,
    embedder: &RwLock<Option<Arc<dyn Embedder>>>,
    cache: &CachePolicy,
) -> Result<(Arc<DirGraph>, Option<BuildReport>), String> {
    let first = previous.lock().unwrap_or_else(|e| e.into_inner()).is_none();
    if first {
        return boot_vault_graph(root, embedder, cache);
    }
    let opts = vault_build_options();
    let BuildOutput { mut graph, report } = kglite::okf::build(root, &opts)?;

    // Same-label carry (VAULT.md / decision D4): a note that kept its label
    // and id keeps its vector, and the carried text hashes are what make the
    // embed pass below re-embed only what changed. A note whose label moved
    // (a folder move) is re-embedded — documented, not worked around.
    if let Some(old) = previous.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        // `build` just made this `Arc` and nothing else has seen it, so the
        // copy-on-write handle hands back the graph itself rather than a fork.
        let (stores, vectors, skipped) = make_dir_graph_mut(&mut graph).copy_embeddings_from(old);
        if stores > 0 {
            tracing::debug!(stores, vectors, skipped, "vault rebuild carried embeddings");
        }
    }

    if !report.embed_targets.is_empty() {
        let bound = read_lock(embedder).as_ref().map(Arc::clone);
        match bound {
            Some(model) => {
                // `changed` mode, on the hashes the carry brought across:
                // without it a 7 000-note vault would re-embed itself on
                // every saved keystroke.
                let hooks = EmbedHooks::default();
                for (label, property) in &report.embed_targets {
                    match embed_property(
                        &mut graph,
                        label,
                        property,
                        EmbedMode::Changed,
                        model.as_ref(),
                        &hooks,
                        None,
                    ) {
                        Ok(outcome) => tracing::info!(
                            label,
                            property,
                            embedded = outcome.embedded,
                            "vault embed target"
                        ),
                        // A model that fails mid-rebuild must not throw away a
                        // graph that is otherwise correct: the notes are
                        // served, `text_score()` simply has less to match on.
                        Err(e) => {
                            tracing::warn!(label, property, error = %e, "vault embed target failed")
                        }
                    }
                }
            }
            None => tracing::warn!(
                targets = report.embed_targets.len(),
                ".kglite/vault.yaml declares embed targets but no embedder is bound \
                 (extensions.embedder + trust.allow_embedder) — no vectors were computed"
            ),
        }
    }
    // After the carry and the embed pass, so the cache holds the graph the
    // server is about to serve rather than the vectorless one the build
    // produced. The write is excluded from `is_vault_path`, so it cannot tag
    // the graph dirty and rebuild the server in a loop.
    store_cache(root, &mut graph, cache);
    Ok((graph, Some(report)))
}

/// The boot leg: the cache, or a build, with `okf::open` deciding which.
///
/// The embedder is read here rather than inside `open` for the same reason the
/// rebuild leg reads it: the slot is the state's, bound before boot, and a
/// deployment with no `extensions.embedder` simply never embeds. `open`
/// then runs the vault's declared `embed:` targets itself on the build path,
/// which is what keeps the cache it writes from being a vectorless one.
fn boot_vault_graph(
    root: &Path,
    embedder: &RwLock<Option<Arc<dyn Embedder>>>,
    cache: &CachePolicy,
) -> Result<(Arc<DirGraph>, Option<BuildReport>), String> {
    let bound = read_lock(embedder).as_ref().map(Arc::clone);
    let opened = kglite::okf::open(
        root,
        &vault_rebuild_options(),
        bound.as_deref().map(|model| model as &dyn Embedder),
        cache.clone(),
    )?;
    match opened {
        Opened::Loaded(graph) => {
            tracing::info!(
                root = %root.display(),
                "vault served from its cached graph (nothing under the root has changed)"
            );
            Ok((graph, None))
        }
        Opened::Rebuilt(out) => {
            tracing::info!(
                root = %root.display(),
                notes = out.report.files_scanned,
                "vault built from its notes"
            );
            for warning in &out.report.warnings {
                tracing::warn!(warning, "vault build");
            }
            Ok((out.graph, Some(out.report)))
        }
    }
}

/// Refresh the cache from the graph the server is about to serve, logging
/// rather than failing when it cannot be written — the whole point of the
/// cache is that the server works without one.
fn store_cache(root: &Path, graph: &mut Arc<DirGraph>, cache: &CachePolicy) {
    let Some(path) = cache.path_for(root) else {
        return;
    };
    match kglite::okf::cache::store(graph, &path) {
        None => tracing::debug!(path = %path.display(), "vault cache refreshed"),
        Some(warning) => tracing::warn!(warning, "vault cache"),
    }
}

/// Whether a changed path can affect the vault graph.
///
/// Deliberately generous: anything under the root that is not inside a hidden
/// directory other than `.kglite/`. A vault rebuild is a walk of a few
/// thousand small text files — sub-second at the scale this mode serves — so
/// the cost of one false accept is far below the cost of one false reject,
/// which serves a stale graph with no sign that it is stale. The rejects that
/// matter are the noisy ones: `.obsidian/workspace.json` rewrites on every
/// pane focus, `.git/` churns on every command, and neither changes a note.
///
/// `.kglite/` is the exception among dot-directories because the build reads
/// it by explicit path (the walk prunes it): `vault.yaml`, `skills/` and
/// `recipes/` are all build inputs. The files kglite writes there are the
/// exception to the exception, named by the same `okf::is_cache_artifact` the
/// fingerprint uses: this server's own cache write lands inside the directory
/// it watches, and treating it as an edit would dirty the graph it just
/// built, rebuild, write the cache again, and never stop.
///
/// Staleness is decided by the watcher, not by `okf::fingerprint`: the
/// fingerprint is a `stat` of every file the build would read — ~7 000 of them
/// at Petrel scale — and asking it per call would put that walk in front of
/// every tool response. There is no "the watcher failed to arm" state to fall
/// back from either: `bind_mode` returns the watcher's error and boot fails
/// with it, so a server that is answering has a watcher. If that ever becomes
/// a degraded mode rather than a refusal, `okf::fingerprint(root, &opts)` is
/// the fallback to wire in here.
fn is_vault_path(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    let mut components = relative.components();
    let first_dir_is_kglite = matches!(
        components.next(),
        Some(Component::Normal(name)) if name == ".kglite"
    );
    if first_dir_is_kglite {
        return !kglite::okf::is_cache_artifact(relative);
    }
    !relative.components().any(|component| {
        matches!(component, Component::Normal(name)
            if name.to_string_lossy().starts_with('.'))
    })
}

#[cfg(test)]
#[path = "vault_tests.rs"]
mod vault_tests;
