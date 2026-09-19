//! `okf::open`: when the cache answers, when it is ignored, and what happens
//! when it cannot be written.

use super::*;
use crate::graph::handle::make_dir_graph_mut;
use crate::graph::io::file::{load_file, save_graph};
use crate::graph::schema::EmbeddingStore;
use std::fs;
use tempfile::TempDir;

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// A vault with two notes and a declaration file — the same shape
/// `fingerprint_tests::vault` uses, so a reader comparing the two suites is
/// comparing behaviour and not fixtures.
fn vault() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "notes/alpha.md",
        "---\nid: alpha\n---\nAlpha.\n",
    );
    write(
        dir.path(),
        "notes/beta.md",
        "---\nid: beta\n---\nSee [[alpha]].\n",
    );
    write(
        dir.path(),
        ".kglite/vault.yaml",
        "kglite_vault: 1\ndefault_label: Note\n",
    );
    dir
}

fn opts() -> RebuildOptions {
    RebuildOptions::for_dialect(Dialect::Obsidian)
}

fn open_default(dir: &TempDir) -> Opened {
    open(dir.path(), &opts(), None, CachePolicy::Default).unwrap()
}

fn warnings(opened: &Opened) -> Vec<String> {
    opened
        .report()
        .map(|r| r.warnings.clone())
        .unwrap_or_default()
}

/// Rewrite the cache with one provenance field changed, which is how a cache
/// written by another build of kglite, or with other options, reaches this
/// build.
fn restamp(path: &Path, edit: impl FnOnce(&mut DirGraph)) {
    let mut graph = load_file(&path.to_string_lossy()).unwrap();
    edit(make_dir_graph_mut(&mut graph));
    save_graph(&mut graph, &path.to_string_lossy()).unwrap();
}

// ── the hit ─────────────────────────────────────────────────────────────────

#[test]
fn the_first_open_builds_and_writes_the_cache_and_the_second_loads_it() {
    let dir = vault();
    let first = open_default(&dir);
    assert!(first.was_rebuilt(), "nothing was cached yet");
    assert_eq!(first.report().unwrap().concepts, 2);
    assert!(warnings(&first).is_empty(), "{:?}", warnings(&first));
    assert!(cache_path(dir.path()).is_file(), "the cache was written");

    let second = open_default(&dir);
    assert!(
        matches!(second, Opened::Loaded(_)),
        "an untouched vault is served from its own cache"
    );
    assert!(second.report().is_none(), "nothing was read, so no report");
    assert_eq!(second.graph().type_indices.get("Note").unwrap().len(), 2);
}

/// The save that writes the cache must not move the fingerprint the cache was
/// stamped with — the whole reason [`is_cache_artifact`] exists. Asserted as a
/// third open, because the failure is "the second open rebuilds, and so does
/// every one after it".
#[test]
fn opening_repeatedly_never_rebuilds_again() {
    let dir = vault();
    assert!(open_default(&dir).was_rebuilt());
    for round in 0..3 {
        assert!(
            matches!(open_default(&dir), Opened::Loaded(_)),
            "round {round} rebuilt a vault nothing touched"
        );
    }
}

#[test]
fn an_edited_note_rebuilds_and_refreshes_the_cache() {
    let dir = vault();
    open_default(&dir);
    write(
        dir.path(),
        "notes/gamma.md",
        "---\nid: gamma\n---\nA third.\n",
    );
    let opened = open_default(&dir);
    assert!(opened.was_rebuilt(), "a new note is a change");
    assert_eq!(opened.report().unwrap().concepts, 3);
    assert!(warnings(&opened).is_empty(), "{:?}", warnings(&opened));

    let next = open_default(&dir);
    assert!(
        matches!(next, Opened::Loaded(_)),
        "the rebuild wrote its own result back"
    );
    assert_eq!(next.graph().type_indices.get("Note").unwrap().len(), 3);
}

