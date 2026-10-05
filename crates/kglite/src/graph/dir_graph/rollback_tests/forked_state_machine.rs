//! Randomised write sequences through a `Session`, checked against a graph
//! that never forks (issue #195).
//!
//! Each step is a create, a targeted delete, a delete-all, a MERGE, an edge
//! write, or taking/dropping a snapshot, and each write goes through either
//! `Session::begin`/`commit` or the `Session::write` guard. Deletes leave slots
//! on the free lists in arbitrary orders, so forks, fold-backs and the
//! deep-copy fallback all occur. After every step:
//!
//! - the session's graph holds the same nodes, edges and type-index sizes as
//!   the reference (see [`logical`] for why not slot for slot);
//! - every type-index entry resolves to a live node of that type, and the
//!   index, `MATCH (n:T)` and the backend agree on the count;
//! - a held snapshot still reads exactly as when it was taken.
//!
//! The generator is a fixed-seed xorshift, so a failure names its seed and
//! step and replays exactly.

use super::*;
use crate::graph::session::{CommitOutcome, Session};
use crate::graph::storage::mode::{new_dir_graph_in_mode, StorageMode};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn content(graph: &DirGraph) -> Fingerprint {
    let mut copy = graph.clone();
    // A disk backend materialises node weights into a per-query arena.
    let _query = copy.graph.begin_query();
    let mut print = fingerprint(&mut copy);
    print.version = 0;
    print
}

/// Node content, edge endpoints and per-type index sizes, without slots.
type Logical = (
    Vec<(String, String, PropPairs, Vec<String>)>,
    Vec<(String, String, String)>,
    Vec<(String, usize)>,
);

/// [`content`] without slot numbers or the slot-derived fabricated titles.
///
/// The reference cannot be compared slot for slot: `DELETE` collects its
/// targets in a `std` `HashSet`, so the order slots join the free list — and
/// with it which slot a later `CREATE` reuses — differs between two graphs
/// given identical statements. Slot identity under a fork is pinned by the
/// deterministic cases in `forked_free_list`.
fn logical(graph: &DirGraph) -> Logical {
    let print = content(graph);
    let key_of: HashMap<usize, String> = print
        .nodes
        .iter()
        .map(|(slot, _, _, _, props, _)| {
            let k = props
                .iter()
                .find(|(name, _)| name == "k")
                .map_or_else(String::new, |(_, value)| value.clone());
            (*slot, k)
        })
        .collect();
    let mut nodes: Vec<_> = print
        .nodes
        .into_iter()
        .map(|(_, node_type, id, _, props, labels)| (node_type, id, props, labels))
        .collect();
    nodes.sort();
    let mut edges: Vec<_> = print
        .edges
        .into_iter()
        .map(|(_, src, tgt, conn, _)| (key_of[&src].clone(), key_of[&tgt].clone(), conn))
        .collect();
    edges.sort();
    let types = print
        .type_indices
        .into_iter()
        .map(|(name, members)| (name, members.len()))
        .collect();
    (nodes, edges, types)
}

fn count(graph: &DirGraph, query: &str) -> usize {
    let params = HashMap::new();
    let out =
        crate::graph::session::execute::execute_read(graph, query, &ExecuteOptions::eager(&params))
            .unwrap_or_else(|e| panic!("{query}: {e}"));
    match out.result.rows[0][0] {
        Value::Int64(n) => n as usize,
        ref other => panic!("{query}: expected a count, got {other:?}"),
    }
}

fn assert_indexes_resolve(graph: &DirGraph, context: &str) {
    let _query = graph.graph.begin_query();
    let mut indexed = 0;
    for (name, members) in graph.type_indices.iter() {
        for idx in members.to_vec() {
            let view = graph
                .graph
                .node_view(idx)
                .unwrap_or_else(|| panic!("{context}: {name} index lists dead slot {idx:?}"));
            assert_eq!(
                view.node_type_str(&graph.interner),
                name.to_string(),
                "{context}: {name} index lists a node of another type"
            );
            indexed += 1;
        }
    }
    assert_eq!(
        indexed,
        graph.graph.node_count(),
        "{context}: index vs backend"
    );
    assert_eq!(
        count(graph, "MATCH (n:T) RETURN count(n)"),
        graph.graph.node_count(),
        "{context}: MATCH (n:T) vs backend"
    );
    assert_eq!(
        count(graph, "MATCH (n) WHERE n.k IS NULL RETURN count(n)"),
        0,
        "{context}: a node lost its property"
    );
}

