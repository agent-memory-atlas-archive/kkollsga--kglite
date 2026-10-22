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
//! Every case runs over a `Memory` base and over a `Mapped` one (the overlay's
//! slot arithmetic is shared, but the fold target differs), and over a `Frozen`
//! base, whose copy-on-write overlay never folds back: the oracle there is the
//! heap graph a promotion builds, and the collapse is the only way out.
//!
//! The generator is a fixed-seed xorshift, so a failure names its case and
//! replays exactly.

use std::collections::HashMap;
use std::sync::Arc;

use petgraph::graph::{EdgeIndex, NodeIndex};
use petgraph::Direction;

use crate::datatypes::Value;
use crate::graph::schema::{EdgeData, InternedKey, NodeData};
use crate::graph::storage::fork_base::{FoldTarget, Folded, ForkBase};
use crate::graph::storage::forked::ForkedGraph;
use crate::graph::storage::frozen::FrozenGraph;
use crate::graph::storage::interner::StringInterner;
use crate::graph::storage::property_storage::{ColumnarRow, PropertyStorage};
use crate::graph::storage::{GraphRead, GraphWrite, MappedGraph, MemoryGraph};

/// What the model needs from a base beyond [`FoldTarget`]: construction, a
/// deep copy, and the way back out of a [`Folded`].
trait Base: FoldTarget + GraphRead + GraphWrite + 'static {
    fn fresh() -> Self;
    fn deep(&self) -> Self;
    fn share(base: Arc<Self>) -> ForkBase;
    fn unfold(folded: Folded) -> Self;
    fn mirror_text(&self) -> String;
}

impl Base for MemoryGraph {
    fn fresh() -> Self {
        MemoryGraph::new()
    }
    fn deep(&self) -> Self {
        self.deep_clone()
    }
    fn share(base: Arc<Self>) -> ForkBase {
        ForkBase::Memory(base)
    }
    fn unfold(folded: Folded) -> Self {
        match folded {
            Folded::Memory(g) => g,
            Folded::Mapped(_) => panic!("a memory base folded into a mapped graph"),
        }
    }
    fn mirror_text(&self) -> String {
        format!("{:?}", self.slot_mirror)
    }
}

impl Base for MappedGraph {
    fn fresh() -> Self {
        MappedGraph::new()
    }
    fn deep(&self) -> Self {
        self.deep_clone()
    }
    fn share(base: Arc<Self>) -> ForkBase {
        ForkBase::Mapped(base)
    }
    fn unfold(folded: Folded) -> Self {
        match folded {
            Folded::Mapped(g) => g,
            Folded::Memory(_) => panic!("a mapped base folded into a memory graph"),
        }
    }
    fn mirror_text(&self) -> String {
        format!("{:?}", self.slot_mirror)
    }
}

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
    /// Nodes are column-backed records, the only shape a frozen base holds.
    columnar: bool,
    /// Every edge has one connection type, so a frozen base lists a node's
    /// edges in the order a heap graph does and the comparison can be exact.
    single_type: bool,
}

impl Maker {
    fn new() -> Self {
        Maker {
            interner: StringInterner::new(),
            serial: 0,
            columnar: false,
            single_type: false,
        }
    }

    fn node(&mut self) -> NodeData {
        self.serial += 1;
        if self.columnar {
            return NodeData {
                node_type: InternedKey::from_str("Item"),
                ident: None,
                properties: PropertyStorage::Columnar(ColumnarRow::new(self.serial as u32)),
            };
        }
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
        let kind = ["R", "S"][if self.single_type { 0 } else { rng.below(2) }];
        let mut props = HashMap::new();
        props.insert("w".to_string(), Value::Int64(self.serial));
        EdgeData::new(kind.to_string(), props, &mut self.interner)
    }
}

/// Everything a read can observe, as text: equal text means equal order too.
fn dump<G: GraphRead>(g: &G) -> String {
    dump_with(g, false)
}

