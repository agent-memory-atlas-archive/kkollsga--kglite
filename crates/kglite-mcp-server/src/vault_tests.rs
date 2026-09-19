//! The `--vault` producer: build, carry, embed targets, relevance policy —
//! and the rebuild path's skill-refresh callback.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use kglite::api::{DirGraph, Embedder, GraphRead};
use kglite::okf::CachePolicy;

use super::*;
use crate::cli::Mode;
use crate::tools::{GraphState, WorkspaceGraphMode, WorkspaceGraphRequest};

/// A vault with two notes, one link and one declared embed target.
fn write_vault(root: &Path) {
    std::fs::create_dir_all(root.join("notes")).expect("mkdir notes");
    std::fs::write(
        root.join("notes/alpha.md"),
        "---\ntitle: Alpha\n---\nAlpha body. See [[beta]].\n",
    )
    .expect("alpha");
    std::fs::write(
        root.join("notes/beta.md"),
        "---\ntitle: Beta\n---\nBeta body.\n",
    )
    .expect("beta");
}

fn write_config(root: &Path, body: &str) {
    std::fs::create_dir_all(root.join(".kglite")).expect("mkdir .kglite");
    std::fs::write(root.join(".kglite/vault.yaml"), body).expect("vault.yaml");
}

fn build_once(root: &Path) -> Arc<DirGraph> {
    let (hooks, _report) = vault_hooks(
        root.to_path_buf(),
        Arc::new(RwLock::new(None)),
        CachePolicy::Disabled,
    );
    let request = WorkspaceGraphRequest::new(
        root.to_path_buf(),
        None,
        WorkspaceGraphMode::Watch,
        crate::tools::WorkspaceGraphChanges::Full,
    );
    let (graph, _) = (hooks.build)(request).expect("vault build").into_parts();
    graph
}

fn node_count(graph: &DirGraph, label: &str) -> usize {
    graph
        .type_indices
        .get(label)
        .map(|nodes| nodes.len())
        .unwrap_or(0)
}

/// Deterministic stand-in for a real model: one dimension carrying the text's
/// length, so a vector can be traced back to the text that produced it.
struct LengthEmbedder {
    calls: Arc<AtomicUsize>,
    texts: Arc<Mutex<Vec<String>>>,
}

impl LengthEmbedder {
    fn new() -> (Arc<Self>, Arc<AtomicUsize>, Arc<Mutex<Vec<String>>>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let texts = Arc::new(Mutex::new(Vec::new()));
        (
            Arc::new(Self {
                calls: Arc::clone(&calls),
                texts: Arc::clone(&texts),
            }),
            calls,
            texts,
        )
    }
}

impl Embedder for LengthEmbedder {
    fn dimension(&self) -> usize {
        2
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        self.calls.fetch_add(texts.len(), Ordering::SeqCst);
        self.texts.lock().unwrap().extend(texts.iter().cloned());
        Ok(texts.iter().map(|t| vec![t.len() as f32, 1.0]).collect())
    }
    fn model_id(&self) -> Option<String> {
        Some("length-2d".to_string())
    }
}

// ── The producer ──────────────────────────────────────────────────────────

#[test]
fn the_producer_builds_the_notes_and_their_links() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_vault(temp.path());
    let graph = build_once(temp.path());

    assert_eq!(node_count(&graph, "notes"), 2, "one node per note");
    let edges: usize = graph.graph.edge_count();
    assert!(edges > 0, "the wikilink must produce an edge");
}

#[test]
fn a_broken_vault_config_fails_the_build_instead_of_serving_an_empty_graph() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_vault(temp.path());
    write_config(temp.path(), "kglite_vault: 7\n");

    let (hooks, _report) = vault_hooks(
        temp.path().to_path_buf(),
        Arc::new(RwLock::new(None)),
        CachePolicy::Disabled,
    );
    let request = WorkspaceGraphRequest::new(
        temp.path().to_path_buf(),
        None,
        WorkspaceGraphMode::Watch,
        crate::tools::WorkspaceGraphChanges::Full,
    );
    let error = match (hooks.build)(request) {
        Err(error) => error,
        Ok(_) => panic!("an unreadable config must fail the build"),
    };
    assert!(
        error.contains("kglite_vault"),
        "the refusal must name what is wrong: {error}"
    );
}

