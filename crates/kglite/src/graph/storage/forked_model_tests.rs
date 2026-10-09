//! Randomised operation sequences against a fork, checked against a graph that
//! never forks.
//!
//! Each case builds a base with holes in both free lists, forks it, and runs
//! the same random sequence of `add_node` / `add_edge` / `remove_edge` /
//! `remove_node` / weight writes against the fork and against a flat copy of
//! the base. Three things are compared:
//!
//! - **Slot identity.** Every add must return the slot the flat graph's
//!   petgraph allocated (issue #195: an overlay that hands out a different
//!   number than the fold-back produces mis-keys every index).
//! - **Reads.** Every few operations the fork's full read surface (counts,
//!   bounds, node and edge scans, per-node edges and neighbours in all three
//!   directions, edges between node pairs) must equal the flat graph's, in
//!   the same order.
//! - **The fold.** The folded graph must read like the flat one and carry the
//!   same free lists: its slot mirror is compared outright, and a probe batch
//!   of adds must land on the same slots in both.
//!
//! The generator is a fixed-seed xorshift, so a failure names its case and
//! replays exactly.

use std::collections::HashMap;
use std::sync::Arc;

use petgraph::graph::{EdgeIndex, NodeIndex};
use petgraph::Direction;

use crate::datatypes::Value;
use crate::graph::schema::{EdgeData, NodeData};
use crate::graph::storage::forked::ForkedGraph;
use crate::graph::storage::interner::StringInterner;
use crate::graph::storage::{GraphRead, GraphWrite, MemoryGraph};

struct Rng(u64);

impl Rng {
    fn new(case: u64) -> Self {
        Rng(case.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03 | 1)
    }

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

struct Maker {
    interner: StringInterner,
    serial: i64,
}

impl Maker {
    fn node(&mut self) -> NodeData {
        self.serial += 1;
        NodeData::new(
            Value::Int64(self.serial),
            Value::String(format!("n{}", self.serial)),
            "Item".to_string(),
            HashMap::new(),
            &mut self.interner,
        )
    }

    fn edge(&mut self, rng: &mut Rng) -> EdgeData {
        self.serial += 1;
        let kind = ["R", "S"][rng.below(2)];
        let mut props = HashMap::new();
        props.insert("w".to_string(), Value::Int64(self.serial));
        EdgeData::new(kind.to_string(), props, &mut self.interner)
    }
}

/// Everything a read can observe, as text: equal text means equal order too.
fn dump<G: GraphRead>(g: &G) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "counts {} {} bounds {} {}",
        g.node_count(),
        g.edge_count(),
        g.node_bound(),
        g.edge_bound()
    );
    let nodes: Vec<NodeIndex> = g.node_indices().collect();
    let _ = writeln!(out, "nodes {nodes:?}");
    for &n in &nodes {
        let _ = writeln!(out, "node {n:?} {:?}", g.node_weight(n));
        for dir in [Direction::Outgoing, Direction::Incoming] {
            let edges: Vec<_> = g
                .edges_directed(n, dir)
                .map(|e| {
                    (
                        e.id(),
                        e.source(),
                        e.target(),
                        e.weight().properties.clone(),
                    )
                })
                .collect();
            let _ = writeln!(out, " {dir:?} {edges:?}");
            let peers: Vec<_> = g.neighbors_directed(n, dir).collect();
            let _ = writeln!(out, " peers {dir:?} {peers:?}");
        }
        let both: Vec<_> = g.neighbors_undirected(n).collect();
        let _ = writeln!(out, " undirected {both:?}");
        let default: Vec<_> = g.edges(n).map(|e| e.id()).collect();
        let _ = writeln!(out, " edges() {default:?}");
        for &m in &nodes {
            let between: Vec<_> = g.edges_connecting(n, m).map(|e| e.id()).collect();
            let found = g.find_edge(n, m);
            let _ = writeln!(out, " {n:?}->{m:?} {between:?} {found:?}");
        }
    }
    let refs: Vec<_> = g
        .edge_references()
        .map(|e| {
            (
                e.id(),
                e.source(),
                e.target(),
                e.weight().connection_type,
                e.weight().properties.clone(),
            )
        })
        .collect();
    let _ = writeln!(out, "refs {refs:?}");
    let indices: Vec<EdgeIndex> = g.edge_indices().collect();
    let _ = writeln!(out, "indices {indices:?}");
    let weights: Vec<_> = g.edge_weights().map(|w| w.properties.clone()).collect();
    let _ = writeln!(out, "weights {weights:?}");
    let keys: Vec<_> = g.edge_endpoint_keys().collect();
    let _ = writeln!(out, "keys {keys:?}");
    for slot in 0..g.edge_bound() + 1 {
        let slot = EdgeIndex::new(slot);
        let _ = writeln!(
            out,
            "slot {slot:?} {:?} {:?}",
            g.edge_endpoints(slot),
            g.edge_weight(slot).map(|w| w.properties.clone())
        );
    }
    out
}

