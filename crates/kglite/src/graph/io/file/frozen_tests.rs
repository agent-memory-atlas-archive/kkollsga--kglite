//! A mapped load that serves the topology from the file's raw sections: what
//! it answers, what it keeps from a heap load (slot identity, free-list order,
//! rollback), when it falls back, and what a damaged section does.

use super::index_sections::with_floors;
use super::index_sections_tests::{load, load_owned, opts, rows, run, save};
use super::mappable::SaveLayout;
use super::*;
use crate::graph::schema::{EdgeData, GraphBackend, InternedKey, NodeData};
use crate::graph::session::execute_mut;
use crate::graph::storage::GraphRead;
use petgraph::graph::{EdgeIndex, NodeIndex};
use petgraph::visit::EdgeRef;
use petgraph::Direction::{Incoming, Outgoing};

/// Types `A`, `B`, `C` and a hub `H`; parallel edges, self-loops, five
/// connection types (one with properties), a deleted node and edge in the
/// middle and a node created and deleted at the very end, so that both node
/// and edge arrays hold interior and trailing vacant slots.
pub(super) fn topo_graph() -> DirGraph {
    let mut g = DirGraph::new();
    for (t, n) in [("A", 60), ("B", 40), ("C", 30)] {
        run(
            &mut g,
            &format!(
                "UNWIND range(1, {n}) AS i CREATE (:{t} {{id: i, n: i, s: '{t}-' + toString(i)}})"
            ),
        );
    }
    run(&mut g, "CREATE (:H {id: 1, n: 0})");
    for _ in 0..2 {
        run(
            &mut g,
            "UNWIND range(1, 60) AS i MATCH (a:A {id: i}), (b:B {id: i % 40 + 1}) CREATE (a)-[:K1]->(b)",
        );
    }
    run(
        &mut g,
        "UNWIND range(1, 60) AS i MATCH (a:A {id: i}), (c:C {id: i % 30 + 1}) CREATE (a)-[:K2]->(c)",
    );
    run(
        &mut g,
        "MATCH (b:B) WHERE b.id % 7 = 0 CREATE (b)-[:K1]->(b)",
    );
    run(
        &mut g,
        "UNWIND range(1, 40) AS i MATCH (b:B {id: i}), (c:C {id: i % 30 + 1}) \
         CREATE (b)-[:K3 {w: i, tag: 'x' + toString(i)}]->(c)",
    );
    run(
        &mut g,
        "UNWIND range(1, 40) AS i MATCH (b:B {id: i}), (b2:B {id: (i * 3) % 40 + 1}) CREATE (b)-[:K2]->(b2)",
    );
    for (k, label, n) in [
        ("K1", "A", 40),
        ("K2", "B", 40),
        ("K3", "C", 30),
        ("K4", "A", 25),
        ("K5", "B", 40),
    ] {
        run(
            &mut g,
            &format!(
                "UNWIND range(1, {n}) AS i MATCH (h:H {{id: 1}}), (x:{label} {{id: i}}) CREATE (h)-[:{k}]->(x)"
            ),
        );
    }
    run(&mut g, "MATCH (a:A {id: 7}) DETACH DELETE a");
    run(&mut g, "MATCH (c:C {id: 3}) DETACH DELETE c");
    run(&mut g, "MATCH (b:B {id: 7})-[r:K1]->(b) DELETE r");
    run(&mut g, "CREATE (:C {id: 900, n: 900})");
    run(
        &mut g,
        "MATCH (c:C {id: 900}), (a:A {id: 1}) CREATE (a)-[:K4]->(c), (c)-[:K1]->(c)",
    );
    run(&mut g, "MATCH (c:C {id: 900}) DETACH DELETE c");
    g
}

/// `topo_graph` plus enough edge-free nodes that the file passes the size
/// below which a load reads it into a buffer instead of mapping it.
pub(super) fn saved_topo_big() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("big.kgl");
    let mut g = topo_graph();
    run(
        &mut g,
        "UNWIND range(1, 3000) AS i CREATE (:F {id: i, n: i})",
    );
    with_floors(10, 10, || save(g, &path, SaveLayout::Auto));
    assert!(std::fs::metadata(&path).unwrap().len() > 65_536 * 2);
    (tmp, path)
}

pub(super) fn saved_topo() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("g.kgl");
    with_floors(10, 10, || save(topo_graph(), &path, SaveLayout::Auto));
    (tmp, path)
}

pub(super) fn section_names(path: &std::path::Path) -> Vec<String> {
    let bytes = std::fs::read(path).unwrap();
    let metadata = metadata_head::read_metadata_head(&bytes, "test").unwrap();
    metadata.mappable.iter().map(|e| e.name.clone()).collect()
}

fn canon_props(graph: &DirGraph, props: &[(InternedKey, Value)]) -> String {
    let mut pairs: Vec<String> = props
        .iter()
        .map(|(k, v)| format!("{}={v:?}", graph.interner.resolve(*k)))
        .collect();
    pairs.sort();
    pairs.join(",")
}

fn canon_edge(
    graph: &DirGraph,
    source: NodeIndex,
    target: NodeIndex,
    id: EdgeIndex,
    conn: InternedKey,
    props: &[(InternedKey, Value)],
) -> String {
    format!(
        "{}>{}#{}:{}[{}]",
        source.index(),
        target.index(),
        id.index(),
        graph.interner.resolve(conn),
        canon_props(graph, props)
    )
}

fn edges_of(graph: &DirGraph, node: NodeIndex, dir: petgraph::Direction) -> Vec<String> {
    let mut out: Vec<String> = graph
        .graph
        .edges_directed(node, dir)
        .map(|e| {
            canon_edge(
                graph,
                e.source(),
                e.target(),
                e.id(),
                e.connection_type(),
                &e.weight().properties,
            )
        })
        .collect();
    out.sort();
    out
}

fn all_edges(graph: &DirGraph) -> Vec<String> {
    let mut out: Vec<String> = graph
        .graph
        .edge_references()
        .map(|e| {
            canon_edge(
                graph,
                e.source(),
                e.target(),
                e.id(),
                e.connection_type(),
                &e.weight().properties,
            )
        })
        .collect();
    out.sort();
    out
}