/// The failure has to reach the rebuild machinery as a failure, not as a graph
/// with nothing in it — an empty graph answers every query "no results", which
/// reads as data rather than as a broken vault.
#[test]
fn a_failing_build_leaves_the_previous_graph_serving() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_vault(temp.path());
    let (hooks, _report) = vault_hooks(
        temp.path().to_path_buf(),
        Arc::new(RwLock::new(None)),
        CachePolicy::Disabled,
    );
    let state = GraphState::new(Some(WorkspaceGraphMode::Watch))
        .with_workspace_graph(Some(Arc::new(hooks)));
    state
        .build_workspace_graph(temp.path(), None)
        .expect("boot build");
    let before = state.schema().expect("a graph is served");

    write_config(temp.path(), "kglite_vault: 7\n");
    state
        .build_workspace_graph(temp.path(), None)
        .expect_err("the broken config must fail the rebuild");
    assert_eq!(
        state.schema().expect("the previous graph is still served"),
        before,
        "a failed rebuild must not replace the served graph"
    );
}

// ── Embedding carry and the declared targets ──────────────────────────────

#[test]
fn declared_embed_targets_are_embedded_when_an_embedder_is_bound() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_vault(temp.path());
    write_config(
        temp.path(),
        "kglite_vault: 1\ndefault_label: Note\nembed:\n  Note: body\n",
    );
    let (model, calls, _texts) = LengthEmbedder::new();
    let slot: Arc<RwLock<Option<Arc<dyn Embedder>>>> = Arc::new(RwLock::new(Some(model)));

    let (hooks, _report) = vault_hooks(
        temp.path().to_path_buf(),
        Arc::clone(&slot),
        CachePolicy::Disabled,
    );
    let request = WorkspaceGraphRequest::new(
        temp.path().to_path_buf(),
        None,
        WorkspaceGraphMode::Watch,
        crate::tools::WorkspaceGraphChanges::Full,
    );
    let (graph, _) = (hooks.build)(request).expect("vault build").into_parts();

    assert_eq!(calls.load(Ordering::SeqCst), 2, "both note bodies embedded");
    let store = graph
        .embeddings
        .get(&("Note".to_string(), "body_emb".to_string()))
        .expect("the declared target's store");
    assert_eq!(store.dimension, 2);
    assert_eq!(store.model_id.as_deref(), Some("length-2d"));
}

#[test]
fn no_bound_embedder_means_no_vectors_and_no_failure() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_vault(temp.path());
    write_config(
        temp.path(),
        "kglite_vault: 1\ndefault_label: Note\nembed:\n  Note: body\n",
    );
    let graph = build_once(temp.path());
    assert!(
        graph.embeddings.is_empty(),
        "a vault declaring embed targets with no embedder still serves its notes"
    );
}