/// Compare two dumps, naming the first line that differs instead of printing
/// both in full.
#[track_caller]
fn assert_same(left: &str, right: &str, context: impl std::fmt::Display) {
    if left == right {
        return;
    }
    let (mut l, mut r) = (left.lines(), right.lines());
    loop {
        match (l.next(), r.next()) {
            (Some(a), Some(b)) if a == b => continue,
            (a, b) => panic!("{context}\n  fork: {a:?}\n  flat: {b:?}"),
        }
    }
}

fn live_nodes(g: &MemoryGraph) -> Vec<NodeIndex> {
    GraphRead::node_indices(g).collect()
}

fn live_edges(g: &MemoryGraph) -> Vec<EdgeIndex> {
    GraphRead::edge_indices(g).collect()
}

/// A base with holes in both free lists, built through the write seam so its
/// slot mirror is synced.
fn holey_base(rng: &mut Rng, maker: &mut Maker) -> MemoryGraph {
    let mut g = MemoryGraph::new();
    let nodes = 3 + rng.below(10);
    for _ in 0..nodes {
        GraphWrite::add_node(&mut g, maker.node());
    }
    for _ in 0..rng.below(24) {
        let live = live_nodes(&g);
        if live.is_empty() {
            break;
        }
        let (a, b) = (live[rng.below(live.len())], live[rng.below(live.len())]);
        GraphWrite::add_edge(&mut g, a, b, maker.edge(rng));
    }
    for _ in 0..rng.below(8) {
        let edges = live_edges(&g);
        if let Some(&e) = edges.get(rng.below(edges.len().max(1))) {
            GraphWrite::remove_edge(&mut g, e);
        }
    }
    for _ in 0..rng.below(4) {
        let live = live_nodes(&g);
        if live.len() > 2 {
            GraphWrite::remove_node(&mut g, live[rng.below(live.len())]);
        }
    }
    g
}

/// One random operation, applied to both graphs; every observable result must
/// match. Returns a label for the failure message.
fn step(
    rng: &mut Rng,
    maker: &mut Maker,
    fork: &mut ForkedGraph,
    flat: &mut MemoryGraph,
) -> &'static str {
    let live = live_nodes(flat);
    let edges = live_edges(flat);
    match rng.below(20) {
        0..=3 => {
            let data = maker.node();
            let flat_slot = GraphWrite::add_node(flat, data.clone());
            let fork_slot = GraphWrite::add_node(fork, data);
            assert_eq!(fork_slot, flat_slot, "add_node handed out another slot");
            "add_node"
        }
        4..=10 if !live.is_empty() => {
            let (a, b) = (live[rng.below(live.len())], live[rng.below(live.len())]);
            let data = maker.edge(rng);
            let flat_slot = GraphWrite::add_edge(flat, a, b, data.clone());
            let fork_slot = GraphWrite::add_edge(fork, a, b, data);
            assert_eq!(fork_slot, flat_slot, "add_edge handed out another slot");
            "add_edge"
        }
        11..=14 if !edges.is_empty() => {
            let e = edges[rng.below(edges.len())];
            let flat_removed = GraphWrite::remove_edge(flat, e);
            let fork_removed = GraphWrite::remove_edge(fork, e);
            assert_eq!(
                fork_removed.map(|w| w.properties),
                flat_removed.map(|w| w.properties),
                "remove_edge returned another weight"
            );
            "remove_edge"
        }
        15 | 16 if live.len() > 1 => {
            let n = live[rng.below(live.len())];
            let flat_removed = GraphWrite::remove_node(flat, n);
            let fork_removed = GraphWrite::remove_node(fork, n);
            assert_eq!(
                fork_removed, flat_removed,
                "remove_node returned another weight"
            );
            "remove_node"
        }
        17 | 18 if !edges.is_empty() => {
            let e = edges[rng.below(edges.len())];
            maker.serial += 1;
            let marker = Value::Int64(maker.serial);
            for g in [
                &mut *flat as &mut dyn FnMutWeight,
                &mut *fork as &mut dyn FnMutWeight,
            ] {
                g.set_edge_marker(e, marker.clone());
            }
            "edge_weight_mut"
        }
        19 if !live.is_empty() => {
            let n = live[rng.below(live.len())];
            maker.serial += 1;
            let title = Value::String(format!("t{}", maker.serial));
            for g in [
                &mut *flat as &mut dyn FnMutWeight,
                &mut *fork as &mut dyn FnMutWeight,
            ] {
                g.set_node_title_direct(n, title.clone());
            }
            "node_weight_mut"
        }
        _ => "noop",
    }
}

