//! `SHOW INDEXES | CONSTRAINTS | PROCEDURES | FUNCTIONS` with a YIELD / WHERE /
//! RETURN / ORDER BY / SKIP / LIMIT pipeline, which the parser reads as the
//! `CALL` of the procedure that lists the same rows.

use crate::datatypes::values::Value;
use crate::graph::dir_graph::DirGraph;
use crate::graph::session::{execute_mut, execute_read, ExecuteOptions};
use std::collections::HashMap;

fn opts(params: &'static HashMap<String, Value>) -> ExecuteOptions<'static> {
    ExecuteOptions::eager(params)
}

fn empty() -> &'static HashMap<String, Value> {
    Box::leak(Box::new(HashMap::new()))
}

fn graph() -> DirGraph {
    let mut graph = DirGraph::new();
    for query in [
        "UNWIND range(1, 5) AS i CREATE (:Person {id: i, name: 'p' + toString(i), age: i * 10})",
        "CREATE INDEX person_age FOR (n:Person) ON (n.age)",
        "CREATE RANGE INDEX person_range FOR (n:Person) ON (n.name)",
        "CREATE CONSTRAINT person_name_unique FOR (p:Person) REQUIRE p.name IS UNIQUE",
        "CREATE CONSTRAINT person_age_typed FOR (p:Person) REQUIRE p.age IS :: INTEGER",
    ] {
        execute_mut(&mut graph, query, &opts(empty())).unwrap_or_else(|e| panic!("{query}: {e}"));
    }
    graph
}

fn query(
    graph: &DirGraph,
    text: &str,
    params: HashMap<String, Value>,
) -> Result<(Vec<String>, Vec<Vec<Value>>), String> {
    let params: &'static HashMap<String, Value> = Box::leak(Box::new(params));
    execute_read(graph, text, &opts(params))
        .map(|out| (out.result.columns.clone(), out.result.rows.clone()))
        .map_err(|e| e.to_string())
}

fn strings(rows: &[Vec<Value>], column: usize) -> Vec<String> {
    rows.iter()
        .map(|row| match &row[column] {
            Value::String(text) => text.clone(),
            other => panic!("not a string: {other:?}"),
        })
        .collect()
}

#[test]
fn show_indexes_yield_where_return_order_and_paging() {
    let g = graph();
    let (columns, rows) = query(
        &g,
        "SHOW INDEXES YIELD name, type, labelsOrTypes WHERE type = 'RANGE' \
         RETURN name, labelsOrTypes ORDER BY name",
        HashMap::new(),
    )
    .unwrap();
    assert_eq!(columns, ["name", "labelsOrTypes"]);
    assert_eq!(strings(&rows, 0), ["Person.name"]);
    assert_eq!(
        rows[0][1],
        Value::List(vec![Value::String("Person".to_string())])
    );

    let (_, all) = query(
        &g,
        "SHOW INDEXES YIELD name RETURN name ORDER BY name",
        HashMap::new(),
    )
    .unwrap();
    // The uniqueness constraint backs its own index on Person.name.
    assert_eq!(
        strings(&all, 0),
        ["Person.age", "Person.name", "Person.name"]
    );
    let (_, page) = query(
        &g,
        "SHOW INDEXES YIELD name RETURN name ORDER BY name DESC SKIP 1 LIMIT $limit",
        HashMap::from([("limit".to_string(), Value::Int64(2))]),
    )
    .unwrap();
    assert_eq!(strings(&page, 0), ["Person.name", "Person.age"]);
    let (_, none) = query(
        &g,
        "SHOW INDEXES YIELD name WHERE name = 'nope' RETURN name",
        HashMap::new(),
    )
    .unwrap();
    assert!(none.is_empty());
}

