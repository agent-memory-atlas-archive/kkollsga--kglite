//! Execution of `CREATE FULLTEXT INDEX` and `CREATE VECTOR INDEX`.
//!
//! A full-text statement installs the BM25 text index over one node label's
//! one property through the same path as `db.node_text_index.build`: one
//! `TextIndexStore` per `(label, property)`. The multi-label, multi-property
//! and relationship forms parse and are refused here by name.
//! `DROP INDEX <name>` is the ordinary drop path: the registered name carries
//! the `Fulltext` class, so it removes the BM25 structure.

use super::super::ast::*;
use super::super::result::MutationStats;
use super::schema_ddl::{
    already_exists_message, indexes_added, validate_ddl_properties_declared, DdlPurpose,
};
use crate::graph::dir_graph::index_names::{
    check_index_name, register_index_name, IndexClass, NamedIndex, VectorSource,
};
use crate::graph::dir_graph::DirGraph;

/// The one analyzer KGLite's BM25 tokenizer implements: lowercase, split on
/// every non-alphanumeric character, no stop list, no stemming.
pub(crate) const FULLTEXT_ANALYZER: &str = "standard-no-stop-words";

pub(super) fn execute_create_fulltext_index(
    graph: &mut DirGraph,
    create: &CreateFulltextIndex,
) -> Result<MutationStats, String> {
    validate_fulltext_options(&create.options)?;
    let label = match &create.target {
        FulltextTarget::Node { labels, .. } => match labels.as_slice() {
            [label] => label.clone(),
            many => {
                return Err(format!(
                    "CREATE FULLTEXT INDEX over {} labels ({}) is not supported: a KGLite \
                     full-text index covers exactly one node label. Create one index per label.",
                    many.len(),
                    many.join("|")
                ))
            }
        },
        FulltextTarget::Relationship { rel_types, .. } => {
            return Err(format!(
                "CREATE FULLTEXT INDEX on relationship type {} is not supported through DDL. \
                 Build a relationship text index with CALL db.relationship_text_index.build(...).",
                rel_types.join("|")
            ))
        }
    };
    let [property] = create.properties.as_slice() else {
        return Err(format!(
            "CREATE FULLTEXT INDEX over {} properties ({}) is not supported: a KGLite \
             full-text index covers exactly one property. Create one index per property.",
            create.properties.len(),
            create.properties.join(", ")
        ));
    };
    super::write_scope::enforce_write_scope(graph, &label)?;
    if graph.schema_locked {
        validate_ddl_properties_declared(
            graph,
            &label,
            std::slice::from_ref(property),
            DdlPurpose::Index,
        )?;
    }

    // Claim the name before building so a refusal leaves the graph untouched;
    // under IF NOT EXISTS a taken name is Neo4j's no-op.
    let named = NamedIndex::node(IndexClass::Fulltext, &label, std::slice::from_ref(property));
    if let Some(name) = &create.name {
        if let Err(taken) = check_index_name(graph, name, &named) {
            if create.if_not_exists {
                return Ok(MutationStats::default());
            }
            return Err(taken);
        }
    }
    if crate::graph::text_indexes::text_index_store(graph, &label, property).is_some() {
        if create.if_not_exists {
            return Ok(MutationStats::default());
        }
        return Err(already_exists_message(&format!("{label}.{property}")));
    }

    crate::graph::text_indexes::build_text_index(graph, &label, property, None)?;
    if let Some(name) = &create.name {
        register_index_name(graph, name, named);
    }
    Ok(indexes_added(1))
}