fn node_dump(graph: &DirGraph) -> Vec<String> {
    let mut out = Vec::new();
    for idx in graph.graph.node_indices() {
        let node = graph.graph.node_view(idx).unwrap();
        let mut props: Vec<String> = node
            .properties_cloned(&graph.interner)
            .iter()
            .map(|(k, v)| format!("{k}={v:?}"))
            .collect();
        props.sort();
        out.push(format!(
            "{}:{}:{:?}:{:?}:{}",
            idx.index(),
            node.node_type_str(&graph.interner),
            node.id(),
            node.title(),
            props.join(",")
        ));
    }
    out
}

pub(super) fn dump(graph: &DirGraph) -> (Vec<String>, Vec<String>) {
    (node_dump(graph), all_edges(graph))
}

/// Everything a reader can ask the backend of `frozen`, compared with the same
/// question asked of `reference` (a heap graph over the same file).
pub(super) fn assert_reads_match(frozen: &DirGraph, reference: &DirGraph, label: &str) {
    let (f, r) = (&frozen.graph, &reference.graph);
    assert_eq!(f.node_count(), r.node_count(), "{label} node_count");
    assert_eq!(f.edge_count(), r.edge_count(), "{label} edge_count");
    assert_eq!(f.node_bound(), r.node_bound(), "{label} node_bound");
    assert_eq!(f.edge_bound(), r.edge_bound(), "{label} edge_bound");
    assert_eq!(
        f.node_indices().collect::<Vec<_>>(),
        r.node_indices().collect::<Vec<_>>(),
        "{label} node_indices"
    );
    assert_eq!(
        f.edge_indices().collect::<Vec<_>>(),
        r.edge_indices().collect::<Vec<_>>(),
        "{label} edge_indices"
    );
    assert_eq!(
        all_edges(frozen),
        all_edges(reference),
        "{label} edge_references"
    );
    assert_eq!(node_dump(frozen), node_dump(reference), "{label} nodes");

    let conns: Vec<InternedKey> = ["K1", "K2", "K3", "K4", "K5", "NOPE"]
        .iter()
        .map(|c| InternedKey::from_str(c))
        .collect();
    let node_types: Vec<InternedKey> = ["A", "B", "C", "H"]
        .iter()
        .map(|c| InternedKey::from_str(c))
        .collect();
    let bound = r.node_bound() + 3;
    for slot in 0..bound {
        assert_slot_reads_match(frozen, reference, label, slot, bound, &conns, &node_types);
    }
    assert_edge_slots_match(frozen, reference, label);
    assert_conn_aggregates_match(frozen, reference, label, &conns);
    assert_edge_weights_match(frozen, reference, label);
}

fn assert_slot_reads_match(
    frozen: &DirGraph,
    reference: &DirGraph,
    label: &str,
    slot: usize,
    bound: usize,
    conns: &[InternedKey],
    node_types: &[InternedKey],
) {
    let (f, r) = (&frozen.graph, &reference.graph);
    let n = NodeIndex::new(slot);
    assert_eq!(f.node_type_of(n), r.node_type_of(n), "{label} type {slot}");
    assert_eq!(f.get_node_id(n), r.get_node_id(n), "{label} id {slot}");
    assert_eq!(
        f.get_node_title(n),
        r.get_node_title(n),
        "{label} title {slot}"
    );
    for dir in [Outgoing, Incoming] {
        assert_directed_reads_match(frozen, reference, label, n, dir, conns, node_types);
    }
    let mut u1: Vec<usize> = f.neighbors_undirected(n).map(|x| x.index()).collect();
    let mut u2: Vec<usize> = r.neighbors_undirected(n).map(|x| x.index()).collect();
    u1.sort_unstable();
    u2.sort_unstable();
    assert_eq!(u1, u2, "{label} undirected {slot}");
    let ends = |g: &crate::graph::schema::GraphBackend| {
        let mut v: Vec<String> = g
            .edges(n)
            .map(|e| {
                format!(
                    "{}>{}#{}",
                    e.source().index(),
                    e.target().index(),
                    e.id().index()
                )
            })
            .collect();
        v.sort();
        v
    };
    assert_eq!(ends(f), ends(r), "{label} edges() {slot}");
    for other in (0..bound).step_by(3) {
        let o = NodeIndex::new(other);
        let connecting = |g: &crate::graph::schema::GraphBackend| {
            let mut v: Vec<usize> = g.edges_connecting(n, o).map(|e| e.id().index()).collect();
            v.sort_unstable();
            v
        };
        assert_eq!(
            connecting(f),
            connecting(r),
            "{label} connecting {slot}->{other}"
        );
        assert_eq!(
            f.find_edge(n, o).is_some(),
            r.find_edge(n, o).is_some(),
            "{label} find_edge {slot}->{other}"
        );
        if let Some(found) = f.find_edge(n, o) {
            assert_eq!(
                f.edge_endpoints(found),
                Some((n, o)),
                "{label} found edge endpoints"
            );
        }
    }
}