/// The statement GraphBridge sends, verbatim, and the columns it names that
/// Neo4j has and KGLite keeps no state for.
#[test]
fn the_graphbridge_index_statements_run() {
    let g = graph();
    let (columns, rows) = query(
        &g,
        "SHOW INDEXES YIELD name, state, type, entityType, labelsOrTypes, properties, \
         indexProvider, options WHERE name = $index \
         RETURN name, state, type, entityType, labelsOrTypes, properties, indexProvider, options \
         LIMIT 2",
        HashMap::from([("index".to_string(), Value::String("Person.age".to_string()))]),
    )
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(columns[6], "indexProvider");
    assert_eq!(rows[0][1], Value::String("ONLINE".to_string()));
    assert_eq!(rows[0][2], Value::String("PROPERTY".to_string()));
    assert_eq!(rows[0][6], Value::Null, "no provider is recorded");
    assert_eq!(rows[0][7], Value::Null, "only a vector index has options");

    let (columns, rows) = query(
        &g,
        "SHOW INDEXES YIELD name, state, type, entityType, labelsOrTypes, properties, \
         owningConstraint RETURN name, state, type, entityType, labelsOrTypes, properties, \
         owningConstraint ORDER BY name LIMIT $limit",
        HashMap::from([("limit".to_string(), Value::Int64(10))]),
    )
    .unwrap();
    assert_eq!(columns.last().map(String::as_str), Some("owningConstraint"));
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|r| r[6] == Value::Null));
}

#[test]
fn show_indexes_reports_population_and_every_neo4j_column() {
    let g = graph();
    let (bare, _) = query(&g, "SHOW INDEXES", HashMap::new()).unwrap();
    assert_eq!(bare.len(), 9, "a bare listing keeps its nine columns");
    assert!(!bare.iter().any(|c| c == "options"));
    let (columns, rows) = query(
        &g,
        "SHOW INDEXES YIELD * WHERE name = 'Person.age'",
        HashMap::new(),
    )
    .unwrap();
    for column in [
        "id",
        "populationPercent",
        "indexProvider",
        "owningConstraint",
        "options",
        "lastRead",
        "readCount",
    ] {
        assert!(columns.iter().any(|c| c == column), "missing {column}");
    }
    let at = |name: &str| columns.iter().position(|c| c == name).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][at("populationPercent")], Value::Float64(100.0));
    for null in [
        "id",
        "indexProvider",
        "owningConstraint",
        "lastRead",
        "readCount",
    ] {
        assert_eq!(rows[0][at(null)], Value::Null, "{null}");
    }
}

#[test]
fn show_constraints_yield_where_return_order_and_paging() {
    let g = graph();
    let (columns, rows) = query(
        &g,
        "SHOW CONSTRAINTS YIELD name, type, entityType, labelsOrTypes, properties, propertyType \
         WHERE type = 'NODE_PROPERTY_TYPE' \
         RETURN name, propertyType ORDER BY name",
        HashMap::new(),
    )
    .unwrap();
    assert_eq!(columns, ["name", "propertyType"]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::String("person_age_typed".to_string()));
    assert!(matches!(&rows[0][1], Value::String(t) if t.contains("INTEGER")));

    let (_, rows) = query(
        &g,
        "SHOW CONSTRAINTS YIELD name RETURN name ORDER BY name SKIP 1 LIMIT 1",
        HashMap::new(),
    )
    .unwrap();
    assert_eq!(strings(&rows, 0), ["person_name_unique"]);

    let (columns, rows) = query(
        &g,
        "SHOW CONSTRAINTS YIELD name, type, entityType, labelsOrTypes, properties \
         RETURN name, type, entityType, labelsOrTypes, properties, null AS propertyType \
         ORDER BY name LIMIT $limit",
        HashMap::from([("limit".to_string(), Value::Int64(1))]),
    )
    .unwrap();
    assert_eq!(columns.last().map(String::as_str), Some("propertyType"));
    assert_eq!(rows.len(), 1);

    let (columns, rows) = query(
        &g,
        "SHOW CONSTRAINTS YIELD * WHERE name = 'person_name_unique'",
        HashMap::new(),
    )
    .unwrap();
    for column in ["id", "ownedIndex", "options"] {
        let at = columns.iter().position(|c| c == column).expect(column);
        assert_eq!(rows[0][at], Value::Null, "{column}");
    }
}

