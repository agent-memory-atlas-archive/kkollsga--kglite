//! Recipe CRUD — the Python face of `kglite::api::recipes`.
//!
//! Thin by design, exactly as `pyapi::skills` is: the identifier rules, the
//! closed JSON-Schema subset, the `$param` ↔ `properties` match, the read-only
//! parse gate and the read-only refusal all live in core, because the MCP
//! boot merge and any other binding must reach the same verdict on the same
//! record. What this file owns is the marshalling — records to dicts,
//! `parameters` between a Python mapping and `serde_json` — and the wheel's
//! write protocol (`check_durable_owner` → `get_graph_mut` → `commit_wal`).

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use crate::datatypes::{py_in, py_out};
use crate::error_py::kg_to_pyerr;
use crate::graph::{get_graph_mut, KnowledgeGraph};
use kglite_core::api::param::{json_value_to_kglite_value, kglite_value_to_json};
use kglite_core::api::recipes::{self, RecipeRecord, SetOutcome};

/// The schema a query with no `$parameters` must carry: an object that accepts
/// nothing. Core's compiler requires `properties`, `required` and an explicitly
/// false `additionalProperties` on every root schema, so there is no shorter
/// spelling and `parameters=None` expands to this rather than to `{}`.
fn empty_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {},
        "required": [],
        "additionalProperties": false,
    })
}

fn record_to_dict<'py>(py: Python<'py>, record: &RecipeRecord) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("recipe", &record.recipe)?;
    dict.set_item("name", &record.name)?;
    dict.set_item("description", &record.description)?;
    dict.set_item(
        "parameters",
        py_out::value_to_py(py, &json_value_to_kglite_value(&record.parameters))?,
    )?;
    dict.set_item("cypher", &record.cypher)?;
    dict.set_item("recipe_description", &record.recipe_description)?;
    dict.set_item("tool", record.tool.as_deref())?;
    Ok(dict)
}

/// The group description a `set_recipe` that omitted one inherits.
///
/// Every member of a group repeats its description, so a second query added to
/// an existing group already has one to copy; a group's *first* query does not,
/// and core refuses an empty one. Sorted order matches
/// `recipes::catalogue_from_graph`, which takes the first record in name order
/// — so what is inherited here is the description the catalogue would serve.
fn inherited_group_description(graph: &KnowledgeGraph, recipe: &str) -> Option<String> {
    recipes::list(&graph.inner)
        .into_iter()
        .find(|stored| stored.recipe == recipe)
        .map(|stored| stored.recipe_description)
}

#[pymethods]
impl KnowledgeGraph {
    /// List every recipe query this graph carries, sorted by recipe then name.
    fn list_recipes(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let result_list = PyList::empty(py);
        for record in recipes::list(&self.inner) {
            result_list.append(record_to_dict(py, &record)?)?;
        }
        Ok(result_list.into())
    }

    /// Read one recipe query by its recipe and name.
    #[pyo3(signature = (recipe, name))]
    fn get_recipe(&self, py: Python<'_>, recipe: &str, name: &str) -> PyResult<Py<PyAny>> {
        let record = recipes::get(&self.inner, recipe, name).map_err(kg_to_pyerr)?;
        Ok(record_to_dict(py, &record)?.into())
    }

    /// Create or replace one recipe query, returning it as stored.
    // The seven catalogue fields are the Python signature, and a params
    // struct cannot cross the pyo3 boundary — the argument count is the
    // API's, not a shape this side is free to choose.
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (recipe, name, description, cypher, parameters=None, recipe_description=None, tool=None))]
    fn set_recipe(
        &mut self,
        py: Python<'_>,
        recipe: &str,
        name: &str,
        description: &str,
        cypher: &str,
        parameters: Option<&Bound<'_, PyAny>>,
        recipe_description: Option<&str>,
        tool: Option<&str>,
    ) -> PyResult<Py<PyAny>> {
        let parameters = match parameters {
            Some(value) => kglite_value_to_json(&py_in::py_value_to_value(value)?),
            None => empty_schema(),
        };
        let group_description = match recipe_description {
            Some(text) => text.to_string(),
            None => inherited_group_description(self, recipe).unwrap_or_default(),
        };
        let record = RecipeRecord {
            recipe: recipe.to_string(),
            name: name.to_string(),
            description: description.to_string(),
            parameters,
            cypher: cypher.to_string(),
            recipe_description: group_description,
            tool: tool.map(str::to_string),
        };
        self.check_durable_owner()?;
        let outcome = recipes::set(get_graph_mut(&mut self.inner), &record).map_err(kg_to_pyerr)?;
        self.commit_wal()?;

        // Read back rather than echo the argument, so what this returns is what
        // a later `get_recipe` will.
        let stored = recipes::get(&self.inner, recipe, name).map_err(kg_to_pyerr)?;
        let dict = record_to_dict(py, &stored)?;
        dict.set_item("created", matches!(outcome, SetOutcome::Created))?;
        Ok(dict.into())
    }

    /// Remove one recipe query; False means there was nothing stored there.
    #[pyo3(signature = (recipe, name))]
    fn delete_recipe(&mut self, recipe: &str, name: &str) -> PyResult<bool> {
        self.check_durable_owner()?;
        let removed =
            recipes::delete(get_graph_mut(&mut self.inner), recipe, name).map_err(kg_to_pyerr)?;
        self.commit_wal()?;
        Ok(removed)
    }

    /// Import a JSON recipe catalogue, replacing same-keyed queries.
    #[pyo3(signature = (path))]
    fn import_recipes(&mut self, path: &str) -> PyResult<Vec<String>> {
        self.check_durable_owner()?;
        let written =
            recipes::import_path(get_graph_mut(&mut self.inner), std::path::Path::new(path))
                .map_err(kg_to_pyerr)?;
        self.commit_wal()?;
        Ok(written
            .into_iter()
            .map(|(recipe, name)| format!("{recipe}/{name}"))
            .collect())
    }

    /// Write every recipe query to `path` as one JSON catalogue document.
    #[pyo3(signature = (path))]
    fn export_recipes(&self, path: &str) -> PyResult<()> {
        recipes::export_path(&self.inner, std::path::Path::new(path)).map_err(kg_to_pyerr)
    }
}