fn assert_directed_reads_match(
    frozen: &DirGraph,
    reference: &DirGraph,
    label: &str,
    n: NodeIndex,
    dir: petgraph::Direction,
    conns: &[InternedKey],
    node_types: &[InternedKey],
) {
    let (f, r) = (&frozen.graph, &reference.graph);
    let slot = n.index();
    assert_eq!(
        edges_of(frozen, n, dir),
        edges_of(reference, n, dir),
        "{label} edges {slot} {dir:?}"
    );
    let sorted = |it: &mut dyn Iterator<Item = NodeIndex>| {
        let mut v: Vec<usize> = it.map(|x| x.index()).collect();
        v.sort_unstable();
        v
    };
    assert_eq!(
        sorted(&mut f.neighbors_directed(n, dir)),
        sorted(&mut r.neighbors_directed(n, dir)),
        "{label} neighbors {slot} {dir:?}"
    );
    for conn in conns {
        let counts = |g: &crate::graph::schema::GraphBackend, other| {
            g.count_edges_filtered(n, dir, Some(*conn), other, None)
                .unwrap()
        };
        assert_eq!(
            counts(f, None),
            counts(r, None),
            "{label} count {slot} {dir:?}"
        );
        for ty in node_types {
            assert_eq!(
                counts(f, Some(*ty)),
                counts(r, Some(*ty)),
                "{label} typed count {slot} {dir:?}"
            );
        }
        let filtered = |g: &crate::graph::schema::GraphBackend| {
            let mut v: Vec<String> = g
                .edges_directed_filtered(n, dir, Some(*conn))
                .filter(|e| e.connection_type() == *conn)
                .map(|e| {
                    format!(
                        "{}>{}#{}",
                        e.source().index(),
                        e.target().index(),
                        e.id().index()
                    )
                })
                .collect();
            v.sort();
            v
        };
        assert_eq!(filtered(f), filtered(r), "{label} filtered {slot} {dir:?}");
        let peers = |g: &crate::graph::schema::GraphBackend| {
            let mut v: Vec<(usize, usize)> = g
                .iter_peers_filtered(n, dir, Some(conn.as_u64()))
                .map(|(p, e)| (p.index(), e.index()))
                .collect();
            v.sort_unstable();
            v
        };
        assert_eq!(peers(f), peers(r), "{label} peers {slot} {dir:?}");
    }
    for other in [None, Some(node_types[0]), Some(node_types[1])] {
        assert_eq!(
            f.count_edges_filtered(n, dir, None, other, None),
            r.count_edges_filtered(n, dir, None, other, None),
            "{label} untyped count {slot} {dir:?}"
        );
        assert_eq!(
            f.count_incoming_nonself_edges_filtered(n, None, other, None),
            r.count_incoming_nonself_edges_filtered(n, None, other, None),
            "{label} nonself {slot}"
        );
    }
}

fn assert_edge_slots_match(frozen: &DirGraph, reference: &DirGraph, label: &str) {
    let (f, r) = (&frozen.graph, &reference.graph);
    for slot in 0..r.edge_bound() + 3 {
        let e = EdgeIndex::new(slot);
        assert_eq!(
            f.edge_endpoints(e),
            r.edge_endpoints(e),
            "{label} endpoints {slot}"
        );
        let weight = |g: &crate::graph::schema::GraphBackend| {
            g.edge_weight(e)
                .map(|w| (w.connection_type, canon_props(reference, &w.properties)))
        };
        assert_eq!(weight(f), weight(r), "{label} weight {slot}");
        assert_eq!(
            f.edge_property_list(e).map(|p| canon_props(reference, &p)),
            r.edge_property_list(e).map(|p| canon_props(reference, &p)),
            "{label} property list {slot}"
        );
    }
    let keys = |g: &crate::graph::schema::GraphBackend| {
        let mut v: Vec<(usize, usize, u64)> = g
            .edge_endpoint_keys()
            .map(|(s, t, c)| (s.index(), t.index(), c.as_u64()))
            .collect();
        v.sort_unstable();
        v
    };
    assert_eq!(keys(f), keys(r), "{label} endpoint keys");
    let mut each_f = Vec::new();
    f.for_each_edge_endpoint_key(|s, t, c| each_f.push((s.index(), t.index(), c.as_u64())));
    each_f.sort_unstable();
    assert_eq!(each_f, keys(r), "{label} for_each_edge_endpoint_key");
}

fn assert_conn_aggregates_match(
    frozen: &DirGraph,
    reference: &DirGraph,
    label: &str,
    conns: &[InternedKey],
) {
    let (f, r) = (&frozen.graph, &reference.graph);
    for conn in conns {
        let of_type = |g: &crate::graph::schema::GraphBackend| {
            let mut v = Vec::new();
            g.for_each_edge_of_conn_type(*conn, |s, t, id, props| {
                v.push(format!(
                    "{}>{}#{id}[{}]",
                    s.index(),
                    t.index(),
                    canon_props(reference, props)
                ));
                true
            });
            v.sort();
            v
        };
        assert_eq!(of_type(f), of_type(r), "{label} for_each_edge_of_conn_type");
        for dir in [Outgoing, Incoming] {
            assert_eq!(
                f.count_edges_grouped_by_peer(*conn, dir, None),
                r.count_edges_grouped_by_peer(*conn, dir, None),
                "{label} grouped {dir:?}"
            );
        }
        // `None` is the answer of a graph that keeps no record (an overlay
        // that wrote an edge): the caller scans, which the groups above cover.
        let looked_up = f.lookup_peer_counts(*conn);
        assert!(
            looked_up.is_none() || looked_up == r.lookup_peer_counts(*conn),
            "{label} lookup_peer_counts"
        );
    }
}

fn assert_edge_weights_match(frozen: &DirGraph, reference: &DirGraph, label: &str) {
    let (f, r) = (&frozen.graph, &reference.graph);
    let mut w_f: Vec<String> = f
        .edge_weights()
        .map(|w| {
            format!(
                "{}[{}]",
                reference.interner.resolve(w.connection_type),
                canon_props(reference, &w.properties)
            )
        })
        .collect();
    let mut w_r: Vec<String> = r
        .edge_weights()
        .map(|w| {
            format!(
                "{}[{}]",
                reference.interner.resolve(w.connection_type),
                canon_props(reference, &w.properties)
            )
        })
        .collect();
    w_f.sort();
    w_r.sort();
    assert_eq!(w_f, w_r, "{label} edge_weights");
}

fn assert_frozen(graph: &DirGraph, expected: bool) {
    assert_eq!(graph.graph.is_frozen(), expected, "frozen = {expected}");
    assert!(
        !expected || graph.graph.is_mapped(),
        "a frozen graph is a mapped one"
    );
}

