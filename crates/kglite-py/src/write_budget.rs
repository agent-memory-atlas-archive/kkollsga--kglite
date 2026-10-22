//! The `write_budget=` argument and the `WriteBudgetWarning` category.

use kglite_core::api::io::{LoadOptions, WriteBudgetSetting};
use kglite_core::api::DirGraph;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyInt, PyString};

pyo3::create_exception!(
    kglite,
    WriteBudgetWarning,
    pyo3::exceptions::PyUserWarning,
    "Unsaved writes over a read-only mapped load reached half of `write_budget`. Warned once per crossing; at the budget further writes raise `WriteBudgetExceededError`. `save()` releases the memory."
);

/// What `write_budget=` was given: the string `"default"` (the argument's
/// default), `None` (no limit) or a byte count.
pub(crate) enum WriteBudgetArg {
    Default,
    Unlimited,
    Bytes(u64),
}

impl<'a, 'py> FromPyObject<'a, 'py> for WriteBudgetArg {
    type Error = PyErr;

    fn extract(obj: Borrowed<'a, 'py, PyAny>) -> PyResult<Self> {
        let refuse = || {
            crate::error_py::kg_to_pyerr(crate::error::KgError::Argument(
                "write_budget must be a byte count (int >= 0), None for no limit, or 'default'"
                    .to_string(),
            ))
        };
        if obj.is_none() {
            return Ok(Self::Unlimited);
        }
        if let Ok(text) = obj.cast::<PyString>() {
            return match text.to_str()? {
                "default" => Ok(Self::Default),
                _ => Err(refuse()),
            };
        }
        if obj.cast::<PyBool>().is_ok() || obj.cast::<PyInt>().is_err() {
            return Err(refuse());
        }
        obj.extract::<u64>().map(Self::Bytes).map_err(|_| refuse())
    }
}

impl WriteBudgetArg {
    pub(crate) fn is_default(&self) -> bool {
        matches!(self, Self::Default)
    }

    pub(crate) fn apply(&self, options: LoadOptions) -> LoadOptions {
        match self {
            Self::Default => options,
            Self::Unlimited => options.with_write_budget(WriteBudgetSetting::Unlimited),
            Self::Bytes(bytes) => options.with_write_budget(WriteBudgetSetting::Bytes(*bytes)),
        }
    }
}

/// Announce the warning a write parked on `graph`, if one is waiting, through
/// `warnings.warn`. A warning filter that raises does not undo a write that
/// already happened, so a failure to warn is dropped.
pub(crate) fn announce(py: Python<'_>, graph: &DirGraph) {
    if let Some(message) = graph.take_write_budget_warning() {
        if let Ok(text) = std::ffi::CString::new(message) {
            let _ = PyErr::warn(py, py.get_type::<WriteBudgetWarning>().as_any(), &text, 1);
        }
    }
}
