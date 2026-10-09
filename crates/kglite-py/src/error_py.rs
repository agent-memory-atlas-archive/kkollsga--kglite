//! Python-side machinery for the [`crate::error::KgError`] taxonomy.
//!
//! Defines the typed Python exception classes
//! (`kglite.CypherSyntaxError`, `kglite.SchemaError`, etc.) via PyO3's
//! `create_exception!` macro, and provides the [`From<KgError> for PyErr`]
//! impl that picks the most specific subclass for each variant at the
//! PyO3 boundary.
//!
//! ## Hierarchy
//!
//! Typed engine errors descend from `kglite.KgError`, which itself descends
//! from `Exception`. Wrapper operations that implement conventional Python
//! protocols may raise built-in exceptions directly.
//!
//! Every instance carries a stable `.code` string (the
//! [`KgErrorCode`](crate::error::KgErrorCode) name, e.g. `"ConstraintViolation"`)
//! so applications branch on a classifier rather than on message prose.
//!
//! ```text
//! Exception
//! └── kglite.KgError                          (base)
//!     ├── kglite.CypherError                   (Cypher pipeline base)
//!     │   ├── kglite.CypherSyntaxError
//!     │   ├── kglite.CypherTimeoutError
//!     │   ├── kglite.CypherExecutionError
//!     │   └── kglite.CypherTypeMismatchError
//!     ├── kglite.SchemaError
//!     ├── kglite.ValidationError
//!     ├── kglite.ExprError
//!     ├── kglite.ConstraintError            (declared-integrity base)
//!     │   ├── kglite.ConstraintViolationError
//!     │   │   └── kglite.OntologyViolationError
//!     │   └── kglite.ConstraintCreationError
//!     ├── kglite.TransactionConflictError
//!     ├── kglite.NodeNotFoundError
//!     ├── kglite.ConnectionNotFoundError
//!     ├── kglite.PropertyNotFoundError
//!     ├── kglite.FileError                     (FileNotFound)
//!     ├── kglite.FileFormatError
//!     ├── kglite.FileIoError
//!     │   └── kglite.WriterLeaseHeldError
//!     ├── kglite.LoadMemoryLimitError
//!     ├── kglite.ArgumentError
//!     │   └── kglite.ReadOnlyError
//!     ├── kglite.NotDurableError               (also a ValueError)
//!     ├── kglite.MissingArgumentError
//!     ├── kglite.InternerCollisionError
//!     └── kglite.InternalError
//! ```
//!
//! ## Built-in exception boundary
//!
//! PyO3's `create_exception!` macro is single-inheritance; combining
//! `kglite.KgError` as a base AND `PyValueError` as an additional
//! base would require Python-level multiple inheritance which PyO3
//! doesn't support cleanly. Engine failures use the typed hierarchy; Python
//! lookup, argument-shape, filesystem, and object-lifecycle conventions keep
//! their documented built-in exception families.

use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyDict, PyModule, PyTuple, PyType};

// Alias Rust types on import — every `create_exception!` macro call
// below generates a Python-side struct (e.g. `KgError`, `SchemaError`)
// in this module, colliding with the Rust enum / pyo3-public types of
// the same names if imported unaliased. The `Rust*` prefix keeps the
// From-impl machinery distinct from the user-facing Python classes.
use crate::error::KgError as RustKgError;

// ─── Exception class declarations (single-inheritance chain) ─────────────────
//
// `create_exception!(module, ClassName, BaseClass, docstring)`. The
// third argument must be a single class. KgError extends PyException
// (Exception); every kglite typed exception extends KgError (or a
// kglite mid-tier like CypherError).

pyo3::create_exception!(
    kglite,
    KgError,
    pyo3::exceptions::PyException,
    "Base class for typed KGLite engine failures."
);

// ── Cypher pipeline ──────────────────────────────────────────────────

pyo3::create_exception!(
    kglite,
    CypherError,
    KgError,
    "Base for all Cypher-related errors (syntax, timeout, execution, type)."
);

pyo3::create_exception!(
    kglite,
    CypherSyntaxError,
    CypherError,
    "Cypher parser / tokenizer rejected the query. Always has `.line` and `.col` attributes (1-indexed); both are `None` when the parser couldn't pin a position."
);

pyo3::create_exception!(
    kglite,
    CypherTimeoutError,
    CypherError,
    "Cypher query exceeded its `timeout_ms`."
);