#[test]
fn a_mapped_load_of_a_recorded_topology_is_frozen_and_a_memory_load_is_not() {
    let (_tmp, path) = saved_topo();
    let names = section_names(&path);
    for expected in [
        "topo:meta",
        "topo:nodes",
        "topo:alive",
        "topo:out_off",
        "topo:in_off",
        "topo:out_edges",
        "topo:in_edges",
        "topo:out_ty",
        "topo:in_ty",
        "topo:edges",
        "topo:type_mask",
        "topo:edge_props",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "{expected} in {names:?}"
        );
    }
    assert_frozen(&load(&path, StorageMode::Mapped), true);
    assert_frozen(&load(&path, StorageMode::Memory), false);
    assert_frozen(
        &load_file_with(path.to_str().unwrap(), &LoadOptions::new()).unwrap(),
        false,
    );
}

#[test]
fn a_frozen_graph_answers_every_read_like_a_memory_load() {
    let (_tmp, path) = saved_topo();
    let frozen = load_owned(&path, StorageMode::Mapped);
    let memory = load_owned(&path, StorageMode::Memory);
    assert_frozen(&frozen, true);
    assert!(
        frozen.graph.node_bound() > frozen.graph.node_count()
            && frozen.graph.edge_bound() > frozen.graph.edge_count(),
        "the fixture must carry vacant node and edge slots"
    );
    assert_reads_match(&frozen, &memory, "frozen vs memory");
    assert_frozen(&frozen, true);
}

#[test]
fn queries_answer_the_same_and_never_promote() {
    let (_tmp, path) = saved_topo();
    let frozen = load(&path, StorageMode::Mapped);
    let memory = load(&path, StorageMode::Memory);
    for q in [
        "MATCH (a:A)-[:K1]->(b:B) RETURN b.id AS i, count(a) AS c ORDER BY c DESC, i LIMIT 9",
        "MATCH (h:H)-[r]->(x) RETURN type(r) AS t, count(x) AS c ORDER BY t",
        "MATCH (b:B)-[r:K3]->(c:C) RETURN sum(r.w), count(r), min(r.tag)",
        "MATCH (b:B)-[:K1]->(b) RETURN b.id ORDER BY b.id",
        "MATCH (a:A {id: 1})-[:K1|K2]->(x)-[:K2|K3]->(y) RETURN count(y)",
        "MATCH (c:C {id: 4})<-[r]-(x) RETURN type(r) AS t, count(x) AS c ORDER BY t",
        "MATCH (a:A)-[r]->(b) RETURN count(r), count(DISTINCT b)",
        "MATCH p = (a:A {id: 2})-[:K1*1..2]->(z) RETURN count(p)",
        "MATCH (n) RETURN labels(n)[0] AS l, count(n) AS c ORDER BY l",
        "MATCH (a:A) WHERE a.id > 55 RETURN a.s ORDER BY a.s",
    ] {
        assert_eq!(rows(&frozen, q), rows(&memory, q), "{q}");
    }
    assert_frozen(&frozen, true);
}

#[test]
fn promotion_rebuilds_the_graph_a_memory_load_holds() {
    let (_tmp, path) = saved_topo();
    let frozen = load_owned(&path, StorageMode::Mapped);
    let memory = load_owned(&path, StorageMode::Memory);
    let crate::graph::schema::GraphBackend::Frozen(backend) = &frozen.graph else {
        panic!("frozen");
    };
    let mut promoted = backend.to_mapped();
    let reference = memory.graph.as_stable_digraph();
    let rebuilt = promoted.inner();
    for node in reference.node_indices() {
        assert_eq!(rebuilt.node_weight(node), reference.node_weight(node));
        for dir in [Outgoing, Incoming] {
            let order = |g: &petgraph::stable_graph::StableDiGraph<_, _>| {
                g.edges_directed(node, dir)
                    .map(|e| e.id().index())
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                order(rebuilt),
                order(reference),
                "adjacency order {node:?} {dir:?}"
            );
        }
    }
    use petgraph::visit::{EdgeIndexable, NodeIndexable};
    assert_eq!(rebuilt.node_bound(), reference.node_bound());
    assert_eq!(rebuilt.edge_bound(), reference.edge_bound());
    // The free lists must hand out the same slots, in the same order, as the
    // heap graph a memory load decoded.
    let mut copy = reference.clone();
    let mut mirror_source = (*memory.graph.as_stable_digraph()).clone();
    let _ = &mut mirror_source;
    let filler = |g: &mut petgraph::stable_graph::StableDiGraph<NodeData, EdgeData>| {
        let mut out = Vec::new();
        for i in 0..8 {
            let node = g.add_node(NodeData::new_preinterned(
                Value::Int64(i),
                Value::Null,
                InternedKey::from_str("A"),
                vec![],
            ));
            out.push(node.index());
        }
        for i in 0..8usize {
            let edge = g.add_edge(
                NodeIndex::new(i),
                NodeIndex::new(i + 1),
                EdgeData::new_interned(InternedKey::from_str("K1"), Vec::new()),
            );
            out.push(1_000_000 + edge.index());
        }
        out
    };
    let mut rebuilt_graph = promoted.inner_mut().clone();
    assert_eq!(
        filler(&mut rebuilt_graph),
        filler(&mut copy),
        "slot reuse order"
    );
}

#[test]
fn writes_after_a_frozen_load_match_writes_after_a_memory_load() {
    let (_tmp, path) = saved_topo();
    let mut frozen = load_owned(&path, StorageMode::Mapped);
    let mut memory = load_owned(&path, StorageMode::Memory);
    assert_frozen(&frozen, true);
    let steps = [
        "CREATE (:A {id: 1000, n: 1, s: 'new'}), (:C {id: 1001, n: 2, s: 'new'})",
        "MATCH (a:A {id: 1000}), (b:B {id: 3}) CREATE (a)-[:K1]->(b), (a)-[:K9 {w: 1}]->(b)",
        "MATCH (b:B {id: 14}) DETACH DELETE b",
        "CREATE (:B {id: 2000, n: 3, s: 'again'})",
        "MATCH (a:A {id: 5})-[r:K2]->() DELETE r",
        "MATCH (a:A {id: 5}), (c:C {id: 5}) CREATE (a)-[:K2 {why: 'back'}]->(c)",
        "MATCH (h:H) SET h.n = 5",
        "UNWIND range(1, 6) AS i CREATE (:C {id: 3000 + i, n: i, s: 'bulk'})",
    ];
    for (i, step) in steps.iter().enumerate() {
        run(&mut frozen, step);
        run(&mut memory, step);
        if i == 0 {
            assert_frozen(&frozen, false);
            assert!(frozen.graph.is_mapped(), "promotion keeps the storage mode");
        }
        assert_eq!(dump(&frozen), dump(&memory), "after: {step}");
        assert_eq!(
            frozen.graph.node_bound(),
            memory.graph.node_bound(),
            "slots after: {step}"
        );
        assert_eq!(frozen.graph.edge_bound(), memory.graph.edge_bound());
    }
}

