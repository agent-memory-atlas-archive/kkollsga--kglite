//! Relationship-embedding writers — `set_` replaces a store, `add_` upserts
//! (as `db.edge_embeddings.set` does), `embed_` generates — over
//! `kglite::api::embeddings`.

use std::collections::HashMap;

use pyo3::exceptions::{PyRuntimeError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyTuple};

use super::vector::{open_progress_bar, resolve_progress_factory};
use crate::datatypes::py_in;
use crate::graph::{get_graph_mut, KnowledgeGraph};
use kglite_core::api::embeddings::{
    EmbedError, EmbedHooks, EmbedMode, RelationshipIngestReport, RelationshipVector,
};
use kglite_core::api::Value;

/// The keys a list-of-dicts row may carry — `relationship_embeddings()`'s row.
const ROW_KEYS: [&str; 6] = [
    "source",
    "target",
    "source_type",
    "target_type",
    "key",
    "vector",
];

#[pymethods]
impl KnowledgeGraph {
    /// Replace a relationship store with vectors addressed by endpoint ids — the relationship twin of set_embeddings.
    #[pyo3(signature = (relationship_type, text_column, embeddings, *, relationship_keys=None, metric=None))]
    fn set_relationship_embeddings(
        &mut self,
        py: Python<'_>,
        relationship_type: &str,
        text_column: &str,
        embeddings: &Bound<'_, PyAny>,
        relationship_keys: Option<HashMap<String, String>>,
        metric: Option<&str>,
    ) -> PyResult<Py<PyAny>> {
        let rows = RelationshipRows {
            relationship_type,
            text_column,
            embeddings,
            relationship_keys,
            metric,
        };
        self.write_relationship_rows(
            py,
            rows,
            kglite_core::api::embeddings::set_relationship_embeddings,
        )
    }

    /// Upsert relationship vectors addressed by endpoint ids — the relationship twin of add_embeddings.
    #[pyo3(signature = (relationship_type, text_column, embeddings, *, relationship_keys=None, metric=None))]
    fn add_relationship_embeddings(
        &mut self,
        py: Python<'_>,
        relationship_type: &str,
        text_column: &str,
        embeddings: &Bound<'_, PyAny>,
        relationship_keys: Option<HashMap<String, String>>,
        metric: Option<&str>,
    ) -> PyResult<Py<PyAny>> {
        let rows = RelationshipRows {
            relationship_type,
            text_column,
            embeddings,
            relationship_keys,
            metric,
        };
        self.write_relationship_rows(
            py,
            rows,
            kglite_core::api::embeddings::add_relationship_embeddings,
        )
    }

    /// Embed a text property for every relationship of a type with the registered model.
    #[pyo3(signature = (relationship_type, text_column, *, mode=None, batch_size=256, show_progress=true, metric=None))]
    // One Rust argument per Python keyword, as embed_texts has.
    #[allow(clippy::too_many_arguments)]
    fn embed_relationship_texts(
        &mut self,
        py: Python<'_>,
        relationship_type: &str,
        text_column: &str,
        mode: Option<&str>,
        batch_size: usize,
        show_progress: bool,
        metric: Option<&str>,
    ) -> PyResult<Py<PyAny>> {
        self.check_durable_owner()?;
        let model = self.get_embedder_or_error()?;
        let mode = match mode.unwrap_or("missing") {
            "missing" => EmbedMode::Missing,
            "changed" => EmbedMode::Changed,
            "all" => EmbedMode::All,
            other => {
                return Err(PyValueError::new_err(format!(
                    "embed_relationship_texts(mode={other:?}): unknown mode. Use 'missing' \
                     (default), 'changed' (re-embed relationships whose text changed), or 'all'."
                )));
            }
        };
        let progress_factory = resolve_progress_factory(py, show_progress);
        let progress_bar: std::cell::RefCell<Option<Bound<'_, PyAny>>> =
            std::cell::RefCell::new(None);
        let open_bar = |total: usize| {
            *progress_bar.borrow_mut() = open_progress_bar(
                progress_factory.as_ref(),
                total,
                format!("Embedding {relationship_type}.{text_column}"),
            );
        };
        let tick = |done: usize| {
            if let Some(bar) = progress_bar.borrow().as_ref() {
                let _ = bar.call_method1("update", (done,));
            }
        };
        // Release the GIL while embedding, as embed_texts does.
        let embed_batch = |texts: &[String]| py.detach(|| model.embed(texts));
        let hooks = EmbedHooks {
            batch_size,
            load_when_idle: false,
            embed_batch: Some(&embed_batch),
            on_start: Some(&open_bar),
            on_batch: Some(&tick),
        };
        let g = get_graph_mut(&mut self.inner);
        let outcome = kglite_core::api::embeddings::embed_relationship_texts(
            g,
            relationship_type,
            text_column,
            mode,
            model.as_ref(),
            &hooks,
            metric,
        );
        if let Some(bar) = progress_bar.borrow().as_ref() {
            let _ = bar.call_method0("close");
        }
        let outcome =
            outcome.map_err(|error| embed_error(error, relationship_type, text_column))?;
        self.commit_wal()?;
        let result = PyDict::new(py);
        result.set_item("embedded", outcome.embedded)?;
        result.set_item("skipped", outcome.skipped)?;
        result.set_item("skipped_existing", outcome.skipped_existing)?;
        result.set_item("reembedded_changed", outcome.reembedded_changed)?;
        result.set_item("dimension", outcome.dimension)?;
        Ok(result.into())
    }
}