/// [`dump`], with each node's edge and neighbour lists put in slot order when
/// `sorted`: a frozen base groups a node's edges by connection type, so only
/// the set a node holds is comparable with a heap graph's.
fn dump_with<G: GraphRead>(g: &G, sorted: bool) -> String {
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
            let mut edges: Vec<_> = g
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
            if sorted {
                edges.sort_by_key(|e| e.0.index());
            }
            let _ = writeln!(out, " {dir:?} {edges:?}");
            let mut peers: Vec<_> = g.neighbors_directed(n, dir).collect();
            if sorted {
                peers.sort();
            }
            let _ = writeln!(out, " peers {dir:?} {peers:?}");
        }
        let mut both: Vec<_> = g.neighbors_undirected(n).collect();
        let mut default: Vec<_> = g.edges(n).map(|e| e.id()).collect();
        if sorted {
            both.sort();
            default.sort();
        }
        let _ = writeln!(out, " undirected {both:?}");
        let _ = writeln!(out, " edges() {default:?}");
        for &m in &nodes {
            let mut between: Vec<_> = g.edges_connecting(n, m).map(|e| e.id()).collect();
            let mut found = g.find_edge(n, m);
            if sorted {
                between.sort();
                // `find_edge` returns the first of the parallel edges.
                found = between.first().copied().and(found).map(|_| between[0]);
            }
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

fn live_nodes(g: &impl GraphRead) -> Vec<NodeIndex> {
    GraphRead::node_indices(g).collect()
}

fn live_edges(g: &impl GraphRead) -> Vec<EdgeIndex> {
    GraphRead::edge_indices(g).collect()
}

/// A base with holes in both free lists, built through the write seam so its
/// slot mirror is synced.
fn holey_base<B: Base>(rng: &mut Rng, maker: &mut Maker) -> B {
    let mut g = B::fresh();
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
fn step<B: Base>(
    rng: &mut Rng,
    maker: &mut Maker,
    fork: &mut ForkedGraph,
    flat: &mut B,
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
            w.set_title(title.clone());
            // A column-backed record keeps its title in a store, so give the
            // write something the dump can see.
            if matches!(w.properties, PropertyStorage::Columnar(_)) {
                w.ident = Some(Box::new((title.clone(), title)));
            }
        }
    }
}

