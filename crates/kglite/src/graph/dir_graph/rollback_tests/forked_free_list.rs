//! Writes under a held reader after deletes (issue #195).
//!
//! A delete puts its slot on petgraph's node free list, and `add_node` pops
//! that list LIFO. The copy-on-write overlay must hand out exactly the slots
//! the fold-back's `add_node` will, or every index keyed on the overlay's
//! numbers is wrong. These tests pin the outcome on every entry point that
//! forks: each run is compared, slot for slot, with the same statements run on
//! a graph that never forked.
//!
//! The delete orders are chosen so the free-list *head* equals `node_bound()`
//! while deeper entries do not — the shape in which an overlay appending past
//! `node_bound()` diverged from the fold-back.

use super::*;
use crate::graph::handle::make_dir_graph_mut;
use crate::graph::session::{CommitOutcome, Session};
use crate::graph::storage::mode::{convert_dir_graph_to_mode, StorageMode};

const SEED: &str = "UNWIND range(0, 4) AS i CREATE (:Repro {k: 'K' + toString(i)})";
const CREATE_TWO: &str = "UNWIND ['A', 'B'] AS k CREATE (:Repro {k: k})";

/// Free list after this order: 0, 3, 2, 1, 4 (head first); `node_bound()` 0.
const DELETE_ALL: [&str; 5] = ["K4", "K1", "K2", "K3", "K0"];
/// Free list after this order: 4, 1 (head first); `node_bound()` 4.
const DELETE_SUBSET: [&str; 2] = ["K1", "K4"];

fn after_deletes(order: &[&str]) -> DirGraph {
    let mut graph = DirGraph::new();
    run(&mut graph, SEED);
    for k in order {
        run(
            &mut graph,
            &format!("MATCH (n:Repro {{k: '{k}'}}) DELETE n"),
        );
    }
    graph
}

/// The fingerprint minus the version counter: a fork-and-fold may take a
/// different number of write entries than the reference, and the counter is
/// not what these tests are about.
fn content(graph: &DirGraph) -> Fingerprint {
    let mut print = fingerprint(&mut graph.clone());
    print.version = 0;
    print
}

/// Every index must agree with the backend: the `{}` ghosts in #195 were
/// type-index entries whose slots held no node of that type.
fn assert_indexes_agree(graph: &DirGraph, context: &str) {
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    let matched = |query: &str| -> i64 {
        let out = crate::graph::session::execute::execute_read(graph, query, &opts)
            .unwrap_or_else(|e| panic!("{context}: {query}: {e}"));
        match out.result.rows[0][0] {
            Value::Int64(n) => n,
            ref other => panic!("{context}: expected a count, got {other:?}"),
        }
    };
    let typed = graph
        .type_indices
        .iter()
        .find(|(name, _)| *name == "Repro")
        .map_or(0, |(_, members)| members.to_vec().len());
    let all = matched("MATCH (n) RETURN count(n)");
    assert_eq!(
        all as usize,
        graph.graph.node_count(),
        "{context}: MATCH (n) must count exactly the live nodes"
    );
    assert_eq!(
        matched("MATCH (n:Repro) RETURN count(n)") as usize,
        typed,
        "{context}: the Repro type index must list exactly the Repro nodes"
    );
    assert_eq!(
        matched("MATCH (n) WHERE n.k IS NULL RETURN count(n)"),
        0,
        "{context}: a node without its property is a ghost left by a broken fold"
    );
}

/// Whether a write taken while a reader holds `graph` lands on an overlay —
/// the observable result of `forked::can_fork` on the shared base.
fn forks_under_a_reader(graph: DirGraph) -> bool {
    let mut writer = Arc::new(graph);
    let _reader = Arc::clone(&writer);
    make_dir_graph_mut(&mut writer).graph.is_forked()
}