/// A rebuild through `open` is a `rebuild_if_changed`, so the vectors of the
/// notes that did not change survive it — the property a cache exists to
/// protect on a vault big enough to want one.
#[test]
fn a_rebuild_through_open_carries_the_vectors_of_the_unchanged_notes() {
    let dir = vault();
    let mut built = open_default(&dir).into_graph();
    let graph = make_dir_graph_mut(&mut built);
    let slots: Vec<usize> = graph
        .type_indices
        .get("Note")
        .unwrap()
        .iter()
        .map(|idx| idx.index())
        .collect();
    let mut store = EmbeddingStore::new(2);
    for slot in &slots {
        store.set_embedding(*slot, &[1.0, 0.0]);
    }
    graph.set_embedding_store("Note", "body", store);
    save_graph(&mut built, &cache_path(dir.path()).to_string_lossy()).unwrap();

    write(
        dir.path(),
        "notes/beta.md",
        "---\nid: beta\n---\nBeta, rewritten.\n",
    );
    let opened = open_default(&dir);
    assert!(opened.was_rebuilt());
    let carried = opened
        .graph()
        .embeddings
        .get(&("Note".to_string(), "body_emb".to_string()))
        .expect("the carried store");
    assert_eq!(carried.len(), 2, "both notes kept their vectors");
}

/// Whether a vault has vectors must not depend on whether its cache happened
/// to hit. `rebuild_if_changed` runs the declared `embed:` targets, so the
/// cold build inside `open` has to as well — otherwise the very first open of
/// a vault writes a vectorless cache, and every later open loads it and finds
/// no targets to run.
#[test]
fn a_cold_open_runs_the_declared_embed_targets_and_caches_the_vectors() {
    let dir = vault();
    write(
        dir.path(),
        ".kglite/vault.yaml",
        "kglite_vault: 1\ndefault_label: Note\nembed:\n  Note: body\n",
    );
    let model = ConstantEmbedder;
    let opened = open(dir.path(), &opts(), Some(&model), CachePolicy::Default).unwrap();
    assert!(opened.was_rebuilt(), "nothing was cached yet");
    assert!(warnings(&opened).is_empty(), "{:?}", warnings(&opened));
    assert!(
        !opened.graph().embeddings.is_empty(),
        "the cold build ran the target the vault declared"
    );

    let cached = open(dir.path(), &opts(), Some(&model), CachePolicy::Default).unwrap();
    assert!(matches!(cached, Opened::Loaded(_)));
    assert!(
        !cached.graph().embeddings.is_empty(),
        "and the vectors were in the cache it wrote"
    );
}

#[test]
fn a_cold_open_with_no_embedder_says_what_it_skipped() {
    let dir = vault();
    write(
        dir.path(),
        ".kglite/vault.yaml",
        "kglite_vault: 1\ndefault_label: Note\nembed:\n  Note: body\n",
    );
    let opened = open_default(&dir);
    assert!(
        warnings(&opened)
            .iter()
            .any(|w| w.contains("no embedder was given")),
        "{:?}",
        warnings(&opened)
    );
}

/// Two dimensions of the same vector for every text — enough to prove a pass
/// ran, and nothing more.
struct ConstantEmbedder;

impl crate::graph::embedder::Embedder for ConstantEmbedder {
    fn dimension(&self) -> usize {
        2
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        Ok(texts.iter().map(|_| vec![0.5, 0.5]).collect())
    }
}

// ── the misses ──────────────────────────────────────────────────────────────