/// The point of the carry: a rebuild after editing one note must not re-embed
/// the vault. Without it a 7 000-note vault re-embeds itself on every save.
#[test]
fn a_rebuild_carries_vectors_and_re_embeds_only_the_changed_note() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_vault(temp.path());
    write_config(
        temp.path(),
        "kglite_vault: 1\ndefault_label: Note\nembed:\n  Note: body\n",
    );
    let (model, calls, texts) = LengthEmbedder::new();
    let slot: Arc<RwLock<Option<Arc<dyn Embedder>>>> = Arc::new(RwLock::new(Some(model)));
    let (hooks, _report) = vault_hooks(
        temp.path().to_path_buf(),
        Arc::clone(&slot),
        CachePolicy::Disabled,
    );
    let build = || {
        let request = WorkspaceGraphRequest::new(
            temp.path().to_path_buf(),
            None,
            WorkspaceGraphMode::Watch,
            crate::tools::WorkspaceGraphChanges::Full,
        );
        (hooks.build)(request).expect("vault build").into_parts().0
    };

    build();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    texts.lock().unwrap().clear();

    std::fs::write(
        temp.path().join("notes/alpha.md"),
        "---\ntitle: Alpha\n---\nAlpha body, rewritten. See [[beta]].\n",
    )
    .expect("edit alpha");
    let graph = build();

    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "only the edited note is re-embedded"
    );
    let second_round = texts.lock().unwrap().clone();
    assert_eq!(second_round.len(), 1);
    assert!(
        second_round[0].contains("rewritten"),
        "the re-embedded text is the edited one: {second_round:?}"
    );
    let store = graph
        .embeddings
        .get(&("Note".to_string(), "body_emb".to_string()))
        .expect("store");
    assert_eq!(
        store.len(),
        2,
        "the unchanged note keeps the vector it was carried"
    );
}

// ── Relevance ─────────────────────────────────────────────────────────────

#[test]
fn relevance_accepts_vault_content_and_rejects_editor_churn() {
    let root = PathBuf::from("/vault");
    for accepted in [
        "notes/alpha.md",
        "img/faults.png",
        ".kglite/vault.yaml",
        ".kglite/skills/one.md",
        ".kglite/recipes/one.md",
    ] {
        assert!(
            is_vault_path(&root, &root.join(accepted)),
            "{accepted} is a build input"
        );
    }
    for rejected in [
        ".obsidian/workspace.json",
        ".git/index",
        "notes/.obsidian/cache",
    ] {
        assert!(
            !is_vault_path(&root, &root.join(rejected)),
            "{rejected} changes nothing the build reads"
        );
    }
    assert!(
        !is_vault_path(&root, Path::new("/elsewhere/notes/alpha.md")),
        "a path outside the vault is never relevant"
    );
}

/// The policy is only worth anything if the producer's hook actually asks it.
/// A hook that answered `true` for everything would rebuild the whole vault on
/// every `.obsidian/workspace.json` rewrite — which Obsidian does on a pane
/// focus — and nothing above would notice.
#[test]
fn the_hooks_relevance_asks_the_policy() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical root");
    let (hooks, _report) = vault_hooks(
        root.clone(),
        Arc::new(RwLock::new(None)),
        CachePolicy::Disabled,
    );
    let ask = |relative: &str| {
        let path = root.join(relative);
        (hooks.is_relevant)(crate::tools::WorkspaceGraphRelevance::new(
            &path,
            WorkspaceGraphMode::Watch,
        ))
    };
    assert!(ask("notes/alpha.md"), "a note is a build input");
    assert!(ask(".kglite/vault.yaml"), "so is the vault config");
    assert!(
        !ask(".obsidian/workspace.json"),
        "editor state changes nothing the build reads"
    );
}

// ── The cache ─────────────────────────────────────────────────────────────

/// The loop this predicate exists to prevent: the server's own cache write
/// lands under `.kglite/`, which is otherwise relevant wholesale, so it would
/// tag the graph it just built dirty, rebuild, write the cache again, and
/// never settle.
#[test]
fn the_servers_own_cache_write_is_not_a_vault_change() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical root");
    for relative in [
        ".kglite/graph.kgl",
        ".kglite/graph.kgl.lock",
        ".kglite/graph.kgl.lock-owner",
        ".kglite/graph.kgl.tmp.991.4",
        ".kglite/export-manifest.json",
    ] {
        assert!(
            !is_vault_path(&root, &root.join(relative)),
            "`{relative}` is this server's own writing, not an edit to the vault"
        );
    }
    for relative in [".kglite/vault.yaml", ".kglite/skills/one.md"] {
        assert!(
            is_vault_path(&root, &root.join(relative)),
            "`{relative}` is still a build input"
        );
    }
}

