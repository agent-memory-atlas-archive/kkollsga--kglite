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
    check_index_name, register_index_name, IndexClass, NamedIndex,
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

pub(super) fn execute_create_vector_index(
    _graph: &mut DirGraph,
    _create: &CreateVectorIndex,
) -> Result<MutationStats, String> {
    Err(
        "CREATE VECTOR INDEX is not supported yet: a KGLite vector index is built over an \
         embedding store, not over a stored list property. Use your binding's \
         build_vector_index(node_type, text_column, ...) — every binding reaches it (Python, \
         Rust, and the C ABI's kglite_session_build_vector_index, which Java and the other C \
         consumers wrap). Run `SHOW INDEXES` to see what is installed."
            .to_string(),
    )
}
