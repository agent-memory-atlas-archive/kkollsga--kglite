//! Writes to a frozen graph: the copy-on-write overlay that serves them, what
//! it keeps from a heap graph that took the same writes (every read, slot
//! identity, rollback, transactions, the log), and the three ways it ends (a
//! save keeps it, the promote valve collapses it, a whole-graph operation
//! collapses it).

use super::frozen_tests::{
    assert_reads_match, dump, flip_in_section, saved_topo, saved_topo_big, section_names,
    topo_graph,
};
use super::index_sections::with_floors;
use super::index_sections_tests::{load, load_owned, opts, rows, run, save};
use super::mappable::SaveLayout;
use super::*;
use crate::graph::schema::InternedKey;
use crate::graph::session::{execute_mut, CommitOutcome, Session};
use crate::graph::storage::fork_base::with_valve_floor;
use crate::graph::storage::GraphRead;
use crate::graph::wal::DurabilityLevel;
use petgraph::Direction::Outgoing;

/// A write script touching every write seam: node and edge creates (with a new
/// connection type), a detach delete (edges freed in adjacency order), a slot
/// reuse, property writes, and a delete of an edge the file already held.
const SCRIPT: [&str; 8] = [
    "CREATE (:A {id: 1000, n: 1, s: 'new'}), (:C {id: 1001, n: 2, s: 'new'})",
    "MATCH (a:A {id: 1000}), (b:B {id: 3}) CREATE (a)-[:K1]->(b), (a)-[:K9 {w: 1}]->(b)",
    "MATCH (b:B {id: 14}) DETACH DELETE b",
    "CREATE (:B {id: 2000, n: 3, s: 'again'})",
    "MATCH (a:A {id: 5})-[r:K2]->() DELETE r",
    "MATCH (a:A {id: 5}), (c:C {id: 5}) CREATE (a)-[:K2 {why: 'back'}]->(c)",
    "MATCH (h:H) SET h.n = 5",
    "UNWIND range(1, 6) AS i CREATE (:C {id: 3000 + i, n: i, s: 'bulk'})",
];

fn assert_overlay(graph: &DirGraph) {
    assert!(graph.graph.is_forked(), "an overlay");
    assert!(graph.graph.has_frozen_base(), "over the file's topology");
    assert!(!graph.graph.is_frozen(), "no longer served directly");
    assert!(graph.graph.is_mapped(), "still mapped storage");
}

fn assert_collapsed(graph: &DirGraph) {
    assert!(!graph.graph.is_forked() && !graph.graph.has_frozen_base());
    assert!(graph.graph.is_mapped(), "a heap `Mapped` graph");
}

fn memory_after(path: &std::path::Path, script: &[&str]) -> DirGraph {
    let mut memory = load_owned(path, StorageMode::Memory);
    for step in script {
        run(&mut memory, step);
    }
    memory
}

#[test]
fn a_write_overlays_a_frozen_graph_and_copies_nothing() {
    let (_tmp, path) = saved_topo();
    let mut graph = load_owned(&path, StorageMode::Mapped);
    let base = graph.graph.as_frozen().expect("frozen").clone();
    let heap_before = base.heap_bytes();
    for step in SCRIPT {
        run(&mut graph, step);
        assert_overlay(&graph);
    }
    assert_eq!(
        base.heap_bytes(),
        heap_before,
        "the base is never written: its heap state is what the load left"
    );
    assert!(
        std::sync::Arc::strong_count(&base) >= 2,
        "the overlay holds the same topology, not a copy"
    );
    let memory = memory_after(&path, &SCRIPT);
    assert_eq!(dump(&graph), dump(&memory));
    assert_reads_match(&graph, &memory, "overlay vs memory");
}

