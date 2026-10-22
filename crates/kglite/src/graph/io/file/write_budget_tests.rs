//! The unsaved-write budget of a mapped load: what enrolls, how the warning and
//! the refusal arrive, and what a save, a durable log and a declined rebase do
//! to it.

use super::frozen_tests::{dump, saved_topo};
use super::index_sections::with_floors;
use super::index_sections_tests::{load, opts, rows, run};
use super::*;
use crate::error::{KgError, KgErrorCode};
use crate::graph::dir_graph::{WriteBudgetSetting, WriteBudgetState};
use crate::graph::session::{execute_mut, CommitOutcome, Session};
use crate::graph::wal::DurabilityLevel;

/// Small enough that a few dozen single-node writes cross it.
const TINY: u64 = 12_000;
const BULK: &str = "UNWIND range(1, 4000) AS i CREATE (:A {id: 10000 + i, n: i, s: 'bulk'})";

fn load_budget(path: &std::path::Path, setting: WriteBudgetSetting) -> Arc<DirGraph> {
    load_file_with(
        path.to_str().unwrap(),
        &LoadOptions::new()
            .with_storage(StorageMode::Mapped)
            .with_write_budget(setting),
    )
    .unwrap()
}

fn owned(path: &std::path::Path, setting: WriteBudgetSetting) -> DirGraph {
    Arc::try_unwrap(load_budget(path, setting)).unwrap_or_else(|_| panic!("uniquely owned"))
}

fn state(graph: &DirGraph) -> WriteBudgetState {
    graph.graph_info().write_budget_state
}

fn count_a(graph: &DirGraph) -> usize {
    let rendered = rows(graph, "MATCH (n:A) RETURN count(n)");
    let digits = rendered.split("Int64(").nth(1).expect("a count");
    digits.split(')').next().unwrap().parse().unwrap()
}

// KgError deliberately carries structured context; boxing it would change the public result type.
#[allow(clippy::result_large_err)]
fn create(graph: &mut DirGraph, id: usize) -> Result<(), KgError> {
    execute_mut(
        graph,
        &format!("CREATE (:A {{id: {}, n: 1, s: 'x'}})", 20_000 + id),
        &opts(),
    )
    .map(|_| ())
}

/// Write single nodes until one is refused, returning how many were accepted
/// and how many warnings were parked on the way.
fn write_until_refused(graph: &mut DirGraph, from: usize) -> (usize, usize, KgError) {
    let mut warnings = 0;
    for step in 0..100_000 {
        match create(graph, from + step) {
            Ok(()) => {
                if graph.take_write_budget_warning().is_some() {
                    warnings += 1;
                }
            }
            Err(error) => return (step, warnings, error),
        }
    }
    panic!("no write was ever refused");
}

#[test]
fn a_mapped_load_over_a_frozen_base_is_enrolled_with_the_default_limit() {
    let (_tmp, path) = saved_topo();
    let graph = load_budget(&path, WriteBudgetSetting::Default);
    let info = graph.graph_info();
    assert_eq!(info.write_budget_bytes, Some(512 << 20), "the floor");
    assert_eq!(info.write_budget_used_bytes, 0);
    assert_eq!(info.write_budget_state, WriteBudgetState::Ok);

    let file_bytes = std::fs::metadata(&path).unwrap().len();
    assert!(file_bytes < 1 << 30, "the floor decides for a small file");

    // Neither a memory graph nor a graph that decoded its topology is metered.
    let memory = load(&path, StorageMode::Memory);
    assert_eq!(state(&memory), WriteBudgetState::NotApplicable);
    let decoded = load_file_with(
        path.to_str().unwrap(),
        &LoadOptions::new()
            .with_storage(StorageMode::Mapped)
            .with_topology(MapPolicy::Memory),
    )
    .unwrap();
    assert_eq!(state(&decoded), WriteBudgetState::NotApplicable);
    assert_eq!(decoded.graph_info().write_budget_bytes, None);
}

#[test]
fn the_default_limit_is_half_the_file_above_the_floor() {
    use crate::graph::dir_graph::write_budget::{limit_for, WRITE_BUDGET_FLOOR_BYTES};
    assert_eq!(
        limit_for(WriteBudgetSetting::Default, 0),
        Some(WRITE_BUDGET_FLOOR_BYTES)
    );
    assert_eq!(
        limit_for(WriteBudgetSetting::Default, 4 << 30),
        Some(2 << 30)
    );
    assert_eq!(limit_for(WriteBudgetSetting::Bytes(7), 4 << 30), Some(7));
    assert_eq!(limit_for(WriteBudgetSetting::Unlimited, 4 << 30), None);
}

