//! `valid_at=` on the Python query entry points: the instant, decoded as a
//! query parameter is, written into the query text by the engine's
//! `prepend_valid_time` (the string `'all'` spells `FOR VALID_TIME ALL`). A refused prefix — an instant no literal can spell,
//! or a query that already carries a context — is a `ValueError`.

use std::borrow::Cow;

use pyo3::prelude::*;

use crate::datatypes::py_in;
use kglite_core::api::cypher::{self, PrependError};

/// `query`, behind `FOR VALID_TIME AS OF <valid_at>` when `valid_at` is set,
/// or `FOR VALID_TIME ALL` for the string `'all'`.
pub(crate) fn prefixed_query<'q>(
    query: &'q str,
    valid_at: Option<&Bound<'_, PyAny>>,
) -> PyResult<Cow<'q, str>> {
    let Some(instant) = valid_at else {
        return Ok(Cow::Borrowed(query));
    };
    let value = py_in::py_query_parameter_to_value("valid_at", instant)?;
    cypher::prepend_valid_time(query, cypher::ValidAt::from_value(&value))
        .map(Cow::Owned)
        .map_err(prepend_error)
}

/// A refused prefix as the `ValueError` every entry point raises.
pub(crate) fn prepend_error(err: PrependError) -> PyErr {
    pyo3::exceptions::PyValueError::new_err(err.to_string())
}
