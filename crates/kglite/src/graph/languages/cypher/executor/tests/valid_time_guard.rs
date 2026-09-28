//! The valid-time guard in the pattern matcher: every pattern matcher the Cypher engine
//! builds carries the statement's filter, and each guard site answers as
//! the unguarded query over only the valid elements would.

use std::path::{Path, PathBuf};

use crate::graph::session::execute::{execute_mut, execute_read, ExecuteOptions};

use super::*;

/// Files under `languages/cypher` (tests excluded) that name a
/// `PatternExecutor` constructor, with how many times.
fn constructor_sites() -> Vec<(PathBuf, usize)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("readable source dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name == "tests") {
                    continue;
                }
                walk(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs")
                && !path.to_string_lossy().ends_with("_tests.rs")
            {
                out.push(path);
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/graph/languages/cypher");
    let mut files = Vec::new();
    walk(&root, &mut files);
    let constructors = [
        "PatternExecutor::new(",
        "PatternExecutor::new_lightweight_with_params(",
        "PatternExecutor::with_bindings_and_params(",
        "PatternExecutor {",
    ];
    files
        .into_iter()
        .filter_map(|path| {
            let source = std::fs::read_to_string(&path).expect("readable source");
            let count = constructors
                .iter()
                .map(|c| source.matches(c).count())
                .sum::<usize>();
            (count > 0).then_some((path, count))
        })
        .collect()
}

/// Every matcher the Cypher engine builds comes from
/// `CypherExecutor::pattern_executor`, which hands it the statement's
/// valid-time filter; a matcher built anywhere else would match without it.
/// The scan found the helper's two constructions when this was written.
#[test]
fn every_pattern_matcher_is_built_by_the_guarded_helper() {
    let sites = constructor_sites();
    let [(path, count)] = sites.as_slice() else {
        panic!(
            "PatternExecutor is constructed outside CypherExecutor::pattern_executor: \
             {sites:?} — route the new site through that helper so it carries the \
             valid-time filter"
        );
    };
    assert!(path.ends_with("executor/mod.rs"), "{path:?}");
    assert_eq!(*count, 2, "the helper builds one matcher per binding form");
}

fn run(graph: &mut DirGraph, query: &str) {
    let params = HashMap::new();
    execute_mut(graph, query, &ExecuteOptions::eager(&params))
        .unwrap_or_else(|e| panic!("{query}: {e}"));
}

fn error(graph: &DirGraph, query: &str) -> String {
    let params = HashMap::new();
    match execute_read(graph, query, &ExecuteOptions::eager(&params)) {
        Ok(_) => panic!("{query}: expected an error"),
        Err(e) => e.to_string(),
    }
}

fn rows(graph: &DirGraph, query: &str) -> Vec<Vec<Value>> {
    let params = HashMap::new();
    let mut rows: Vec<Vec<Value>> = execute_read(graph, query, &ExecuteOptions::eager(&params))
        .unwrap_or_else(|e| panic!("{query}: {e}"))
        .result
        .rows;
    rows.sort_by_key(|row| format!("{row:?}"));
    rows
}

/// `values` as one-column rows, in [`rows`]' order.
fn ints(values: &[i64]) -> Vec<Vec<Value>> {
    let mut rows: Vec<Vec<Value>> = values.iter().map(|v| vec![Value::Int64(*v)]).collect();
    rows.sort_by_key(|row| format!("{row:?}"));
    rows
}

/// Two wells, one closed in 2010, and a licence per source type: `Field`
/// licences are keyed on `f_from`/`f_to`, every other source's on the
/// unkeyed `from`/`to`. A node carrying `Well` as a secondary label is
/// governed by `Well`'s declaration too.
fn registry() -> DirGraph {
    let mut graph = DirGraph::new();
    for query in [
        "CREATE (:Well {id: 1, vf: date('2000-01-01'), vt: date('2010-01-01')}), \
         (:Well {id: 2, vf: date('2005-01-01')}), \
         (:Field {id: 10}), (:Company {id: 20}), (:Pad {id: 30, vf: date('2012-01-01')})",
        "MATCH (p:Pad) SET p:Well",
        "MATCH (f:Field), (c:Company) \
         CREATE (f)-[:LICENSED {f_from: date('2000-01-01'), f_to: date('2004-12-31')}]->(c)",
        "MATCH (w:Well {id: 2}), (c:Company) \
         CREATE (w)-[:LICENSED {from: date('2008-01-01'), to: date('2030-01-01')}]->(c)",
        "MATCH (w:Well {id: 1}), (f:Field) CREATE (w)-[:IN]->(f)",
        "MATCH (w:Well {id: 2}), (f:Field) CREATE (w)-[:IN]->(f)",
        "CALL db.temporal.declare({node: 'Well', from: 'vf', to: 'vt', convention: 'closed'}) \
         YIELD declared RETURN declared",
        "CALL db.temporal.declare({relationship: 'LICENSED', source_type: 'Field', \
         from: 'f_from', to: 'f_to', convention: 'closed'}) YIELD declared RETURN declared",
        "CALL db.temporal.declare({relationship: 'LICENSED', from: 'from', to: 'to', \
         convention: 'half_open'}) YIELD declared RETURN declared",
    ] {
        run(&mut graph, query);
    }
    graph
}

fn at(date: &str, body: &str) -> String {
    format!("FOR VALID_TIME AS OF date('{date}') {body}")
}

#[test]
fn anchors_scans_and_untyped_nodes_see_only_valid_nodes() {
    let graph = registry();
    let q = "MATCH (w:Well) RETURN w.id";
    assert_eq!(rows(&graph, &at("2003-01-01", q)), ints(&[1]));
    assert_eq!(rows(&graph, &at("2011-01-01", q)), ints(&[2]));
    // The secondary carrier passes only once its own interval opens.
    assert_eq!(rows(&graph, &at("2013-01-01", q)), ints(&[2, 30]));
    // `MATCH (n)` applies every declared label a node carries.
    let untyped = "MATCH (n) WHERE n.id < 100 RETURN n.id";
    assert_eq!(rows(&graph, &at("2011-01-01", untyped)), ints(&[2, 10, 20]));
    // An id seek re-tests the node it finds.
    assert!(rows(
        &graph,
        &at("2011-01-01", "MATCH (w:Well {id: 1}) RETURN w.id")
    )
    .is_empty());
    assert!(rows(&graph, &at("2011-01-01", "MATCH (n {id: 1}) RETURN n.id")).is_empty());
}

#[test]
fn a_hop_tests_both_endpoints_and_keys_the_relationship_on_its_source() {
    let graph = registry();
    let q = "MATCH (a)-[:LICENSED]->(c:Company) RETURN a.id";
    // Field's keyed licence ends in 2004; Well 2's unkeyed one starts 2008.
    assert_eq!(rows(&graph, &at("2003-01-01", q)), ints(&[10]));
    assert!(rows(&graph, &at("2006-01-01", q)).is_empty());
    assert_eq!(rows(&graph, &at("2009-01-01", q)), ints(&[2]));
    // The far endpoint is tested even unnamed: Well 1 is gone by 2011.
    let unnamed = "MATCH (f:Field)<-[:IN]-() RETURN count(*) AS c";
    assert_eq!(rows(&graph, &at("2006-01-01", unnamed)), ints(&[2]));
    assert_eq!(rows(&graph, &at("2011-01-01", unnamed)), ints(&[1]));
    // Anchored from the other side, the untyped seed the relationship-type
    // inverted index names is tested too.
    let seeded = "MATCH (n)-[:IN]->(f) RETURN n.id";
    assert_eq!(rows(&graph, &at("2011-01-01", seeded)), ints(&[2]));
}

#[test]
fn counts_answer_with_the_guard() {
    let graph = registry();
    for (query, want) in [
        ("MATCH (w:Well) RETURN count(w) AS c", 1),
        ("MATCH (n) RETURN count(n) AS c", 3),
        ("MATCH ()-[r:IN]->() RETURN count(*) AS c", 1),
        ("MATCH ()-[r]->() RETURN count(r) AS c", 2),
    ] {
        assert_eq!(
            rows(&graph, &at("2011-01-01", query)),
            ints(&[want]),
            "{query}"
        );
    }
    // The COUNT { } shortcut takes the guarded counter.
    let per_field = "MATCH (f:Field) RETURN COUNT { (f)<-[:IN]-(w) } AS c";
    assert_eq!(rows(&graph, &at("2006-01-01", per_field)), ints(&[2]));
    assert_eq!(rows(&graph, &at("2011-01-01", per_field)), ints(&[1]));
}

/// OPTIONAL MATCH, pattern comprehensions and `COUNT { }` run their own
/// matches through the guarded matcher: an invisible match NULL-pads or is
/// left out of the list and the count.
#[test]
fn optional_match_and_subqueries_see_only_valid_matches() {
    let graph = registry();
    let optional = "MATCH (f:Field) OPTIONAL MATCH (f)-[:LICENSED]->(c) RETURN f.id, c.id";
    assert_eq!(
        rows(&graph, &at("2006-01-01", optional)),
        vec![vec![Value::Int64(10), Value::Null]]
    );
    assert_eq!(
        rows(&graph, &at("2003-01-01", optional)),
        vec![vec![Value::Int64(10), Value::Int64(20)]]
    );
    for (date, want) in [("2006-01-01", 2), ("2011-01-01", 1)] {
        for query in [
            "MATCH (f:Field) RETURN size([(f)<-[:IN]-(w) | w.id])",
            "MATCH (f:Field) RETURN size([p = (f)<-[:IN]-(w) | length(p)])",
            "MATCH (f:Field) RETURN COUNT { (f)<-[:IN]-(w) WHERE w.id > 0 }",
        ] {
            assert_eq!(
                rows(&graph, &at(date, query)),
                ints(&[want]),
                "{date}: {query}"
            );
        }
    }
}

/// Version nodes that share a user id: the id index holds one of them, so
/// the seek must find the one valid at the instant.
#[test]
fn an_id_seek_finds_the_valid_version_among_several() {
    let mut graph = DirGraph::new();
    for query in [
        "CREATE (:Muni {id: 363, name: 'old', vf: date('1900-01-01'), vt: date('1999-12-31')}), \
         (:Muni {id: 363, name: 'new', vf: date('2000-01-01')})",
        "CALL db.temporal.declare({node: 'Muni', from: 'vf', to: 'vt', convention: 'closed'}) \
         YIELD declared RETURN declared",
    ] {
        run(&mut graph, query);
    }
    for (date, name) in [("1950-01-01", "old"), ("2020-01-01", "new")] {
        for body in [
            "MATCH (m:Muni {id: 363}) RETURN m.name",
            "MATCH (m {id: 363}) RETURN m.name",
            "MATCH (m:Muni) WHERE m.id IN [363] RETURN m.name",
        ] {
            assert_eq!(
                rows(&graph, &at(date, body)),
                vec![vec![Value::String(name.into())]],
                "{date}: {body}"
            );
        }
    }
}

/// A declared `LINK` network: stop 2 closes in 2005, the direct 1→3 link
/// runs 2000–2005, and 4→5 has two parallel links, `xy1` (2000–2005) and
/// `xy2` (from 2006). At 2008 the only valid route from 1 to 3 is 1→4→5→3
/// over `xy2`.
fn network() -> DirGraph {
    let mut graph = DirGraph::new();
    for query in [
        "CREATE (s1:Stop {id: 1}), (s2:Stop {id: 2, vf: date('2000-01-01'), vt: date('2005-01-01')}), \
         (s3:Stop {id: 3}), (s4:Stop {id: 4}), (s5:Stop {id: 5}), \
         (s1)-[:LINK {k: 'a2'}]->(s2), (s2)-[:LINK {k: '2b'}]->(s3), \
         (s1)-[:LINK {k: 'ab', since: date('2000-01-01'), until: date('2005-01-01')}]->(s3), \
         (s1)-[:LINK {k: 'ax'}]->(s4), \
         (s4)-[:LINK {k: 'xy1', since: date('2000-01-01'), until: date('2005-01-01')}]->(s5), \
         (s4)-[:LINK {k: 'xy2', since: date('2006-01-01')}]->(s5), (s5)-[:LINK {k: 'yb'}]->(s3)",
        "CALL db.temporal.declare({node: 'Stop', from: 'vf', to: 'vt', convention: 'closed'}) \
         YIELD declared RETURN declared",
        "CALL db.temporal.declare({relationship: 'LINK', from: 'since', to: 'until', \
         convention: 'half_open'}) YIELD declared RETURN declared",
    ] {
        run(&mut graph, query);
    }
    graph
}

fn strings(values: &[&str]) -> Vec<Value> {
    values.iter().map(|v| Value::String((*v).into())).collect()
}

#[test]
fn var_length_segments_cross_only_valid_relationships_and_nodes() {
    let graph = network();
    let count = "MATCH (:Stop {id: 1})-[:LINK*1..3]->(:Stop {id: 3}) RETURN count(*) AS c";
    assert_eq!(rows(&graph, count), ints(&[4]));
    assert_eq!(rows(&graph, &at("2008-01-01", count)), ints(&[1]));
    let list = "MATCH (:Stop {id: 1})-[r:LINK*1..3]->(:Stop {id: 3}) RETURN [x IN r | x.k]";
    assert_eq!(
        rows(&graph, &at("2008-01-01", list)),
        vec![vec![Value::List(strings(&["ax", "xy2", "yb"]))]]
    );
    // The distance frontier and the trail expansion: stop 2 is invisible and
    // the direct link closed, so two hops from 1 reach only 4 and 5.
    for reach in [
        "MATCH (:Stop {id: 1})-[:LINK*1..2]->(t) RETURN count(DISTINCT t)",
        "MATCH p = (:Stop {id: 1})-[:LINK*1..2]->(t) RETURN count(DISTINCT t)",
    ] {
        assert_eq!(rows(&graph, reach), ints(&[4]), "{reach}");
        assert_eq!(
            rows(&graph, &at("2008-01-01", reach)),
            ints(&[2]),
            "{reach}"
        );
    }
    // The frontier marks a node only once a valid relationship reaches it:
    // stop 3 is first met over the closed direct link, and still reached
    // through 4 and 5.
    let frontier = "MATCH (:Stop {id: 1})-[:LINK*1..3]->(t) RETURN count(DISTINCT t)";
    assert_eq!(rows(&graph, &at("2008-01-01", frontier)), ints(&[3]));
    // The undirected closed trail 4–5–4 needs both parallel links.
    let closed = "MATCH (s:Stop {id: 4})-[:LINK*1..2]-(t:Stop) RETURN DISTINCT t.id";
    assert_eq!(rows(&graph, closed), ints(&[1, 2, 3, 4, 5]));
    assert_eq!(rows(&graph, &at("2008-01-01", closed)), ints(&[1, 3, 5]));
    let exists = "MATCH (s:Stop) WHERE EXISTS { (s)-[:LINK*2..2]->(:Stop {id: 3}) } RETURN s.id";
    assert_eq!(rows(&graph, &at("2008-01-01", exists)), ints(&[4]));
}

#[test]
fn shortest_paths_take_the_longer_valid_route() {
    let graph = network();
    let single = "MATCH p = shortestPath((:Stop {id: 1})-[:LINK*]->(:Stop {id: 3})) \
                  RETURN [r IN relationships(p) | r.k]";
    assert_eq!(
        rows(&graph, single),
        vec![vec![Value::List(strings(&["ab"]))]]
    );
    assert_eq!(
        rows(&graph, &at("2008-01-01", single)),
        vec![vec![Value::List(strings(&["ax", "xy2", "yb"]))]]
    );
    let incoming =
        "MATCH p = shortestPath((:Stop {id: 3})<-[:LINK*]-(:Stop {id: 1})) RETURN length(p)";
    assert_eq!(rows(&graph, &at("2008-01-01", incoming)), ints(&[3]));
    let all = "MATCH p = allShortestPaths((:Stop {id: 4})-[:LINK*]-(:Stop {id: 5})) \
               RETURN [r IN relationships(p) | r.k]";
    assert_eq!(
        rows(&graph, &at("2008-01-01", all)),
        vec![vec![Value::List(strings(&["xy2"]))]]
    );
    assert_eq!(
        rows(&graph, &at("2003-01-01", all)),
        vec![vec![Value::List(strings(&["xy1"]))]]
    );
    let routed = "MATCH p = allShortestPaths((:Stop {id: 1})-[:LINK*]->(:Stop {id: 3})) \
                  RETURN length(p)";
    assert_eq!(rows(&graph, &at("2008-01-01", routed)), ints(&[3]));
}

/// A bound the evaluator cannot read raises instead of hiding the element.
#[test]
fn an_unreadable_bound_raises() {
    let mut graph = DirGraph::new();
    for query in [
        "CREATE (:Site {id: 1, vf: date('2000-01-01'), vt: date('2010-01-01')})",
        "CALL db.temporal.declare({node: 'Site', from: 'vf', to: 'vt', convention: 'closed'}) \
         YIELD declared RETURN declared",
        "MATCH (s:Site) SET s.vt = 42",
    ] {
        crate::graph::features::temporal::unchecked(|| run(&mut graph, query));
    }
    let err = error(&graph, &at("2006-01-01", "MATCH (s:Site) RETURN s.id"));
    assert!(
        err.contains("node '1'") && err.contains("property 'vt'"),
        "{err}"
    );
}

/// Inline-map values are lowered like any other expression: a topology
/// function inside one is refused, and a row-independent one is evaluated
/// under the filter at execution, never folded at plan time without it.
#[test]
fn inline_map_values_are_lowered_and_evaluated_under_the_filter() {
    let graph = registry();
    let degree = "MATCH (f:Field) MATCH (c:Company {id: degree(f) + 17}) RETURN c.id";
    let err = error(&graph, &at("2011-01-01", degree));
    assert!(err.contains("degree()"), "{err}");
    // At 2011 only Well 2 is visible (Well 1 closed in 2010, the Pad carrier
    // opens in 2012), so the count is 1 and the id 20.
    let counted = "MATCH (c:Company {id: COUNT { (:Well) } + 19}) RETURN c.id";
    assert_eq!(rows(&graph, &at("2011-01-01", counted)), ints(&[20]));
    assert!(rows(&graph, counted).is_empty(), "three Wells unguarded");
    // An unfolded constant reaches the node-scan aggregate and top-k shapes.
    for (query, want) in [
        ("MATCH (c:Company {id: 19 + 1}) RETURN count(c)", 1),
        ("MATCH (c:Company {id: 19 + 1}) RETURN c.id", 20),
        (
            "MATCH (c:Company {id: 19 + 1}) RETURN c.id ORDER BY c.id LIMIT 1",
            20,
        ),
    ] {
        assert_eq!(
            rows(&graph, &at("2011-01-01", query)),
            ints(&[want]),
            "{query}"
        );
    }
}

/// Several visible versions share an id: the seek returns the last one in
/// the type's node order (the id index's own choice for its node),
/// whichever version the index holds.
#[test]
fn an_id_seek_returns_the_last_visible_version_in_node_order() {
    let mut graph = DirGraph::new();
    for query in [
        "CREATE (:M {id: 1, name: 'a', vf: date('2000-01-01')}), \
         (:M {id: 1, name: 'b', vf: date('2000-01-01')}), \
         (:M {id: 1, name: 'c', vf: date('2030-01-01'), vt: date('2040-01-01')}), \
         (:M {id: 2, name: 'd', vf: date('2000-01-01')})",
        "CALL db.temporal.declare({node: 'M', from: 'vf', to: 'vt', convention: 'closed'}) \
         YIELD declared RETURN declared",
    ] {
        run(&mut graph, query);
    }
    for body in [
        "MATCH (m:M {id: 1}) RETURN m.name",
        "MATCH (m {id: 1}) RETURN m.name",
        "MATCH (m:M) WHERE m.id IN [1] RETURN m.name",
    ] {
        assert_eq!(
            rows(&graph, &at("2020-01-01", body)),
            vec![vec![Value::String("b".into())]],
            "{body}"
        );
        assert_eq!(
            rows(&graph, &at("2035-01-01", body)),
            vec![vec![Value::String("c".into())]],
            "{body}"
        );
    }
    // Over the byte cap there is no map; the walk keeps the same rule.
    crate::graph::features::temporal::endpoint_index::set_byte_cap(&graph, 1);
    let seek = "MATCH (m:M {id: 1}) RETURN m.name";
    assert_eq!(
        rows(&graph, &at("2020-01-01", seek)),
        vec![vec![Value::String("b".into())]]
    );
}

/// A seek compares ids before it reads a bound, so an unrelated node's
/// unreadable bound does not raise from it; a scan that reads it still does.
#[test]
fn an_id_seek_reads_no_other_ids_bounds() {
    let mut graph = DirGraph::new();
    for query in [
        "CREATE (:Muni {id: 363, name: 'old', vf: date('1900-01-01'), vt: date('1999-12-31')}), \
         (:Muni {id: 363, name: 'new', vf: date('2000-01-01')}), \
         (:Muni {id: 999, name: 'bad', vf: date('1900-01-01')})",
        "CALL db.temporal.declare({node: 'Muni', from: 'vf', to: 'vt', convention: 'closed'}) \
         YIELD declared RETURN declared",
        "MATCH (m:Muni {id: 999}) SET m.vt = 42",
    ] {
        crate::graph::features::temporal::unchecked(|| run(&mut graph, query));
    }
    let seek = "MATCH (m:Muni {id: 363}) RETURN m.name";
    assert_eq!(
        rows(&graph, &at("1950-01-01", seek)),
        vec![vec![Value::String("old".into())]]
    );
    let capped = graph.clone();
    crate::graph::features::temporal::endpoint_index::set_byte_cap(&capped, 1);
    assert_eq!(
        rows(&capped, &at("1950-01-01", seek)),
        vec![vec![Value::String("old".into())]]
    );
    let err = error(&graph, &at("1950-01-01", "MATCH (m:Muni) RETURN m.name"));
    assert!(err.contains("node '999'"), "{err}");
}

/// [`rows`] with the streaming pipeline on, with lazy projection (the
/// Python live graph's setting) or without it (a frozen view's, a session's
/// and a transaction's); `rows` runs eager, where the pipeline is off.
fn streamed_rows(graph: &DirGraph, query: &str, lazy_eligible: bool) -> Vec<Vec<Value>> {
    let params = HashMap::new();
    let options = ExecuteOptions {
        lazy_eligible,
        streaming: true,
        ..ExecuteOptions::eager(&params)
    };
    let result = execute_read(graph, query, &options).unwrap_or_else(|e| panic!("{query}: {e}"));
    assert!(
        result.result.lazy.is_none(),
        "{query}: a guarded scope never goes lazy"
    );
    let mut rows = result.result.rows;
    rows.sort_by_key(|row| format!("{row:?}"));
    rows
}

/// Every shape the streaming pipeline absorbs — a grouped aggregate in
/// `RETURN` or `WITH` (with its `WHERE`), `DISTINCT` over a node and over a
/// value, a subquery argument, and the `ORDER BY … LIMIT` heap — answers
/// under a context as the materialized path does, over the valid rows only.
#[test]
fn the_streaming_pipeline_answers_as_the_materialized_path_under_a_context() {
    let graph = network();
    for query in [
        "MATCH (:Stop {id: 1})-[:LINK*1..3]->(t) RETURN count(DISTINCT t)",
        "MATCH (s:Stop)-[:LINK]->(t) RETURN s.id, count(t)",
        "MATCH (s:Stop)-[r:LINK]->(t) RETURN count(DISTINCT t.id), count(*), count(r)",
        "MATCH (s:Stop)-[:LINK]->(t) WITH s, count(t) AS c WHERE c > 0 RETURN s.id, c",
        "MATCH (s:Stop)-[:LINK]->(t) RETURN s.id AS s, count(*) AS c ORDER BY s DESC LIMIT 2",
        "MATCH (s:Stop)-[:LINK]->(t) RETURN min(t.id), max(t.id), sum(t.id), avg(t.id)",
        "MATCH (s:Stop)-[:LINK]->(t) RETURN s.id, sum(COUNT { (t)-[:LINK]->() }) AS n",
    ] {
        let context = at("2008-01-01", query);
        let probe =
            crate::graph::languages::cypher::executor::stream::pipeline::absorbed_probe::take;
        probe();
        let materialized = rows(&graph, &context);
        assert_eq!(probe(), 0, "{query}: eager options stream nothing");
        for lazy_eligible in [true, false] {
            assert_eq!(
                streamed_rows(&graph, &context, lazy_eligible),
                materialized,
                "{query}"
            );
            assert!(probe() > 0, "{query}: the streaming options did not stream");
        }
        assert_ne!(
            materialized,
            rows(&graph, query),
            "{query}: filters nothing"
        );
    }
}