#[test]
fn a_failed_statement_rolls_back_through_promotion() {
    let (_tmp, path) = saved_topo();
    let mut frozen = load_owned(&path, StorageMode::Mapped);
    let before = dump(&frozen);
    let failed = execute_mut(
        &mut frozen,
        "CREATE (:A {id: 4000, n: 1}), (:A {id: 4001, bad: duration({months: 2147483648})})",
        &opts(),
    );
    assert!(failed.is_err(), "the statement must fail so it rolls back");
    assert_eq!(dump(&frozen), before, "rollback restores every slot");
    assert_eq!(rows(&frozen, "MATCH (a:A {id: 4000}) RETURN a.n"), "[]");

    let failed = execute_mut(
        &mut frozen,
        "MATCH (a:A {id: 2}) DETACH DELETE a CREATE (:A {id: 4002, bad: duration({months: 2147483648})})",
        &opts(),
    );
    assert!(failed.is_err());
    assert_eq!(
        dump(&frozen),
        before,
        "a rolled-back delete restores its edges on their slots"
    );
    assert!(frozen.graph.is_mapped());
}

#[test]
fn a_clone_keeps_the_frozen_topology_while_the_writer_promotes() {
    let (_tmp, path) = saved_topo();
    let reader = load(&path, StorageMode::Mapped);
    let before = dump(&reader);
    let mut writer = (*reader).clone();
    assert_frozen(&writer, true);
    run(&mut writer, "MATCH (a:A {id: 2}) DETACH DELETE a");
    assert_frozen(&writer, false);
    assert_frozen(&reader, true);
    assert_eq!(dump(&reader), before, "the reader's snapshot is untouched");
    assert_ne!(dump(&writer), before);
}

#[test]
fn saving_a_frozen_graph_keeps_it_frozen_and_reproduces_every_section() {
    let (tmp, path) = saved_topo();
    let mut frozen = load(&path, StorageMode::Mapped);
    let again = tmp.path().join("again.kgl");
    with_floors(10, 10, || {
        save_graph_with_layout(&mut frozen, again.to_str().unwrap(), true, SaveLayout::Auto)
    })
    .unwrap();
    assert_frozen(&frozen, true);
    let reloaded = load(&again, StorageMode::Mapped);
    assert_frozen(&reloaded, true);
    assert_eq!(dump(&reloaded), dump(&frozen));
    // The metadata records the storage mode the graph was loaded in, so the
    // two files differ there; every section's bytes must be identical.
    let head = |p: &std::path::Path| {
        metadata_head::read_metadata_head(&std::fs::read(p).unwrap(), "test").unwrap()
    };
    let (first, second) = (head(&path), head(&again));
    assert_eq!(
        first.mappable, second.mappable,
        "raw sections, topology included"
    );
    assert_eq!(
        first.section_digests, second.section_digests,
        "compressed sections"
    );
}

#[test]
fn a_compact_save_or_an_old_file_loads_the_heap_topology() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("compact.kgl");
    with_floors(10, 10, || save(topo_graph(), &path, SaveLayout::Compact));
    assert!(section_names(&path).iter().all(|n| !n.starts_with("topo:")));
    let mapped = load(&path, StorageMode::Mapped);
    assert_frozen(&mapped, false);
    assert!(mapped.graph.is_mapped());
    let without = tmp.path().join("without.kgl");
    topology_sections::without_topology(|| {
        with_floors(10, 10, || save(topo_graph(), &without, SaveLayout::Auto))
    });
    assert_frozen(&load(&without, StorageMode::Mapped), false);
    assert_reads_match(
        &load_owned(&without, StorageMode::Mapped),
        &load_owned(&path, StorageMode::Memory),
        "heap mapped vs memory",
    );
}

#[test]
fn a_graph_under_the_floors_records_no_topology() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("small.kgl");
    save(topo_graph(), &path, SaveLayout::Auto);
    assert!(section_names(&path).iter().all(|n| !n.starts_with("topo:")));
}

#[test]
fn heap_placed_sections_serve_the_same_graph() {
    let (_tmp, path) = saved_topo();
    let heap = load_file_with(
        path.to_str().unwrap(),
        &LoadOptions::new()
            .with_storage(StorageMode::Mapped)
            .with_columns(MapPolicy::Memory),
    )
    .unwrap();
    assert_frozen(&heap, true);
    let GraphBackend::Frozen(backend) = &heap.graph else {
        panic!("frozen");
    };
    assert!(
        backend.shared_sections().is_empty(),
        "nothing is mapped under columns=memory"
    );
    assert_reads_match(
        &heap,
        &load(&path, StorageMode::Memory),
        "heap-placed vs memory",
    );
    let bytes = std::fs::read(&path).unwrap();
    let from_bytes = load_kgl_bytes_with(
        &bytes,
        &LoadOptions::new().with_storage(StorageMode::Mapped),
    )
    .unwrap();
    assert_frozen(&from_bytes, true);
    assert_reads_match(
        &from_bytes,
        &load(&path, StorageMode::Memory),
        "byte-buffer load vs memory",
    );
}