#[test]
fn an_overlay_answers_every_read_like_a_memory_graph_after_each_write() {
    let (_tmp, path) = saved_topo();
    let mut graph = load_owned(&path, StorageMode::Mapped);
    let mut memory = load_owned(&path, StorageMode::Memory);
    for step in SCRIPT {
        run(&mut graph, step);
        run(&mut memory, step);
        assert_reads_match(&graph, &memory, step);
        assert_eq!(
            graph.graph.node_bound(),
            memory.graph.node_bound(),
            "{step}"
        );
        assert_eq!(
            graph.graph.edge_bound(),
            memory.graph.edge_bound(),
            "{step}"
        );
    }
    for q in [
        "MATCH (a:A)-[:K1]->(b:B) RETURN b.id AS i, count(a) AS c ORDER BY c DESC, i LIMIT 9",
        "MATCH (h:H)-[r]->(x) RETURN type(r) AS t, count(x) AS c ORDER BY t",
        "MATCH (b:B)-[r:K3]->(c:C) RETURN sum(r.w), count(r), min(r.tag)",
        "MATCH (a:A {id: 1000})-[r]->(b) RETURN type(r) AS t, count(b) AS c ORDER BY t",
        "MATCH (c:C)<-[r]-(x) RETURN type(r) AS t, count(x) AS c ORDER BY t",
        "MATCH p = (a:A {id: 2})-[:K1*1..2]->(z) RETURN count(p)",
        "MATCH (n) RETURN labels(n)[0] AS l, count(n) AS c ORDER BY l",
    ] {
        assert_eq!(rows(&graph, q), rows(&memory, q), "{q}");
    }
    assert_overlay(&graph);
}

#[test]
fn a_failed_statement_over_a_frozen_base_restores_every_slot() {
    let (_tmp, path) = saved_topo();
    let mut graph = load_owned(&path, StorageMode::Mapped);
    run(&mut graph, SCRIPT[0]);
    run(&mut graph, SCRIPT[2]);
    let before = dump(&graph);
    for failing in [
        "CREATE (:A {id: 4000, n: 1}), (:A {id: 4001, bad: duration({months: 2147483648})})",
        "MATCH (a:A {id: 2}) DETACH DELETE a CREATE (:A {id: 4002, bad: duration({months: 2147483648})})",
        "MATCH (a:A {id: 6})-[r]->() DELETE r SET a.n = 9 CREATE (:A {id: 4003, bad: duration({months: 2147483648})})",
    ] {
        let failed = execute_mut(&mut graph, failing, &opts());
        assert!(failed.is_err(), "{failing} must fail so it rolls back");
        assert_eq!(dump(&graph), before, "rollback restores {failing}");
    }
    assert_overlay(&graph);
    // The next allocations land where a graph that never failed puts them.
    let mut reference = memory_after(&path, &[SCRIPT[0], SCRIPT[2]]);
    for step in [SCRIPT[3], SCRIPT[7]] {
        run(&mut graph, step);
        run(&mut reference, step);
    }
    assert_eq!(dump(&graph), dump(&reference));
    assert_eq!(graph.graph.node_bound(), reference.graph.node_bound());
}

fn run_in(session: &Session, script: &[&str]) -> crate::graph::session::Transaction {
    let mut tx = session.begin();
    for step in script {
        let working = tx.working_mut().expect("working copy");
        execute_mut(working, step, &opts()).unwrap_or_else(|e| panic!("{step}: {e}"));
    }
    tx
}