/// Boot writes the cache; a second producer over the same directory loads it
/// and reports nothing, because nothing was read. The report slot's `None` is
/// exactly that state.
#[test]
fn a_second_boot_over_an_unchanged_vault_loads_the_cache() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_vault(temp.path());
    let cache = temp.path().join(".kglite/graph.kgl");

    let (first, first_report) = vault_hooks(
        temp.path().to_path_buf(),
        Arc::new(RwLock::new(None)),
        CachePolicy::Default,
    );
    let graph = (first.build)(full_request(temp.path()))
        .expect("boot build")
        .into_parts()
        .0;
    assert_eq!(node_count(&graph, "notes"), 2);
    assert!(cache.is_file(), "the boot wrote the vault's cache");
    assert!(
        first_report.lock().unwrap().is_some(),
        "a build has a report"
    );

    let (second, second_report) = vault_hooks(
        temp.path().to_path_buf(),
        Arc::new(RwLock::new(None)),
        CachePolicy::Default,
    );
    let loaded = (second.build)(full_request(temp.path()))
        .expect("boot from cache")
        .into_parts()
        .0;
    assert_eq!(node_count(&loaded, "notes"), 2, "the cached graph serves");
    assert!(
        second_report.lock().unwrap().is_none(),
        "nothing was read, so there is no build report to render"
    );
}

/// A rebuild refreshes the cache, and the cache it leaves behind is itself
/// current — if the write moved the fingerprint, the next boot would rebuild
/// and write again, forever. Asserted as "the next boot loads".
#[test]
fn a_rebuild_refreshes_the_cache_and_leaves_it_current() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_vault(temp.path());
    let (hooks, report) = vault_hooks(
        temp.path().to_path_buf(),
        Arc::new(RwLock::new(None)),
        CachePolicy::Default,
    );
    (hooks.build)(full_request(temp.path())).expect("boot build");

    std::fs::write(
        temp.path().join("notes/gamma.md"),
        "---\ntitle: Gamma\n---\nA third note.\n",
    )
    .expect("new note");
    let rebuilt = (hooks.build)(full_request(temp.path()))
        .expect("rebuild")
        .into_parts()
        .0;
    assert_eq!(node_count(&rebuilt, "notes"), 3);
    assert_eq!(
        report.lock().unwrap().as_ref().map(|r| r.files_scanned),
        Some(3),
        "a rebuild reports what it read"
    );

    let (next, next_report) = vault_hooks(
        temp.path().to_path_buf(),
        Arc::new(RwLock::new(None)),
        CachePolicy::Default,
    );
    let served = (next.build)(full_request(temp.path()))
        .expect("boot from the refreshed cache")
        .into_parts()
        .0;
    assert_eq!(
        node_count(&served, "notes"),
        3,
        "the cache the rebuild wrote holds the new note"
    );
    assert!(
        next_report.lock().unwrap().is_none(),
        "and it is current — a write that moved the fingerprint would rebuild here"
    );
}

#[test]
fn a_disabled_cache_writes_nothing_into_the_vault() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_vault(temp.path());
    let (hooks, _report) = vault_hooks(
        temp.path().to_path_buf(),
        Arc::new(RwLock::new(None)),
        CachePolicy::Disabled,
    );
    (hooks.build)(full_request(temp.path())).expect("boot build");
    (hooks.build)(full_request(temp.path())).expect("rebuild");
    assert!(
        !temp.path().join(".kglite/graph.kgl").exists(),
        "--vault-cache none leaves the vault alone"
    );
}

/// `--vault-cache` is the operator's whole control over this, so the three
/// spellings have to resolve where the flag's help says they do.
#[test]
fn the_vault_cache_flag_resolves_to_its_three_policies() {
    let vault = PathBuf::from("/vaults/notes");
    let policy = |flag: Option<&str>| {
        let mut argv = vec!["kglite-mcp-server", "--vault", "/vaults/notes"];
        if let Some(flag) = flag {
            argv.extend(["--vault-cache", flag]);
        }
        let cli = <crate::cli::Cli as clap::Parser>::parse_from(argv);
        crate::cli::vault_cache_policy(&cli).path_for(&vault)
    };
    assert_eq!(policy(None), Some(vault.join(".kglite/graph.kgl")));
    assert_eq!(policy(Some("none")), None);
    assert_eq!(
        policy(Some("/tmp/elsewhere.kgl")),
        Some(PathBuf::from("/tmp/elsewhere.kgl"))
    );
}