pub(super) fn flip_in_section(path: &std::path::Path, name: &str) {
    let mut bytes = std::fs::read(path).unwrap();
    let metadata = metadata_head::read_metadata_head(&bytes, "test").unwrap();
    let end = 13 + u32::from_le_bytes(bytes[9..13].try_into().unwrap()) as usize;
    let base = data_region_base(&metadata, end, bytes.len() as u64).unwrap();
    let entry = metadata.mappable.iter().find(|e| e.name == name).unwrap();
    bytes[(base + entry.offset) as usize + entry.len as usize / 2] ^= 0x40;
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn a_damaged_node_section_refuses_a_frozen_load_but_not_a_memory_load() {
    let (_tmp, path) = saved_topo_big();
    flip_in_section(&path, "topo:nodes");
    let error = load_file_with(
        path.to_str().unwrap(),
        &LoadOptions::new().with_storage(StorageMode::Mapped),
    )
    .err()
    .expect("a flipped byte in the node records must refuse the mapped load");
    assert!(error.to_string().contains("corrupt"), "{error}");
    assert_frozen(&load(&path, StorageMode::Memory), false);
}

#[test]
fn a_damaged_section_of_a_small_file_is_refused_when_it_is_copied_at_open() {
    // A file under the mapping threshold is read into a buffer, so every
    // section is copied and checked as it is opened.
    for name in section_names(&saved_topo().1)
        .iter()
        .filter(|n| n.starts_with("topo:"))
    {
        let (_tmp, path) = saved_topo();
        flip_in_section(&path, name);
        let error = load_file_with(
            path.to_str().unwrap(),
            &LoadOptions::new().with_storage(StorageMode::Mapped),
        )
        .err()
        .unwrap_or_else(|| panic!("{name}: a flipped byte must refuse the load"));
        assert!(error.to_string().contains("corrupt"), "{name}: {error}");
    }
}

#[test]
fn a_damaged_section_of_a_mapped_file_is_named_by_verify_and_never_reaches_the_heap() {
    let names: Vec<String> = section_names(&saved_topo_big().1)
        .into_iter()
        .filter(|n| n.starts_with("topo:") && n != "topo:nodes")
        .collect();
    assert!(names.len() >= 9, "{names:?}");
    let mut reached_promotion = 0;
    for name in &names {
        let (_tmp, path) = saved_topo_big();
        flip_in_section(&path, name);
        let report = verify_file(path.to_str().unwrap()).unwrap();
        assert!(
            report
                .sections
                .iter()
                .any(|s| &s.name == name && s.error.is_some()),
            "verify_file names {name}"
        );
        let loaded = load_file_with(
            path.to_str().unwrap(),
            &LoadOptions::new().with_storage(StorageMode::Mapped),
        );
        // Either the load refuses (the section is read while the graph is
        // built), or the graph opens and refuses to copy the damaged section to
        // the heap, where nothing would ever check it again.
        let Ok(mut graph) = loaded else { continue };
        let again = path.with_file_name("refused.kgl");
        let saved = with_floors(10, 10, || {
            save_graph_with_layout(&mut graph, again.to_str().unwrap(), true, SaveLayout::Auto)
        });
        let error = saved.expect_err(&format!("{name}: a save must not copy a damaged section"));
        assert!(error.to_string().contains("corrupt"), "{name}: {error}");
        assert!(!again.exists(), "{name}: a refused save leaves no file");
        let graph = Arc::get_mut(&mut graph).expect("a fresh load is unique");
        let promoted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            graph.graph.promote_frozen();
        }));
        assert!(
            promoted.is_err(),
            "{name}: promotion must refuse a damaged section"
        );
        reached_promotion += 1;
    }
    // Windows never maps a topology section (an open mapping blocks a save's
    // rename), so a load copies and verifies every section and refuses a
    // damaged file before any graph exists to promote.
    if cfg!(windows) {
        assert_eq!(reached_promotion, 0, "Windows verifies at load");
    } else {
        assert!(
            reached_promotion >= 4,
            "the lazily verified sections must be the ones promotion guards: {reached_promotion}"
        );
    }
}

#[test]
fn a_truncated_topology_region_is_refused_at_open() {
    let (_tmp, path) = saved_topo();
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(&path, &bytes[..bytes.len() - 100]).unwrap();
    let error = load_file_with(
        path.to_str().unwrap(),
        &LoadOptions::new().with_storage(StorageMode::Mapped),
    )
    .err()
    .expect("a truncated file is refused");
    assert!(error.to_string().contains("truncated"), "{error}");
}

#[test]
fn only_the_topology_owner_and_kinds_are_claimed() {
    let (_tmp, path) = saved_topo();
    let bytes = std::fs::read(&path).unwrap();
    let metadata = metadata_head::read_metadata_head(&bytes, "test").unwrap();
    let topo: Vec<_> = metadata
        .mappable
        .iter()
        .filter(|e| e.name.starts_with("topo:"))
        .collect();
    assert_eq!(topo.len(), 12);
    assert!(topo.iter().all(|e| e.owner == topology_sections::OWNER));
    assert!(topo.iter().all(|e| (10..=18).contains(&e.kind)));
    assert!(topo.iter().all(|e| e.offset % 64 == 0), "64-byte aligned");
}

#[test]
fn a_memory_load_reads_no_topology_section() {
    let (_tmp, path) = saved_topo_big();
    // The sections are absent from the compressed path: damaging every one of
    // them must leave a memory load intact.
    for name in section_names(&path)
        .iter()
        .filter(|n| n.starts_with("topo:"))
    {
        flip_in_section(&path, name);
    }
    let memory = load(&path, StorageMode::Memory);
    assert_frozen(&memory, false);
    assert!(rows(&memory, "MATCH (a:A) RETURN count(a)").contains("59"));
}

#[test]
fn an_edge_write_after_a_frozen_load_is_counted_by_the_next_aggregate() {
    let (_tmp, path) = saved_topo();
    let mut frozen = load_owned(&path, StorageMode::Mapped);
    let count = "MATCH (a:A)-[r:K1]->(b:B) RETURN count(r)";
    let grouped = "MATCH (a:A)-[:K1]->(b:B) RETURN b.id AS i, count(a) AS c ORDER BY i LIMIT 3";
    let (before, before_grouped) = (rows(&frozen, count), rows(&frozen, grouped));
    run(
        &mut frozen,
        "MATCH (a:A {id: 1}), (b:B {id: 1}) CREATE (a)-[:K1]->(b)",
    );
    assert_ne!(before, rows(&frozen, count), "the new edge is counted");
    assert_ne!(
        before_grouped,
        rows(&frozen, grouped),
        "and so is its target's group"
    );
}