#[test]
fn a_transaction_over_a_frozen_base_commits_and_rolls_back_exactly() {
    let (_tmp, path) = saved_topo();
    let session = Session::from_arc(load(&path, StorageMode::Mapped));
    let original = dump(&session.snapshot());

    // An abandoned transaction leaves the published graph as it was.
    drop(run_in(&session, &SCRIPT[..4]));
    assert_eq!(dump(&session.snapshot()), original);
    assert!(
        session.snapshot().graph.is_frozen(),
        "the reader keeps the file's topology"
    );

    // A transaction that rolls back a statement and then commits keeps the rest.
    let mut tx = run_in(&session, &SCRIPT[..3]);
    let working = tx.working_mut().unwrap();
    assert!(execute_mut(
        working,
        "MATCH (a:A {id: 2}) DETACH DELETE a CREATE (:A {id: 4002, bad: duration({months: 2147483648})})",
        &opts()
    )
    .is_err());
    let working = tx.working_mut().unwrap();
    run(working, SCRIPT[3]);
    assert!(matches!(
        session.commit(tx, true),
        CommitOutcome::Committed { .. }
    ));
    let published = session.snapshot();
    assert_overlay(&published);
    let reference = memory_after(&path, &SCRIPT[..4]);
    assert_eq!(dump(&published), dump(&reference));
    assert_reads_match(&published, &reference, "after the commit");

    // A second transaction forks the overlay itself (delta and all).
    let tx = run_in(&session, &SCRIPT[4..]);
    assert!(matches!(
        session.commit(tx, true),
        CommitOutcome::Committed { .. }
    ));
    let published = session.snapshot();
    assert_overlay(&published);
    assert_eq!(dump(&published), dump(&memory_after(&path, &SCRIPT)));
}

#[test]
fn a_reader_of_the_frozen_graph_never_sees_the_writer() {
    let (_tmp, path) = saved_topo();
    let reader = load(&path, StorageMode::Mapped);
    let before = dump(&reader);
    let mut writer = (*reader).clone();
    for step in SCRIPT {
        run(&mut writer, step);
    }
    assert_overlay(&writer);
    assert!(reader.graph.is_frozen());
    assert_eq!(dump(&reader), before, "the reader's snapshot is untouched");
    let mut second = (*reader).clone();
    run(&mut second, SCRIPT[2]);
    assert_eq!(
        dump(&second),
        dump(&memory_after(&path, &SCRIPT[2..3])),
        "a second writer starts from the file, not from the first"
    );
}

#[test]
fn the_valve_collapses_an_overlay_that_outgrows_a_quarter_of_its_base() {
    let (_tmp, path) = saved_topo();
    // saved_topo holds a few hundred nodes and edges; a floor of 8 lets the
    // fraction decide.
    with_valve_floor(8, || {
        let mut graph = load_owned(&path, StorageMode::Mapped);
        let mut memory = load_owned(&path, StorageMode::Memory);
        let slots = graph.graph.node_count() + graph.graph.edge_count();
        let mut collapsed_at = None;
        for i in 0..slots {
            let step = format!("CREATE (:F {{id: {}, n: {i}}})", 10_000 + i);
            run(&mut graph, &step);
            run(&mut memory, &step);
            if collapsed_at.is_none() && !graph.graph.is_forked() {
                collapsed_at = Some(i + 1);
            }
            assert_eq!(dump(&graph), dump(&memory), "{step}");
        }
        let at = collapsed_at.expect("the overlay must collapse once it passes the fraction");
        assert!(
            at > slots / 8 && at <= slots / 2,
            "collapsed after {at} writes over {slots} slots: not near a quarter"
        );
        assert_collapsed(&graph);
        assert_reads_match(&graph, &memory, "after the valve");
        // The heap graph carries on as mapped storage always did.
        run(&mut graph, SCRIPT[2]);
        run(&mut memory, SCRIPT[2]);
        assert_eq!(dump(&graph), dump(&memory));
    });
}

#[test]
fn the_valve_keeps_a_rolled_back_statement_exact() {
    let (_tmp, path) = saved_topo();
    with_valve_floor(8, || {
        let mut graph = load_owned(&path, StorageMode::Mapped);
        let before = dump(&graph);
        // The statement crosses the valve part-way through, then fails: the
        // journal moves with the collapse and still restores every slot.
        let failed = execute_mut(
            &mut graph,
            "UNWIND range(1, 400) AS i CREATE (:F {id: 20000 + i, n: i}) \
             WITH 1 AS one CREATE (:F {id: 99999, bad: duration({months: 2147483648})})",
            &opts(),
        );
        assert!(failed.is_err());
        assert_eq!(dump(&graph), before);
    });
}

