//! Quantized scan copies of embedding stores: building, dropping and
//! measuring them, plus the `precision=` plumbing the embedding writers share.

use pyo3::prelude::*;
use pyo3::types::PyDict;
use pyo3::IntoPyObjectExt;

use crate::graph::KnowledgeGraph;
use kglite_core::api::embeddings::{
    embedding_store_exists, quantization_recall, quantize_embeddings, EmbeddingEntity, Precision,
    QuantizationInfo, RecallOptions,
};

pub(super) fn parse_entity(entity: &str) -> PyResult<EmbeddingEntity> {
    EmbeddingEntity::parse(entity).map_err(crate::error_py::ArgumentError::new_err)
}

/// The `precision=` keyword, parsed before any write so a typo changes nothing.
pub(super) fn parse_precision(precision: Option<&str>) -> PyResult<Option<Precision>> {
    precision
        .map(Precision::parse)
        .transpose()
        .map_err(crate::error_py::ArgumentError::new_err)
}

/// A store that would be built with the Poincare metric cannot be quantized;
/// said before the write instead of after it.
pub(super) fn refuse_poincare(precision: Option<Precision>, metric: Option<&str>) -> PyResult<()> {
    if precision.is_some() && metric == Some("poincare") {
        return Err(crate::error_py::ArgumentError::new_err(
            "precision: the poincare metric is non-linear in the vector norms, so a quantized \
             copy cannot rank it; drop precision= or choose another metric",
        ));
    }
    Ok(())
}

pub(super) fn quantization_dict<'py>(
    py: Python<'py>,
    info: &QuantizationInfo,
) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("precision", info.precision)?;
    dict.set_item("oversample", info.oversample)?;
    dict.set_item("storage", info.storage)?;
    dict.set_item("bytes", info.bytes)?;
    dict.set_item("kernel", info.kernel)?;
    Ok(dict)
}

pub(super) fn quantization_value(
    py: Python<'_>,
    info: Option<&QuantizationInfo>,
) -> PyResult<Py<PyAny>> {
    match info {
        Some(info) => quantization_dict(py, info)?.into_py_any(py),
        None => Ok(py.None()),
    }
}

impl KnowledgeGraph {
    /// Quantize a store a writer just filled, when the caller asked for
    /// `precision=`. A store that does not exist (nothing was written) is
    /// left alone. Returns the copy's description for the writer's report.
    pub(super) fn quantize_written(
        &mut self,
        entity: EmbeddingEntity,
        type_name: &str,
        text_column: &str,
        precision: Option<Precision>,
    ) -> PyResult<Option<QuantizationInfo>> {
        let Some(precision) = precision else {
            return Ok(None);
        };
        self.unlogged_write(|g| {
            let exists = embedding_store_exists(g, entity, type_name, text_column);
            if !exists {
                return Ok(None);
            }
            quantize_embeddings(g, entity, type_name, text_column, Some(precision), None)
                .map(|report| report.quantization)
                .map_err(|message| {
                    crate::error_py::ArgumentError::new_err(format!(
                        "the vectors were stored, but they could not be quantized: {message}"
                    ))
                })
        })
    }
}

#[pymethods]
impl KnowledgeGraph {
    /// Build, replace or drop the quantized scan copy of a node store (default) or, with entity='relationship', a relationship store.
    #[pyo3(signature = (node_type, text_column, precision, oversample=None, *, entity="node"))]
    fn quantize_embeddings(
        &mut self,
        py: Python<'_>,
        node_type: &str,
        text_column: &str,
        precision: Option<&str>,
        oversample: Option<u32>,
        entity: &str,
    ) -> PyResult<Py<PyAny>> {
        let entity = parse_entity(entity)?;
        let precision = parse_precision(precision)?;
        self.check_durable_owner()?;
        // Building reads every vector once; keep the GIL free for other threads.
        let report = self.unlogged_write(|g| {
            py.detach(|| {
                quantize_embeddings(g, entity, node_type, text_column, precision, oversample)
            })
            .map_err(crate::error_py::ArgumentError::new_err)
        })?;
        let result = PyDict::new(py);
        result.set_item("entity", entity.as_str())?;
        result.set_item(
            match entity {
                EmbeddingEntity::Node => "node_type",
                EmbeddingEntity::Relationship => "relationship_type",
            },
            node_type,
        )?;
        result.set_item("text_column", text_column)?;
        result.set_item("count", report.count)?;
        result.set_item("dimension", report.dimension)?;
        result.set_item("previous", report.previous)?;
        result.set_item(
            "quantization",
            quantization_value(py, report.quantization.as_ref())?,
        )?;
        result.into_py_any(py)
    }

    /// Measure how a store's quantized copy ranks against exact f32 search, replaying stored vectors as queries.
    #[pyo3(signature = (node_type, text_column, sample=100, top_k=10, oversample=None, *, entity="node", seed=0))]
    // One Rust argument per Python keyword.
    #[allow(clippy::too_many_arguments)]
    fn quantization_recall(
        &self,
        py: Python<'_>,
        node_type: &str,
        text_column: &str,
        sample: usize,
        top_k: usize,
        oversample: Option<u32>,
        entity: &str,
        seed: u64,
    ) -> PyResult<Py<PyAny>> {
        let entity = parse_entity(entity)?;
        let graph = self.graph();
        let report = py
            .detach(|| {
                quantization_recall(
                    graph,
                    entity,
                    node_type,
                    text_column,
                    &RecallOptions::default()
                        .with_sample(sample)
                        .with_top_k(top_k)
                        .with_oversample(oversample)
                        .with_seed(seed),
                )
            })
            .map_err(crate::error_py::ArgumentError::new_err)?;
        let result = PyDict::new(py);
        result.set_item("precision", report.precision)?;
        result.set_item("oversample", report.oversample)?;
        result.set_item("sampled", report.sampled)?;
        result.set_item("top_k", report.top_k)?;
        result.set_item("recall", report.recall)?;
        result.set_item("recall_rescored", report.recall_rescored)?;
        result.into_py_any(py)
    }
}