#[test]
fn a_mapped_frozen_load_keeps_its_arrays_in_the_file() {
    let (_tmp, path) = saved_topo_big();
    let loaded = load(&path, StorageMode::Mapped);
    let GraphBackend::Frozen(backend) = &loaded.graph else {
        panic!("frozen");
    };
    // nodes, both offset arrays, both adjacency arrays, both type arrays, the
    // endpoint array and the type mask are read in place. (The filler nodes reused every
    // vacated node slot, so this file has no alive bitset.)
    assert!(section_names(&path).iter().all(|n| n != "topo:alive"));
    // Windows copies every section (an open mapping blocks a save's rename),
    // so nothing is shared there and the heap holds the arrays.
    if cfg!(windows) {
        assert_eq!(backend.shared_sections().len(), 0);
    } else {
        assert_eq!(backend.shared_sections().len(), 9);
        let templates_and_props = backend.heap_bytes();
        assert!(
            templates_and_props < 8 * 1024,
            "only the per-type templates and the sparse edge properties are heap: {templates_and_props} B"
        );
    }
    // verify_graph checks only mapped data; Windows' copies were checked at
    // load, so its report is empty.
    let report = verify_graph(&loaded);
    if cfg!(windows) {
        assert!(report.sections.is_empty(), "{:?}", report.sections);
        return;
    }
    for name in ["topo:nodes", "topo:out_edges", "topo:edges"] {
        assert!(
            report
                .sections
                .iter()
                .any(|s| s.name == name && s.error.is_none()),
            "verify_graph reports {name}"
        );
    }
}

#[test]
fn a_frozen_load_without_type_sections_builds_the_type_indexes_from_the_records() {
    // An edge floor alone records the topology but no node-type members.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("edges-only.kgl");
    with_floors(usize::MAX, 10, || {
        save(topo_graph(), &path, SaveLayout::Auto)
    });
    let names = section_names(&path);
    assert!(names.iter().any(|n| n == "topo:nodes"));
    assert!(names.iter().all(|n| !n.starts_with("idx:type:")));
    let frozen = load_owned(&path, StorageMode::Mapped);
    assert_frozen(&frozen, true);
    assert_reads_match(
        &frozen,
        &load_owned(&path, StorageMode::Memory),
        "no type sections",
    );
}

#[test]
fn a_typed_run_lists_edges_in_the_order_a_heap_graph_does() {
    // Newest edge first within one connection type: the order a heap graph's
    // adjacency lists give, kept so the sequence a query returns without an
    // ORDER BY does not change when a graph is loaded frozen.
    let (_tmp, path) = saved_topo();
    let frozen = load_owned(&path, StorageMode::Mapped);
    let memory = load_owned(&path, StorageMode::Memory);
    let mut compared = 0;
    for slot in 0..memory.graph.node_bound() {
        let node = NodeIndex::new(slot);
        for dir in [Outgoing, Incoming] {
            for conn in ["K1", "K2", "K3", "K4", "K5"] {
                let key = InternedKey::from_str(conn);
                let order = |g: &DirGraph| -> Vec<usize> {
                    g.graph
                        .edges_directed(node, dir)
                        .filter(|e| e.connection_type() == key)
                        .map(|e| e.id().index())
                        .collect()
                };
                assert_eq!(order(&frozen), order(&memory), "{conn} {dir:?} at {slot}");
                compared += order(&memory).len();
            }
        }
    }
    assert!(compared > 500, "the fixture must exercise real runs");
}

#[test]
fn node_records_that_disagree_with_the_type_indexes_are_refused() {
    let (_tmp, path) = saved_topo();
    let frozen = load_owned(&path, StorageMode::Mapped);
    let GraphBackend::Frozen(backend) = &frozen.graph else {
        panic!("frozen");
    };
    topology_sections::check_rows_against_types(&frozen).expect("an honest load agrees");
    let honest: Vec<NodeData> = backend.node_slice().to_vec();
    let tamper = |change: &dyn Fn(&mut Vec<NodeData>)| {
        let mut nodes = honest.clone();
        change(&mut nodes);
        let mut copy = frozen.clone();
        copy.graph = GraphBackend::Frozen(Arc::new(backend.with_nodes(nodes)));
        topology_sections::check_rows_against_types(&copy)
    };
    // Two live nodes of one type swap rows: each would read the other's values.
    let first_two: Vec<usize> = (0..honest.len())
        .filter(|i| frozen.graph.node_weight(NodeIndex::new(*i)).is_some())
        .take(2)
        .collect();
    assert!(tamper(&|n| {
        let swapped = n[first_two[0]].clone();
        n[first_two[0]] = n[first_two[1]].clone();
        n[first_two[1]] = swapped;
    })
    .is_err());
    // A record of the wrong node type.
    assert!(tamper(&|n| n[first_two[0]].node_type = InternedKey::from_str("Elsewhere")).is_err());
}

#[test]
fn a_frozen_graph_serves_the_recorded_peer_counts_and_sources_without_a_scan() {
    let (_tmp, path) = saved_topo();
    let frozen = load(&path, StorageMode::Mapped);
    let memory = load(&path, StorageMode::Memory);
    let GraphBackend::Frozen(backend) = &frozen.graph else {
        panic!("frozen");
    };
    assert!(
        backend.peer_base().is_some(),
        "the file recorded peer counts"
    );
    for conn in ["K1", "K2", "K3", "K4", "K5"] {
        let key = InternedKey::from_str(conn);
        assert!(
            backend.ensure_type_index(key).persisted.is_some(),
            "{conn} is served from the file"
        );
        let mut scanned: Vec<u32> = memory
            .graph
            .edge_endpoint_keys()
            .filter(|(_, _, c)| *c == key)
            .map(|(s, _, _)| s.index() as u32)
            .collect();
        scanned.sort_unstable();
        scanned.dedup();
        assert_eq!(
            frozen.graph.sources_for_conn_type_bounded(key, None),
            Some(scanned.clone()),
            "{conn} sources"
        );
        assert_eq!(
            frozen.graph.sources_for_conn_type_bounded(key, Some(2)),
            Some(scanned.into_iter().take(2).collect()),
        );
    }
}