pyo3::create_exception!(
    kglite,
    CypherExecutionError,
    CypherError,
    "Cypher executor failure during query evaluation. Has `.line` and `.col` attributes when the failure is pinned to a source position."
);

pyo3::create_exception!(
    kglite,
    CypherTypeMismatchError,
    CypherError,
    "Cypher value-type mismatch in an expression (e.g. arithmetic on a String)."
);

// ── Schema / validation ──────────────────────────────────────────────

pyo3::create_exception!(
    kglite,
    SchemaError,
    KgError,
    "Schema validation failure (unknown property, type mismatch at pattern literal)."
);

pyo3::create_exception!(
    kglite,
    ValidationError,
    KgError,
    "Structural validation failure (missing required field, wrong connection endpoint, etc.)."
);

pyo3::create_exception!(
    kglite,
    ExprError,
    KgError,
    "Blueprint expression evaluation failure."
);

pyo3::create_exception!(
    kglite,
    ConstraintError,
    KgError,
    "Base class for declared-integrity-constraint failures (UNIQUE / NOT NULL / NODE KEY). Catch this to handle any constraint problem."
);

pyo3::create_exception!(
    kglite,
    ConstraintViolationError,
    ConstraintError,
    "A write violated a declared constraint — a UNIQUE duplicate, or a NOT NULL / NODE KEY property left absent. The write was rejected before touching storage, so the graph is unchanged."
);

pyo3::create_exception!(
    kglite,
    ConstraintCreationError,
    ConstraintError,
    "Declaring a constraint failed because the stored data already violates it. Deduplicate the node type, then re-declare."
);

pyo3::create_exception!(
    kglite,
    OntologyViolationError,
    ConstraintViolationError,
    "A write was refused by the declared ontology, or a declaration was refused because stored data already violates it. Subclass of `ConstraintViolationError`; the graph is unchanged."
);

// ── Concurrency ──────────────────────────────────────────────────────

pyo3::create_exception!(
    kglite,
    TransactionConflictError,
    KgError,
    "An optimistic-concurrency commit lost its race — the graph advanced between `begin()` and `commit()`, so nothing was applied. Retry the whole transaction against a fresh `begin()`; `kglite.retry_on_conflict` does this for you."
);

// ── Resource / access ────────────────────────────────────────────────

pyo3::create_exception!(
    kglite,
    NodeNotFoundError,
    KgError,
    "A node identified by `(node_type, id)` doesn't exist."
);

pyo3::create_exception!(
    kglite,
    ConnectionNotFoundError,
    KgError,
    "A connection type isn't declared in the schema."
);

pyo3::create_exception!(
    kglite,
    PropertyNotFoundError,
    KgError,
    "A property is missing from a node or relationship."
);

// ── File / I/O ───────────────────────────────────────────────────────

pyo3::create_exception!(
    kglite,
    FileError,
    KgError,
    "A file the user named doesn't exist on disk."
);

pyo3::create_exception!(
    kglite,
    FileFormatError,
    KgError,
    "A file's contents are malformed (bad .kgl header, truncated blueprint, etc.)."
);

pyo3::create_exception!(
    kglite,
    FileIoError,
    KgError,
    "Generic I/O failure (permission denied, mid-read EOF, mmap failure)."
);

pyo3::create_exception!(
    kglite,
    WriterLeaseHeldError,
    FileIoError,
    "Another process (or an un-closed handle in this one) holds the writer lease for the path. Subclass of `FileIoError`, so an existing `except FileIoError` still catches it. Retriable as it stands: wait and try again. `.holder` is a dict with `pid`, `since`, `label` (each `None` when unknown) and `self` (True when the holder is this process)."
);

pyo3::create_exception!(
    kglite,
    LoadMemoryLimitError,
    KgError,
    "A .kgl load exceeded max_load_mb / KGLITE_MAX_LOAD_MB at the metadata precheck or the pre-publication legacy portable-normalization check. The file is valid."
);

// ── No write-ahead log ───────────────────────────────────────────────

/// `kglite.NotDurableError`, a subclass of both `KgError` and `ValueError`.
/// `create_exception!` takes one base, and `sync()` raised a bare `ValueError`
/// before the class existed, so it is built once with two bases and every
/// `except ValueError` around `sync()` keeps working.
static NOT_DURABLE_ERROR: PyOnceLock<Py<PyType>> = PyOnceLock::new();