/// The fold's result must be the flat graph, free lists included.
fn assert_folded_like_flat<B: Base>(folded: &mut B, flat: &mut B, label: &str) {
    assert_same(
        &dump(folded),
        &dump(flat),
        format!("{label}: folded graph reads differently"),
    );
    assert_eq!(
        folded.mirror_text(),
        flat.mirror_text(),
        "{label}: the folded free lists differ from the flat graph's"
    );
    // The mirror could be wrong on both sides; the probe asks petgraph.
    let mut maker = Maker::new();
    maker.serial = 1_000_000;
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

fn run_case<B: Base>(case: u64, steps: usize) -> (usize, usize) {
    let mut rng = Rng::new(case);
    let mut maker = Maker::new();
    let base: B = holey_base(&mut rng, &mut maker);
    let mut flat = base.deep();
    let reader = case.is_multiple_of(2);
    let shared = Arc::new(base);
    let reader_dump = dump(&*shared);
    let held = reader.then(|| Arc::clone(&shared));
    let mut fork = ForkedGraph::new(B::share(shared));
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
    let mut copied = B::unfold(
        fork.to_folded_graph()
            .unwrap_or_else(|e| panic!("case {case}: to_folded_graph: {e}")),
    );
    assert_folded_like_flat(&mut copied, &mut flat.deep(), &format!("case {case} copy"));
    if held.is_some() {
        assert!(
            fork.fold_in_place().is_none(),
            "case {case}: folded under a held reader"
        );
        drop(held);
    }
    let mut folded =
        B::unfold(fork.fold_in_place().unwrap_or_else(|| {
            panic!("case {case}: the in-place fold was refused after {trace:?}")
        }));
    assert_folded_like_flat(&mut folded, &mut flat, &format!("case {case} fold"));
    (steps, removals)
}

fn model_over<B: Base>(label: &str) {
    let (mut ops, mut removals) = (0, 0);
    for case in 0..12_000 {
        let (steps, removed) = run_case::<B>(case, 20 + (case as usize % 50));
        ops += steps;
        removals += removed;
    }
    assert!(removals > ops / 10, "the generator rarely removes: vacuous");
    eprintln!("forked model test ({label}): 12000 cases, {ops} operations, {removals} removals");
}

#[test]
fn random_topology_operations_match_a_graph_that_never_forks() {
    model_over::<MemoryGraph>("memory base");
}

#[test]
fn random_topology_operations_match_a_graph_that_never_forks_over_a_mapped_base() {
    model_over::<MappedGraph>("mapped base");
}

/// Per-peer edge counts of every connection type and direction: the fork's
/// answer (the file's counts corrected by the overlay) against `reference`'s.
fn assert_peer_counts_agree<G: GraphRead>(fork: &ForkedGraph, reference: &G, label: &str) {
    for ty in ["R", "S"] {
        let conn = InternedKey::from_str(ty);
        for dir in [Direction::Outgoing, Direction::Incoming] {
            assert_eq!(
                fork.count_edges_grouped_by_peer(conn, dir, None),
                reference.count_edges_grouped_by_peer(conn, dir, None),
                "{label}: peer counts of {ty} {dir:?}"
            );
        }
    }
}

/// One case over a frozen base with one connection type: the fork's reads, slot
/// identity and collapse against `flat`, the heap graph a promotion of the same
/// base builds. With one type a frozen base lists a node's edges in the order a
/// heap graph does, so every read is compared in order and every add must land
/// on the slot the heap graph hands out.
fn run_frozen_case(case: u64, steps: usize) -> (usize, usize) {
    let mut rng = Rng::new(case ^ 0xF20_2E17);
    let mut maker = Maker::new();
    maker.columnar = true;
    maker.single_type = true;
    let heap: MemoryGraph = holey_base(&mut rng, &mut maker);
    let frozen = Arc::new(FrozenGraph::from_heap(heap.inner()));
    let mut flat = frozen.to_mapped();
    // Both free lists are what a heap load of the same vacancy pattern holds.
    assert_eq!(
        format!("{:?}", frozen.slot_mirror()),
        flat.mirror_text(),
        "case {case}: the frozen mirror differs from a heap load's"
    );
    let held = Arc::clone(&frozen);
    let held_dump = dump(&*held);
    let mut fork = ForkedGraph::new(ForkBase::Frozen(frozen));
    assert_same(
        &dump(&fork),
        &dump(&flat),
        format!("case {case}: a fresh fork"),
    );
    let (mut removals, mut trace) = (0, Vec::new());
    for i in 0..steps {
        let label = step(&mut rng, &mut maker, &mut fork, &mut flat);
        trace.push(label);
        removals += usize::from(label.starts_with("remove"));
        if i % 9 == 8 || i + 1 == steps {
            assert_same(
                &dump(&fork),
                &dump(&flat),
                format!("case {case} step {i}: the fork reads differently after {trace:?}"),
            );
            assert_peer_counts_agree(
                &fork,
                &flat,
                &format!("case {case} step {i} after {trace:?}"),
            );
        }
    }
    assert_same(
        &dump(&*held),
        &held_dump,
        format!("case {case}: the frozen base moved"),
    );
    assert!(
        fork.fold_in_place().is_none(),
        "case {case}: an overlay over a frozen base must not fold"
    );
    // The way out: a copy collapsed into a heap graph, then the overlay itself
    // (which the second collapse consumes). Each must read like the flat graph
    // and carry its free lists.
    let mut copied = MappedGraph::unfold_for_test(fork.to_folded_graph());
    assert_folded_like_flat(
        &mut copied,
        &mut flat.deep_clone(),
        &format!("case {case} copy"),
    );
    let mut collapsed = MappedGraph::unfold_for_test(fork.to_folded_graph());
    probe_alike(
        &mut fork.clone(),
        &mut collapsed,
        &format!("case {case} probe"),
    );
    let mut consumed = MappedGraph::unfold_for_test(fork.materialise());
    assert_folded_like_flat(&mut consumed, &mut flat, &format!("case {case} collapse"));
    (steps, removals)
}

/// A frozen base groups a node's edges by connection type, which a heap graph
/// cannot reproduce, so edges freed with a node leave in another order and the
/// slots later adds reuse differ from a heap graph's. With two types the
/// oracle is therefore the overlay's own collapse: the same operations, the
/// reads compared as sets, and every add must land on the slot the collapse's
/// petgraph allocates.
fn run_frozen_solo_case(case: u64, steps: usize) -> (usize, usize) {
    let mut rng = Rng::new(case ^ 0x501_0C45E);
    let mut maker = Maker::new();
    maker.columnar = true;
    let heap: MemoryGraph = holey_base(&mut rng, &mut maker);
    let frozen = Arc::new(FrozenGraph::from_heap(heap.inner()));
    let reference = frozen.to_mapped();
    assert_same(
        &dump_with(
            &ForkedGraph::new(ForkBase::Frozen(Arc::clone(&frozen))),
            true,
        ),
        &dump_with(&reference, true),
        format!("case {case}: a fresh fork"),
    );
    let mut fork = ForkedGraph::new(ForkBase::Frozen(frozen));
    let (mut removals, mut trace) = (0, Vec::new());
    for i in 0..steps {
        let label = step_solo(&mut rng, &mut maker, &mut fork);
        trace.push(label);
        removals += usize::from(label.starts_with("remove"));
        if i % 9 == 8 || i + 1 == steps {
            let collapsed = MappedGraph::unfold_for_test(fork.to_folded_graph());
            assert_same(
                &dump_with(&fork, true),
                &dump_with(&collapsed, true),
                format!("case {case} step {i}: the collapse reads differently after {trace:?}"),
            );
            assert_peer_counts_agree(
                &fork,
                &collapsed,
                &format!("case {case} step {i} after {trace:?}"),
            );
        }
    }
    let mut collapsed = MappedGraph::unfold_for_test(fork.to_folded_graph());
    probe_alike(&mut fork, &mut collapsed, &format!("case {case} probe"));
    (steps, removals)
}

/// [`step`] without an oracle: the same operation mix on the fork alone.
fn step_solo(rng: &mut Rng, maker: &mut Maker, fork: &mut ForkedGraph) -> &'static str {
    let live = live_nodes(fork);
    let edges = live_edges(fork);
    match rng.below(20) {
        0..=3 => {
            GraphWrite::add_node(fork, maker.node());
            "add_node"
        }
        4..=10 if !live.is_empty() => {
            let (a, b) = (live[rng.below(live.len())], live[rng.below(live.len())]);
            GraphWrite::add_edge(fork, a, b, maker.edge(rng));
            "add_edge"
        }
        11..=14 if !edges.is_empty() => {
            GraphWrite::remove_edge(fork, edges[rng.below(edges.len())]);
            "remove_edge"
        }
        15 | 16 if live.len() > 1 => {
            GraphWrite::remove_node(fork, live[rng.below(live.len())]);
            "remove_node"
        }
        17 | 18 if !edges.is_empty() => {
            maker.serial += 1;
            fork.set_edge_marker(edges[rng.below(edges.len())], Value::Int64(maker.serial));
            "edge_weight_mut"
        }
        19 if !live.is_empty() => {
            maker.serial += 1;
            let title = Value::String(format!("t{}", maker.serial));
            fork.set_node_title_direct(live[rng.below(live.len())], title);
            "node_weight_mut"
        }
        _ => "noop",
    }
}