fn validate_fulltext_options(options: &IndexOptions) -> Result<(), String> {
    if let Some(provider) = &options.index_provider {
        return Err(format!(
            "indexProvider '{provider}' is not supported: KGLite has one full-text \
             implementation. Remove the indexProvider option."
        ));
    }
    for (key, value) in &options.index_config {
        match (key.as_str(), value) {
            ("fulltext.analyzer", DdlOptionValue::Str(analyzer)) => {
                if analyzer != FULLTEXT_ANALYZER {
                    return Err(format!(
                        "fulltext.analyzer '{analyzer}' is not supported: KGLite's BM25 \
                         tokenizer is '{FULLTEXT_ANALYZER}' (lowercase, split on \
                         non-alphanumerics, no stop words, no stemming)."
                    ));
                }
            }
            ("fulltext.analyzer", _) => {
                return Err("fulltext.analyzer must be a string".to_string());
            }
            // Both values are served identically: an index is read-your-writes
            // for a small delta and catches up at query entry otherwise.
            ("fulltext.eventually_consistent", DdlOptionValue::Bool(_)) => {}
            ("fulltext.eventually_consistent", _) => {
                return Err("fulltext.eventually_consistent must be true or false".to_string());
            }
            (other, _) => {
                return Err(format!(
                    "unknown full-text indexConfig key '{other}': the accepted keys are \
                     `fulltext.analyzer` and `fulltext.eventually_consistent`"
                ));
            }
        }
    }
    Ok(())
}

/// `CREATE VECTOR INDEX name FOR (n:L) ON (n.p) OPTIONS {indexConfig: {...}}`:
/// install a vector index that reads node property `p`, and register its name.
/// See [`crate::graph::property_vector_indexes`] for what counts as indexed.
pub(super) fn execute_create_vector_index(
    graph: &mut DirGraph,
    create: &CreateVectorIndex,
) -> Result<MutationStats, String> {
    let source = validate_vector_options(&create.options)?;
    let label = match &create.target {
        DdlTarget::Node { label, .. } => label.clone(),
        DdlTarget::Relationship { rel_type, .. } => {
            return Err(format!(
                "CREATE VECTOR INDEX on relationship type {rel_type} is not supported through \
                 DDL. Build a relationship vector index with \
                 CALL db.relationship_embeddings.build_index(...)."
            ))
        }
    };
    let Some(name) = &create.name else {
        return Err(
            "CREATE VECTOR INDEX needs a name: db.index.vector.queryNodes and SEARCH address \
             a vector index by name. Write CREATE VECTOR INDEX <name> FOR ..."
                .to_string(),
        );
    };
    let property = &create.property;
    super::write_scope::enforce_write_scope(graph, &label)?;
    if graph.schema_locked {
        validate_ddl_properties_declared(
            graph,
            &label,
            std::slice::from_ref(property),
            DdlPurpose::Index,
        )?;
    }
    let named = NamedIndex::property_vector(&label, property, source.clone());
    if let Err(taken) = check_index_name(graph, name, &named) {
        if create.if_not_exists {
            return Ok(MutationStats::default());
        }
        return Err(taken);
    }
    if crate::graph::property_vector_indexes::is_property_sourced(graph, &label, property) {
        if create.if_not_exists {
            return Ok(MutationStats::default());
        }
        return Err(already_exists_message(&format!("{label}.{property}")));
    }
    crate::graph::property_vector_indexes::build_property_vector_index(
        graph, &label, property, &source,
    )?;
    register_index_name(graph, name, named);
    Ok(indexes_added(1))
}

fn validate_vector_options(options: &IndexOptions) -> Result<VectorSource, String> {
    if let Some(provider) = &options.index_provider {
        return Err(format!(
            "indexProvider '{provider}' is not supported: KGLite has one vector index \
             implementation. Remove the indexProvider option."
        ));
    }
    let mut dimension = None;
    let mut metric = "cosine";
    for (key, value) in &options.index_config {
        match (key.as_str(), value) {
            ("vector.dimensions", DdlOptionValue::Int(d)) if *d >= 1 => {
                dimension = Some(*d as usize);
            }
            ("vector.dimensions", _) => {
                return Err("vector.dimensions must be a positive integer".to_string());
            }
            ("vector.similarity_function", DdlOptionValue::Str(function)) => {
                metric = crate::graph::property_vector_indexes::metric_for_similarity(function)?;
            }
            ("vector.similarity_function", _) => {
                return Err("vector.similarity_function must be a string".to_string());
            }
            (other, _) => {
                return Err(format!(
                    "unknown vector indexConfig key '{other}': the accepted keys are \
                     `vector.dimensions` and `vector.similarity_function`"
                ));
            }
        }
    }
    let dimension = dimension.ok_or_else(|| {
        "CREATE VECTOR INDEX needs OPTIONS {indexConfig: {`vector.dimensions`: <n>}}".to_string()
    })?;
    Ok(VectorSource {
        dimension,
        metric: metric.to_string(),
    })
}