fn full_request(root: &Path) -> WorkspaceGraphRequest {
    WorkspaceGraphRequest::new(
        root.to_path_buf(),
        None,
        WorkspaceGraphMode::Watch,
        crate::tools::WorkspaceGraphChanges::Full,
    )
}

/// Without the `Mode::Vault` arm on the two watcher functions a vault server
/// registers no watcher at all: it builds once at boot and then serves that
/// graph forever, with `rebuild_graph` the only way to notice an edit. Nothing
/// about the served output says so.
#[test]
fn vault_mode_arms_a_watcher_over_its_root() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_vault(temp.path());
    let mode = Mode::Vault {
        dir: temp.path().to_path_buf(),
    };
    let state = vault_state(temp.path());
    assert!(
        crate::watcher::mode_change_handler(&mode, &state).is_some(),
        "vault mode must install a change handler"
    );
    assert_eq!(
        crate::watcher::resolved_mode_watch_root(&mode).expect("resolve the watch root"),
        Some(temp.path().canonicalize().expect("canonical root")),
        "and register its own directory with the OS"
    );
}

// ── The rebuild's skill refresh ───────────────────────────────────────────

fn vault_state(root: &Path) -> GraphState {
    let (hooks, _report) = vault_hooks(
        root.to_path_buf(),
        Arc::new(RwLock::new(None)),
        CachePolicy::Disabled,
    );
    GraphState::new(Some(WorkspaceGraphMode::Watch)).with_workspace_graph(Some(Arc::new(hooks)))
}

/// The defect D6 would otherwise ship with: a vault's `.kglite/skills/*.md`
/// reaches the graph on every build, but the served skill registry was
/// resolved once at boot and nothing re-resolved it after a watcher rebuild.
#[test]
fn a_lazy_rebuild_runs_the_after_rebuild_callback() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_vault(temp.path());
    let state = vault_state(temp.path());
    state
        .build_workspace_graph(temp.path(), None)
        .expect("boot build");

    let refreshes = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&refreshes);
    state.set_after_rebuild(Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    }));

    state.ensure_graph_fresh();
    assert_eq!(
        refreshes.load(Ordering::SeqCst),
        0,
        "nothing changed, so nothing was rebuilt and nothing needs re-resolving"
    );

    std::fs::write(
        temp.path().join(".kglite/skills/one.md"),
        "placeholder — the tag is what this test drives",
    )
    .ok();
    state.tag_workspace_graph_dirty(&[temp.path().join("notes/alpha.md")]);
    state.ensure_graph_fresh();
    assert_eq!(
        refreshes.load(Ordering::SeqCst),
        1,
        "a rebuild that installed a graph must re-resolve the skills read from it"
    );
}

#[test]
fn a_failed_rebuild_does_not_run_the_after_rebuild_callback() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_vault(temp.path());
    let state = vault_state(temp.path());
    state
        .build_workspace_graph(temp.path(), None)
        .expect("boot build");

    let refreshes = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&refreshes);
    state.set_after_rebuild(Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    }));

    write_config(temp.path(), "kglite_vault: 7\n");
    state.tag_workspace_graph_dirty(&[temp.path().join(".kglite/vault.yaml")]);
    state.ensure_graph_fresh();
    assert_eq!(
        refreshes.load(Ordering::SeqCst),
        0,
        "no new graph was installed, so the boot-resolved skills are still the right ones"
    );
}

