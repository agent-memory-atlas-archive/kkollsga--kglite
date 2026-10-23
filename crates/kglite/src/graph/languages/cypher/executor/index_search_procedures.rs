//! Neo4j-named index search: `db.index.fulltext.queryNodes` / `queryRelationships`
//! and `db.index.vector.queryNodes` / `queryRelationships`.
//!
//! These are names over machinery that already exists. `indexName` resolves
//! against the rows `SHOW INDEXES` lists for the matching kind and entity, and
//! the call then runs the BM25 index (the same query-entry freshness and
//! valid-time policy `text_bm25()` follows) or delegates to the
//! `db.node_embeddings.query` / `db.relationship_embeddings.query` ranking,
//! HNSW once the index is online. Nothing here scores a document itself.
//!
//! **Scores.** BM25 scores are KGLite's, not Lucene's, and a vector score is
//! the store's own metric (cosine similarity for a cosine store), not Neo4j's
//! `(1 + cos) / 2` normalisation. Each ranks best first, which is the order a
//! client sorts by. A full-text `queryString` is analysed by the index's
//! tokenizer; Lucene query syntax (`AND`, `title:x`, wildcards) is not parsed.

use std::collections::HashMap;

use petgraph::graph::{EdgeIndex, NodeIndex};

use super::edge_embedding_procedures::require_string;
use super::procedure_params::reject_unknown_keys;
use super::*;
use crate::graph::introspection::schema_overview::{
    collect_indexes_structured, IndexInfo, IndexKind,
};
use crate::graph::text_indexes::edge_text::{edge_text_index_store, refresh_edge_text_index};
use crate::graph::text_indexes::{refresh_text_index, text_index_store};

const FULLTEXT_KEYS: &[&str] = &["indexName", "queryString", "options"];
const FULLTEXT_OPTIONS: &[&str] = &["limit", "skip"];
const VECTOR_KEYS: &[&str] = &["indexName", "numberOfNearestNeighbours", "query"];

fn proc_name(kind: IndexKind, relationship: bool) -> &'static str {
    match (kind, relationship) {
        (IndexKind::Text, false) => "db.index.fulltext.queryNodes",
        (IndexKind::Text, true) => "db.index.fulltext.queryRelationships",
        (_, false) => "db.index.vector.queryNodes",
        (_, true) => "db.index.vector.queryRelationships",
    }
}

/// The index `name` refers to: a listed row of `kind` over nodes (or
/// relationships). The error names what is listed, because a mistyped or
/// not-yet-built index is the usual reason to be here.
fn resolve_index(
    graph: &DirGraph,
    kind: IndexKind,
    relationship: bool,
    name: &str,
) -> Result<IndexInfo, String> {
    let entity = if relationship { "RELATIONSHIP" } else { "NODE" };
    let listed: Vec<IndexInfo> = collect_indexes_structured(graph)
        .into_iter()
        .filter(|info| info.kind == kind && info.entity_type == entity)
        .collect();
    if let Some(info) = listed
        .iter()
        .find(|info| info.name == name)
        .or_else(|| listed.iter().find(|info| info.canonical_name() == name))
    {
        return Ok(info.clone());
    }
    let proc = proc_name(kind, relationship);
    let available = if listed.is_empty() {
        "none exist".to_string()
    } else {
        listed
            .iter()
            .map(|info| format!("'{}'", info.name))
            .collect::<Vec<_>>()
            .join(", ")
    };
    Err(format!(
        "CALL {proc}: no {} index named '{name}'. {} indexes over {} today: {available}.",
        kind.neo4j_type(),
        kind.neo4j_type(),
        if relationship {
            "relationships"
        } else {
            "nodes"
        },
    ))
}

/// A non-negative integer option, `None` when absent or null.
fn count_option(value: Option<&Value>, proc: &str, key: &str) -> Result<Option<usize>, String> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Int64(n)) if *n >= 0 => Ok(Some(*n as usize)),
        Some(other) => Err(format!(
            "CALL {proc}: option '{key}' must be a non-negative integer, got {}",
            other.type_name()
        )),
    }
}

