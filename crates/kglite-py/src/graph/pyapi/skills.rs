//! Skill CRUD — the Python face of `kglite::api::skills`.
//!
//! Thin by design: validation, the Cypher write shape, the read-only refusal
//! and the SKILL.md dialect all live in core, because the CLI, the MCP server
//! and any other binding need the identical behaviour. What this file owns is
//! the marshalling — records to dicts, dicts to `PyErr`s — and the wheel's own
//! write protocol (`check_durable_owner` → `get_graph_mut` → `commit_wal`),
//! which no core call can perform on the binding's behalf.

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use crate::error_py::kg_to_pyerr;
use crate::graph::{get_graph_mut, KnowledgeGraph};
use kglite_core::api::skills::{self, Delivery, SetOutcome, SkillRecord};

/// Marshal one record. `body` is omitted entirely when the caller asked for a
/// listing: core returns an empty body there, and a key whose value is always
/// `""` would read as "this skill has no body".
fn record_to_dict<'py>(
    py: Python<'py>,
    record: &SkillRecord,
    with_body: bool,
) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("name", &record.name)?;
    dict.set_item("description", &record.description)?;
    if with_body {
        dict.set_item("body", &record.body)?;
    }
    dict.set_item("references_tools", record.references_tools.clone())?;
    dict.set_item("delivery", record.delivery.as_str())?;
    Ok(dict)
}

#[pymethods]
impl KnowledgeGraph {
    /// List the skills this graph carries, sorted by name, without their bodies.
    fn list_skills(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let result_list = PyList::empty(py);
        for record in skills::list(&self.inner) {
            result_list.append(record_to_dict(py, &record, false)?)?;
        }
        Ok(result_list.into())
    }

    /// Read one skill by name, body included.
    #[pyo3(signature = (name))]
    fn get_skill(&self, py: Python<'_>, name: &str) -> PyResult<Py<PyAny>> {
        let record = skills::get(&self.inner, name).map_err(kg_to_pyerr)?;
        Ok(record_to_dict(py, &record, true)?.into())
    }

    /// Create or replace a skill, returning it as stored.
    #[pyo3(signature = (name, description, body="", references_tools=None, delivery="lazy"))]
    fn set_skill(
        &mut self,
        py: Python<'_>,
        name: &str,
        description: &str,
        body: &str,
        references_tools: Option<Vec<String>>,
        delivery: &str,
    ) -> PyResult<Py<PyAny>> {
        let record = SkillRecord {
            name: name.to_string(),
            description: description.to_string(),
            body: body.to_string(),
            references_tools: references_tools.unwrap_or_default(),
            delivery: Delivery::parse(delivery).map_err(kg_to_pyerr)?,
        };
        self.check_durable_owner()?;
        let outcome = skills::set(get_graph_mut(&mut self.inner), &record).map_err(kg_to_pyerr)?;
        self.commit_wal()?;

        // Read back rather than echo the argument: what a later `get_skill`
        // returns is what the caller should have been handed here.
        let stored = skills::get(&self.inner, name).map_err(kg_to_pyerr)?;
        let dict = record_to_dict(py, &stored, true)?;
        dict.set_item("created", matches!(outcome, SetOutcome::Created))?;
        Ok(dict.into())
    }

    /// Remove a skill; False means there was no skill by that name.
    #[pyo3(signature = (name))]
    fn delete_skill(&mut self, name: &str) -> PyResult<bool> {
        self.check_durable_owner()?;
        let removed = skills::delete(get_graph_mut(&mut self.inner), name).map_err(kg_to_pyerr)?;
        self.commit_wal()?;
        Ok(removed)
    }

    /// Import SKILL.md files — one file, or every `.md` in a directory.
    #[pyo3(signature = (path))]
    fn import_skills(&mut self, path: &str) -> PyResult<Vec<String>> {
        self.check_durable_owner()?;
        let names = skills::import_path(get_graph_mut(&mut self.inner), std::path::Path::new(path))
            .map_err(kg_to_pyerr)?;
        self.commit_wal()?;
        Ok(names)
    }

    /// Write every skill to a directory as `<name>.md`, creating it if needed.
    #[pyo3(signature = (path))]
    fn export_skills(&self, path: &str) -> PyResult<Vec<String>> {
        skills::export_dir(&self.inner, std::path::Path::new(path)).map_err(kg_to_pyerr)
    }
}