/// A vault copied to another path carries a cache stamped with the *old* root,
/// and `rebuild_if_changed` fingerprints the stamped root rather than the one
/// it was handed — so the copy would be served the original's graph, of the
/// original's files, for as long as the original stands still. `open`
/// compares the roots before it asks.
#[test]
fn a_vault_copied_to_another_path_does_not_serve_the_originals_cache() {
    let dir = vault();
    open_default(&dir);

    let moved = tempfile::tempdir().unwrap();
    for rel in [
        "notes/alpha.md",
        "notes/beta.md",
        ".kglite/vault.yaml",
        ".kglite/graph.kgl",
    ] {
        let to = moved.path().join(rel);
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        fs::copy(dir.path().join(rel), to).unwrap();
    }
    write(
        moved.path(),
        "notes/delta.md",
        "---\nid: delta\n---\nOnly in the copy.\n",
    );

    let opened = open_default(&moved);
    assert!(opened.was_rebuilt(), "the copy is not the original");
    assert_eq!(opened.report().unwrap().concepts, 3);
    assert_eq!(
        opened.graph().source_root.as_deref(),
        Some(
            moved
                .path()
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .as_ref()
        )
    );
}

#[test]
fn a_cache_written_by_another_build_of_kglite_is_a_miss() {
    let dir = vault();
    open_default(&dir);
    restamp(&cache_path(dir.path()), |g| {
        g.source_build_version = Some("0.0.1-from-the-past".to_string());
    });
    assert!(
        open_default(&dir).was_rebuilt(),
        "a graph this build did not produce is not a cache it can use"
    );
    assert!(
        matches!(open_default(&dir), Opened::Loaded(_)),
        "and the rebuild restamped it"
    );
}

#[test]
fn a_cache_built_with_other_options_is_a_miss() {
    let dir = vault();
    let with_body = RebuildOptions {
        with_body: Some(true),
        ..opts()
    };
    let first = open(dir.path(), &with_body, None, CachePolicy::Default).unwrap();
    assert!(first.was_rebuilt());

    let without = RebuildOptions {
        with_body: Some(false),
        ..opts()
    };
    let second = open(dir.path(), &without, None, CachePolicy::Default).unwrap();
    assert!(
        second.was_rebuilt(),
        "the fingerprint cannot see the knobs, so the stamp has to"
    );
    // …and the same knobs again hit.
    assert!(matches!(
        open(dir.path(), &without, None, CachePolicy::Default).unwrap(),
        Opened::Loaded(_)
    ));
}

/// `skip_dirs` is compared as a set: the order the caller listed them in is
/// not something the build did differently.
#[test]
fn the_options_stamp_does_not_depend_on_the_order_skip_dirs_were_listed_in() {
    let one = RebuildOptions {
        skip_dirs: vec!["b".into(), "a".into()],
        ..opts()
    };
    let other = RebuildOptions {
        skip_dirs: vec!["a".into(), "b".into()],
        ..opts()
    };
    assert_eq!(
        options_stamp(&one.resolve(Dialect::Obsidian)),
        options_stamp(&other.resolve(Dialect::Obsidian))
    );
}

#[test]
fn a_cache_with_no_dialect_stamp_is_a_miss_when_the_caller_names_none() {
    let dir = vault();
    open(
        dir.path(),
        &RebuildOptions::default(),
        None,
        CachePolicy::Default,
    )
    .unwrap();
    restamp(&cache_path(dir.path()), |g| g.source_dialect = None);
    assert!(
        open(
            dir.path(),
            &RebuildOptions::default(),
            None,
            CachePolicy::Default
        )
        .unwrap()
        .was_rebuilt(),
        "without a stamp there is no dialect to compare the fingerprint under"
    );
}

#[test]
fn a_cache_stamped_with_another_dialect_is_a_miss() {
    let dir = vault();
    open(
        dir.path(),
        &RebuildOptions::default(),
        None,
        CachePolicy::Default,
    )
    .unwrap();
    assert!(
        open_default(&dir).was_rebuilt(),
        "the okf cache is not the obsidian one"
    );
}