#[test]
fn counts_and_sources_without_recorded_peers_come_from_the_adjacency() {
    // An edge floor above the fixture's edge count records the topology (node
    // floor) but no peer counts, so the counts are derived from the CSR runs.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("no-peers.kgl");
    with_floors(10, usize::MAX, || {
        save(topo_graph(), &path, SaveLayout::Auto)
    });
    assert!(section_names(&path)
        .iter()
        .all(|n| !n.starts_with("idx:peer:")));
    let frozen = load_owned(&path, StorageMode::Mapped);
    assert_frozen(&frozen, true);
    let GraphBackend::Frozen(backend) = &frozen.graph else {
        panic!("frozen");
    };
    assert!(backend.peer_base().is_none());
    let memory = load_owned(&path, StorageMode::Memory);
    for conn in ["K1", "K2", "K3", "K4", "K5", "NOPE"] {
        let key = InternedKey::from_str(conn);
        for dir in [Outgoing, Incoming] {
            assert_eq!(
                frozen.graph.count_edges_grouped_by_peer(key, dir, None),
                memory.graph.count_edges_grouped_by_peer(key, dir, None),
                "{conn} {dir:?}"
            );
        }
        let mut expected: Vec<u32> = memory
            .graph
            .edge_endpoint_keys()
            .filter(|(_, _, c)| *c == key)
            .map(|(s, _, _)| s.index() as u32)
            .collect();
        expected.sort_unstable();
        expected.dedup();
        assert_eq!(
            frozen.graph.sources_for_conn_type_bounded(key, None),
            Some(expected)
        );
    }
}

#[test]
fn a_frozen_load_carries_the_spill_policy_of_mapped_storage() {
    let (_tmp, path) = saved_topo();
    let frozen = load_owned(&path, StorageMode::Mapped);
    assert_frozen(&frozen, true);
    assert_eq!(
        frozen.memory_limit,
        Some(0),
        "columns a later write adds spill, as they do on any mapped graph"
    );
    let memory = load_owned(&path, StorageMode::Memory);
    assert_eq!(memory.memory_limit, None);
}

/// Every `(node, direction, type)` typed lookup of `frozen` against `memory`,
/// as sorted edge slots. Returns how many lookups the mask proved empty and how
/// many it let through to an empty run (a collision).
fn compare_typed_lookups(frozen: &DirGraph, memory: &DirGraph, types: &[String]) -> (usize, usize) {
    let GraphBackend::Frozen(backend) = &frozen.graph else {
        panic!("frozen");
    };
    let (mut proven, mut collided) = (0, 0);
    for slot in 0..memory.graph.node_bound() {
        let node = NodeIndex::new(slot);
        for dir in [Outgoing, Incoming] {
            for name in types {
                let key = InternedKey::from_str(name);
                let ids = |g: &DirGraph| -> Vec<usize> {
                    let mut v: Vec<usize> = g
                        .graph
                        .edges_directed_filtered(node, dir, Some(key))
                        .filter(|e| e.connection_type() == key)
                        .map(|e| e.id().index())
                        .collect();
                    v.sort();
                    v
                };
                let (got, want) = (ids(frozen), ids(memory));
                assert_eq!(got, want, "{name} {dir:?} at {slot}");
                if want.is_empty() {
                    match backend.ordinal_of(key) {
                        Some(o) if backend.may_have_type(slot, dir, o) => collided += 1,
                        _ => proven += 1,
                    }
                }
            }
        }
    }
    (proven, collided)
}

fn many_types_graph(count: usize) -> (DirGraph, Vec<String>) {
    let mut g = DirGraph::new();
    run(&mut g, "UNWIND range(1, 50) AS i CREATE (:A {id: i, n: i})");
    let names: Vec<String> = (0..count).map(|t| format!("T{t:02}")).collect();
    for (t, name) in names.iter().enumerate() {
        run(
            &mut g,
            &format!(
                "UNWIND range(1, 50) AS i MATCH (a:A {{id: i}}), (b:A {{id: (i * {m}) % 50 + 1}}) \
                 WHERE i % {m} = 0 CREATE (a)-[:{name}]->(b)",
                m = t + 2
            ),
        );
    }
    (g, names)
}

#[test]
fn a_type_mask_never_hides_an_edge_when_ordinals_collide() {
    // 40 connection types: ordinals 0..7 and 32..39 share mask bits, so a set bit
    // is not proof of presence while a clear bit must be proof of absence.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("many.kgl");
    let (graph, mut names) = many_types_graph(40);
    with_floors(10, 10, || save(graph, &path, SaveLayout::Auto));
    names.push("Absent".to_string());
    let frozen = load_owned(&path, StorageMode::Mapped);
    let memory = load_owned(&path, StorageMode::Memory);
    assert_frozen(&frozen, true);
    let (proven, collided) = compare_typed_lookups(&frozen, &memory, &names);
    assert!(proven > 1000, "the mask must prove absences: {proven}");
    assert!(collided > 0, "the fixture must exercise a collision");
}

#[test]
fn a_file_without_the_type_mask_loads_frozen_and_answers_the_same() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("nomask.kgl");
    let (graph, mut names) = many_types_graph(12);
    topology_sections::without_type_mask(|| {
        with_floors(10, 10, || save(graph, &path, SaveLayout::Auto))
    });
    let sections = section_names(&path);
    assert!(sections.iter().any(|n| n == "topo:out_ty"));
    assert!(sections.iter().all(|n| n != "topo:type_mask"));
    names.push("Absent".to_string());
    let frozen = load_owned(&path, StorageMode::Mapped);
    let memory = load_owned(&path, StorageMode::Memory);
    assert_frozen(&frozen, true);
    let (proven, collided) = compare_typed_lookups(&frozen, &memory, &names);
    assert!(
        proven == memory.graph.node_bound() * 2,
        "without a mask only an absent type is proven empty: {proven}"
    );
    assert!(collided > 1000, "empty runs fall through to the lookup");
}
