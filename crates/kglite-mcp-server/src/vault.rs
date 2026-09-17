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
use kglite::okf::{BuildOptions, BuildOutput, BuildReport, Dialect};

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

/// Settle which producer boot installs, for any mode.
///
/// Vault mode builds its own and refuses to share; every other mode passes the
/// embedding binary's injection straight through. One function so the refusal
/// cannot be bypassed by a second construction site.
pub(crate) fn vault_producer(
    mode: &crate::cli::Mode,
    injected: Option<WorkspaceGraphHooks>,
    embedder: &Arc<RwLock<Option<Arc<dyn Embedder>>>>,
) -> Result<(Option<WorkspaceGraphHooks>, Option<VaultReportSlot>), String> {
    let crate::cli::Mode::Vault { dir } = mode else {
        return Ok((injected, None));
    };
    if injected.is_some() {
        return Err(VAULT_HOOKS_CONFLICT_MSG.to_string());
    }
    let root = dir.canonicalize().unwrap_or_else(|_| dir.clone());
    let (hooks, report) = vault_hooks(root, Arc::clone(embedder));
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
            let (built, report) = build_vault_graph(request.root(), &previous, &embedder)?;
            *previous.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&built));
            *report_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(report);
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
/// A build failure (a `vault.yaml` that will not parse, an unreadable root) is
/// returned as `Err` and reaches the caller through the workspace rebuild's
/// hot-fail path, which keeps serving the previous graph and surfaces the
/// message. It must never degrade to an empty graph: that answers every query
/// with "no results", which reads as data rather than as a broken vault.
fn build_vault_graph(
    root: &Path,
    previous: &Mutex<Option<Arc<DirGraph>>>,
    embedder: &RwLock<Option<Arc<dyn Embedder>>>,
) -> Result<(Arc<DirGraph>, BuildReport), String> {
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
    Ok((graph, report))
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
/// `recipes/` are all build inputs.
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
        return true;
    }
    !relative.components().any(|component| {
        matches!(component, Component::Normal(name)
            if name.to_string_lossy().starts_with('.'))
    })
}

#[cfg(test)]
#[path = "vault_tests.rs"]
mod vault_tests;