/// A truncated, half-written or format-refused cache is a cache miss, not an
/// error: the caller asked for the graph and the directory can still answer.
#[test]
fn a_cache_this_build_cannot_read_is_a_miss_not_an_error() {
    let dir = vault();
    write(dir.path(), ".kglite/graph.kgl", "not a kgl file at all");
    let opened = open_default(&dir).expect_rebuilt();
    assert_eq!(opened.report.concepts, 2);
    assert!(
        opened.report.warnings.is_empty(),
        "an unreadable cache is the ordinary cold state, not a warning: {:?}",
        opened.report.warnings
    );
    assert!(
        matches!(open_default(&dir), Opened::Loaded(_)),
        "and it was replaced with one this build can read"
    );
}

// ── the policies ────────────────────────────────────────────────────────────

#[test]
fn a_disabled_cache_is_never_read_and_never_written() {
    let dir = vault();
    for round in 0..2 {
        let opened = open(dir.path(), &opts(), None, CachePolicy::Disabled).unwrap();
        assert!(opened.was_rebuilt(), "round {round}: nothing to be hit");
        assert!(warnings(&opened).is_empty(), "{:?}", warnings(&opened));
        assert!(
            !cache_path(dir.path()).exists(),
            "round {round}: it wrote a cache it was told not to keep"
        );
    }
}

#[test]
fn a_relocated_cache_is_read_and_written_where_it_was_put() {
    let dir = vault();
    let elsewhere = tempfile::tempdir().unwrap();
    let path = elsewhere.path().join("nested/vault.kgl");
    let policy = CachePolicy::At(path.clone());

    assert!(open(dir.path(), &opts(), None, policy.clone())
        .unwrap()
        .was_rebuilt());
    assert!(path.is_file(), "created along with its directory");
    assert!(
        !cache_path(dir.path()).exists(),
        "and the default location was left alone"
    );
    assert!(matches!(
        open(dir.path(), &opts(), None, policy).unwrap(),
        Opened::Loaded(_)
    ));
}

// ── the write failures ──────────────────────────────────────────────────────

/// Stands for the whole class — a read-only vault, a full volume, a path the
/// process cannot create. A plain file where the cache's directory should be
/// is the one that fails the same way for every user, root included.
#[test]
fn a_cache_that_cannot_be_written_warns_and_still_returns_the_graph() {
    let dir = vault();
    let blocker = tempfile::tempdir().unwrap();
    write(blocker.path(), "in-the-way", "a file, not a directory");
    let path = blocker.path().join("in-the-way/graph.kgl");

    let opened = open(dir.path(), &opts(), None, CachePolicy::At(path.clone()))
        .unwrap()
        .expect_rebuilt();
    assert_eq!(opened.report.concepts, 2, "the caller got their graph");
    let warning = opened
        .report
        .warnings
        .iter()
        .find(|w| w.contains("was not written"))
        .unwrap_or_else(|| panic!("no cache warning in {:?}", opened.report.warnings));
    assert!(warning.contains(&path.display().to_string()), "{warning}");
    assert!(!path.exists());
}

#[test]
fn a_cache_another_process_holds_is_left_alone_with_a_warning() {
    let dir = vault();
    let path = cache_path(dir.path());
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let _held = GraphWriterLease::acquire_ex(&path, Duration::ZERO).expect("free lease");

    let opened = open_default(&dir).expect_rebuilt();
    assert_eq!(opened.report.concepts, 2, "the caller got their graph");
    let warning = opened
        .report
        .warnings
        .iter()
        .find(|w| w.contains("held by another process"))
        .unwrap_or_else(|| panic!("no contention warning in {:?}", opened.report.warnings));
    assert!(warning.contains(&path.display().to_string()), "{warning}");
    assert!(!path.exists(), "the cache itself was not written");
}

impl Opened {
    /// The build output, or a panic naming what came back instead — the test
    /// suite asks for `Rebuilt`'s report often enough to be worth a name.
    fn expect_rebuilt(self) -> BuildOutput {
        match self {
            Opened::Rebuilt(out) => *out,
            Opened::Loaded(_) => panic!("expected a rebuild, got a cache hit"),
        }
    }
}