#[test]
fn a_whole_graph_operation_collapses_the_overlay_and_keeps_every_answer() {
    let (_tmp, path) = saved_topo();
    let mut graph = load_owned(&path, StorageMode::Mapped);
    let mut memory = load_owned(&path, StorageMode::Memory);
    for step in &SCRIPT[..4] {
        run(&mut graph, step);
        run(&mut memory, step);
    }
    assert_overlay(&graph);
    graph.vacuum();
    memory.vacuum();
    assert_collapsed(&graph);
    assert_eq!(dump(&graph), dump(&memory));
}

#[test]
fn a_save_after_writes_keeps_the_overlay_and_reloads_to_what_memory_holds() {
    let (tmp, path) = saved_topo();
    let mut graph = load(&path, StorageMode::Mapped);
    {
        let writer = crate::graph::handle::make_dir_graph_mut(&mut graph);
        for step in SCRIPT {
            run(writer, step);
        }
    }
    assert_overlay(&graph);
    let again = tmp.path().join("again.kgl");
    with_floors(10, 10, || {
        save_graph_with_layout(&mut graph, again.to_str().unwrap(), true, SaveLayout::Auto)
    })
    .unwrap();
    // The overlay sits over the old inode, which a rename does not disturb, so
    // it stays valid and keeps answering.
    assert_overlay(&graph);
    let reference = memory_after(&path, &SCRIPT);
    assert_eq!(dump(&graph), dump(&reference));

    let names = {
        let bytes = std::fs::read(&again).unwrap();
        let head = metadata_head::read_metadata_head(&bytes, "test").unwrap();
        head.mappable
            .iter()
            .map(|e| e.name.clone())
            .collect::<Vec<_>>()
    };
    assert!(
        names.iter().any(|n| n == "topo:nodes") && names.iter().any(|n| n == "topo:edges"),
        "the merged topology is recorded as raw sections: {names:?}"
    );
    // Frozen again, and the same graph a memory load of the new file holds
    // and the same one that took the writes in memory.
    let frozen = load_owned(&again, StorageMode::Mapped);
    assert!(frozen.graph.is_frozen());
    let from_file = load_owned(&again, StorageMode::Memory);
    assert_eq!(dump(&frozen), dump(&reference));
    assert_eq!(dump(&from_file), dump(&reference));
    assert_reads_match(&frozen, &from_file, "reloaded frozen vs reloaded memory");

    // The saved overlay carries on: more writes, a second save, a second reload.
    {
        let writer = crate::graph::handle::make_dir_graph_mut(&mut graph);
        run(writer, "MATCH (a:A {id: 1000}) DETACH DELETE a");
        run(writer, "CREATE (:A {id: 1000, n: 77, s: 'second'})");
    }
    let third = tmp.path().join("third.kgl");
    with_floors(10, 10, || {
        save_graph_with_layout(&mut graph, third.to_str().unwrap(), true, SaveLayout::Auto)
    })
    .unwrap();
    let mut expected = memory_after(&path, &SCRIPT);
    run(&mut expected, "MATCH (a:A {id: 1000}) DETACH DELETE a");
    run(&mut expected, "CREATE (:A {id: 1000, n: 77, s: 'second'})");
    assert_eq!(
        dump(&load_owned(&third, StorageMode::Mapped)),
        dump(&expected)
    );
    assert_eq!(
        dump(&load_owned(&third, StorageMode::Memory)),
        dump(&expected)
    );
}