#[test]
fn writes_warn_once_then_are_refused_and_a_save_starts_over() {
    let (_tmp, path) = saved_topo();
    let mut arc = load_budget(&path, WriteBudgetSetting::Bytes(TINY));
    let base_a = count_a(&arc);
    let mut base_a = base_a;
    for round in 0..2 {
        let (accepted, warnings, error) = {
            let graph = crate::graph::handle::make_dir_graph_mut(&mut arc);
            let from = round * 100_000;
            write_until_refused(graph, from)
        };
        assert!(accepted > 3, "round {round}: some writes fit ({accepted})");
        assert_eq!(warnings, 1, "round {round}: one warning per crossing");
        assert_eq!(error.code(), KgErrorCode::WriteBudgetExceeded);
        let message = error.to_string();
        assert!(
            message.contains("Save the graph") && message.contains("durable"),
            "{message}"
        );
        assert_ne!(state(&arc), WriteBudgetState::Ok);

        // Reads keep working, and a refused statement left nothing behind.
        let count = rows(&arc, "MATCH (n:A) RETURN count(n)");
        assert!(
            count.contains(&format!("({})", base_a + accepted)),
            "{count}"
        );
        let before = dump(&arc);
        assert!(create(crate::graph::handle::make_dir_graph_mut(&mut arc), 1).is_err());
        assert_eq!(dump(&arc), before, "a refused write changes nothing");

        // A save writes the overlay out and rebases: the budget starts over.
        let target = path.with_file_name(format!("saved{round}.kgl"));
        with_floors(10, 10, || {
            save_graph_with_layout(&mut arc, target.to_str().unwrap(), true, SaveLayout::Auto)
                .unwrap()
        });
        if cfg!(windows) {
            // The rebase is declined where sections are copied: the save wrote
            // the changes but the heap they hold stays, and the refusal says a
            // save will not help.
            assert_ne!(state(&arc), WriteBudgetState::Ok);
            let refused = create(crate::graph::handle::make_dir_graph_mut(&mut arc), 3)
                .expect_err("still at the limit");
            assert!(
                refused.to_string().contains("could not release"),
                "{refused}"
            );
            return;
        }
        assert!(arc.graph.is_frozen(), "rebased onto the saved file");
        assert_eq!(state(&arc), WriteBudgetState::Ok);
        assert_eq!(arc.graph_info().write_budget_used_bytes, 0);
        // Writes resume (and the limit now follows the new file).
        create(crate::graph::handle::make_dir_graph_mut(&mut arc), 2).unwrap();
        base_a = count_a(&arc);
    }
}

#[test]
fn the_statement_that_crosses_the_limit_is_rolled_back_whole() {
    let (_tmp, path) = saved_topo();
    let mut graph = owned(&path, WriteBudgetSetting::Bytes(TINY));
    run(&mut graph, "CREATE (:A {id: 9001, n: 1, s: 'kept'})");
    let before = dump(&graph);
    let nodes = graph.graph_info().node_count;
    let used = graph.graph_info().write_budget_used_bytes;
    let error = execute_mut(&mut graph, BULK, &opts())
        .err()
        .expect("refused");
    assert_eq!(error.code(), KgErrorCode::WriteBudgetExceeded);
    assert_eq!(
        dump(&graph),
        before,
        "nodes, edges and properties unchanged"
    );
    assert_eq!(graph.graph_info().node_count, nodes);
    assert!(rows(&graph, "MATCH (n:A {id: 9001}) RETURN n.s").contains("kept"));
    assert_eq!(
        rows(&graph, "MATCH (n:A {id: 10001}) RETURN count(n)"),
        "[[Int64(0)]]"
    );
    // The rollback takes the statement's overlay entries with it: the budget
    // reads what it read before, and the next small write is accepted.
    assert_eq!(graph.graph_info().write_budget_used_bytes, used);
    assert_eq!(state(&graph), WriteBudgetState::Ok);
    for round in 0..20 {
        assert!(execute_mut(&mut graph, BULK, &opts()).is_err());
        assert_eq!(
            graph.graph_info().write_budget_used_bytes,
            used,
            "refusal {round}"
        );
    }
    create(&mut graph, 0).unwrap();
}

#[test]
fn a_transaction_crossing_the_limit_refuses_the_statement_and_stays_usable() {
    let (_tmp, path) = saved_topo();
    let session = Session::from_arc(load_budget(&path, WriteBudgetSetting::Bytes(TINY)));
    let original = dump(&session.snapshot());
    let mut tx = session.begin();
    let working = tx.working_mut().expect("working copy");
    run(working, "CREATE (:A {id: 9001, n: 1, s: 'in tx'})");
    let inside = dump(working);
    let error = execute_mut(working, BULK, &opts()).err().expect("refused");
    assert_eq!(error.code(), KgErrorCode::WriteBudgetExceeded);
    assert_eq!(dump(working), inside, "the earlier statement survives");
    assert!(rows(working, "MATCH (n:A {id: 9001}) RETURN count(n)").contains("1"));
    drop(tx);
    assert_eq!(dump(&session.snapshot()), original);

    // The transaction that stays inside the budget commits.
    let mut tx = session.begin();
    run(
        tx.working_mut().unwrap(),
        "CREATE (:A {id: 9003, n: 1, s: 'fits'})",
    );
    assert!(matches!(
        session.commit(tx, true),
        CommitOutcome::Committed { .. }
    ));
    assert_ne!(dump(&session.snapshot()), original);

    // A rolled-back transaction leaves the published graph as it was.
    let published = dump(&session.snapshot());
    let mut tx = session.begin();
    let working = tx.working_mut().unwrap();
    assert!(execute_mut(working, BULK, &opts()).is_err());
    drop(tx);
    assert_eq!(dump(&session.snapshot()), published);
}