/// Four nodes and four edges added to the live overlay and to its collapsed
/// copy must land on the same slots: the overlay's predictions are what the
/// collapse's petgraph allocates.
fn probe_alike(fork: &mut ForkedGraph, collapsed: &mut MappedGraph, label: &str) {
    let mut maker = Maker::new();
    maker.serial = 2_000_000;
    maker.columnar = true;
    let mut rng = Rng::new(11);
    let nodes: Vec<NodeIndex> = (0..4)
        .map(|_| {
            let data = maker.node();
            let from_collapsed = GraphWrite::add_node(collapsed, data.clone());
            assert_eq!(
                GraphWrite::add_node(fork, data),
                from_collapsed,
                "{label}: probe node"
            );
            from_collapsed
        })
        .collect();
    for i in 0..4 {
        let data = maker.edge(&mut rng);
        let (a, b) = (nodes[i], nodes[(i + 1) % 4]);
        let from_collapsed = GraphWrite::add_edge(collapsed, a, b, data.clone());
        assert_eq!(
            GraphWrite::add_edge(fork, a, b, data),
            from_collapsed,
            "{label}: probe edge"
        );
    }
}

impl MappedGraph {
    fn unfold_for_test(folded: Result<Folded, String>) -> MappedGraph {
        match folded.unwrap_or_else(|e| panic!("collapse refused: {e}")) {
            Folded::Mapped(g) => g,
            Folded::Memory(_) => panic!("a frozen base collapsed into a memory graph"),
        }
    }
}