#[test]
fn an_overlay_never_copies_a_damaged_base_section_into_a_save_or_the_heap() {
    let names: Vec<String> = section_names(&saved_topo_big().1)
        .into_iter()
        .filter(|n| n.starts_with("topo:") && n != "topo:nodes")
        .collect();
    let mut reached_overlay = 0;
    for name in &names {
        let (_tmp, path) = saved_topo_big();
        flip_in_section(&path, name);
        // Either the load refuses (the section is read while the graph is
        // built), or the graph opens, takes writes, and refuses to copy the
        // damaged section anywhere nothing would check it again.
        let Ok(mut graph) = load_file_with(
            path.to_str().unwrap(),
            &LoadOptions::new().with_storage(StorageMode::Mapped),
        ) else {
            continue;
        };
        let writer = crate::graph::handle::make_dir_graph_mut(&mut graph);
        run(writer, "CREATE (:A {id: 7000, n: 1, s: 'w'})");
        assert_overlay(writer);
        let target = path.with_file_name("refused.kgl");
        let saved = with_floors(10, 10, || {
            save_graph_with_layout(&mut graph, target.to_str().unwrap(), true, SaveLayout::Auto)
        });
        let error = saved.expect_err(&format!("{name}: a save must not copy a damaged section"));
        assert!(error.to_string().contains("corrupt"), "{name}: {error}");
        assert!(!target.exists(), "{name}: a refused save leaves no file");
        let graph = std::sync::Arc::get_mut(&mut graph).expect("unique");
        let collapsed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            graph.graph.flatten_fork();
        }));
        assert!(
            collapsed.is_err(),
            "{name}: the valve must refuse a damaged section"
        );
        reached_overlay += 1;
    }
    if cfg!(windows) {
        assert_eq!(reached_overlay, 0, "Windows verifies every section at load");
    } else {
        assert!(
            reached_overlay >= 4,
            "the lazily verified sections must be the ones the overlay guards: {reached_overlay}"
        );
    }
}

/// What a graph holds without its slot numbers, which a log replay does not
/// preserve: every node by label and properties, every edge by its ends.
fn logical(graph: &DirGraph) -> Vec<String> {
    [
        "MATCH (n) RETURN labels(n)[0] AS l, n.id AS id, n.n AS n, n.s AS s ORDER BY l, id",
        "MATCH (a)-[r]->(b) RETURN labels(a)[0] AS la, a.id AS ia, type(r) AS t, \
         labels(b)[0] AS lb, b.id AS ib, r.w AS w, r.why AS why ORDER BY la, ia, t, lb, ib, w, why",
    ]
    .iter()
    .map(|q| rows(graph, q))
    .collect()
}

#[test]
fn a_durable_session_over_a_frozen_file_replays_its_writes_after_a_crash() {
    for level in [DurabilityLevel::Full, DurabilityLevel::Normal] {
        let (tmp, path) = saved_topo();
        let path_str = path.to_str().unwrap().to_string();
        let session = Session::attach_log(load(&path, StorageMode::Mapped), &path_str, level)
            .unwrap_or_else(|e| panic!("{level:?}: attach: {e}"));
        for step in &SCRIPT[..6] {
            session
                .write_logged(|g| {
                    execute_mut(g, step, &opts())
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                })
                .unwrap_or_else(|e| panic!("{level:?} {step}: {e:?}"));
        }
        let published = session.snapshot();
        assert_overlay(&published);
        drop(published);
        drop(session); // crash-shaped: the log holds every write, the file none

        let reopened = Session::attach_log(load(&path, StorageMode::Mapped), &path_str, level)
            .unwrap_or_else(|e| panic!("{level:?}: reopen: {e}"));
        let recovered = reopened.snapshot();
        let reference = memory_after(&path, &SCRIPT[..6]);
        assert_eq!(
            logical(&recovered),
            logical(&reference),
            "{level:?}: replay"
        );
        assert_overlay(&recovered);

        // A write after recovery, then a checkpoint into the file, then a reload.
        let expected = memory_after(&path, &SCRIPT[..7]);
        reopened
            .write_logged(|g| {
                execute_mut(g, SCRIPT[6], &opts())
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            })
            .unwrap();
        with_floors(10, 10, || reopened.save(&path_str, true)).unwrap();
        drop(reopened);
        let after = load_owned(&path, StorageMode::Mapped);
        assert!(
            after.graph.is_frozen(),
            "{level:?}: the checkpoint is frozen again"
        );
        assert_eq!(
            logical(&after),
            logical(&expected),
            "{level:?}: the checkpoint is what a graph that took every write holds"
        );
        assert_eq!(dump(&after), dump(&load_owned(&path, StorageMode::Memory)));
        drop(tmp);
    }
}

