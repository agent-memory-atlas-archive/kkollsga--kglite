//! PyO3 entry for the Rust blueprint loader.
//!
//! Thin wrapper: returns the populated `KnowledgeGraph` plus the output
//! path declared in the blueprint (if any). Save and `lock_schema` are
//! invoked from the Python shim using the existing `KnowledgeGraph`
//! methods — avoids duplicating the v3 save pipeline here.

use crate::datatypes::py_in;
use crate::graph::KnowledgeGraph;
use kglite_core::api::blueprint::{self, Diagnostic, DiagnosticGroup};
use kglite_core::datatypes::values::{ColumnType, DataFrame};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::path::Path;
use std::sync::Arc;

/// Parse a JSON blueprint and build a `KnowledgeGraph` from its inputs.
///
/// Returns `(graph, output_path_or_none)` — the Python shim saves and applies
/// `lock_schema` on top. Exposed as `kglite.kglite.from_blueprint_rust` to
/// avoid colliding with the user-facing `kglite.from_blueprint` wrapper.
#[pyfunction]
#[pyo3(signature = (blueprint_path, *, verbose=false, storage=None, path=None, frames=None, strict=None))]
pub fn from_blueprint_rust(
    py: Python<'_>,
    blueprint_path: String,
    verbose: bool,
    storage: Option<&str>,
    path: Option<&str>,
    frames: Option<&Bound<'_, PyDict>>,
    strict: Option<&Bound<'_, PyAny>>,
) -> PyResult<(KnowledgeGraph, Option<String>)> {
    let bp_path = Path::new(&blueprint_path).to_path_buf();
    if !bp_path.exists() {
        return Err(pyo3::exceptions::PyFileNotFoundError::new_err(format!(
            "Blueprint file not found: {}",
            bp_path.display()
        )));
    }

    // Parsed under the GIL, before the build is detached: converting a pandas
    // frame needs both the GIL and the blueprint's declared types, and the
    // core decides which frames are missing or unexpected — this side only
    // marshals what it was handed.
    let mut parsed = blueprint::load_blueprint_file(&bp_path)
        .map_err(pyo3::exceptions::PyValueError::new_err)?;
    // The argument overrides `settings.strict`; `None` leaves the setting.
    if let Some(strict) = strict.filter(|s| !s.is_none()) {
        parsed.settings.strict = Some(strict_setting(strict)?);
    }
    let inputs = convert_frames(&parsed, frames)?;

    let bp_dir = bp_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();

    let (kg, report, output_path) = py
        .detach(
            || -> Result<(KnowledgeGraph, blueprint::BuildReport, Option<std::path::PathBuf>), String> {
                // Construct the backing DirGraph with the requested storage
                // mode via the shared core builder (one mode vocabulary across
                // wheel / servers / C ABI). Empty string is treated as default.
                let mode = match storage {
                    None | Some("") => kglite_core::api::storage::StorageMode::Memory,
                    Some(s) => kglite_core::api::storage::StorageMode::parse(s)?,
                };
                let mut graph =
                    kglite_core::api::storage::new_dir_graph_in_mode(mode, path.map(Path::new))?;

                let (report, output_path) =
                    blueprint::from_blueprint(&mut graph, parsed, &bp_dir, inputs)?;

                let kg = KnowledgeGraph {
                    inner: Arc::new(graph),
                    cursor: crate::graph::CursorState::new(),
                    embedder: None,
                    default_timeout_ms: None,
                    default_max_work_units: None,
                    default_row_limit: None,
                    lifecycle: crate::graph::GraphLifecycle::detached(),
                };
                Ok((kg, report, output_path))
            },
        )
        .map_err(pyo3::exceptions::PyValueError::new_err)?;

    // Python's `print`, not Rust's: `contextlib.redirect_stdout` and a
    // notebook's output capture replace `sys.stdout`, never file descriptor 1.
    let text = report.render_text(verbose);
    if !text.is_empty() {
        py.import("builtins")?
            .getattr("print")?
            .call1((text.trim_end_matches('\n'),))?;
    }
    // One `UserWarning` per non-empty group, whatever `verbose` is: that is
    // the contract the stub documents, and it is what makes
    // `warnings.simplefilter` / `logging.captureWarnings` able to route them.
    // A summary line counting them would be a second, uncapturable channel
    // saying less.
    warn_by_group(py, "from_blueprint", &report.diagnostics)?;
    // Errors are not warnings and stay on stderr: a per-spec failure has
    // already been survived by the build, and the graph is returned regardless.
    for e in &report.errors {
        eprintln!("error: {}", e);
    }

    Ok((kg, output_path.map(|p| p.to_string_lossy().into_owned())))
}