#[test]
fn a_graph_forks_unless_its_free_list_order_is_unknown() {
    // The comparisons below are only about the overlay if these graphs fork.
    for (name, order) in [
        ("no deletes", &[][..]),
        ("delete-all", &DELETE_ALL[..]),
        ("subset", &DELETE_SUBSET[..]),
        ("top slot only", &["K4"][..]),
        ("ascending", &["K0", "K1", "K2", "K3", "K4"][..]),
    ] {
        assert!(
            forks_under_a_reader(after_deletes(order)),
            "{name}: a graph built in this process knows its free-list order"
        );
    }

    // Converting moves the same `StableDiGraph`, free lists and all, into a
    // new backend; the mirror moves with it.
    let mut converted = after_deletes(&DELETE_SUBSET);
    convert_dir_graph_to_mode(&mut converted, StorageMode::Mapped).expect("to mapped");
    convert_dir_graph_to_mode(&mut converted, StorageMode::Memory).expect("to memory");
    assert!(
        forks_under_a_reader(converted),
        "a storage-mode conversion keeps the free-list order it knew"
    );

    // A reload knows its free lists too: serde links them in slot order.
    for (name, order) in RELOAD_ORDERS {
        assert!(
            forks_under_a_reader(reloaded(&after_deletes_with_edges(order))),
            "{name}: a graph loaded from .kgl with holes forks"
        );
    }
}

/// Deletes that leave a hole a save keeps (an interior node, or an edge),
/// in orders that differ from slot order.
const RELOAD_ORDERS: [(&str, &[&str]); 3] = [
    ("interior nodes", &["K3", "K1"]),
    ("interior and top", &["K1", "K4", "K2"]),
    ("edge only", &[]),
];

/// [`after_deletes`] on a seed whose nodes carry a chain of edges, with the
/// first edge deleted: the edge hole survives a save, as interior node holes
/// do (a save trims only trailing holes).
fn after_deletes_with_edges(order: &[&str]) -> DirGraph {
    let mut graph = DirGraph::new();
    run(&mut graph, SEED);
    run(
        &mut graph,
        "MATCH (a:Repro), (b:Repro) WHERE b.k > a.k CREATE (a)-[:R]->(b)",
    );
    run(
        &mut graph,
        "MATCH (:Repro {k: 'K0'})-[r:R]->(:Repro {k: 'K1'}) DELETE r",
    );
    for k in order {
        run(
            &mut graph,
            &format!("MATCH (n:Repro {{k: '{k}'}}) DETACH DELETE n"),
        );
    }
    graph
}

fn reloaded(graph: &DirGraph) -> DirGraph {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("g.kgl").to_string_lossy().into_owned();
    crate::graph::io::file::save_graph(&mut Arc::new(graph.clone()), &path).expect("save");
    let loaded = crate::graph::io::file::load_file(&path).expect("load");
    let loaded = Arc::try_unwrap(loaded)
        .ok()
        .expect("a fresh load is unshared");
    // Without a hole every graph forks; the fixtures are about the holes.
    assert!(
        loaded.graph.node_count() < loaded.graph.node_bound()
            || loaded.graph.edge_count() < loaded.graph.edge_bound(),
        "precondition — the reloaded graph keeps a hole"
    );
    loaded
}

/// The reloaded graph forks under a reader, and every write under it lands
/// where the same writes land on the reloaded graph without a reader —
/// creates into the restored free lists, an edge write, and the fold.
#[test]
fn writes_under_a_held_reader_on_a_reloaded_graph_match_the_unforked_graph() {
    for (name, order) in RELOAD_ORDERS {
        let saved = after_deletes_with_edges(order);
        let mut reference = reloaded(&saved);
        let mut writer = Arc::new(reloaded(&saved));
        let reader = Arc::clone(&writer);
        let reader_before = content(&reader);

        for query in [
            CREATE_TWO,
            "MATCH (a:Repro {k: 'A'}), (b:Repro {k: 'B'}) CREATE (a)-[:R]->(b)",
            "UNWIND ['C', 'D', 'E'] AS k CREATE (:Repro {k: k})",
        ] {
            run(make_dir_graph_mut(&mut writer), query);
            run(&mut reference, query);
            // The first write forks off the reader. The edge write collapses
            // the overlay into the writer's own copy, which later writes edit.
            if query == CREATE_TWO {
                assert!(writer.graph.is_forked(), "{name}: no overlay");
            }
            assert_eq!(content(&writer), content(&reference), "{name}: {query}");
        }
        assert_eq!(content(&reader), reader_before, "{name}: the reader moved");
        drop(reader);
        run(make_dir_graph_mut(&mut writer), "CREATE (:Repro {k: 'F'})");
        run(&mut reference, "CREATE (:Repro {k: 'F'})");
        assert!(!writer.graph.is_forked(), "{name}: the fold ran");
        assert_eq!(
            content(&writer),
            content(&reference),
            "{name}: after the fold"
        );
        assert_indexes_agree(&writer, name);
    }
}

