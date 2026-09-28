//! The bulk loaders on a declared temporal relationship type: a row identical
//! to a relationship already between its endpoints is dropped, and any other
//! row — a different `from`, a different `to`, any other property different —
//! is a new, parallel relationship. Nothing stored is updated in place, in one
//! call or across calls. Undeclared types keep merging on the endpoints alone.

use std::collections::HashMap;

use chrono::{NaiveDate, NaiveDateTime};

use super::declarations::{declare, TemporalTarget};
use super::eval::IntervalConvention::Closed;
use crate::datatypes::{DataFrame, Value};
use crate::graph::dir_graph::DirGraph;
use crate::graph::introspection::reporting::ConnectionOperationReport;
use crate::graph::mutation::edge_specs::{add_edges_from_specs, EdgeSpec};
use crate::graph::mutation::maintain::{add_connections, add_nodes, replace_connections};
use crate::graph::session::execute::{execute_mut, execute_read, ExecuteOptions};

fn day(text: &str) -> Value {
    Value::DateTime(NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap())
}

fn at(text: &str) -> Value {
    Value::Timestamp(NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M").unwrap())
}

fn text(s: &str) -> Value {
    Value::String(s.to_string())
}

fn docs(graph: &mut DirGraph, node_type: &str) {
    let rows = vec![vec![Value::Int64(1)], vec![Value::Int64(2)]];
    let df = DataFrame::from_cypher_rows(vec!["id".to_string()], rows).unwrap();
    add_nodes(
        graph,
        df,
        node_type.to_string(),
        "id".to_string(),
        None,
        None,
    )
    .unwrap();
}

fn frame(rows: Vec<(Value, Value)>, extra: Option<Value>) -> DataFrame {
    let mut columns = vec!["src", "tgt", "vf", "vt"];
    if extra.is_some() {
        columns.push("x");
    }
    let rows = rows
        .into_iter()
        .map(|(vf, vt)| {
            let mut row = vec![Value::Int64(1), Value::Int64(2), vf, vt];
            row.extend(extra.clone());
            row
        })
        .collect();
    let columns = columns.into_iter().map(str::to_string).collect();
    DataFrame::from_cypher_rows(columns, rows).unwrap()
}

fn load(
    graph: &mut DirGraph,
    rows: Vec<(Value, Value)>,
    mode: Option<&str>,
) -> Result<ConnectionOperationReport, String> {
    load_frame(graph, frame(rows, None), "Doc", mode)
}

fn load_frame(
    graph: &mut DirGraph,
    df: DataFrame,
    source: &str,
    mode: Option<&str>,
) -> Result<ConnectionOperationReport, String> {
    add_connections(
        graph,
        df,
        "IN".to_string(),
        source.to_string(),
        "src".to_string(),
        "Doc".to_string(),
        "tgt".to_string(),
        None,
        None,
        mode.map(str::to_string),
    )
}

fn counts(report: &ConnectionOperationReport) -> (usize, usize) {
    (report.connections_created, report.connections_updated)
}

fn periods(graph: &DirGraph) -> Vec<Vec<Value>> {
    let params = HashMap::new();
    execute_read(
        graph,
        "MATCH ()-[r:IN]->(:Doc) RETURN r.vf AS vf, r.vt AS vt ORDER BY vf, vt",
        &ExecuteOptions::eager(&params),
    )
    .unwrap()
    .result
    .rows
}

fn in_type(source: Option<&str>) -> TemporalTarget {
    TemporalTarget::Relationship {
        rel_type: "IN".into(),
        source_type: source.map(str::to_string),
    }
}

/// `Doc` 1 → 2 holds `IN` for 2000..2005; `declared` then declares `IN`.
fn first_period(declared: Option<TemporalTarget>) -> DirGraph {
    let mut graph = DirGraph::new();
    docs(&mut graph, "Doc");
    load(
        &mut graph,
        vec![(day("2000-01-01"), day("2005-01-01"))],
        None,
    )
    .unwrap();
    if let Some(target) = declared {
        declare(&mut graph, &target, "vf", "vt", Closed).unwrap();
    }
    graph
}

const MODES: [Option<&str>; 6] = [
    None,
    Some("update"),
    Some("replace"),
    Some("preserve"),
    Some("skip"),
    Some("sum"),
];

#[test]
fn a_later_period_on_a_declared_type_is_a_parallel_relationship_in_every_mode() {
    for target in [in_type(None), in_type(Some("Doc"))] {
        for mode in MODES {
            let mut graph = first_period(Some(target.clone()));
            let report = load(&mut graph, vec![(day("2010-01-01"), Value::Null)], mode).unwrap();
            assert_eq!(counts(&report), (1, 0), "{target:?} {mode:?}");
            assert_eq!(
                periods(&graph),
                vec![
                    vec![day("2000-01-01"), day("2005-01-01")],
                    vec![day("2010-01-01"), Value::Null],
                ],
                "{target:?} {mode:?}"
            );
        }
    }
}

/// The undeclared behaviour this change must keep, mode by mode.
#[test]
fn an_undeclared_type_still_merges_on_the_endpoints() {
    let (old, new) = (day("2000-01-01"), day("2010-01-01"));
    let old_end = day("2005-01-01");
    for (mode, expected, reported) in [
        (None, vec![new.clone(), old_end.clone()], (0, 1)),
        (Some("update"), vec![new.clone(), old_end.clone()], (0, 1)),
        (Some("replace"), vec![new.clone(), Value::Null], (0, 1)),
        (Some("preserve"), vec![old.clone(), old_end.clone()], (0, 1)),
        (Some("skip"), vec![old.clone(), old_end.clone()], (0, 0)),
        (Some("sum"), vec![new.clone(), old_end.clone()], (0, 1)),
    ] {
        let mut graph = first_period(None);
        let report = load(&mut graph, vec![(new.clone(), Value::Null)], mode).unwrap();
        assert_eq!(counts(&report), reported, "{mode:?}");
        assert_eq!(periods(&graph), vec![expected], "{mode:?}");
    }
}

/// Declare `IN` on `vf`/`vt` before any `IN` exists, as `add_relationships`'
/// `column_types` does, so the next load is the type's first.
fn declare_before_any_load(graph: &mut DirGraph) {
    let config = crate::graph::schema::TemporalConfig {
        valid_from: "vf".to_string(),
        valid_to: "vt".to_string(),
        convention: Closed,
        source_type: None,
    };
    graph.temporal.insert(&in_type(None), config, None);
}

/// Closing an open period, re-loading it with another `to`, or re-loading it
/// under `skip` each adds a version; only an identical row is dropped.
#[test]
fn a_reload_never_rewrites_a_stored_version() {
    let mut graph = first_period(Some(in_type(None)));
    load(&mut graph, vec![(day("2010-01-01"), Value::Null)], None).unwrap();
    let report = load(
        &mut graph,
        vec![(day("2010-01-01"), day("2015-01-01"))],
        Some("update"),
    )
    .unwrap();
    assert_eq!(counts(&report), (1, 0));
    for mode in MODES {
        let report = load(
            &mut graph,
            vec![(day("2010-01-01"), day("2015-01-01"))],
            mode,
        )
        .unwrap();
        assert_eq!(counts(&report), (0, 0), "{mode:?}");
    }
    let report = load(
        &mut graph,
        vec![(day("2010-01-01"), day("2016-01-01"))],
        Some("skip"),
    )
    .unwrap();
    assert_eq!(counts(&report), (1, 0));
    assert_eq!(
        periods(&graph),
        vec![
            vec![day("2000-01-01"), day("2005-01-01")],
            vec![day("2010-01-01"), day("2015-01-01")],
            vec![day("2010-01-01"), day("2016-01-01")],
            vec![day("2010-01-01"), Value::Null],
        ]
    );
}

/// The rows of one call and the same rows over several calls leave the same
/// relationships, on a type's first load (which otherwise writes one
/// relationship per row) as on a later one.
#[test]
fn one_call_and_several_calls_leave_the_same_versions() {
    let rows = [
        (day("2010-01-01"), Value::Null, Value::Int64(1)),
        (day("2010-01-01"), day("2012-01-01"), Value::Int64(2)),
        (day("2010-01-01"), Value::Null, Value::Int64(1)),
        (day("2010-01-01"), Value::Null, Value::Int64(3)),
    ];
    let frame_of = |rows: &[(Value, Value, Value)]| {
        let rows = rows
            .iter()
            .map(|(vf, vt, x)| {
                vec![
                    Value::Int64(1),
                    Value::Int64(2),
                    vf.clone(),
                    vt.clone(),
                    x.clone(),
                ]
            })
            .collect();
        let columns = ["src", "tgt", "vf", "vt", "x"].map(str::to_string).to_vec();
        DataFrame::from_cypher_rows(columns, rows).unwrap()
    };
    let versions = |graph: &DirGraph| {
        let params = HashMap::new();
        execute_read(
            graph,
            "MATCH ()-[r:IN]->() RETURN r.vf AS vf, r.vt AS vt, r.x AS x ORDER BY vf, vt, x",
            &ExecuteOptions::eager(&params),
        )
        .unwrap()
        .result
        .rows
    };
    let declared = || {
        let mut graph = DirGraph::new();
        docs(&mut graph, "Doc");
        declare_before_any_load(&mut graph);
        graph
    };

    let mut one_call = declared();
    let report = load_frame(&mut one_call, frame_of(&rows), "Doc", None).unwrap();
    assert_eq!(counts(&report), (3, 0));

    let mut several = declared();
    let mut created = 0;
    for row in &rows {
        let report = load_frame(
            &mut several,
            frame_of(std::slice::from_ref(row)),
            "Doc",
            None,
        )
        .unwrap();
        assert_eq!(report.connections_updated, 0);
        created += report.connections_created;
    }
    assert_eq!(created, 3);
    assert_eq!(versions(&one_call), versions(&several));
    assert_eq!(versions(&one_call).len(), 3);
}

/// `auto_timestamp` stamps every row afresh; the stamps do not make a
/// redelivered row a new version.
#[test]
fn provenance_stamps_do_not_make_a_redelivery_a_new_version() {
    let mut graph = DirGraph::new();
    docs(&mut graph, "Doc");
    let mut schema = crate::graph::schema::SchemaDefinition::default();
    schema.connection_schemas.insert(
        "IN".to_string(),
        crate::graph::schema::ConnectionSchemaDefinition {
            source_type: "Doc".to_string(),
            target_type: "Doc".to_string(),
            cardinality: None,
            required_properties: Vec::new(),
            property_types: HashMap::new(),
            auto_timestamp: Some(true),
        },
    );
    graph.schema_definition = Some(schema);
    declare_before_any_load(&mut graph);
    let row = vec![(day("2010-01-01"), Value::Null)];
    assert_eq!(
        counts(&load(&mut graph, row.clone(), None).unwrap()),
        (1, 0)
    );
    assert_eq!(counts(&load(&mut graph, row, None).unwrap()), (0, 0));
    assert_eq!(periods(&graph).len(), 1);
}

#[test]
fn two_new_periods_in_one_call_are_two_relationships() {
    let mut graph = first_period(Some(in_type(None)));
    let report = load(
        &mut graph,
        vec![
            (day("2006-01-01"), day("2009-12-31")),
            (day("2010-01-01"), Value::Null),
        ],
        None,
    )
    .unwrap();
    assert_eq!(counts(&report), (2, 0));
    assert_eq!(periods(&graph).len(), 3);
}

#[test]
fn a_declaration_keyed_to_another_source_leaves_this_source_merging() {
    let mut graph = first_period(None);
    docs(&mut graph, "Other");
    load_frame(
        &mut graph,
        frame(vec![(day("2001-01-01"), Value::Null)], None),
        "Other",
        None,
    )
    .unwrap();
    declare(&mut graph, &in_type(Some("Other")), "vf", "vt", Closed).unwrap();
    let report = load(&mut graph, vec![(day("2010-01-01"), Value::Null)], None).unwrap();
    assert_eq!(counts(&report), (0, 1));
    // Other → Doc now takes the key.
    let report = load_frame(
        &mut graph,
        frame(vec![(day("2011-01-01"), Value::Null)], None),
        "Other",
        None,
    )
    .unwrap();
    assert_eq!(counts(&report), (1, 0));
}

#[test]
fn replace_connections_keeps_each_period_of_the_frame() {
    let mut graph = first_period(Some(in_type(None)));
    let report = replace_connections(
        &mut graph,
        frame(
            vec![
                (day("2000-01-01"), day("2005-01-01")),
                (day("2010-01-01"), Value::Null),
            ],
            None,
        ),
        "IN".to_string(),
        "Doc".to_string(),
        "src".to_string(),
        "Doc".to_string(),
        "tgt".to_string(),
        None,
        None,
        None,
    )
    .unwrap();
    assert_eq!(counts(&report), (2, 0));
    assert_eq!(periods(&graph).len(), 2);
}

#[test]
fn the_spec_path_keys_a_declared_type_on_its_from_bound() {
    let spec = |vf: &str| EdgeSpec {
        source_type: "Doc".into(),
        source_id: Value::Int64(1),
        target_type: "Doc".into(),
        target_id: Value::Int64(2),
        edge_type: "IN".into(),
        properties: HashMap::from([("vf".to_string(), day(vf))]),
    };
    let mut graph = first_period(Some(in_type(Some("Doc"))));
    let report = add_edges_from_specs(&mut graph, vec![spec("2010-01-01")]).unwrap();
    assert_eq!(
        (report.connections_created, report.connections_updated),
        (1, 0)
    );
    let report = add_edges_from_specs(&mut graph, vec![spec("2010-01-01")]).unwrap();
    assert_eq!(
        (report.connections_created, report.connections_updated),
        (0, 0)
    );
    assert_eq!(periods(&graph).len(), 2);
}

/// The constraint gate judges a declared type's rows as the loader writes
/// them: a row lacking a NOT NULL property is a new relationship, never a
/// merge that keeps the stored value, even for the stored period.
#[test]
fn the_constraint_gate_judges_a_new_period_as_its_own_relationship() {
    let mut graph = DirGraph::new();
    docs(&mut graph, "Doc");
    load_frame(
        &mut graph,
        frame(
            vec![(day("2000-01-01"), day("2005-01-01"))],
            Some(Value::Int64(7)),
        ),
        "Doc",
        None,
    )
    .unwrap();
    let params: HashMap<String, Value> = HashMap::new();
    execute_mut(
        &mut graph,
        "CREATE CONSTRAINT FOR ()-[r:IN]-() REQUIRE r.x IS NOT NULL",
        &ExecuteOptions::eager(&params),
    )
    .unwrap();
    // Undeclared: the row merges into the stored edge, which keeps x.
    let report = load(&mut graph, vec![(day("2010-01-01"), Value::Null)], None).unwrap();
    assert_eq!(counts(&report), (0, 1));

    let mut graph = DirGraph::new();
    docs(&mut graph, "Doc");
    load_frame(
        &mut graph,
        frame(
            vec![(day("2000-01-01"), day("2005-01-01"))],
            Some(Value::Int64(7)),
        ),
        "Doc",
        None,
    )
    .unwrap();
    execute_mut(
        &mut graph,
        "CREATE CONSTRAINT FOR ()-[r:IN]-() REQUIRE r.x IS NOT NULL",
        &ExecuteOptions::eager(&params),
    )
    .unwrap();
    declare(&mut graph, &in_type(None), "vf", "vt", Closed).unwrap();
    let err = load(&mut graph, vec![(day("2010-01-01"), Value::Null)], None).unwrap_err();
    assert!(err.contains("IN.x"), "{err}");
    assert_eq!(periods(&graph).len(), 1);
    let err = load(
        &mut graph,
        vec![(day("2000-01-01"), day("2006-01-01"))],
        None,
    )
    .unwrap_err();
    assert!(err.contains("IN.x"), "{err}");
    // An identical row is dropped.
    let report = load_frame(
        &mut graph,
        frame(
            vec![(day("2000-01-01"), day("2005-01-01"))],
            Some(Value::Int64(7)),
        ),
        "Doc",
        None,
    )
    .unwrap();
    assert_eq!(counts(&report), (0, 0));
    assert_eq!(periods(&graph).len(), 1);
}

/// One bound written as a date, a midnight datetime or an ISO string is one
/// instant: re-loading a version in another spelling is dropped instead of
/// adding a parallel relationship, and another `to` is a new version.
#[test]
fn one_start_in_any_spelling_is_one_period() {
    let spellings = [
        day("2009-01-01"),
        at("2009-01-01T00:00"),
        text("2009-01-01"),
        text("2009-01-01T00:00:00"),
    ];
    for stored in &spellings {
        for reloaded in &spellings {
            for mode in MODES {
                let mut graph = DirGraph::new();
                docs(&mut graph, "Doc");
                load(&mut graph, vec![(stored.clone(), day("2010-01-01"))], None).unwrap();
                declare(&mut graph, &in_type(None), "vf", "vt", Closed).unwrap();
                let report = load(
                    &mut graph,
                    vec![(reloaded.clone(), text("2010-01-01"))],
                    mode,
                )
                .unwrap();
                assert_eq!(
                    counts(&report),
                    (0, 0),
                    "{stored:?} then {reloaded:?} {mode:?}"
                );
                let report = load(
                    &mut graph,
                    vec![(reloaded.clone(), day("2012-01-01"))],
                    mode,
                )
                .unwrap();
                assert_eq!(
                    counts(&report),
                    (1, 0),
                    "{stored:?} then {reloaded:?} {mode:?}"
                );
                assert_eq!(
                    periods(&graph).len(),
                    2,
                    "{stored:?} then {reloaded:?} {mode:?}"
                );
            }
        }
    }
}

#[test]
fn a_datetime_within_the_day_is_its_own_start() {
    let mut graph = first_period(Some(in_type(None)));
    let report = load(
        &mut graph,
        vec![(at("2000-01-01T12:30"), Value::Null)],
        None,
    )
    .unwrap();
    assert_eq!(counts(&report), (1, 0));
    let report = load(
        &mut graph,
        vec![(at("2000-01-01T12:30"), Value::Null)],
        None,
    )
    .unwrap();
    assert_eq!(counts(&report), (0, 0));
    assert_eq!(periods(&graph).len(), 2);
}

/// A load onto a declared type refuses a start the evaluator cannot read;
/// one a graph already holds (an earlier version accepted such writes) keys
/// its row on the raw value.
#[test]
fn an_unreadable_start_keys_on_its_raw_value() {
    let mut graph = first_period(Some(in_type(None)));
    let err = load(&mut graph, vec![(text("garbage"), Value::Null)], None).unwrap_err();
    assert!(
        err.contains("row 0 (0-based) of the load, property"),
        "{err}"
    );
    super::unchecked(|| {
        for (value, expected) in [
            (text("garbage"), (1, 0)),
            (text("garbage"), (0, 0)),
            (Value::Int64(2009), (1, 0)),
            (Value::Int64(2009), (0, 0)),
        ] {
            let report = load(&mut graph, vec![(value.clone(), Value::Null)], None).unwrap();
            assert_eq!(counts(&report), expected, "{value:?}");
        }
    });
    assert_eq!(periods(&graph).len(), 3);
}

/// The gate and the loader read a bound alike: the stored version in another
/// spelling is dropped, and a row lacking the constrained property is judged
/// as the new relationship it would be.
#[test]
fn the_constraint_gate_reads_the_start_as_the_loader_does() {
    let mut graph = DirGraph::new();
    docs(&mut graph, "Doc");
    load_frame(
        &mut graph,
        frame(
            vec![(at("2000-01-01T00:00"), day("2005-01-01"))],
            Some(Value::Int64(7)),
        ),
        "Doc",
        None,
    )
    .unwrap();
    let params: HashMap<String, Value> = HashMap::new();
    execute_mut(
        &mut graph,
        "CREATE CONSTRAINT FOR ()-[r:IN]-() REQUIRE r.x IS NOT NULL",
        &ExecuteOptions::eager(&params),
    )
    .unwrap();
    declare(&mut graph, &in_type(None), "vf", "vt", Closed).unwrap();
    for start in [day("2000-01-01"), text("2000-01-01")] {
        let row = frame(vec![(start, day("2005-01-01"))], Some(Value::Int64(7)));
        let report = load_frame(&mut graph, row, "Doc", None).unwrap();
        assert_eq!(counts(&report), (0, 0));
    }
    let err = load(&mut graph, vec![(text("2000-01-02"), Value::Null)], None).unwrap_err();
    assert!(err.contains("IN.x"), "{err}");
    assert_eq!(periods(&graph).len(), 1);
}

/// A legacy type holding several unkeyed declarations keys each row on the
/// first declaration whose `from` it carries, so rows bounded by the second
/// declaration keep their periods apart too.
#[test]
fn an_ambiguous_type_keys_each_row_on_the_declaration_it_carries() {
    let mut graph = first_period(Some(in_type(None)));
    let second = crate::graph::schema::TemporalConfig {
        valid_from: "sf".to_string(),
        valid_to: "st".to_string(),
        convention: Closed,
        source_type: None,
    };
    graph.temporal.insert(&in_type(None), second, None);
    assert!(graph.temporal.is_ambiguous("IN"));
    let second_frame = |sf: Value| {
        let rows = vec![vec![Value::Int64(1), Value::Int64(2), sf]];
        let columns = ["src", "tgt", "sf"].map(str::to_string).to_vec();
        DataFrame::from_cypher_rows(columns, rows).unwrap()
    };
    for (sf, expected) in [
        (day("2011-01-01"), (1, 0)),
        (day("2012-01-01"), (1, 0)),
        (text("2012-01-01"), (0, 0)),
    ] {
        let report = load_frame(&mut graph, second_frame(sf.clone()), "Doc", None).unwrap();
        assert_eq!(counts(&report), expected, "{sf:?}");
    }
    // The first declaration's rows still key on its own bounds.
    let report = load(
        &mut graph,
        vec![(day("2000-01-01"), day("2005-01-01"))],
        None,
    )
    .unwrap();
    assert_eq!(counts(&report), (0, 0));
    let params = HashMap::new();
    let total = execute_read(
        &graph,
        "MATCH ()-[r:IN]->() RETURN count(r) AS n",
        &ExecuteOptions::eager(&params),
    )
    .unwrap()
    .result
    .rows;
    assert_eq!(total, vec![vec![Value::Int64(3)]]);
}