fn frozen_model(label: &str, single_type: bool) {
    let (mut ops, mut removals) = (0, 0);
    for case in 0..6_000 {
        let steps = 20 + (case as usize % 50);
        let (steps, removed) = if single_type {
            run_frozen_case(case, steps)
        } else {
            run_frozen_solo_case(case, steps)
        };
        ops += steps;
        removals += removed;
    }
    assert!(removals > ops / 10, "the generator rarely removes: vacuous");
    eprintln!("forked model test ({label}): 6000 cases, {ops} operations, {removals} removals");
}

#[test]
fn random_topology_operations_over_a_frozen_base_match_a_heap_load_in_order() {
    frozen_model("frozen base, one connection type", true);
}

#[test]
fn random_topology_operations_over_a_frozen_base_collapse_to_what_they_read() {
    frozen_model("frozen base, two connection types", false);
}

/// A cheaper dump for cases with thousands of slots: counts, bounds, the node
/// and edge scans, every live edge's endpoints and weight, and each node's
/// outgoing and incoming edges, in order. Misses nothing [`dump`] sees except
/// the all-pairs `edges_connecting` probe, which is quadratic.
fn dump_light<G: GraphRead>(g: &G) -> String {
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
    for n in g.node_indices() {
        let _ = writeln!(out, "node {n:?} {:?}", g.node_weight(n));
        for dir in [Direction::Outgoing, Direction::Incoming] {
            let edges: Vec<_> = g.edges_directed(n, dir).map(|e| e.id()).collect();
            let _ = writeln!(out, " {dir:?} {edges:?}");
        }
    }
    for e in g.edge_references() {
        let _ = writeln!(
            out,
            "edge {:?} {:?} {:?} {:?}",
            e.id(),
            e.source(),
            e.target(),
            e.weight().properties
        );
    }
    out
}