/// `(limit, skip)` from the `options` map. Unknown keys are refused rather
/// than ignored: an `analyzer` or `eventually_consistent` option silently
/// dropped would leave a caller believing the query ran under it.
fn fulltext_options(options: Option<&Value>, proc: &str) -> Result<(Option<usize>, usize), String> {
    let map = match options {
        None | Some(Value::Null) => return Ok((None, 0)),
        Some(Value::Map(map)) => map,
        Some(other) => {
            return Err(format!(
                "CALL {proc}: 'options' must be a map, got {}",
                other.type_name()
            ))
        }
    };
    reject_unknown_keys(
        &format!("CALL {proc} options"),
        map.keys(),
        FULLTEXT_OPTIONS,
    )?;
    Ok((
        count_option(map.get("limit"), proc, "limit")?,
        count_option(map.get("skip"), proc, "skip")?.unwrap_or(0),
    ))
}

impl CypherExecutor<'_> {
    /// The query-entry freshness policy shared by `text_bm25()` and the
    /// full-text procedures (release-train-0-16-10, decision 11a): a delta
    /// within the index's `auto_refresh_limit` is folded in before the query
    /// is prepared; a larger one, or a read-only graph, serves the index as it
    /// stands with a warning naming the delta and the call that fixes it. A
    /// missing index is left to the caller's own error.
    pub(super) fn catch_up_text_index(&self, relationship: bool, owner: &str, property: &str) {
        let graph = self.graph;
        let Some(store) = (if relationship {
            edge_text_index_store(graph, owner, property)
        } else {
            text_index_store(graph, owner, property)
        }) else {
            return;
        };
        let (stale, delta, can_fold) = if relationship {
            (
                store.relationship_is_stale(graph),
                store.relationship_delta_size(graph),
                store.relationship_can_auto_refresh(graph),
            )
        } else {
            (
                store.is_stale(graph),
                store.delta_size(graph),
                store.can_auto_refresh(graph),
            )
        };
        if !stale {
            return;
        }
        if !graph.read_only && can_fold {
            if relationship {
                refresh_edge_text_index(graph, owner, property);
            } else {
                refresh_text_index(graph, owner, property);
            }
            return;
        }
        // "up to": the delta over-counts — it is a watermark gap plus a dirty
        // set, and a slot in either may turn out to hold no document at all.
        let reason = if graph.read_only {
            "and this graph is read-only, so a query cannot catch it up".to_string()
        } else {
            format!(
                "over its auto_refresh_limit of {}",
                store.auto_refresh_limit()
            )
        };
        if relationship {
            self.warn(format!(
                "relationship text index '{owner}.{property}' is stale: up to {delta} \
                 documents are unindexed, {reason} — a new relationship scores null and a \
                 changed one scores its previously indexed text. Refresh with \
                 CALL db.relationship_text_index.refresh({{type: '{owner}', text_column: \
                 '{property}'}})."
            ));
        } else {
            self.warn(format!(
                "text index '{owner}.{property}' is stale: up to {delta} documents are \
                 unindexed, {reason} — a new node scores null and a changed one scores its \
                 previously indexed text. Rebuild with build_text_index('{owner}', \
                 '{property}')."
            ));
        }
    }
}