/// One writer call's arguments, as the Python caller passed them.
struct RelationshipRows<'a, 'py> {
    relationship_type: &'a str,
    text_column: &'a str,
    embeddings: &'a Bound<'py, PyAny>,
    relationship_keys: Option<HashMap<String, String>>,
    metric: Option<&'a str>,
}

/// The core writer both methods call: replace or upsert.
type RowWriter = fn(
    &mut kglite_core::api::DirGraph,
    &str,
    &str,
    Vec<RelationshipVector>,
    &kglite_core::api::embeddings::RelationshipKeys,
    Option<&str>,
) -> Result<RelationshipIngestReport, String>;

impl KnowledgeGraph {
    fn write_relationship_rows(
        &mut self,
        py: Python<'_>,
        args: RelationshipRows<'_, '_>,
        write: RowWriter,
    ) -> PyResult<Py<PyAny>> {
        self.check_durable_owner()?;
        let rows = marshal_relationship_rows(args.embeddings)?;
        let keys = args.relationship_keys.unwrap_or_default();
        let g = get_graph_mut(&mut self.inner);
        let report = write(
            g,
            args.relationship_type,
            args.text_column,
            rows,
            &keys,
            args.metric,
        )
        .map_err(PyValueError::new_err)?;
        self.commit_wal()?;
        let result = PyDict::new(py);
        result.set_item("embeddings_stored", report.stored)?;
        result.set_item("dimension", report.dimension)?;
        result.set_item("changed", report.changed)?;
        result.set_item("store_created", report.store_created)?;
        Ok(result.into())
    }
}

/// `embeddings` as the rows the engine resolves: a dict keyed by an endpoint
/// tuple, or the list of row dicts `relationship_embeddings()` returns. Only
/// the shape is checked here; every address rule lives in core.
fn marshal_relationship_rows(embeddings: &Bound<'_, PyAny>) -> PyResult<Vec<RelationshipVector>> {
    let mut vectors = py_in::F32Rows::default();
    if let Ok(dict) = embeddings.cast::<PyDict>() {
        let mut rows = Vec::with_capacity(dict.len());
        for (address, vector) in dict.iter() {
            rows.push(tuple_row(&address, vectors.extract(&vector)?)?);
        }
        return Ok(rows);
    }
    if let Ok(list) = embeddings.cast::<PyList>() {
        let mut rows = Vec::with_capacity(list.len());
        for (position, item) in list.iter().enumerate() {
            rows.push(dict_row(&item, position, &mut vectors)?);
        }
        return Ok(rows);
    }
    Err(PyTypeError::new_err(format!(
        "set_relationship_embeddings(): embeddings must be a dict keyed by endpoint tuples or \
         a list of row dicts, got {}",
        type_name(embeddings)
    )))
}