#[test]
fn a_saved_graph_that_took_deletes_over_a_frozen_base_keeps_its_tombstones_consistent() {
    // The fixture already holds vacant node and edge slots; an overlay that
    // deletes more and re-creates must save a file whose free-list-derived
    // slots equal the heap graph's, so a later write on either lands alike.
    let (tmp, path) = saved_topo();
    let mut graph = load(&path, StorageMode::Mapped);
    {
        let writer = crate::graph::handle::make_dir_graph_mut(&mut graph);
        run(writer, "MATCH (b:B {id: 21}) DETACH DELETE b");
        run(writer, "MATCH (a:A {id: 9}) DETACH DELETE a");
        run(
            writer,
            "CREATE (:B {id: 5000, n: 1, s: 'x'}), (:A {id: 5001, n: 1, s: 'y'})",
        );
    }
    let saved = tmp.path().join("holes.kgl");
    with_floors(10, 10, || {
        save_graph_with_layout(&mut graph, saved.to_str().unwrap(), true, SaveLayout::Auto)
    })
    .unwrap();
    let mut reference = memory_after(
        &path,
        &[
            "MATCH (b:B {id: 21}) DETACH DELETE b",
            "MATCH (a:A {id: 9}) DETACH DELETE a",
            "CREATE (:B {id: 5000, n: 1, s: 'x'}), (:A {id: 5001, n: 1, s: 'y'})",
        ],
    );
    let mut reloaded = load_owned(&saved, StorageMode::Mapped);
    assert_eq!(dump(&reloaded), dump(&reference));
    for step in [
        "CREATE (:A {id: 6000, n: 1, s: 'z'})",
        "CREATE (:B {id: 6001, n: 1, s: 'z'})",
    ] {
        run(&mut reloaded, step);
        run(&mut reference, step);
    }
    assert_eq!(dump(&reloaded), dump(&reference));
    assert_eq!(reloaded.graph.node_bound(), reference.graph.node_bound());
}

#[test]
fn an_overlay_of_a_large_file_holds_only_what_was_written() {
    let (_tmp, path) = {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("big.kgl");
        let mut g = topo_graph();
        run(
            &mut g,
            "UNWIND range(1, 20000) AS i CREATE (:F {id: i, n: i})",
        );
        with_floors(10, 10, || save(g, &path, SaveLayout::Auto));
        (tmp, path)
    };
    let mut graph = load_owned(&path, StorageMode::Mapped);
    let base = graph.graph.as_frozen().expect("frozen").clone();
    let heap_before = base.heap_bytes();
    run(&mut graph, "MATCH (f:F {id: 17}) SET f.n = -1");
    run(&mut graph, "CREATE (:F {id: 99999, n: 5})");
    run(
        &mut graph,
        "MATCH (a:A {id: 1}), (f:F {id: 99999}) CREATE (a)-[:K1]->(f)",
    );
    assert_overlay(&graph);
    assert_eq!(base.heap_bytes(), heap_before, "the base is untouched");
    let overlay = graph.graph.overlay_entries().expect("an overlay");
    assert!(
        overlay <= 8,
        "three small writes put {overlay} entries in the overlay, not a copy of the graph"
    );
    assert_eq!(
        rows(&graph, "MATCH (f:F) WHERE f.n = -1 RETURN f.id"),
        "[[Int64(17)]]"
    );
}