fn not_durable_class(py: Python<'_>) -> Bound<'_, PyType> {
    NOT_DURABLE_ERROR
        .get_or_init(py, || {
            let build = || -> PyResult<Py<PyType>> {
                let bases = PyTuple::new(
                    py,
                    [
                        py.get_type::<KgError>(),
                        py.get_type::<pyo3::exceptions::PyValueError>(),
                    ],
                )?;
                let namespace = PyDict::new(py);
                namespace.set_item("__module__", "kglite")?;
                namespace.set_item(
                    "__doc__",
                    "`sync()` was called on a graph that keeps no write-ahead log (`durable='off'`, a disk graph or a non-durable graph). Subclass of `KgError` and `ValueError`; nothing was flushed. Call `save()` for a checkpoint, or reopen with `durable='normal'`.",
                )?;
                namespace.set_item("code", crate::error::KgErrorCode::NotDurable.as_str())?;
                let class = py
                    .import("builtins")?
                    .getattr("type")?
                    .call1(("NotDurableError", bases, namespace))?;
                Ok(class.cast_into::<PyType>()?.unbind())
            };
            build().expect("NotDurableError class construction")
        })
        .bind(py)
        .clone()
}

// ── Argument validation ──────────────────────────────────────────────

pyo3::create_exception!(
    kglite,
    ArgumentError,
    KgError,
    "A user-supplied argument violated a precondition."
);

pyo3::create_exception!(
    kglite,
    ReadOnlyError,
    ArgumentError,
    "A write was refused because the handle is read-only (`read_only(True)` or a read-only transaction). Subclass of `ArgumentError`, so an existing `except ArgumentError` still catches it; the graph is unchanged."
);

pyo3::create_exception!(
    kglite,
    MissingArgumentError,
    KgError,
    "A required argument wasn't passed."
);

// ── Internal ─────────────────────────────────────────────────────────

pyo3::create_exception!(
    kglite,
    InternalError,
    KgError,
    "Invariant violation — kglite-internal bug. Reports the source location."
);

pyo3::create_exception!(
    kglite,
    InternerCollisionError,
    KgError,
    "Two distinct names collided on the persisted interner key; the operation was rejected unchanged."
);

// ─── PyErr boundary ──────────────────────────────────────────────────────────

/// Convert a Rust [`RustKgError`] into a Python [`PyErr`], picking
/// the most specific subclass for the variant.
///
/// This is the canonical conversion at the PyO3 boundary. The
/// `impl From<KgError> for PyErr` form is orphan-rule blocked (KgError
/// lives in the kglite engine, PyErr in pyo3, neither local to this
/// crate), so callers explicitly route through this function via
/// `Err(kg_to_pyerr(KgError::Foo(...)))` or `.map_err(kg_to_pyerr)?`.
pub fn kg_to_pyerr(e: RustKgError) -> PyErr {
    let message = e.to_string();
    // Stable classifier, captured before `e` is consumed by the match. Every
    // `kglite.*` exception instance carries it as `.code`, so an application
    // can branch on a wire-stable string (`"ConstraintViolation"`) instead of
    // the message prose — the promise `docs/python/guides/primary-store.md`
    // makes. `Cancelled` is excluded: it maps to the builtin
    // `KeyboardInterrupt`, which is deliberately outside the KgError family.
    let code = e.code().as_str();
    let is_cancelled = matches!(e, RustKgError::Cancelled);
    let err = kg_to_pyerr_class(e, message);
    if is_cancelled {
        return err;
    }
    with_code_attr(err, code)
}

/// Pick the most specific Python exception class for `e` and construct it with
/// `message`. Split from [`kg_to_pyerr`] so the `.code` decoration applies
/// uniformly to every arm rather than being repeated 20 times.
fn kg_to_pyerr_class(e: RustKgError, message: String) -> PyErr {
    match e {
        RustKgError::CypherSyntax { line, col, .. } => {
            // `.line` / `.col` are always present on CypherSyntaxError —
            // `None` when the parser couldn't pin a position.
            with_position_attrs(CypherSyntaxError::new_err(message), line, col)
        }
        RustKgError::CypherTimeout { .. } => CypherTimeoutError::new_err(message),
        RustKgError::CypherExecution { position, .. } => {
            let (line, col) = match position {
                Some((l, c)) => (Some(l), Some(c)),
                None => (None, None),
            };
            with_position_attrs(CypherExecutionError::new_err(message), line, col)
        }
        RustKgError::CypherTypeMismatch { .. } => CypherTypeMismatchError::new_err(message),
        // Cooperative cancellation (the wheel's Ctrl-C handler flipped the
        // cancel flag mid-query) surfaces as the builtin KeyboardInterrupt,
        // not a kglite.* error class — it's an interrupt, not a query fault.
        RustKgError::Cancelled => {
            pyo3::exceptions::PyKeyboardInterrupt::new_err("Query interrupted")
        }
        RustKgError::Schema { .. } => SchemaError::new_err(message),
        RustKgError::Validation(_) => ValidationError::new_err(message),
        RustKgError::ConstraintViolation { .. } => ConstraintViolationError::new_err(message),
        RustKgError::ConstraintCreationFailed { .. } => ConstraintCreationError::new_err(message),
        RustKgError::OntologyViolation {
            rule,
            entity,
            entity_type,
            property,
            report,
            ..
        } => with_ontology_attrs(
            OntologyViolationError::new_err(message),
            (rule, entity, entity_type, property),
            report,
        ),
        RustKgError::TransactionConflict { .. } => TransactionConflictError::new_err(message),
        // No `DurabilityFailedError` class: a rejected log append has always
        // been a `FileIoError`, and a wheel caller handling that keeps
        // working. Its `.code` (`DurabilityFailed`) is what separates it.
        RustKgError::DurabilityFailed { .. } => FileIoError::new_err(message),
        RustKgError::WriterLeaseHeld { holder, .. } => {
            with_holder_attr(WriterLeaseHeldError::new_err(message), holder)
        }
        RustKgError::ReadOnly { .. } => ReadOnlyError::new_err(message),
        RustKgError::NotDurable { .. } => {
            Python::attach(|py| PyErr::from_type(not_durable_class(py), message))
        }
        RustKgError::Expr(_) => ExprError::new_err(message),
        RustKgError::NodeNotFound { .. } => NodeNotFoundError::new_err(message),
        RustKgError::ConnectionNotFound { .. } => ConnectionNotFoundError::new_err(message),
        RustKgError::PropertyNotFound { .. } => PropertyNotFoundError::new_err(message),
        RustKgError::FileNotFound(_) => FileError::new_err(message),
        RustKgError::FileFormat { .. } => FileFormatError::new_err(message),
        RustKgError::FileIo(_) => FileIoError::new_err(message),
        RustKgError::LoadMemoryLimit(_) => LoadMemoryLimitError::new_err(message),
        RustKgError::InvalidArgument { .. } | RustKgError::Argument(_) => {
            ArgumentError::new_err(message)
        }
        RustKgError::MissingArgument(_) => MissingArgumentError::new_err(message),
        RustKgError::InternerCollision(_) => InternerCollisionError::new_err(message),
        RustKgError::Internal { .. } => InternalError::new_err(message),
    }
}

// Post-G.3a: `impl From<RustKgError> for PyErr` would violate
// Rust's orphan rule (both types foreign to this crate — KgError
// lives in the kglite engine, PyErr in pyo3). All call sites use
// `kg_to_pyerr(...)` directly.

/// Set `.line` / `.col` attributes (1-indexed source position) on the
/// exception *value*. `PyErr::new_err` is lazy, so `err.value(py)`
/// normalizes the exception first; attribute assignment on an exception
/// instance can't reasonably fail, but any failure is swallowed rather
/// than masking the original error.
/// Set the stable `.code` classifier on the exception *value*. Same
/// normalize-then-setattr shape as [`with_position_attrs`]; a failure here
/// would mask the real error, so it is swallowed.
fn with_code_attr(err: PyErr, code: &'static str) -> PyErr {
    Python::attach(|py| {
        let value = err.value(py);
        let _ = value.setattr("code", code);
    });
    err
}

/// `.holder` on a [`WriterLeaseHeldError`]: `{pid, since, label, self}`, each
/// `None` when the holder's record could not be read.
fn with_holder_attr(err: PyErr, holder: kglite_core::api::io::LeaseHolder) -> PyErr {
    Python::attach(|py| {
        let value = err.value(py);
        let dict = pyo3::types::PyDict::new(py);
        let _ = dict.set_item("pid", holder.pid);
        let _ = dict.set_item("since", holder.since.as_deref());
        let _ = dict.set_item("label", holder.label.as_deref());
        let _ = dict.set_item("self", holder.is_self());
        let _ = value.setattr("holder", dict);
    });
    err
}

/// A write-ahead-log failure as the wheel reports it: a `FileIoError` whose
/// `.code` is `DurabilityFailed`, carrying `message` verbatim.
///
/// Every logged-write path (`cypher()`, the fluent writers, `Session`) raises
/// this one identity. The message is the caller's rather than `KgError`'s
/// rendering, because the two paths differ in what is true: a `Session` commit
/// was not applied, while a `KnowledgeGraph` statement is applied in memory
/// and only its log append failed.
pub(crate) fn durability_failed_pyerr(message: String) -> PyErr {
    with_code_attr(
        FileIoError::new_err(message),
        crate::error::KgErrorCode::DurabilityFailed.as_str(),
    )
}

fn with_position_attrs(err: PyErr, line: Option<usize>, col: Option<usize>) -> PyErr {
    Python::attach(|py| {
        let value = err.value(py);
        let _ = value.setattr("line", line);
        let _ = value.setattr("col", col);
    });
    err
}

/// `.rule` / `.entity` / `.entity_type` / `.property` (the headline) and
/// `.report` (the per-rule breakdown of a refused declaration; empty for a
/// refused write) on an [`OntologyViolationError`].
fn with_ontology_attrs(
    err: PyErr,
    headline: (&'static str, &'static str, String, Option<String>),
    report: Vec<kglite_core::api::OntologyReportEntry>,
) -> PyErr {
    Python::attach(|py| {
        let value = err.value(py);
        let (rule, entity, entity_type, property) = headline;
        let _ = value.setattr("rule", rule);
        let _ = value.setattr("entity", entity);
        let _ = value.setattr("entity_type", entity_type);
        let _ = value.setattr("property", property);
        let rows = pyo3::types::PyList::empty(py);
        for entry in report {
            let row = pyo3::types::PyDict::new(py);
            let _ = row.set_item("rule", entry.rule.as_str());
            let _ = row.set_item(
                "entity",
                match entry.entity {
                    kglite_core::api::EntityKind::Node => "node",
                    kglite_core::api::EntityKind::Relationship => "relationship",
                },
            );
            let _ = row.set_item("entity_type", entry.entity_type);
            let _ = row.set_item("property", entry.property);
            let _ = row.set_item("count", entry.count);
            let _ = rows.append(row);
        }
        let _ = value.setattr("report", rows);
    });
    err
}

// ─── Module registration ─────────────────────────────────────────────────────

/// Register every typed exception class on the `kglite` Python module.
/// Called from `#[pymodule] fn kglite(...)` in `src/lib.rs`.
pub(crate) fn register(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("KgError", py.get_type::<KgError>())?;

    // Cypher pipeline
    m.add("CypherError", py.get_type::<CypherError>())?;
    m.add("CypherSyntaxError", py.get_type::<CypherSyntaxError>())?;
    // Class-level `.line` / `.col` defaults (None) for the two
    // position-carrying classes: instances raised with a known position
    // shadow these with instance attributes (see `with_position_attrs`),
    // and the attributes stay readable on any instance either way.
    for cls in [
        py.get_type::<CypherSyntaxError>(),
        py.get_type::<CypherExecutionError>(),
    ] {
        cls.setattr("line", py.None())?;
        cls.setattr("col", py.None())?;
    }
    m.add("CypherTimeoutError", py.get_type::<CypherTimeoutError>())?;
    m.add(
        "CypherExecutionError",
        py.get_type::<CypherExecutionError>(),
    )?;
    m.add(
        "CypherTypeMismatchError",
        py.get_type::<CypherTypeMismatchError>(),
    )?;

    // Schema / validation
    m.add("SchemaError", py.get_type::<SchemaError>())?;
    m.add("ValidationError", py.get_type::<ValidationError>())?;
    m.add("ExprError", py.get_type::<ExprError>())?;
    m.add("ConstraintError", py.get_type::<ConstraintError>())?;
    m.add(
        "ConstraintViolationError",
        py.get_type::<ConstraintViolationError>(),
    )?;
    m.add(
        "ConstraintCreationError",
        py.get_type::<ConstraintCreationError>(),
    )?;
    m.add(
        "OntologyViolationError",
        py.get_type::<OntologyViolationError>(),
    )?;

    // Concurrency
    m.add(
        "TransactionConflictError",
        py.get_type::<TransactionConflictError>(),
    )?;

    // Resource / access
    m.add("NodeNotFoundError", py.get_type::<NodeNotFoundError>())?;
    m.add(
        "ConnectionNotFoundError",
        py.get_type::<ConnectionNotFoundError>(),
    )?;
    m.add(
        "PropertyNotFoundError",
        py.get_type::<PropertyNotFoundError>(),
    )?;

    // File / I/O
    m.add("FileError", py.get_type::<FileError>())?;
    m.add("FileFormatError", py.get_type::<FileFormatError>())?;
    m.add("FileIoError", py.get_type::<FileIoError>())?;
    m.add(
        "WriterLeaseHeldError",
        py.get_type::<WriterLeaseHeldError>(),
    )?;
    m.add(
        "LoadMemoryLimitError",
        py.get_type::<LoadMemoryLimitError>(),
    )?;

    // Argument validation
    m.add("ArgumentError", py.get_type::<ArgumentError>())?;
    m.add("ReadOnlyError", py.get_type::<ReadOnlyError>())?;
    m.add("NotDurableError", not_durable_class(py))?;
    m.add(
        "MissingArgumentError",
        py.get_type::<MissingArgumentError>(),
    )?;

    // Internal
    m.add(
        "InternerCollisionError",
        py.get_type::<InternerCollisionError>(),
    )?;
    m.add("InternalError", py.get_type::<InternalError>())?;

    register_class_codes(py)?;

    Ok(())
}

/// Publish the stable [`KgErrorCode`](crate::error::KgErrorCode) string as a
/// **class-level** `.code` on each concrete exception class, so callers can
/// compare against `kglite.ConstraintViolationError.code` without an instance
/// and `.code` is readable even on an exception constructed by hand.
///
/// Instances raised by the engine shadow these with the code of the actual
/// `KgError` variant (see `with_code_attr`); the two always agree because both
/// come from `KgError::code()`. The three abstract bases — `KgError`,
/// `CypherError`, `ConstraintError` — cover several codes, so they get `None`
/// rather than an arbitrary pick.
fn register_class_codes(py: Python<'_>) -> PyResult<()> {
    use crate::error::KgErrorCode as C;

    py.get_type::<KgError>().setattr("code", py.None())?;
    py.get_type::<CypherError>().setattr("code", py.None())?;
    py.get_type::<ConstraintError>()
        .setattr("code", py.None())?;

    py.get_type::<CypherSyntaxError>()
        .setattr("code", C::CypherSyntax.as_str())?;
    py.get_type::<CypherTimeoutError>()
        .setattr("code", C::CypherTimeout.as_str())?;
    py.get_type::<CypherExecutionError>()
        .setattr("code", C::CypherExecution.as_str())?;
    py.get_type::<CypherTypeMismatchError>()
        .setattr("code", C::CypherTypeMismatch.as_str())?;
    py.get_type::<SchemaError>()
        .setattr("code", C::Schema.as_str())?;
    py.get_type::<ValidationError>()
        .setattr("code", C::Validation.as_str())?;
    py.get_type::<ExprError>()
        .setattr("code", C::Expr.as_str())?;
    py.get_type::<ConstraintViolationError>()
        .setattr("code", C::ConstraintViolation.as_str())?;
    py.get_type::<ConstraintCreationError>()
        .setattr("code", C::ConstraintCreationFailed.as_str())?;
    py.get_type::<OntologyViolationError>()
        .setattr("code", C::OntologyViolation.as_str())?;
    py.get_type::<TransactionConflictError>()
        .setattr("code", C::TransactionConflict.as_str())?;
    py.get_type::<NodeNotFoundError>()
        .setattr("code", C::NodeNotFound.as_str())?;
    py.get_type::<ConnectionNotFoundError>()
        .setattr("code", C::ConnectionNotFound.as_str())?;
    py.get_type::<PropertyNotFoundError>()
        .setattr("code", C::PropertyNotFound.as_str())?;
    py.get_type::<FileError>()
        .setattr("code", C::FileNotFound.as_str())?;
    py.get_type::<FileFormatError>()
        .setattr("code", C::FileFormat.as_str())?;
    py.get_type::<FileIoError>()
        .setattr("code", C::FileIo.as_str())?;
    py.get_type::<WriterLeaseHeldError>()
        .setattr("code", C::WriterLeaseHeld.as_str())?;
    py.get_type::<ReadOnlyError>()
        .setattr("code", C::ReadOnly.as_str())?;
    py.get_type::<LoadMemoryLimitError>()
        .setattr("code", C::LoadMemoryLimit.as_str())?;
    py.get_type::<ArgumentError>()
        .setattr("code", C::InvalidArgument.as_str())?;
    py.get_type::<MissingArgumentError>()
        .setattr("code", C::MissingArgument.as_str())?;
    // Both collapse to `Internal` in `KgError::code()`.
    py.get_type::<InternerCollisionError>()
        .setattr("code", C::Internal.as_str())?;
    py.get_type::<InternalError>()
        .setattr("code", C::Internal.as_str())?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interner_collision_maps_to_dedicated_python_error() {
        Python::initialize();
        Python::attach(|py| {
            let collision = kglite_core::api::InternerCollision {
                key: 7,
                existing: "first".into(),
                conflicting: "second".into(),
            };
            let error = kg_to_pyerr(RustKgError::InternerCollision(collision));
            assert!(error.is_instance_of::<InternerCollisionError>(py));
        });
    }

    #[test]
    fn durability_failed_is_a_file_io_error_with_its_own_code_on_every_route() {
        Python::initialize();
        Python::attach(|py| {
            // The `Session` route (a `KgError` through `kg_to_pyerr`) and the
            // `KnowledgeGraph` route (`durability_failed_pyerr`) are one identity.
            for error in [
                kg_to_pyerr(RustKgError::DurabilityFailed {
                    message: "log refused".into(),
                }),
                durability_failed_pyerr("log refused".into()),
            ] {
                assert!(error.is_instance_of::<FileIoError>(py));
                let code: String = error.value(py).getattr("code").unwrap().extract().unwrap();
                assert_eq!(code, "DurabilityFailed");
            }
        });
    }

    #[test]
    fn lease_and_read_only_map_to_their_subclasses_with_code_and_holder() {
        Python::initialize();
        Python::attach(|py| {
            let lease = kg_to_pyerr(RustKgError::WriterLeaseHeld {
                message: "held".into(),
                holder: kglite_core::api::io::LeaseHolder {
                    pid: Some(7),
                    since: Some("t".into()),
                    label: None,
                },
            });
            assert!(lease.is_instance_of::<WriterLeaseHeldError>(py));
            assert!(lease.is_instance_of::<FileIoError>(py));
            let holder = lease.value(py).getattr("holder").unwrap();
            let pid: u32 = holder.get_item("pid").unwrap().extract().unwrap();
            assert_eq!(pid, 7);
            assert!(holder.get_item("label").unwrap().is_none());

            let read_only = kg_to_pyerr(RustKgError::read_only("ro"));
            assert!(read_only.is_instance_of::<ReadOnlyError>(py));
            assert!(read_only.is_instance_of::<ArgumentError>(py));
            let code: String = read_only
                .value(py)
                .getattr("code")
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(code, "ReadOnly");
        });
    }

    #[test]
    fn not_durable_is_a_kg_error_and_a_value_error_with_its_code() {
        Python::initialize();
        Python::attach(|py| {
            let err = kg_to_pyerr(RustKgError::not_durable("no log"));
            assert!(err.is_instance_of::<KgError>(py));
            assert!(err.is_instance_of::<pyo3::exceptions::PyValueError>(py));
            let code: String = err.value(py).getattr("code").unwrap().extract().unwrap();
            assert_eq!(code, "NotDurable");
        });
    }

    #[test]
    fn ontology_violation_maps_to_constraint_violation_subclass() {
        Python::initialize();
        Python::attach(|py| {
            let error = kg_to_pyerr(RustKgError::OntologyViolation {
                rule: "closed_labels",
                entity: "node",
                entity_type: "Ghost".into(),
                property: None,
                message: "label Ghost is not declared".into(),
                report: Vec::new(),
            });
            assert!(error.is_instance_of::<OntologyViolationError>(py));
            assert!(error.is_instance_of::<ConstraintViolationError>(py));
        });
    }
}