/// One transaction-shaped case over a frozen base: random writes interleaved
/// with `begin` (the snapshot a transaction forks from), `commit` (keep what
/// was written) and `rollback` (return to the snapshot), nested to any depth.
/// `bulk` writes go in first, so every transaction forks an overlay that
/// already spans several pages of each shared map. The oracle is a flat heap
/// graph put through the same sequence, and every snapshot must read exactly
/// as it did when it was taken, however much was written after.
fn run_frozen_tx_case(case: u64, steps: usize, bulk: usize) -> usize {
    let mut rng = Rng::new(case ^ 0x7A5_0C0DE);
    let mut maker = Maker::new();
    maker.columnar = true;
    maker.single_type = true;
    let heap: MemoryGraph = holey_base(&mut rng, &mut maker);
    let frozen = Arc::new(FrozenGraph::from_heap(heap.inner()));
    let mut flat = frozen.to_mapped();
    let mut fork = ForkedGraph::new(ForkBase::Frozen(frozen));
    let light = bulk > 0;
    let read = |fork: &ForkedGraph, flat: &MappedGraph| {
        if light {
            (dump_light(fork), dump_light(flat))
        } else {
            (dump(fork), dump(flat))
        }
    };
    for _ in 0..bulk {
        step(&mut rng, &mut maker, &mut fork, &mut flat);
    }
    let (left, right) = read(&fork, &flat);
    assert_same(&left, &right, format!("case {case}: after the bulk writes"));
    let mut stack: Vec<(ForkedGraph, MappedGraph, String)> = Vec::new();
    let (mut began, mut rolled_back, mut trace) = (0, 0, Vec::new());
    for i in 0..steps {
        match rng.below(10) {
            0 if stack.len() < 4 => {
                let (as_dumped, _) = read(&fork, &flat);
                stack.push((fork.clone(), flat.deep_clone(), as_dumped));
                began += 1;
                trace.push("begin");
            }
            1 if !stack.is_empty() => {
                stack.pop();
                trace.push("commit");
            }
            2 if !stack.is_empty() => {
                let (snapshot, snapshot_flat, as_dumped) = stack.pop().unwrap();
                let (restored, _) = read(&snapshot, &snapshot_flat);
                assert_same(
                    &restored,
                    &as_dumped,
                    format!("case {case} step {i}: a snapshot moved after {trace:?}"),
                );
                fork = snapshot;
                flat = snapshot_flat;
                rolled_back += 1;
                trace.push("rollback");
            }
            _ => trace.push(step(&mut rng, &mut maker, &mut fork, &mut flat)),
        }
        if i % 7 == 6 {
            let (left, right) = read(&fork, &flat);
            assert_same(
                &left,
                &right,
                format!("case {case} step {i}: the fork reads differently after {trace:?}"),
            );
        }
    }
    let (left, right) = read(&fork, &flat);
    assert_same(
        &left,
        &right,
        format!("case {case}: at the end of {trace:?}"),
    );
    // Every snapshot still open reads as it did, and a collapse of the final
    // state carries the flat graph's free lists.
    for (snapshot, snapshot_flat, as_dumped) in &stack {
        let (restored, _) = read(snapshot, snapshot_flat);
        assert_same(
            &restored,
            as_dumped,
            format!("case {case}: an open snapshot moved"),
        );
    }
    let mut collapsed = MappedGraph::unfold_for_test(fork.to_folded_graph());
    assert_folded_like_flat(&mut collapsed, &mut flat, &format!("case {case} collapse"));
    began * 1000 + rolled_back
}

#[test]
fn transactions_over_a_frozen_base_match_a_heap_graph_through_commit_and_rollback() {
    let (mut began, mut rolled_back) = (0, 0);
    for case in 0..3_000 {
        let tally = run_frozen_tx_case(case, 30 + (case as usize % 50), 0);
        began += tally / 1000;
        rolled_back += tally % 1000;
    }
    assert!(
        began > 1_000 && rolled_back > 300,
        "the generator barely begins or rolls back: vacuous"
    );
}

#[test]
fn transactions_after_thousands_of_writes_match_a_heap_graph() {
    for case in 0..12 {
        run_frozen_tx_case(case, 60, 2_500 + 400 * case as usize);
    }
}