/// `strict=` as the core's setting: a bool, or a list of group names.
fn strict_setting(value: &Bound<'_, PyAny>) -> PyResult<blueprint::StrictSetting> {
    if let Ok(flag) = value.cast::<pyo3::types::PyBool>() {
        return Ok(blueprint::StrictSetting::Flag(flag.is_true()));
    }
    value
        .extract::<Vec<String>>()
        .map(blueprint::StrictSetting::Groups)
        .map_err(|_| {
            pyo3::exceptions::PyTypeError::new_err(
                "strict= must be None, a bool, or a list of diagnostic group names",
            )
        })
}

/// Convert every frame the caller passed into a core `DataFrame`, typed by
/// what the blueprint declares for the input of that name.
///
/// A frame is coerced to the blueprint's declared property types — that is the
/// contract, and it is what makes a `frames=` build produce the same graph as a
/// CSV of the same data. Where the blueprint declares nothing, the frame's own
/// dtype is kept and reported to the loader as a known column type.
///
/// Names are not checked here: a declared-but-missing or passed-but-undeclared
/// frame is the core's error to phrase, and it phrases it once for every
/// binding.
fn convert_frames(
    parsed: &blueprint::Blueprint,
    frames: Option<&Bound<'_, PyDict>>,
) -> PyResult<blueprint::BuildInputs> {
    let mut inputs = blueprint::BuildInputs::default();
    let Some(frames) = frames else {
        return Ok(inputs);
    };
    for (key, value) in frames.iter() {
        let name: String = key.extract().map_err(|_| {
            pyo3::exceptions::PyTypeError::new_err(
                "frames= keys must be strings naming a 'files' entry",
            )
        })?;
        inputs
            .frames
            .insert(name.clone(), to_dataframe(parsed, &name, &value)?);
    }
    Ok(inputs)
}

fn to_dataframe(
    parsed: &blueprint::Blueprint,
    name: &str,
    df: &Bound<'_, PyAny>,
) -> PyResult<DataFrame> {
    let columns: Vec<String> = df
        .getattr("columns")
        .and_then(|c| c.extract())
        .map_err(|_| {
            pyo3::exceptions::PyTypeError::new_err(format!(
                "frames['{name}'] is not a DataFrame with string column names — pass a pandas \
                 DataFrame (or anything with .to_pandas()) whose columns are strings"
            ))
        })?;

    let declared = blueprint::declared_column_types(parsed, name)
        .map_err(pyo3::exceptions::PyValueError::new_err)?;
    let types = PyDict::new(df.py());
    for (column, ct) in &declared {
        if let Some(keyword) = pandas_type_keyword(ct) {
            types.set_item(column, keyword)?;
        }
    }

    // No identity special casing or implicit float downcast: declarations
    // control text grammar, while undeclared/native cells keep their dtype.
    py_in::pandas_to_blueprint_dataframe(df, &columns, &types)
}

/// The type name `py_in` reads for a blueprint type. The blueprint's keyword
/// vocabulary is wider (`str`, `integer`, `validFrom`, …) and `py_in`'s is a
/// different set, so the `ColumnType` in between is what the two agree on.
fn pandas_type_keyword(ct: &ColumnType) -> Option<&'static str> {
    match ct {
        ColumnType::String => Some("string"),
        ColumnType::Int64 => Some("int"),
        ColumnType::Float64 => Some("float"),
        ColumnType::Boolean => Some("bool"),
        ColumnType::DateTime => Some("date"),
        ColumnType::List => Some("list"),
        ColumnType::Duration => Some("duration"),
        // `map_blueprint_type` yields none of these, so the arm is unreachable
        // from a blueprint; leaving the column untyped is the honest fallback.
        ColumnType::UniqueId | ColumnType::Timestamp | ColumnType::Map => None,
    }
}