/// `edge_weight_mut` / `node_weight_mut` through the trait, object-safe.
trait FnMutWeight {
    fn set_edge_marker(&mut self, e: EdgeIndex, marker: Value);
    fn set_node_title_direct(&mut self, n: NodeIndex, title: Value);
}

impl<G: GraphWrite> FnMutWeight for G {
    fn set_edge_marker(&mut self, e: EdgeIndex, marker: Value) {
        if let Some(w) = self.edge_weight_mut(e) {
            if let Some(slot) = w.properties.first_mut() {
                slot.1 = marker;
            }
        }
    }

    fn set_node_title_direct(&mut self, n: NodeIndex, title: Value) {
        if let Some(w) = self.node_weight_mut(n) {
            w.title = title;
        }
    }
}

/// The fold's result must be the flat graph, free lists included.
fn assert_folded_like_flat(folded: &mut MemoryGraph, flat: &mut MemoryGraph, label: &str) {
    assert_same(
        &dump(folded),
        &dump(flat),
        format!("{label}: folded graph reads differently"),
    );
    assert_eq!(
        format!("{:?}", folded.slot_mirror),
        format!("{:?}", flat.slot_mirror),
        "{label}: the folded free lists differ from the flat graph's"
    );
    // The mirror could be wrong on both sides; the probe asks petgraph.
    let mut maker = Maker {
        interner: StringInterner::new(),
        serial: 1_000_000,
    };
    let mut rng = Rng::new(7);
    let nodes: Vec<NodeIndex> = (0..4)
        .map(|_| {
            let data = maker.node();
            let flat_slot = GraphWrite::add_node(flat, data.clone());
            let folded_slot = GraphWrite::add_node(folded, data);
            assert_eq!(folded_slot, flat_slot, "{label}: probe node slot");
            flat_slot
        })
        .collect();
    for i in 0..4 {
        let data = maker.edge(&mut rng);
        let a = nodes[i];
        let b = nodes[(i + 1) % 4];
        let flat_slot = GraphWrite::add_edge(flat, a, b, data.clone());
        let folded_slot = GraphWrite::add_edge(folded, a, b, data);
        assert_eq!(folded_slot, flat_slot, "{label}: probe edge slot");
    }
}

fn run_case(case: u64, steps: usize) -> (usize, usize) {
    let mut rng = Rng::new(case);
    let mut maker = Maker {
        interner: StringInterner::new(),
        serial: 0,
    };
    let base = holey_base(&mut rng, &mut maker);
    let mut flat = base.deep_clone();
    let reader = case % 2 == 0;
    let shared = Arc::new(base);
    let reader_dump = dump(&*shared);
    let held = reader.then(|| Arc::clone(&shared));
    let mut fork = ForkedGraph::new(shared);
    assert_same(
        &dump(&fork),
        &dump(&flat),
        format!("case {case}: a fresh fork"),
    );

    let mut removals = 0;
    let mut trace = Vec::new();
    for i in 0..steps {
        let label = step(&mut rng, &mut maker, &mut fork, &mut flat);
        trace.push(label);
        if label.starts_with("remove") {
            removals += 1;
        }
        if i % 9 == 8 || i + 1 == steps {
            assert_same(
                &dump(&fork),
                &dump(&flat),
                format!("case {case} step {i}: the fork reads differently after {trace:?}"),
            );
        }
    }
    if let Some(held) = &held {
        assert_same(
            &dump(&**held),
            &reader_dump,
            format!("case {case}: the reader's base moved"),
        );
    }

    // Both ways out of a fork: a copy folded into a deep clone while the
    // reader is held, and the in-place fold once it has gone.
    let mut copied = fork
        .to_memory_graph()
        .unwrap_or_else(|e| panic!("case {case}: to_memory_graph: {e}"));
    assert_folded_like_flat(
        &mut copied,
        &mut flat.deep_clone(),
        &format!("case {case} copy"),
    );
    if held.is_some() {
        assert!(
            fork.fold_in_place().is_none(),
            "case {case}: folded under a held reader"
        );
        drop(held);
    }
    let mut folded = fork
        .fold_in_place()
        .unwrap_or_else(|| panic!("case {case}: the in-place fold was refused after {trace:?}"));
    assert_folded_like_flat(&mut folded, &mut flat, &format!("case {case} fold"));
    (steps, removals)
}

#[test]
fn random_topology_operations_match_a_graph_that_never_forks() {
    let (mut ops, mut removals) = (0, 0);
    for case in 0..12_000 {
        let (steps, removed) = run_case(case, 20 + (case as usize % 50));
        ops += steps;
        removals += removed;
    }
    assert!(removals > ops / 10, "the generator rarely removes: vacuous");
    eprintln!("forked model test: 12000 cases, {ops} operations, {removals} removals");
}