/// Editing a carried skill changes what a refresh would compose — the other
/// half of the pair above, and the reason the callback is worth the plumbing.
#[test]
fn a_rebuild_picks_up_an_edited_carried_skill() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_vault(temp.path());
    std::fs::create_dir_all(temp.path().join(".kglite/skills")).expect("mkdir skills");
    let skill = temp.path().join(".kglite/skills/vault_notes.md");
    let body = |line: &str| {
        format!(
            "---\nname: vault_notes\ndescription: How this vault is organised.\n---\n\n{line}\n"
        )
    };
    std::fs::write(&skill, body("First wording.")).expect("skill");

    let state = vault_state(temp.path());
    state
        .build_workspace_graph(temp.path(), None)
        .expect("boot build");
    let served = |state: &GraphState| {
        crate::skills::read_graph_skills(
            &Mode::Vault {
                dir: temp.path().to_path_buf(),
            },
            state,
        )
    };
    let first = served(&state);
    assert_eq!(first.len(), 1, "the vault's own skill reached the graph");
    assert!(first[0].body.contains("First wording."));

    std::fs::write(&skill, body("Second wording.")).expect("edit skill");
    state.tag_workspace_graph_dirty(std::slice::from_ref(&skill));
    state.ensure_graph_fresh();

    let second = served(&state);
    assert_eq!(second.len(), 1);
    assert!(
        second[0].body.contains("Second wording."),
        "the rebuild must serve the edited skill, not the one boot read: {}",
        second[0].body
    );
}

// ── Which producer owns the graph ─────────────────────────────────────────

fn noop_hooks() -> WorkspaceGraphHooks {
    WorkspaceGraphHooks {
        build: Box::new(|_| Err("not this one".to_string())),
        is_relevant: Box::new(|_| true),
    }
}

#[test]
fn vault_mode_builds_its_own_producer() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mode = Mode::Vault {
        dir: temp.path().to_path_buf(),
    };
    let (hooks, report) = vault_producer(
        &mode,
        None,
        &Arc::new(RwLock::new(None)),
        CachePolicy::Disabled,
    )
    .expect("vault mode installs its own producer");
    assert!(hooks.is_some(), "--vault must not boot with NO_BUILDER_MSG");
    assert!(report.is_some(), "rebuild_graph needs the report slot");
}

/// Both would write the same `GraphState` producer slot, and the silent
/// resolution — whichever construction ran last — decides what the server is.
#[test]
fn vault_mode_refuses_an_injected_producer() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mode = Mode::Vault {
        dir: temp.path().to_path_buf(),
    };
    let error = match vault_producer(
        &mode,
        Some(noop_hooks()),
        &Arc::new(RwLock::new(None)),
        CachePolicy::Disabled,
    ) {
        Err(error) => error,
        Ok(_) => panic!("two producers for one graph must be refused"),
    };
    assert!(error.contains("--vault"), "{error}");
    assert!(
        error.contains("--watch"),
        "the refusal names the way out: {error}"
    );
}

#[test]
fn every_other_mode_keeps_the_injected_producer_and_gets_no_slot() {
    let temp = tempfile::tempdir().expect("tempdir");
    for mode in [
        Mode::Watch {
            dir: temp.path().to_path_buf(),
        },
        Mode::Workspace {
            dir: temp.path().to_path_buf(),
        },
        Mode::Bare,
    ] {
        let (hooks, report) = vault_producer(
            &mode,
            Some(noop_hooks()),
            &Arc::new(RwLock::new(None)),
            CachePolicy::Disabled,
        )
        .expect("injection passes through");
        assert!(hooks.is_some(), "{mode:?}");
        assert!(report.is_none(), "{mode:?} has no vault report to render");
    }
}

/// `workspace_graph_mode` is the site that fails most quietly: without the
/// alias the state is built with `None`, `prepare_workspace_graph` bails, and
/// a `--vault` server serves an empty graph with no producer ever called.
#[test]
fn vault_mode_is_a_workspace_graph_mode() {
    assert_eq!(
        crate::cli::workspace_graph_mode(&Mode::Vault {
            dir: PathBuf::from("/vault")
        }),
        Some(WorkspaceGraphMode::Watch),
    );
}