#[test]
fn creates_under_a_held_reader_after_deletes_land_on_the_unforked_slots() {
    for (name, order) in [
        ("delete-all", &DELETE_ALL[..]),
        ("subset", &DELETE_SUBSET[..]),
    ] {
        let mut reference = after_deletes(order);
        let mut writer = Arc::new(after_deletes(order));
        let reader = Arc::clone(&writer);
        let reader_before = content(&reader);

        run(make_dir_graph_mut(&mut writer), CREATE_TWO);
        run(&mut reference, CREATE_TWO);
        assert_eq!(
            content(&reader),
            reader_before,
            "{name}: the reader must not see the writer's creates"
        );
        // While the overlay is live, its scan must already be the unforked
        // graph's: reused slots merged into the base's gaps, in order.
        assert!(
            writer.graph.is_forked(),
            "{name}: precondition — an overlay"
        );
        assert_eq!(
            writer.graph.node_indices().collect::<Vec<_>>(),
            reference.graph.node_indices().collect::<Vec<_>>(),
            "{name}: node_indices under the overlay"
        );
        assert_eq!(
            content(&writer),
            content(&reference),
            "{name}: under the overlay"
        );
        drop(reader);

        // The first write after the reader drops is where the fold ran.
        run(make_dir_graph_mut(&mut writer), "CREATE (:Repro {k: 'C'})");
        run(&mut reference, "CREATE (:Repro {k: 'C'})");
        assert_eq!(content(&writer), content(&reference), "{name}");
        assert_indexes_agree(&writer, name);

        // The reporter's follow-up statements, on the same graph.
        let merge = "UNWIND ['K0', 'K1', 'K2'] AS k MERGE (s:Repro {k: k}) SET s.v = 1";
        run(make_dir_graph_mut(&mut writer), merge);
        run(&mut reference, merge);
        assert_eq!(content(&writer), content(&reference), "{name}: after MERGE");
        assert_indexes_agree(&writer, name);
    }
}

#[test]
fn a_failed_statement_under_a_held_reader_after_deletes_rolls_back_cleanly() {
    let mut reference = after_deletes(&DELETE_ALL);
    let mut writer = Arc::new(after_deletes(&DELETE_ALL));
    let reader = Arc::clone(&writer);
    let reader_before = content(&reader);
    let writer_before = content(&writer);

    // Two creates commit, then the third row violates the write scope.
    expect_failure(
        make_dir_graph_mut(&mut writer),
        "UNWIND ['A', 'B', 'X'] AS k CREATE (:Repro {k: k}) \
         WITH k WHERE k = 'X' CREATE (:Blocked {k: k})",
        Some(&["Repro"]),
    );
    assert_eq!(content(&reader), reader_before, "reader after the rollback");
    assert_eq!(content(&writer), writer_before, "writer after the rollback");
    drop(reader);

    run(make_dir_graph_mut(&mut writer), "CREATE (:Repro {k: 'C'})");
    run(&mut reference, "CREATE (:Repro {k: 'C'})");
    assert_eq!(content(&writer), content(&reference));
    assert_indexes_agree(&writer, "failed statement");
}