#[test]
fn show_procedures_and_functions_take_the_full_pipeline() {
    let g = graph();
    let (columns, rows) = query(
        &g,
        "SHOW PROCEDURES YIELD name, mode WHERE name STARTS WITH 'db.constraints' \
         RETURN name, mode",
        HashMap::new(),
    )
    .unwrap();
    assert_eq!(columns, ["name", "mode"]);
    assert_eq!(strings(&rows, 0), ["db.constraints"]);
    assert_eq!(rows[0][1], Value::String("READ".to_string()));

    let (_, rows) = query(
        &g,
        "SHOW PROCEDURES YIELD name RETURN name ORDER BY name LIMIT 3",
        HashMap::new(),
    )
    .unwrap();
    assert_eq!(rows.len(), 3);
    let sorted = {
        let mut names = strings(&rows, 0);
        names.sort();
        names
    };
    assert_eq!(strings(&rows, 0), sorted);

    let (columns, rows) = query(
        &g,
        "SHOW FUNCTIONS YIELD name, category, aliases WHERE name = 'toUpper' RETURN name, aliases",
        HashMap::new(),
    )
    .unwrap();
    assert_eq!(columns, ["name", "aliases"]);
    assert_eq!(strings(&rows, 0), ["toUpper"]);
    assert!(matches!(&rows[0][1], Value::List(a) if !a.is_empty()));

    // The projection-only forms still answer as before.
    let (columns, _) = query(
        &g,
        "SHOW FUNCTIONS YIELD name, description, signature",
        HashMap::new(),
    )
    .unwrap();
    assert_eq!(columns, ["name", "description", "signature"]);
    let (columns, _) = query(&g, "SHOW PROCEDURES", HashMap::new()).unwrap();
    assert_eq!(columns, ["name", "description", "mode", "worksOnSystem"]);
}

#[test]
fn an_unknown_column_is_an_error_naming_the_choices() {
    let g = graph();
    for noun in ["INDEXES", "CONSTRAINTS", "PROCEDURES", "FUNCTIONS"] {
        let err = query(&g, &format!("SHOW {noun} YIELD nope"), HashMap::new()).unwrap_err();
        assert!(
            err.contains("nope") && err.contains("Available"),
            "{noun}: {err}"
        );
    }
    // A column of the other listing is unknown here.
    let err = query(&g, "SHOW CONSTRAINTS YIELD state", HashMap::new()).unwrap_err();
    assert!(err.contains("state"), "{err}");
}

#[test]
fn a_filter_is_never_silently_ignored() {
    let g = graph();
    let (_, filtered) = query(
        &g,
        "SHOW CONSTRAINTS WHERE name = 'person_name_unique'",
        HashMap::new(),
    )
    .unwrap();
    assert_eq!(filtered.len(), 1);
    let (_, all) = query(&g, "SHOW CONSTRAINTS", HashMap::new()).unwrap();
    assert_eq!(all.len(), 2);
    // An unparsed tail is refused rather than dropped.
    assert!(query(&g, "SHOW INDEXES VERBOSE", HashMap::new()).is_err());
    assert!(query(&g, "SHOW INDEXES BRIEF", HashMap::new()).is_err());
}

#[test]
fn the_bare_listing_and_the_call_return_the_same_rows() {
    let g = graph();
    let (show_columns, show_rows) = query(&g, "SHOW INDEXES", HashMap::new()).unwrap();
    let (call_columns, call_rows) = query(&g, "CALL db.indexes()", HashMap::new()).unwrap();
    assert_eq!(show_columns, call_columns);
    assert_eq!(show_rows, call_rows);
    let (show_columns, show_rows) = query(&g, "SHOW CONSTRAINTS", HashMap::new()).unwrap();
    let (call_columns, call_rows) = query(&g, "CALL db.constraints()", HashMap::new()).unwrap();
    assert_eq!(show_columns, call_columns);
    assert_eq!(show_rows, call_rows);
}

#[test]
fn a_vector_index_row_carries_its_dimension_and_similarity_in_options() {
    let mut g = graph();
    crate::graph::embeddings::set_embeddings(
        &mut g,
        "Person",
        "name",
        Some("cosine"),
        (1..=5).map(|i| (Value::Int64(i), vec![i as f32, 1.0, 0.5])),
    )
    .unwrap();
    crate::graph::embeddings::build_vector_index(
        &mut g, "Person", "name", None, None, None, None, None,
    )
    .unwrap();
    let (columns, rows) = query(
        &g,
        "SHOW INDEXES YIELD name, type, options WHERE type = 'VECTOR' RETURN name, options",
        HashMap::new(),
    )
    .unwrap();
    assert_eq!(columns, ["name", "options"]);
    assert_eq!(rows.len(), 1);
    let Value::Map(options) = &rows[0][1] else {
        panic!("options is a map: {:?}", rows[0][1]);
    };
    let config = options.get("indexConfig").expect("indexConfig");
    let Value::Map(config) = config else {
        panic!("indexConfig is a map")
    };
    assert_eq!(config.get("vector.dimensions"), Some(&Value::Int64(3)));
    assert_eq!(
        config.get("vector.similarity_function"),
        Some(&Value::String("COSINE".to_string()))
    );
}