/// Build a `KnowledgeGraph` from an inline JSON records spec (nodes +
/// connections), no CSV files on disk. JSON-native sibling to
/// `from_blueprint_rust`. Returns the populated graph; the Python shim handles
/// optional save / lock_schema. Exposed as `kglite.kglite.from_records_rust`.
#[pyfunction]
#[pyo3(signature = (records_json, *, storage=None, path=None, on_missing_endpoint=None))]
pub fn from_records_rust(
    py: Python<'_>,
    records_json: String,
    storage: Option<&str>,
    path: Option<&str>,
    on_missing_endpoint: Option<&str>,
) -> PyResult<KnowledgeGraph> {
    let mut spec: serde_json::Value = serde_json::from_str(&records_json)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("invalid JSON: {}", e)))?;
    let spec_obj = spec.as_object_mut().ok_or_else(|| {
        pyo3::exceptions::PyValueError::new_err("from_records: top-level JSON must be an object")
    })?;
    // Only when the caller passed one. The spec's own `on_missing_endpoint`
    // is a documented top-level key, and inserting the argument's default
    // unconditionally overwrote it — a spec asking for `drop` vivified in
    // silence.
    if let Some(policy) = on_missing_endpoint {
        spec_obj.insert(
            "on_missing_endpoint".to_string(),
            serde_json::Value::String(policy.to_string()),
        );
    }

    let (kg, diagnostics) = py
        .detach(|| -> Result<(KnowledgeGraph, Vec<Diagnostic>), String> {
            let mode = match storage {
                None | Some("") => kglite_core::api::storage::StorageMode::Memory,
                Some(s) => kglite_core::api::storage::StorageMode::parse(s)?,
            };
            let mut graph =
                kglite_core::api::storage::new_dir_graph_in_mode(mode, path.map(Path::new))?;

            let report = blueprint::from_records(&mut graph, &spec)?;

            Ok((
                KnowledgeGraph {
                    inner: Arc::new(graph),
                    cursor: crate::graph::CursorState::new(),
                    embedder: None,
                    default_timeout_ms: None,
                    default_max_work_units: None,
                    default_row_limit: None,
                    lifecycle: crate::graph::GraphLifecycle::detached(),
                },
                report.diagnostics,
            ))
        })
        .map_err(pyo3::exceptions::PyValueError::new_err)?;

    warn_by_group(py, "from_records", &diagnostics)?;

    Ok(kg)
}

/// Items one group's warning lists before saying how many more there are.
const WARNING_ITEMS_SHOWN: usize = 10;

/// Raise one `UserWarning` per non-empty group, most severe group first.
///
/// `stacklevel` 2: the caller is the shim's `from_blueprint` (or the
/// `from_records` wrapper), and the warning belongs to whoever called that.
fn warn_by_group(py: Python<'_>, caller: &str, diagnostics: &[Diagnostic]) -> PyResult<()> {
    for group in DiagnosticGroup::ALL {
        let items: Vec<&Diagnostic> = diagnostics.iter().filter(|d| d.group == group).collect();
        if items.is_empty() {
            continue;
        }
        let mut text = format!("{caller} [{}] {} warning(s):", group.as_str(), items.len());
        for d in items.iter().take(WARNING_ITEMS_SHOWN) {
            text.push_str("\n  - ");
            text.push_str(&d.message);
        }
        if items.len() > WARNING_ITEMS_SHOWN {
            text.push_str(&format!(
                "\n  … and {} more",
                items.len() - WARNING_ITEMS_SHOWN
            ));
        }
        let message = std::ffi::CString::new(text).unwrap_or_default();
        PyErr::warn(
            py,
            py.get_type::<pyo3::exceptions::PyUserWarning>().as_any(),
            message.as_c_str(),
            2,
        )?;
    }
    Ok(())
}
