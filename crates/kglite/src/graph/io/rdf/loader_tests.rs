//! RDF 1.2 / `kg:` import behaviours: language maps, reifier edge properties,
//! the manifest fast path, typed literals.

use super::*;
use crate::graph::storage::GraphRead;
use std::io::Write;

fn load_str(content: &str, ext: &str, config: &RdfConfig) -> (DirGraph, RdfStats) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(format!("fixture.{ext}"));
    File::create(&path)
        .unwrap()
        .write_all(content.as_bytes())
        .unwrap();
    let mut graph = DirGraph::new();
    let stats = load_rdf(&mut graph, path.to_str().unwrap(), config).unwrap();
    (graph, stats)
}

fn node_props(graph: &DirGraph, uri: &str) -> HashMap<String, Value> {
    for idx in GraphRead::node_indices(&graph.graph) {
        let node = GraphRead::node_view(&graph.graph, idx).unwrap();
        if matches!(node.get_property("uri").as_deref(), Some(Value::String(s)) if s == uri) {
            return node.properties_cloned(&graph.interner);
        }
    }
    panic!("no node {uri}");
}

const LABELS: &str =
    "<http://e.org/d1> <http://www.w3.org/2000/01/rdf-schema#label> \"Sales\"@en .\n\
<http://e.org/d1> <http://www.w3.org/2000/01/rdf-schema#label> \"Salg\"@no .\n\
<http://e.org/d1> <http://e.org/motto> \"Go\"@en .\n";

#[test]
fn language_maps_off_drops_the_tag() {
    let (g, _) = load_str(LABELS, "nt", &RdfConfig::default());
    let props = node_props(&g, "http://e.org/d1");
    assert_eq!(props.get("motto"), Some(&Value::String("Go".into())));
    assert!(!props.contains_key("rdfs__label"));
}

#[test]
fn language_maps_on_keeps_every_tag() {
    let cfg = RdfConfig {
        language_maps: true,
        ..RdfConfig::default()
    };
    let (g, _) = load_str(LABELS, "nt", &cfg);
    let props = node_props(&g, "http://e.org/d1");
    let Some(Value::Map(label)) = props.get("rdfs__label") else {
        panic!("label map missing: {props:?}");
    };
    assert_eq!(label.get("en"), Some(&Value::String("Sales".into())));
    assert_eq!(label.get("no"), Some(&Value::String("Salg".into())));
    assert!(matches!(props.get("motto"), Some(Value::Map(_))));
}

const REIFIED: &str = "<http://e.org/p1> <http://e.org/WORKS_IN> <http://e.org/d1> .\n\
_:r1 <http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies> <<( <http://e.org/p1> <http://e.org/WORKS_IN> <http://e.org/d1> )>> .\n\
_:r1 <http://e.org/since> \"2020\"^^<http://www.w3.org/2001/XMLSchema#integer> .\n\
_:r2 <http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies> <<( <http://e.org/p1> <http://e.org/WORKS_IN> <http://e.org/d1> )>> .\n\
_:r2 <http://e.org/since> \"2023\"^^<http://www.w3.org/2001/XMLSchema#integer> .\n";

#[test]
fn reifiers_become_parallel_edges_with_properties() {
    let (g, stats) = load_str(REIFIED, "nq", &RdfConfig::default());
    assert_eq!(stats.nodes_created, 2, "reifiers are not nodes");
    assert_eq!(stats.edges_created, 2);
    let mut since: Vec<Value> = GraphRead::edge_indices(&g.graph)
        .map(|e| {
            GraphRead::edge_weight(&g.graph, e)
                .unwrap()
                .properties_cloned(&g.interner)["since"]
                .clone()
        })
        .collect();
    since.sort_by_key(|v| format!("{v:?}"));
    assert_eq!(since, vec![Value::Int64(2020), Value::Int64(2023)]);
}