impl CypherExecutor<'_> {
    /// `db.index.fulltext.queryNodes` / `queryRelationships`.
    pub(super) fn fulltext_index_query(
        &self,
        relationship: bool,
        params: &HashMap<String, Value>,
        yields: &[YieldItem],
    ) -> Result<Vec<ResultRow>, String> {
        let proc = proc_name(IndexKind::Text, relationship);
        reject_unknown_keys(
            &format!("CALL {proc}"),
            params.keys().map(String::as_str),
            FULLTEXT_KEYS,
        )?;
        let index_name = require_string(params, "indexName", proc)?;
        let query = match params.get("queryString") {
            Some(Value::String(text)) => text.as_str(),
            Some(other) => {
                return Err(format!(
                    "CALL {proc}: 'queryString' must be a string, got {}",
                    other.type_name()
                ))
            }
            None => return Err(format!("CALL {proc}: missing parameter 'queryString'")),
        };
        let (limit, skip) = fulltext_options(params.get("options"), proc)?;
        let info = resolve_index(self.graph, IndexKind::Text, relationship, &index_name)?;
        let (owner, property) = (
            info.labels_or_types[0].as_str(),
            info.properties[0].as_str(),
        );

        self.catch_up_text_index(relationship, owner, property);
        let hits = if relationship {
            let store = edge_text_index_store(self.graph, owner, property)
                .ok_or_else(|| format!("CALL {proc}: index '{index_name}' vanished mid-query"))?;
            self.ranked_text_hits(store, (relationship, owner, property), query, (limit, skip))?
        } else {
            let store = text_index_store(self.graph, owner, property)
                .ok_or_else(|| format!("CALL {proc}: index '{index_name}' vanished mid-query"))?;
            self.ranked_text_hits(store, (relationship, owner, property), query, (limit, skip))?
        };

        let mut rows = Vec::with_capacity(hits.len());
        for (slot, score) in hits {
            let mut row = ResultRow::new();
            for item in yields {
                let alias = item.alias.clone().unwrap_or_else(|| item.name.clone());
                match item.name.as_str() {
                    "node" => {
                        row.node_bindings.insert(alias, slot);
                    }
                    "relationship" => {
                        let edge = EdgeIndex::new(slot.index());
                        let value = super::helpers::materialize_rel_value_with_incarnation(
                            edge,
                            self.graph,
                            self.relationship_incarnation(edge),
                        )
                        .ok_or_else(|| {
                            format!(
                                "CALL {proc}: could not materialize relationship slot {}",
                                slot.index()
                            )
                        })?;
                        row.projected
                            .insert(alias, Value::Relationship(Box::new(value)));
                    }
                    "score" => {
                        row.projected.insert(alias, Value::Float64(score));
                    }
                    _ => {}
                }
            }
            rows.push(row);
        }
        Ok(rows)
    }

    /// The matching documents' slots and scores, best first, after `skip`.
    fn ranked_text_hits(
        &self,
        store: &crate::graph::text_indexes::TextIndexStore,
        (relationship, owner, property): (bool, &str, &str),
        query: &str,
        (limit, skip): (Option<usize>, usize),
    ) -> Result<Vec<(NodeIndex, f64)>, String> {
        let view = store.read();
        let generation = store.generation();
        let (prepared, _) =
            self.prepare_text_query(&view, query, (relationship, owner, property), generation)?;
        let wanted = limit.unwrap_or(usize::MAX).saturating_add(skip);
        if wanted == skip {
            return Ok(Vec::new());
        }
        let hits = match self.text_filter(relationship, owner)? {
            Some(filter) => {
                let admits = self.text_admits(&filter, relationship);
                view.top_k_allowed(&prepared, wanted, |node| admits(node.index() as u32))
            }
            None => view.top_k(&prepared, wanted),
        };
        Ok(hits.into_iter().skip(skip).collect())
    }

    /// `db.index.vector.queryNodes` / `queryRelationships`: resolve the index,
    /// then rank its whole store through the embedding-query procedure.
    pub(super) fn vector_index_query(
        &self,
        relationship: bool,
        params: &HashMap<String, Value>,
        yields: &[YieldItem],
    ) -> Result<Vec<ResultRow>, String> {
        let proc = proc_name(IndexKind::Vector, relationship);
        reject_unknown_keys(
            &format!("CALL {proc}"),
            params.keys().map(String::as_str),
            VECTOR_KEYS,
        )?;
        let index_name = require_string(params, "indexName", proc)?;
        let top_k = match params.get("numberOfNearestNeighbours") {
            Some(Value::Int64(k)) if *k >= 1 => *k,
            Some(Value::Int64(_)) => {
                return Err(format!(
                    "CALL {proc}: 'numberOfNearestNeighbours' must be a positive integer"
                ))
            }
            Some(other) => {
                return Err(format!(
                    "CALL {proc}: 'numberOfNearestNeighbours' must be an integer, got {}",
                    other.type_name()
                ))
            }
            None => {
                return Err(format!(
                    "CALL {proc}: missing parameter 'numberOfNearestNeighbours'"
                ))
            }
        };
        let vector = match params.get("query") {
            Some(Value::List(items))
                if items
                    .iter()
                    .all(|v| matches!(v, Value::Float64(_) | Value::Int64(_))) =>
            {
                Value::List(items.clone())
            }
            Some(other) => {
                return Err(format!(
                    "CALL {proc}: 'query' must be a list of numbers, got {}",
                    other.type_name()
                ))
            }
            None => return Err(format!("CALL {proc}: missing parameter 'query'")),
        };
        let info = resolve_index(self.graph, IndexKind::Vector, relationship, &index_name)?;
        let delegated = HashMap::from([
            (
                "type".to_string(),
                Value::String(info.labels_or_types[0].clone()),
            ),
            (
                "text_column".to_string(),
                Value::String(info.properties[0].clone()),
            ),
            ("vector".to_string(), vector),
            ("top_k".to_string(), Value::Int64(top_k)),
        ]);
        if relationship {
            self.execute_edge_embedding_query(&delegated, yields)
        } else {
            super::node_embedding_procedures::query(
                self.graph,
                &delegated,
                yields,
                self.retrieval_filter()?.as_deref(),
            )
        }
    }
}