#[test]
fn an_unlimited_budget_never_warns_or_refuses() {
    let (_tmp, path) = saved_topo();
    let mut graph = owned(&path, WriteBudgetSetting::Unlimited);
    run(&mut graph, BULK);
    assert_eq!(graph.take_write_budget_warning(), None);
    let info = graph.graph_info();
    assert_eq!(info.write_budget_state, WriteBudgetState::Unlimited);
    assert_eq!(info.write_budget_bytes, None);
    assert!(info.write_budget_used_bytes > 0, "the reading still moves");
}

#[test]
fn a_zero_budget_refuses_every_write() {
    let (_tmp, path) = saved_topo();
    let mut graph = owned(&path, WriteBudgetSetting::Bytes(0));
    let before = count_a(&graph);
    let error = create(&mut graph, 0).expect_err("refused");
    assert_eq!(error.code(), KgErrorCode::WriteBudgetExceeded);
    assert_eq!(count_a(&graph), before);
}

#[test]
fn a_durable_session_carries_no_budget() {
    let (_tmp, path) = saved_topo();
    let path_str = path.to_str().unwrap().to_string();
    let session = Session::attach_log(
        load_budget(&path, WriteBudgetSetting::Bytes(0)),
        &path_str,
        DurabilityLevel::Normal,
    )
    .unwrap();
    session
        .write_logged(|g| {
            execute_mut(g, BULK, &opts())
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
        .unwrap();
    let info = session.snapshot().graph_info();
    assert_eq!(info.write_budget_state, WriteBudgetState::NotApplicable);
}

#[test]
fn a_save_that_cannot_rebase_keeps_the_budget_and_says_so() {
    let (_tmp, path) = saved_topo();
    let mut arc = load_budget(&path, WriteBudgetSetting::Bytes(TINY));
    let (_, _, first) = write_until_refused(crate::graph::handle::make_dir_graph_mut(&mut arc), 0);
    assert!(first.to_string().contains("Save the graph"));

    // A compact save writes no raw topology, so the graph cannot rebase.
    let target = path.with_file_name("compact.kgl");
    save_graph_with_layout(
        &mut arc,
        target.to_str().unwrap(),
        true,
        SaveLayout::Compact,
    )
    .unwrap();
    assert!(arc.graph.has_frozen_base(), "still an overlay");
    assert_eq!(
        state(&arc),
        WriteBudgetState::Exceeded,
        "the heap is still held"
    );
    let error = create(crate::graph::handle::make_dir_graph_mut(&mut arc), 7).expect_err("refused");
    let message = error.to_string();
    assert!(message.contains("could not release"), "{message}");
    assert!(!message.contains("Save the graph to continue"), "{message}");
}

#[test]
fn an_operator_spelling_parses_to_a_setting() {
    for (text, expected) in [
        ("default", WriteBudgetSetting::Default),
        ("Unlimited", WriteBudgetSetting::Unlimited),
        ("none", WriteBudgetSetting::Unlimited),
        ("1000", WriteBudgetSetting::Bytes(1000)),
        ("512M", WriteBudgetSetting::Bytes(512 << 20)),
        ("2GiB", WriteBudgetSetting::Bytes(2 << 30)),
        (" 3 kb ", WriteBudgetSetting::Bytes(3 << 10)),
    ] {
        assert_eq!(WriteBudgetSetting::parse(text), Ok(expected), "{text}");
    }
    for text in ["", "lots", "12T", "-5", "1.5G", "99999999999G"] {
        assert!(WriteBudgetSetting::parse(text).is_err(), "{text}");
    }
}

#[test]
fn a_graph_at_its_limit_refuses_before_the_statement_runs() {
    let (_tmp, path) = saved_topo();
    let mut graph = owned(&path, WriteBudgetSetting::Bytes(0));
    assert!(graph.check_write_budget().is_err(), "at the limit");
    // A statement that would fail on its own is never run: the budget answers.
    let error = execute_mut(&mut graph, "CREATE (:A {id: 1 / 0})", &opts())
        .err()
        .expect("refused");
    assert_eq!(error.code(), KgErrorCode::WriteBudgetExceeded);
}

#[test]
fn the_warning_rearms_when_the_writes_that_raised_it_are_dropped() {
    let (_tmp, path) = saved_topo();
    let mut graph = owned(&path, WriteBudgetSetting::Bytes(TINY));
    let warn_in = |g: &mut DirGraph| {
        for i in 0..10_000 {
            if create(g, i).is_err() {
                return false;
            }
            if g.take_write_budget_warning().is_some() {
                return true;
            }
        }
        false
    };
    let mut fork = graph.clone();
    assert!(warn_in(&mut fork), "the fork crosses half the limit");
    drop(fork);
    // The published graph never saw those writes, so it is under half again
    // and warns when it crosses.
    assert!(
        warn_in(&mut graph),
        "re-armed by the first statement under half"
    );
}
