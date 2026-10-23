//! `SHOW INDEXES` — the row projection for the index listing.
//!
//! Its own file rather than a section of `schema_ddl.rs` because the listing is
//! a pure projection of
//! [`collect_indexes_structured`](crate::graph::introspection::schema_overview::collect_indexes_structured)
//! and shares nothing with the DDL that installs indexes. `CALL db.indexes()`
//! projects the same collector through `call_clause::indexes_to_rows`, which is
//! why the column list below is the one both agree on.

use super::super::result::{ResultRow, ResultSet};
use crate::datatypes::prop_map::PropMap;
use crate::datatypes::values::Value;
use crate::graph::dir_graph::DirGraph;
use crate::graph::introspection::schema_overview::{
    collect_indexes_structured, IndexInfo, IndexKind,
};

/// Columns a bare `SHOW INDEXES` and a bare `CALL db.indexes()` return. The
/// first six are Neo4j's own. `stale`, `delta` and `unembedded` are
/// KGLite-specific (null for the index kinds that are maintained on every
/// write; a BM25 index catches up at query entry, so "is it behind, and by how
/// much" is a real question about it; `unembedded` counts, on VECTOR rows,
/// nodes with no vector at all).
///
/// `SHOW INDEXES YIELD` and `CALL db.indexes() YIELD` also accept Neo4j 5's
/// other columns, which `YIELD *` includes (see [`index_cell`]):
/// `populationPercent` is `100.0` for an ONLINE index (built atomically) and
/// null for a DEFERRED one; `options` carries the vector dimension and
/// similarity function on a VECTOR row and is null elsewhere; `id`,
/// `indexProvider`, `lastRead` and `readCount` are null, because KGLite keeps
/// no such state (there are no providers or usage counters).
/// `owningConstraint` names the UNIQUE / NODE KEY constraint a hash-equality or
/// composite index belongs to, and is null for an index nothing owns. The
/// registry entry of `db.indexes` spells out the full list. Documented in
/// CYPHER.md.
pub(crate) const SHOW_INDEXES_COLUMNS: &[&str] = &[
    "name",
    "type",
    "entityType",
    "labelsOrTypes",
    "properties",
    "state",
    "stale",
    "delta",
    "unembedded",
];

/// `SHOW INDEXES` — a read, over the shared collector named above.
pub(crate) fn show_indexes_result_set(graph: &DirGraph) -> ResultSet {
    let mut out = ResultSet::new();
    out.rows = collect_indexes_structured(graph)
        .iter()
        .map(|info| {
            let mut row = ResultRow::new();
            for column in SHOW_INDEXES_COLUMNS {
                if let Some(value) = index_cell(info, column) {
                    row.projected.insert((*column).to_string(), value);
                }
            }
            row
        })
        .collect();
    out.columns = SHOW_INDEXES_COLUMNS.iter().map(|c| c.to_string()).collect();
    out
}

/// One cell of an index row, or `None` for a name outside
/// the registry's list for `db.indexes` (the YIELD validator rejects those first).
pub(crate) fn index_cell(info: &IndexInfo, column: &str) -> Option<Value> {
    let strings =
        |items: &[String]| Value::List(items.iter().cloned().map(Value::String).collect());
    Some(match column {
        "name" => Value::String(info.name.clone()),
        "type" => Value::String(info.type_label().to_string()),
        "entityType" => Value::String(info.entity_type.to_string()),
        "labelsOrTypes" => strings(&info.labels_or_types),
        "properties" => strings(&info.properties),
        "state" => Value::String(info.state.to_string()),
        "stale" => info.stale.map_or(Value::Null, Value::Boolean),
        "delta" => info
            .delta
            .map_or(Value::Null, |delta| Value::Int64(delta as i64)),
        "unembedded" => info
            .unembedded
            .map_or(Value::Null, |count| Value::Int64(count as i64)),
        "populationPercent" => {
            if info.state == "ONLINE" {
                Value::Float64(100.0)
            } else {
                Value::Null
            }
        }
        "options" => match info.kind {
            IndexKind::Text => index_config([
                (
                    "fulltext.analyzer",
                    Value::String(FULLTEXT_ANALYZER.to_string()),
                ),
                ("fulltext.eventually_consistent", Value::Boolean(true)),
            ]),
            _ => info
                .vector_config
                .as_ref()
                .map_or(Value::Null, |(dim, metric)| {
                    index_config([
                        ("vector.dimensions", Value::Int64(*dim as i64)),
                        (
                            "vector.similarity_function",
                            Value::String(metric.to_ascii_uppercase()),
                        ),
                    ])
                }),
        },
        "indexProvider" => match info.kind {
            IndexKind::Text => Value::String(FULLTEXT_PROVIDER.to_string()),
            IndexKind::Vector => Value::String(VECTOR_PROVIDER.to_string()),
            _ => Value::Null,
        },
        "owningConstraint" => info
            .owning_constraint
            .clone()
            .map_or(Value::Null, Value::String),
        "id" | "lastRead" | "readCount" => Value::Null,
        _ => return None,
    })
}

/// The `indexProvider` a `FULLTEXT` row reports: Neo4j's provider string for its
/// Lucene full-text index, which a client keys on to recognise the kind.
const FULLTEXT_PROVIDER: &str = "fulltext-1.0";
/// The `indexProvider` a `VECTOR` row reports: Neo4j's string for its vector
/// index.
const VECTOR_PROVIDER: &str = "vector-2.0";
/// The `fulltext.analyzer` a `FULLTEXT` row reports. KGLite's one tokenizer
/// lowercases, splits on every non-alphanumeric character, and applies no stop
/// list or stemming, which is Neo4j's `standard-no-stop-words` by behaviour.
const FULLTEXT_ANALYZER: &str = "standard-no-stop-words";

/// `{indexConfig: {…}}` — the shape Neo4j nests an index's settings under.
fn index_config<const N: usize>(settings: [(&'static str, Value); N]) -> Value {
    Value::Map(PropMap::from_iter([(
        "indexConfig",
        Value::Map(PropMap::from_iter(settings)),
    )]))
}

#[cfg(test)]
#[path = "show_pipeline_tests.rs"]
mod show_pipeline_tests;