fn new_graph(mode: StorageMode, dir: Option<&std::path::Path>) -> DirGraph {
    new_dir_graph_in_mode(mode, dir).expect("graph in mode")
}

fn run_machine(mode: StorageMode, seed: u64, steps: usize) {
    let dirs = (
        tempfile::tempdir().expect("tempdir"),
        tempfile::tempdir().expect("tempdir"),
    );
    let disk = matches!(mode, StorageMode::Disk);
    let session = Session::new(new_graph(mode, disk.then(|| dirs.0.path())));
    let mut reference = new_graph(mode, disk.then(|| dirs.1.path()));
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut live: Vec<String> = Vec::new();
    let mut next_key = 0usize;
    let mut held: Option<(Arc<DirGraph>, Fingerprint)> = None;

    for step in 0..steps {
        let context = format!("{mode:?} seed {seed} step {step}");
        let query = match rng.below(10) {
            0..=2 => {
                let keys: Vec<String> = (0..1 + rng.below(3))
                    .map(|_| {
                        next_key += 1;
                        format!("'k{next_key}'")
                    })
                    .collect();
                format!("UNWIND [{}] AS k CREATE (:T {{k: k}})", keys.join(", "))
            }
            3 | 4 if !live.is_empty() => {
                let k = live[rng.below(live.len())].clone();
                format!("MATCH (n:T {{k: '{k}'}}) DETACH DELETE n")
            }
            5 if rng.below(3) == 0 => "MATCH (n) DETACH DELETE n".to_string(),
            6 => {
                next_key += 1;
                let fresh = format!("k{next_key}");
                let k = if live.is_empty() || rng.below(2) == 0 {
                    fresh
                } else {
                    live[rng.below(live.len())].clone()
                };
                format!("MERGE (n:T {{k: '{k}'}}) SET n.v = {step}")
            }
            7 if live.len() >= 2 => {
                let a = live[rng.below(live.len())].clone();
                let b = live[rng.below(live.len())].clone();
                format!("MATCH (a:T {{k: '{a}'}}), (b:T {{k: '{b}'}}) CREATE (a)-[:R]->(b)")
            }
            8 => {
                held = match held.take() {
                    Some(_) => None,
                    None => {
                        let snapshot = session.snapshot();
                        let print = content(&snapshot);
                        Some((snapshot, print))
                    }
                };
                continue;
            }
            _ => continue,
        };

        let via_tx = rng.below(2) == 0;
        if std::env::var("SM_TRACE").is_ok() {
            eprintln!("{context}: tx={via_tx} held={} {query}", held.is_some());
        }
        if via_tx {
            let mut tx = session.begin();
            run(tx.working_mut().expect("read-write tx"), &query);
            let outcome = session.commit(tx, true);
            assert!(
                matches!(outcome, CommitOutcome::Committed { .. }),
                "{context}: {query}: {outcome:?}"
            );
        } else {
            run(&mut session.write(), &query);
        }
        run(&mut reference, &query);

        let snapshot = session.snapshot();
        assert_eq!(
            logical(&snapshot),
            logical(&reference),
            "{context}: {query}"
        );
        assert_indexes_resolve(&snapshot, &context);
        if let Some((reader, print)) = &held {
            assert_eq!(&content(reader), print, "{context}: held snapshot moved");
        }
        drop(snapshot);

        live = {
            let params = HashMap::new();
            crate::graph::session::execute::execute_read(
                &reference,
                "MATCH (n:T) RETURN n.k",
                &ExecuteOptions::eager(&params),
            )
            .expect("live keys")
            .result
            .rows
            .into_iter()
            .map(|row| match &row[0] {
                Value::String(k) => k.clone(),
                other => panic!("{context}: key {other:?}"),
            })
            .collect()
        };
    }
}

#[test]
fn random_writes_in_memory_match_a_graph_that_never_forks() {
    for seed in 0..24 {
        run_machine(StorageMode::Memory, seed, 80);
    }
}

#[test]
fn random_writes_in_mapped_mode_match_a_graph_that_never_forks() {
    for seed in 0..6 {
        run_machine(StorageMode::Mapped, seed, 60);
    }
}

#[test]
fn random_writes_in_disk_mode_match_a_graph_that_never_forks() {
    for seed in 0..3 {
        run_machine(StorageMode::Disk, seed, 40);
    }
}