fn tx_commit(session: &Session, query: &str) {
    let mut tx = session.begin();
    run(tx.working_mut().expect("read-write tx"), query);
    let outcome = session.commit(tx, true);
    assert!(
        matches!(outcome, CommitOutcome::Committed { .. }),
        "{query}: expected a commit, got {outcome:?}"
    );
}

#[test]
fn session_commit_after_deletes_matches_the_unforked_graph() {
    for (name, order) in [
        ("delete-all", &DELETE_ALL[..]),
        ("subset", &DELETE_SUBSET[..]),
    ] {
        let session = Session::new(after_deletes(order));
        let mut reference = after_deletes(order);

        // `begin` shares the published graph, so the working copy forks off
        // it; `commit` publishes and then compacts.
        tx_commit(&session, CREATE_TWO);
        run(&mut reference, CREATE_TWO);
        assert_eq!(content(&session.snapshot()), content(&reference), "{name}");

        // A retrying driver re-sends the same batch.
        tx_commit(&session, CREATE_TWO);
        run(&mut reference, CREATE_TWO);
        let snapshot = session.snapshot();
        assert_eq!(content(&snapshot), content(&reference), "{name}: retry");
        assert_indexes_agree(&snapshot, name);
    }
}

#[test]
fn session_transact_then_an_edge_write_matches_the_unforked_graph() {
    let session = Session::new(after_deletes(&DELETE_ALL));
    let mut reference = after_deletes(&DELETE_ALL);
    let transact = |query: &str| {
        session
            .transact(|graph| {
                let params = HashMap::new();
                execute_mut(graph, query, &ExecuteOptions::eager(&params))
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            })
            .unwrap_or_else(|e| panic!("{query}: {e}"));
    };

    transact(CREATE_TWO);
    run(&mut reference, CREATE_TWO);
    // An edge write cannot be expressed in an overlay, so it collapses one.
    let edge = "MATCH (a:Repro {k: 'A'}), (b:Repro {k: 'B'}) CREATE (a)-[:R]->(b)";
    transact(edge);
    run(&mut reference, edge);
    let snapshot = session.snapshot();
    assert_eq!(content(&snapshot), content(&reference));
    assert_indexes_agree(&snapshot, "transact");
}

#[test]
fn session_write_guard_under_a_live_snapshot_matches_the_unforked_graph() {
    let session = Session::new(after_deletes(&DELETE_ALL));
    let mut reference = after_deletes(&DELETE_ALL);

    let snapshot = session.snapshot();
    run(&mut session.write(), CREATE_TWO);
    run(&mut reference, CREATE_TWO);
    drop(snapshot);

    // A delete collapses the overlay the guard's write produced.
    let delete = "MATCH (n:Repro {k: 'A'}) DELETE n";
    run(&mut session.write(), delete);
    run(&mut reference, delete);
    let snapshot = session.snapshot();
    assert_eq!(content(&snapshot), content(&reference));
    assert_indexes_agree(&snapshot, "write guard");
}

/// `vacuum` rebuilds from one concrete `StableDiGraph`, so it must collapse an
/// overlay first. A held reader on a graph with holes makes the write entry
/// fork; before the collapse, `vacuum` reached `take_heap_graph` on the
/// overlay and panicked (0.19.3: `g.freeze()` then `g.vacuum()`).
#[test]
fn a_vacuum_under_a_held_reader_after_deletes_rebuilds_the_writer_only() {
    let mut reference = after_deletes(&DELETE_SUBSET);
    let mut writer = Arc::new(after_deletes(&DELETE_SUBSET));
    let reader = Arc::clone(&writer);
    let reader_before = content(&reader);

    let graph = make_dir_graph_mut(&mut writer);
    assert!(graph.graph.is_forked(), "precondition — an overlay");
    assert!(
        graph.vacuum().describes_rebuild(),
        "the fixture has holes, so the vacuum must rebuild"
    );
    reference.vacuum();
    assert_eq!(content(&writer), content(&reference));
    assert_eq!(
        content(&reader),
        reader_before,
        "the reader keeps its holes"
    );
}