/// A dict key: `(source_id, target_id)`, `(source_id, target_id, key)`,
/// `(source_type, source_id, target_type, target_id)` or that plus `key`.
fn tuple_row(address: &Bound<'_, PyAny>, vector: Vec<f32>) -> PyResult<RelationshipVector> {
    let shape_error = || {
        PyTypeError::new_err(format!(
            "set_relationship_embeddings(): a dict key must be (source_id, target_id), \
             (source_id, target_id, key), (source_type, source_id, target_type, target_id) or \
             (source_type, source_id, target_type, target_id, key); got {}",
            address
                .repr()
                .map_or_else(|_| "?".to_string(), |r| r.to_string())
        ))
    };
    let tuple = address.cast::<PyTuple>().map_err(|_| shape_error())?;
    let item = |index: usize| tuple.get_item(index);
    let value = |index: usize| py_in::py_value_to_value(&item(index)?);
    let name = |index: usize| -> PyResult<Option<String>> { Ok(Some(item(index)?.extract()?)) };
    let (source_type, source_id, target_type, target_id, key) = match tuple.len() {
        2 => (None, value(0)?, None, value(1)?, None),
        3 => (None, value(0)?, None, value(1)?, Some(value(2)?)),
        4 => (name(0)?, value(1)?, name(2)?, value(3)?, None),
        5 => (name(0)?, value(1)?, name(2)?, value(3)?, Some(value(4)?)),
        _ => return Err(shape_error()),
    };
    Ok(RelationshipVector {
        source_type,
        source_id,
        target_type,
        target_id,
        key: key.filter(|key| !matches!(key, Value::Null)),
        vector,
    })
}

/// A `relationship_embeddings()` row: `source`, `target` and `vector`
/// required; `source_type`, `target_type` and `key` optional (`None` = absent).
fn dict_row(
    item: &Bound<'_, PyAny>,
    position: usize,
    vectors: &mut py_in::F32Rows,
) -> PyResult<RelationshipVector> {
    let row = item.cast::<PyDict>().map_err(|_| {
        PyTypeError::new_err(format!(
            "set_relationship_embeddings(): embeddings[{position}] must be a dict like the rows \
             relationship_embeddings() returns, got {}",
            type_name(item)
        ))
    })?;
    for key in row.keys() {
        let key: String = key.extract()?;
        if !ROW_KEYS.contains(&key.as_str()) {
            return Err(PyTypeError::new_err(format!(
                "set_relationship_embeddings(): embeddings[{position}] has unknown key '{key}'. \
                 Accepted: {}",
                ROW_KEYS.join(", ")
            )));
        }
    }
    let present = |name: &str| -> PyResult<Option<Bound<'_, PyAny>>> {
        Ok(row.get_item(name)?.filter(|value| !value.is_none()))
    };
    let required = |name: &str| {
        present(name)?.ok_or_else(|| {
            PyTypeError::new_err(format!(
                "set_relationship_embeddings(): embeddings[{position}] is missing '{name}'"
            ))
        })
    };
    let name = |field: &str| -> PyResult<Option<String>> {
        present(field)?.map(|value| value.extract()).transpose()
    };
    Ok(RelationshipVector {
        source_type: name("source_type")?,
        source_id: py_in::py_value_to_value(&required("source")?)?,
        target_type: name("target_type")?,
        target_id: py_in::py_value_to_value(&required("target")?)?,
        key: present("key")?
            .map(|key| py_in::py_value_to_value(&key))
            .transpose()?,
        vector: vectors.extract(&required("vector")?)?,
    })
}

fn type_name(value: &Bound<'_, PyAny>) -> String {
    value
        .get_type()
        .name()
        .map_or_else(|_| "?".to_string(), |name| name.to_string())
}

/// A core [`EmbedError`] as `embed_texts` raises its node twin, with the
/// relationship remedies spelled out.
fn embed_error(error: EmbedError, relationship_type: &str, text_column: &str) -> PyErr {
    match error {
        EmbedError::Dimension { store, model } => PyValueError::new_err(format!(
            "embed_relationship_texts(): the model produces {model}-d vectors but the existing \
             '{relationship_type}.{text_column}_emb' relationship store is {store}-d — embedding \
             the rest would mix dimensions and corrupt search. Re-embed with mode='all' to \
             rebuild at the new dimension, or drop the store first with CALL \
             db.edge_embeddings.drop({{type: '{relationship_type}', text_property: \
             '{text_column}'}})."
        )),
        EmbedError::Column(message) | EmbedError::Output(message) => PyValueError::new_err(message),
        EmbedError::Model(message) => PyRuntimeError::new_err(message),
    }
}