#[test]
fn an_integrity_check_of_an_overlay_still_covers_the_file_topology() {
    let (_tmp, path) = saved_topo_big();
    let mut graph = load_owned(&path, StorageMode::Mapped);
    let topology_sections = |graph: &DirGraph| {
        let mut names: Vec<String> = verify_graph(graph)
            .sections
            .into_iter()
            .filter(|section| section.name.starts_with("topo:"))
            .map(|section| {
                assert!(
                    section.error.is_none(),
                    "{}: {:?}",
                    section.name,
                    section.error
                );
                section.name
            })
            .collect();
        names.sort();
        names
    };
    let before = topology_sections(&graph);
    if cfg!(windows) {
        assert!(before.is_empty(), "Windows copies every section at open");
        return;
    }
    assert!(
        before.len() >= 9,
        "the mapped topology sections: {before:?}"
    );
    run(&mut graph, SCRIPT[0]);
    assert_overlay(&graph);
    assert_eq!(topology_sections(&graph), before);
}

#[test]
fn an_overlay_serves_the_files_peer_counts_until_an_edge_is_written() {
    let (_tmp, path) = saved_topo();
    let mut graph = load_owned(&path, StorageMode::Mapped);
    let pure = load_owned(&path, StorageMode::Mapped);
    let k1 = InternedKey::from_str("K1");
    let recorded = graph.graph.lookup_peer_counts(k1);
    assert!(recorded.is_some(), "a frozen graph answers from the file");
    // Nodes only: no edge moved, so the file's counts and sources still hold.
    run(&mut graph, SCRIPT[0]);
    assert_overlay(&graph);
    assert_eq!(graph.graph.lookup_peer_counts(k1), recorded);
    assert_eq!(
        graph.graph.sources_for_conn_type_bounded(k1, None),
        pure.graph.sources_for_conn_type_bounded(k1, None)
    );
    assert!(graph
        .graph
        .cached_edge_counts_grouped_by_peer(k1, Outgoing, None)
        .unwrap()
        .is_some());
    // An edge write makes the file's counts stale: the overlay stops serving them.
    run(&mut graph, SCRIPT[1]);
    assert!(graph.graph.lookup_peer_counts(k1).is_none());
    assert!(graph
        .graph
        .cached_edge_counts_grouped_by_peer(k1, Outgoing, None)
        .unwrap()
        .is_none());
    assert!(graph
        .graph
        .sources_for_conn_type_bounded(k1, None)
        .is_none());
}

#[test]
fn transactions_over_an_overlay_do_not_spill_its_rows_to_files() {
    // A heap tail of more than the mapped floor (1 MiB) behind a type of
    // 30,000 rows. Each transaction forks the overlay; a spill pass over that
    // fork copied the tail and wrote it to fresh files every time.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("rows.kgl");
    let mut seed = DirGraph::new();
    run(
        &mut seed,
        "UNWIND range(1, 30000) AS i CREATE (:F {id: i, n: i, s: 'row-' + toString(i)})",
    );
    with_floors(10, 10, || save(seed, &path, SaveLayout::Auto));
    with_valve_floor(10_000_000, || {
        let session = Session::from_arc(load(&path, StorageMode::Mapped));
        let mut tx = session.begin();
        run(
            tx.working_mut().unwrap(),
            "UNWIND range(100001, 120000) AS i CREATE (:F {id: i, n: i, s: 'bulk-' + toString(i)})",
        );
        assert!(matches!(
            session.commit(tx, true),
            CommitOutcome::Committed { .. }
        ));
        assert_overlay(&session.snapshot());
        for round in 0..4 {
            let mut tx = session.begin();
            run(
                tx.working_mut().unwrap(),
                &format!("CREATE (:F {{id: {}, n: 1, s: 'one'}})", 200_000 + round),
            );
            if round % 2 == 0 {
                assert!(matches!(
                    session.commit(tx, true),
                    CommitOutcome::Committed { .. }
                ));
            } else {
                drop(tx);
            }
        }
        let published = session.snapshot();
        assert_overlay(&published);
        assert!(
            published.spill_dir.is_none(),
            "the overlay's rows were spilled to {:?}",
            published.spill_dir
        );
        assert_eq!(
            rows(&published, "MATCH (f:F) RETURN count(f)"),
            "[[Int64(50002)]]"
        );
    });
}
