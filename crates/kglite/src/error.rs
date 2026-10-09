//! Typed error taxonomy for KGLite: the [`KgError`] enum plus a
//! [`KgErrorCode`] classification. The per-module error types
//! (`SchemaError`, `ValidationError`, `ExprError`) are bridged in via `From`
//! impls rather than duplicated here.
//!
//! ## Why
//!
//! - Python consumers can `except kglite.CypherSyntaxError:` instead of
//!   grep'ing message strings.
//! - Cypher parser line/col survives the boundary instead of being
//!   embedded in the formatted string.
//! - Bolt FAILURE-code mapping needs typed codes, so the whole engine
//!   surface is uniformly classified.
//! - MCP server error responses gain structured codes; agents can
//!   react programmatically.
//!
//! ## Hierarchy
//!
//! Every kglite-raised exception subclasses `kglite.KgError` — except
//! [`Cancelled`](KgError::Cancelled), which deliberately surfaces as the
//! builtin `KeyboardInterrupt`. The Python class chain is defined in
//! kglite-py's `error_py` via PyO3's `create_exception!` macro; the
//! `kg_to_pyerr` function there picks the most specific subclass per variant
//! (the `From<KgError> for PyErr` impl form is orphan-rule blocked).
//!
//! Cypher: `CypherSyntaxError`, `CypherTimeoutError`,
//! `CypherExecutionError`, `CypherTypeMismatchError` — all subclass
//! `CypherError`, which subclasses `KgError`.
//!
//! Constraints: `ConstraintViolationError`, `ConstraintCreationError` — both
//! subclass `ConstraintError`. `OntologyViolationError` subclasses
//! `ConstraintViolationError`, so existing `except` clauses still catch it.
//!
//! Everything else subclasses `KgError` directly: `SchemaError`,
//! `ValidationError`, `ExprError`, `TransactionConflictError`,
//! `NodeNotFoundError`, `ConnectionNotFoundError`, `PropertyNotFoundError`,
//! `FileError`, `FileFormatError`, `FileIoError`, `LoadMemoryLimitError`,
//! `ArgumentError`,
//! `MissingArgumentError`, `InternerCollisionError`, `InternalError`.
//!
//! Internal identity: `InternerCollisionError` reports a rejected persisted
//! name-key collision. `InternalError` is reserved for invariants that should
//! never trip (e.g. node-binding lookup guaranteed by upstream pattern match).

use std::fmt;
use std::path::PathBuf;

use crate::graph::blueprint::expr::ExprError;
use crate::graph::io::open::{LeaseHolder, LeaseRefusal};
use crate::graph::languages::cypher::planner::schema_check::SchemaError;
use crate::graph::schema::ValidationError;
use crate::graph::storage::interner::InternerCollision;

/// Canonical classification of every error KGLite raises.
///
/// Every [`KgError`] variant maps to one code, but not one-to-one: `Argument`
/// shares `InvalidArgument` and `InternerCollision` shares `Internal`. Unlike
/// `KgError` this is `Copy + Eq + Hash`, so it can key match dispatch tables
/// (e.g. the Bolt FAILURE-code lookup).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KgErrorCode {
    CypherSyntax,
    CypherTimeout,
    CypherExecution,
    CypherTypeMismatch,

    // Cooperative cancellation (a binding flipped the cancel flag, e.g.
    // the Python wheel's Ctrl-C / KeyboardInterrupt handler).
    Cancelled,

    Schema,
    Validation,
    Expr,

    // Declared integrity constraints (UNIQUE / NOT NULL / NODE KEY).
    // Split from `Schema` so a binding can distinguish "your write broke a
    // constraint" (retry with different data) from "your declaration cannot be
    // installed" (fix the data first, then re-declare) — they carry different
    // Neo4j status codes.
    ConstraintViolation,
    ConstraintCreationFailed,

    // A write (or a declaration over existing data) refused by the declared
    // ontology. Own code so a client can tell "the ontology said no" from
    // "a UNIQUE constraint said no"; shares the constraint Neo4j status so
    // drivers treat both alike.
    OntologyViolation,

    // Optimistic concurrency control. Split from `InvalidArgument` because a
    // conflict is the one error in the taxonomy whose correct handling is
    // "retry the whole transaction" rather than "fix the call" — bindings and
    // drivers route on that difference.
    TransactionConflict,

    // A commit the write-ahead log could not record, so it was not published
    // (`CommitOutcome::DurabilityFailed`). Split from `FileIo` because the
    // statement was fine and nothing client-side can repair the log: a
    // surface routes it as a server fault that names the lost write.
    DurabilityFailed,

    // Another process (or an un-closed handle in this one) holds the
    // cross-process writer lease for the path. Split from `FileIo` because the
    // reaction is opposite: this one is retriable as it stands, and a binding
    // must not have to string-match prose to tell the two apart.
    WriterLeaseHeld,

    // A write aimed at a handle opened or put into read-only mode. Split from
    // `InvalidArgument` because the call was well-formed: the handle, not the
    // arguments, refused it, and the fix is to write through a writable handle.
    ReadOnly,

    NodeNotFound,
    ConnectionNotFound,
    PropertyNotFound,

    FileNotFound,
    FileFormat,
    FileIo,

    // A `.kgl` load refused by the caller's own memory ceiling
    // (`LoadOptions::max_load_bytes` / `KGLITE_MAX_LOAD_MB`). Split from
    // `FileFormat` because the two call for opposite reactions: a format error
    // says "this file is broken, rebuild it", while this says "this file is
    // fine and this process cannot afford it" — raise the ceiling, defer the
    // index rebuild, or load it somewhere with more memory. A binding that
    // reported it as corruption would send an operator to rebuild a graph that
    // is not broken.
    LoadMemoryLimit,

    InvalidArgument,
    MissingArgument,

    Internal,
}

impl KgErrorCode {
    /// Every code, in declaration order. A binding's exhaustiveness test
    /// iterates this to prove its own table covers each one; the
    /// `all_lists_every_code` test keeps it complete.
    pub const ALL: &'static [KgErrorCode] = &[
        KgErrorCode::CypherSyntax,
        KgErrorCode::CypherTimeout,
        KgErrorCode::CypherExecution,
        KgErrorCode::CypherTypeMismatch,
        KgErrorCode::Cancelled,
        KgErrorCode::Schema,
        KgErrorCode::Validation,
        KgErrorCode::Expr,
        KgErrorCode::ConstraintViolation,
        KgErrorCode::ConstraintCreationFailed,
        KgErrorCode::OntologyViolation,
        KgErrorCode::TransactionConflict,
        KgErrorCode::DurabilityFailed,
        KgErrorCode::WriterLeaseHeld,
        KgErrorCode::ReadOnly,
        KgErrorCode::NodeNotFound,
        KgErrorCode::ConnectionNotFound,
        KgErrorCode::PropertyNotFound,
        KgErrorCode::FileNotFound,
        KgErrorCode::FileFormat,
        KgErrorCode::FileIo,
        KgErrorCode::LoadMemoryLimit,
        KgErrorCode::InvalidArgument,
        KgErrorCode::MissingArgument,
        KgErrorCode::Internal,
    ];

    /// Stable string representation (the PascalCase variant name). Published
    /// as the MCP server's `kglite_code`, the wheel's `.code` exception
    /// attribute, and the C ABI's `kglite_status_code_name`. The Bolt server
    /// reports [`Self::neo4j_status_code`] instead.
    pub const fn as_str(&self) -> &'static str {
        match self {
            KgErrorCode::CypherSyntax => "CypherSyntax",
            KgErrorCode::CypherTimeout => "CypherTimeout",
            KgErrorCode::CypherExecution => "CypherExecution",
            KgErrorCode::CypherTypeMismatch => "CypherTypeMismatch",
            KgErrorCode::Cancelled => "Cancelled",
            KgErrorCode::Schema => "Schema",
            KgErrorCode::Validation => "Validation",
            KgErrorCode::Expr => "Expr",
            KgErrorCode::ConstraintViolation => "ConstraintViolation",
            KgErrorCode::ConstraintCreationFailed => "ConstraintCreationFailed",
            KgErrorCode::OntologyViolation => "OntologyViolation",
            KgErrorCode::TransactionConflict => "TransactionConflict",
            KgErrorCode::DurabilityFailed => "DurabilityFailed",
            KgErrorCode::WriterLeaseHeld => "WriterLeaseHeld",
            KgErrorCode::ReadOnly => "ReadOnly",
            KgErrorCode::NodeNotFound => "NodeNotFound",
            KgErrorCode::ConnectionNotFound => "ConnectionNotFound",
            KgErrorCode::PropertyNotFound => "PropertyNotFound",
            KgErrorCode::FileNotFound => "FileNotFound",
            KgErrorCode::FileFormat => "FileFormat",
            KgErrorCode::FileIo => "FileIo",
            KgErrorCode::LoadMemoryLimit => "LoadMemoryLimit",
            KgErrorCode::InvalidArgument => "InvalidArgument",
            KgErrorCode::MissingArgument => "MissingArgument",
            KgErrorCode::Internal => "Internal",
        }
    }

    /// Canonical HTTP status code for this error, for REST / gRPC
    /// bindings. Routes client mistakes to 4xx and server-side
    /// failures to 5xx so consumers can decide retry behaviour at
    /// the protocol layer.
    ///
    /// - `CypherSyntax`, `CypherTypeMismatch`, `InvalidArgument`,
    ///   `MissingArgument` → 400 Bad Request
    /// - `NodeNotFound`, `ConnectionNotFound`, `PropertyNotFound`,
    ///   `FileNotFound` → 404 Not Found
    /// - `CypherTimeout` → 408 Request Timeout
    /// - `TransactionConflict`, `WriterLeaseHeld` → 409 Conflict
    /// - `ReadOnly` → 403 Forbidden
    /// - `Schema`, `Validation`, `Expr`, `ConstraintViolation`,
    ///   `ConstraintCreationFailed`, `OntologyViolation`, `CypherExecution` → 422 Unprocessable
    ///   Entity
    /// - `LoadMemoryLimit` → 507 Insufficient Storage
    /// - `Cancelled` → 499 Client Closed Request
    /// - `FileFormat`, `FileIo`, `DurabilityFailed`, `Internal` → 500 Internal Server Error
    ///
    /// Companion to [`Self::neo4j_status_code`] for HTTP-shaped bindings.
    pub fn http_status_code(&self) -> u16 {
        match self {
            KgErrorCode::CypherSyntax
            | KgErrorCode::CypherTypeMismatch
            | KgErrorCode::InvalidArgument
            | KgErrorCode::MissingArgument => 400,

            KgErrorCode::NodeNotFound
            | KgErrorCode::ConnectionNotFound
            | KgErrorCode::PropertyNotFound
            | KgErrorCode::FileNotFound => 404,

            KgErrorCode::CypherTimeout => 408,

            // 409 Conflict — the request was well-formed but lost an
            // optimistic-concurrency race. Retriable as-is, unlike every
            // other 4xx here.
            KgErrorCode::TransactionConflict | KgErrorCode::WriterLeaseHeld => 409,

            // 403 Forbidden — well-formed, but this handle does not accept
            // writes. Retrying it unchanged fails the same way.
            KgErrorCode::ReadOnly => 403,

            // 499 Client Closed Request (nginx convention) — the caller
            // interrupted the query before it finished.
            KgErrorCode::Cancelled => 499,

            // 507 Insufficient Storage — the request was well-formed and the
            // stored data is fine; this server cannot hold what serving it
            // would take. Retrying it unchanged fails the same way, so it is
            // deliberately not a 4xx the client is invited to repeat.
            KgErrorCode::LoadMemoryLimit => 507,

            // A statement that failed on what it was given — a malformed
            // `valid_at` date, a property the type does not have, an
            // undeclared type, `1/0`, a work budget exceeded — is the
            // client's to fix, as `Validation` / `Expr` are. Server faults
            // surface as `Internal` / `FileIo`.
            KgErrorCode::Schema
            | KgErrorCode::Validation
            | KgErrorCode::Expr
            | KgErrorCode::CypherExecution
            | KgErrorCode::ConstraintViolation
            | KgErrorCode::ConstraintCreationFailed
            | KgErrorCode::OntologyViolation => 422,

            KgErrorCode::FileFormat
            | KgErrorCode::FileIo
            | KgErrorCode::DurabilityFailed
            | KgErrorCode::Internal => 500,
        }
    }

    /// Canonical Neo4j Bolt status code for this error code, of the
    /// shape `Neo.{Class}.{Category}.{Title}`. The Bolt protocol
    /// wraps these in a `FAILURE` response and drivers route by the
    /// class prefix (`ClientError` vs `DatabaseError` vs
    /// `TransientError`).
    ///
    /// Shared here so any Neo4j-wire-compatible binding gets the canonical
    /// mapping without re-deriving the table. Bindings still own the wrapping
    /// in their own error type — only the code string is shared.
    pub fn neo4j_status_code(&self) -> &'static str {
        match self {
            KgErrorCode::CypherSyntax => "Neo.ClientError.Statement.SyntaxError",
            KgErrorCode::CypherTimeout => "Neo.ClientError.Transaction.TransactionTimedOut",
            KgErrorCode::Cancelled => "Neo.ClientError.Transaction.Terminated",
            KgErrorCode::CypherTypeMismatch => "Neo.ClientError.Statement.TypeError",
            KgErrorCode::Schema => "Neo.ClientError.Schema.ConstraintValidationFailed",
            KgErrorCode::ConstraintViolation | KgErrorCode::OntologyViolation => {
                "Neo.ClientError.Schema.ConstraintValidationFailed"
            }
            KgErrorCode::ConstraintCreationFailed => {
                "Neo.ClientError.Schema.ConstraintCreationFailed"
            }
            // The `TransientError` *class* is the contract here, not the
            // title: Neo4j drivers decide retryability from the class
            // prefix, so an OCC conflict must be published in this class
            // for a managed transaction to re-run the unit of work
            // instead of raising through to the caller. `Outdated` is
            // Neo4j's published code for "transaction saw state
            // invalidated by applied updates; may succeed if retried",
            // which is exactly whole-graph OCC. Both the prefix and the
            // exact string are pinned by tests (error_map.rs unit test,
            // tests/test_bolt_server_transactions.py, and the JS/Java
            // conformance corpora).
            KgErrorCode::TransactionConflict => "Neo.TransientError.Transaction.Outdated",
            // Neo4j's code for a database that cannot take the request yet;
            // its `TransientError` class matches the retriable-as-is reaction a
            // held lease calls for.
            KgErrorCode::WriterLeaseHeld => "Neo.TransientError.General.DatabaseUnavailable",
            // Neo4j's published code for "this is a read only database,
            // writing is not allowed". Deliberately not
            // `General.ForbiddenOnReadOnlyDatabase`: the Neo4j drivers class
            // that one as a *transient* routing signal and re-run a managed
            // transaction against it, so a write to a read-only server would
            // retry for the driver's whole retry window instead of failing
            // once. `General.ReadOnly` is a permanent `ClientError` (the
            // drivers' `Forbidden`).
            KgErrorCode::ReadOnly => "Neo.ClientError.General.ReadOnly",
            // `CypherExecution` is a statement that failed on its inputs (see
            // `http_status_code`); publishing it as `DatabaseError` told a
            // driver the server broke when the query was at fault.
            KgErrorCode::Validation | KgErrorCode::Expr | KgErrorCode::CypherExecution => {
                "Neo.ClientError.Statement.ArgumentError"
            }
            KgErrorCode::NodeNotFound
            | KgErrorCode::ConnectionNotFound
            | KgErrorCode::PropertyNotFound => "Neo.ClientError.Statement.EntityNotFound",
            KgErrorCode::InvalidArgument => "Neo.ClientError.Statement.ArgumentError",
            KgErrorCode::MissingArgument => "Neo.ClientError.Statement.ParameterMissing",
            // Neo4j's own published code for "there is not enough memory to
            // perform the current task". Its `TransientError` class invites a
            // driver to retry, which is inert here: a `.kgl` load is not
            // reachable through a Bolt managed transaction, and the wire
            // mapping exists so a Neo4j-compatible binding reports the
            // condition Neo4j reports rather than an unknown-error catch-all.
            KgErrorCode::LoadMemoryLimit => "Neo.TransientError.General.OutOfMemoryError",
            KgErrorCode::FileNotFound
            | KgErrorCode::FileFormat
            | KgErrorCode::FileIo
            | KgErrorCode::DurabilityFailed
            | KgErrorCode::Internal => "Neo.DatabaseError.General.UnknownError",
        }
    }
}

impl fmt::Display for KgErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The canonical error type for KGLite. Every fallible operation
/// reachable from the public API returns `Result<T, KgError>`
/// (directly or via `?` from a `From`-convertible source type).
///
/// At the PyO3 boundary, kglite-py's `error_py::kg_to_pyerr` picks the most
/// specific Python exception subclass based on the variant.
#[derive(Debug)]
pub enum KgError {
    /// Cypher syntax error from the tokenizer or parser. Carries the
    /// line and column (1-indexed) where parsing failed. Both are
    /// `Option` because some parser-internal errors aren't pinned to
    /// a specific position (e.g. "expected end of input").
    CypherSyntax {
        message: String,
        line: Option<usize>,
        col: Option<usize>,
    },

    /// Cypher query exceeded its `timeout_ms` budget. Both elapsed and
    /// limit reported so the agent can decide whether to retry with a
    /// longer budget or rewrite the query. `limit_ms` is the configured
    /// budget and `elapsed_ms` is measured from the same origin — the instant
    /// the caller resolved the deadline (see
    /// `ExecuteOptions::deadline_origin`).
    ///
    /// `message` is the abort site's own prose, which carries the hint that
    /// applies to *where* the deadline fired — the generic
    /// "anchor the query" advice from the executor's poll, the
    /// "add an index on a predicate property" advice from an unanchored node
    /// scan. `limit_ms == 0` means no budget was measurable, and the numbers
    /// are then omitted from the rendered message rather than shown as zeroes.
    CypherTimeout {
        elapsed_ms: u64,
        limit_ms: u64,
        message: String,
    },

    /// A statement that failed while executing, on what it was given — a
    /// malformed function argument, a property or declaration the query
    /// relies on that does not exist, a stored value an operation cannot
    /// read, an exceeded work budget. A client error on every wire (Bolt
    /// `ClientError`, HTTP 422). Optional position points at the AST node
    /// when known.
    CypherExecution {
        message: String,
        position: Option<(usize, usize)>,
    },

    /// Type mismatch in Cypher evaluation (e.g. arithmetic on a String,
    /// IN over a non-list). Distinct from `CypherExecution` so consumers
    /// can react with a type-coercion retry vs a bail.
    CypherTypeMismatch {
        expected: String,
        found: String,
        context: String,
    },

    /// The query was cooperatively cancelled — a binding flipped the
    /// [`ExecuteOptions::cancel`](crate::api::session::ExecuteOptions)
    /// flag mid-run (the Python wheel does this from its Ctrl-C / SIGINT
    /// handler). Bindings map this to their interrupt type
    /// (`KeyboardInterrupt` in the wheel). Distinct from `CypherTimeout`
    /// (a deadline) and `CypherExecution` (a genuine failure).
    Cancelled,

    /// Query validation failure (unknown property, unknown node type under
    /// a locked schema, or undefined variable).
    /// Bridged from
    /// [`SchemaError`](crate::graph::languages::cypher::planner::schema_check::SchemaError)
    /// via `From`.
    Schema {
        kind: SchemaErrorKindRepr,
        message: String,
    },

    /// Structural validation failure (missing required field, wrong
    /// connection endpoint, etc.). Wraps the existing 6-variant
    /// [`ValidationError`] enum verbatim.
    Validation(ValidationError),

    /// A write violated a declared integrity constraint — a UNIQUE duplicate, or
    /// a NOT NULL / NODE KEY property left absent. The write was rejected before
    /// touching storage, so the graph is unchanged.
    ///
    /// `kind` is the Cypher spelling (`UNIQUE` / `NOT NULL` / `NODE KEY`) and
    /// `descriptor` the canonical `Label.property` / `Label.(a, b)` name, so a
    /// binding can report which constraint fired without parsing `message`.
    ConstraintViolation {
        kind: &'static str,
        node_type: String,
        properties: Vec<String>,
        descriptor: String,
        message: String,
    },

    /// Declaring a constraint failed because the existing data already violates
    /// it. Distinct from [`Self::ConstraintViolation`]: the fix is to deduplicate
    /// the stored rows and re-declare, not to change an incoming write.
    ConstraintCreationFailed {
        kind: &'static str,
        node_type: String,
        properties: Vec<String>,
        descriptor: String,
        message: String,
    },

    /// A write was refused by the declared ontology, or a declaration was
    /// refused because stored data already violates it. The graph is unchanged.
    ///
    /// `rule` is `required_property` / `property_type` / `closed_labels` /
    /// `domain` / `range`; `entity` is `node` / `relationship`; `entity_type`
    /// the label or relationship type; `property` the offending property when
    /// the rule is a property rule. `report` is empty for a refused write and
    /// holds the per-rule breakdown for a refused declaration.
    OntologyViolation {
        rule: &'static str,
        entity: &'static str,
        entity_type: String,
        property: Option<String>,
        message: String,
        report: Vec<crate::graph::ontology::violation::OntologyReportEntry>,
    },

    /// An optimistic-concurrency commit lost its race: the graph advanced
    /// between `begin()` and `commit()`, so the transaction's working copy is
    /// stale and applying it would silently discard the newer commit.
    ///
    /// The transaction is spent — the caller re-runs its work against a fresh
    /// `begin()`. Both versions are carried so a binding can report the gap
    /// (and a retry loop can log how far behind it was) without parsing
    /// `message`.
    ///
    /// Note this is a *whole-graph* version check, not a read/write-set
    /// intersection: commit publishes the transaction's working copy by
    /// pointer swap, so any concurrent commit — even one touching entirely
    /// different nodes — makes this transaction's copy stale. See
    /// `docs/concepts/concurrency.md`.
    TransactionConflict {
        base_version: u64,
        current_version: u64,
    },

    /// A commit the write-ahead log could not record (`--durability full` /
    /// `normal`), so the engine did not publish it and the graph is unchanged.
    /// The statement itself was fine and a re-run may succeed, but nothing the
    /// caller can change repairs the log; a surface must not acknowledge it.
    /// `message` is the log's own error text.
    DurabilityFailed { message: String },

    /// Another process (or an un-closed handle in this one) holds the writer
    /// lease for the path. `holder` is the structured record the holder
    /// published, best effort: a contender that loses a startup race can read an
    /// empty one. `message` is the engine's prose for a human.
    WriterLeaseHeld {
        message: String,
        holder: LeaseHolder,
    },

    /// A write was refused because the handle is read-only (a `readOnly` open,
    /// `read_only(True)`, a read-only transaction or session). The graph is
    /// unchanged.
    ReadOnly { message: String },

    /// Blueprint expression evaluation failure. Wraps the existing
    /// 7-variant [`ExprError`] enum verbatim.
    Expr(ExprError),

    /// A node identified by `(node_type, id)` doesn't exist in the
    /// graph. Used by mutation and traversal paths that expect a node.
    NodeNotFound { node_type: String, id: String },

    /// A connection type isn't declared in the schema.
    ConnectionNotFound { connection_type: String },

    /// A property is missing from a node or relationship.
    PropertyNotFound { node_type: String, property: String },

    /// A file the user named doesn't exist on disk.
    FileNotFound(PathBuf),

    /// A file exists but its contents are malformed (bad `.kgl` header,
    /// truncated blueprint JSON, etc.). The v3→v4 hard-break message
    /// surfaces here too.
    FileFormat { path: PathBuf, message: String },

    /// Generic I/O failure (permission denied, mid-read EOF, mmap
    /// failure). Carries the original [`std::io::Error`] for
    /// downstream inspection.
    FileIo(std::io::Error),

    /// A `.kgl` load was refused *before decoding* because its estimated peak
    /// memory exceeded the ceiling the caller set
    /// (`LoadOptions::max_load_bytes` / `KGLITE_MAX_LOAD_MB`).
    ///
    /// The file is valid and nothing was read past its metadata head. The
    /// message carries the estimate, the ceiling, the terms it is made of, and
    /// the ways out — see `graph::io::file`'s `load_memory_refusal`.
    LoadMemoryLimit(String),

    /// A user-supplied argument violated a precondition with full
    /// structured context — argument name, what was expected, what
    /// was found. Used when the call site can naturally populate all
    /// three; agents can react programmatically on the structured fields.
    InvalidArgument {
        argument: String,
        expected: String,
        found: String,
    },

    /// A user-supplied argument violated a precondition; free-form
    /// message form for sites where the existing message is already
    /// good and forcing a structured shape would lose information.
    /// Maps to the same `kglite.ArgumentError` Python class as
    /// `InvalidArgument`.
    Argument(String),

    /// A required argument wasn't passed.
    MissingArgument(String),

    /// Two distinct names mapped to the same persisted u64 interner key. The
    /// existing mapping is retained; callers must reject the operation.
    InternerCollision(InternerCollision),

    /// An invariant was violated. Reserved for "should never happen"
    /// — e.g. a node-binding lookup whose existence was guaranteed by
    /// an upstream pattern match. Used in place of an `unwrap()`: where
    /// the unwrap would have panicked, this returns the typed error
    /// instead. The `location` is a `'static str` pointing at the source
    /// site (e.g. `"match_clause.rs::evaluate_pattern node_var lookup"`).
    Internal {
        message: String,
        location: &'static str,
    },
}

/// Wire-stable repr of [`SchemaErrorKind`](crate::graph::languages::cypher::planner::schema_check::SchemaErrorKind).
///
/// We don't re-export `SchemaErrorKind` because that crate path is
/// nested deep in the cypher tree; the repr lives next to `KgError`
/// for ergonomic match-on-variant in downstream callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaErrorKindRepr {
    UnknownProperty,
    UnknownNodeType,
    UndefinedVariable,
}

impl From<crate::graph::languages::cypher::planner::schema_check::SchemaErrorKind>
    for SchemaErrorKindRepr
{
    fn from(
        value: crate::graph::languages::cypher::planner::schema_check::SchemaErrorKind,
    ) -> Self {
        use crate::graph::languages::cypher::planner::schema_check::SchemaErrorKind;
        match value {
            SchemaErrorKind::UnknownProperty => SchemaErrorKindRepr::UnknownProperty,
            SchemaErrorKind::UnknownNodeType => SchemaErrorKindRepr::UnknownNodeType,
            SchemaErrorKind::UndefinedVariable => SchemaErrorKindRepr::UndefinedVariable,
        }
    }
}

impl KgError {
    /// Canonical [`KgErrorCode`] for this error. Drives kglite-py's
    /// `kg_to_pyerr` boundary mapping and the Bolt server's
    /// [`neo4j_status_code`](KgErrorCode::neo4j_status_code) lookup.
    pub fn code(&self) -> KgErrorCode {
        match self {
            KgError::CypherSyntax { .. } => KgErrorCode::CypherSyntax,
            KgError::CypherTimeout { .. } => KgErrorCode::CypherTimeout,
            KgError::CypherExecution { .. } => KgErrorCode::CypherExecution,
            KgError::CypherTypeMismatch { .. } => KgErrorCode::CypherTypeMismatch,
            KgError::Cancelled => KgErrorCode::Cancelled,
            KgError::Schema { .. } => KgErrorCode::Schema,
            KgError::Validation(_) => KgErrorCode::Validation,
            KgError::ConstraintViolation { .. } => KgErrorCode::ConstraintViolation,
            KgError::ConstraintCreationFailed { .. } => KgErrorCode::ConstraintCreationFailed,
            KgError::OntologyViolation { .. } => KgErrorCode::OntologyViolation,
            KgError::TransactionConflict { .. } => KgErrorCode::TransactionConflict,
            KgError::DurabilityFailed { .. } => KgErrorCode::DurabilityFailed,
            KgError::WriterLeaseHeld { .. } => KgErrorCode::WriterLeaseHeld,
            KgError::ReadOnly { .. } => KgErrorCode::ReadOnly,
            KgError::Expr(_) => KgErrorCode::Expr,
            KgError::NodeNotFound { .. } => KgErrorCode::NodeNotFound,
            KgError::ConnectionNotFound { .. } => KgErrorCode::ConnectionNotFound,
            KgError::PropertyNotFound { .. } => KgErrorCode::PropertyNotFound,
            KgError::FileNotFound(_) => KgErrorCode::FileNotFound,
            KgError::FileFormat { .. } => KgErrorCode::FileFormat,
            KgError::FileIo(_) => KgErrorCode::FileIo,
            KgError::LoadMemoryLimit(_) => KgErrorCode::LoadMemoryLimit,
            KgError::InvalidArgument { .. } | KgError::Argument(_) => KgErrorCode::InvalidArgument,
            KgError::MissingArgument(_) => KgErrorCode::MissingArgument,
            KgError::InternerCollision(_) => KgErrorCode::Internal,
            KgError::Internal { .. } => KgErrorCode::Internal,
        }
    }

    /// Source position (1-indexed line and column) when the error has
    /// one. Currently set by `CypherSyntax` (always when the tokenizer/
    /// parser knows it) and optionally by `CypherExecution`. Returns
    /// `None` for everything else.
    pub fn position(&self) -> Option<(usize, usize)> {
        match self {
            KgError::CypherSyntax {
                line: Some(l),
                col: Some(c),
                ..
            } => Some((*l, *c)),
            KgError::CypherExecution {
                position: Some(p), ..
            } => Some(*p),
            _ => None,
        }
    }
}

impl fmt::Display for KgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KgError::CypherSyntax { message, line, col } => match (line, col) {
                (Some(l), Some(c)) => write!(
                    f,
                    "Cypher syntax error at line {}, col {}: {}",
                    l, c, message
                ),
                _ => write!(f, "Cypher syntax error: {}", message),
            },
            KgError::CypherTimeout {
                elapsed_ms,
                limit_ms,
                message,
            } => {
                if *limit_ms == 0 {
                    write!(f, "{}", message)
                } else {
                    write!(
                        f,
                        "{} (elapsed {}ms, limit {}ms)",
                        message, elapsed_ms, limit_ms
                    )
                }
            }
            KgError::CypherExecution { message, position } => match position {
                Some((l, c)) => write!(
                    f,
                    "Cypher execution error at line {}, col {}: {}",
                    l, c, message
                ),
                None => write!(f, "Cypher execution error: {}", message),
            },
            KgError::Cancelled => write!(f, "Query cancelled"),
            KgError::CypherTypeMismatch {
                expected,
                found,
                context,
            } => write!(
                f,
                "Cypher type mismatch in {}: expected {}, found {}",
                context, expected, found
            ),
            KgError::Schema { message, .. } => write!(f, "Schema error: {}", message),
            KgError::Validation(v) => write!(f, "Validation error: {}", v),
            // The core already renders these in Neo4j's shape (see
            // `graph::constraints::ConstraintViolation`), so don't re-wrap them
            // in a prefix that would bury the actionable part.
            KgError::ConstraintViolation { message, .. }
            | KgError::ConstraintCreationFailed { message, .. }
            | KgError::OntologyViolation { message, .. } => f.write_str(message),
            KgError::TransactionConflict {
                base_version,
                current_version,
            } => write!(
                f,
                "Transaction conflict: the graph was modified since begin() \
                 (began at version {}, now at version {}), so this \
                 transaction's changes are based on a stale snapshot and were \
                 not applied. Retry the transaction — re-run the work against \
                 a fresh begin().",
                base_version, current_version
            ),
            KgError::DurabilityFailed { message } => write!(
                f,
                "commit was NOT applied — the write-ahead log rejected it, and a write \
                 that cannot be logged is not acknowledged: {message}"
            ),
            KgError::WriterLeaseHeld { message, .. } | KgError::ReadOnly { message } => {
                f.write_str(message)
            }
            KgError::Expr(e) => write!(f, "Expression error: {}", e),
            KgError::NodeNotFound { node_type, id } => {
                write!(f, "Node not found: {} with id {:?}", node_type, id)
            }
            KgError::ConnectionNotFound { connection_type } => {
                write!(f, "Connection type not found: {}", connection_type)
            }
            KgError::PropertyNotFound {
                node_type,
                property,
            } => write!(f, "Property '{}' not found on {}", property, node_type),
            KgError::FileNotFound(path) => write!(f, "File not found: {}", path.display()),
            KgError::FileFormat { path, message } => {
                write!(f, "File format error ({}): {}", path.display(), message)
            }
            KgError::FileIo(e) => write!(f, "File I/O error: {}", e),
            KgError::LoadMemoryLimit(message) => {
                write!(f, "Load refused by the memory ceiling: {message}")
            }
            KgError::InvalidArgument {
                argument,
                expected,
                found,
            } => write!(
                f,
                "Invalid argument '{}': expected {}, found {}",
                argument, expected, found
            ),
            KgError::Argument(message) => write!(f, "Invalid argument: {}", message),
            KgError::MissingArgument(name) => write!(f, "Missing required argument: {}", name),
            KgError::InternerCollision(collision) => collision.fmt(f),
            KgError::Internal { message, location } => {
                write!(f, "Internal error at {}: {}", location, message)
            }
        }
    }
}

impl std::error::Error for KgError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            KgError::FileIo(e) => Some(e),
            KgError::InternerCollision(e) => Some(e),
            _ => None,
        }
    }
}

impl From<SchemaError> for KgError {
    fn from(e: SchemaError) -> Self {
        KgError::Schema {
            kind: e.kind.into(),
            message: e.message,
        }
    }
}

impl From<ValidationError> for KgError {
    fn from(e: ValidationError) -> Self {
        KgError::Validation(e)
    }
}

impl From<ExprError> for KgError {
    fn from(e: ExprError) -> Self {
        KgError::Expr(e)
    }
}

impl From<std::io::Error> for KgError {
    fn from(e: std::io::Error) -> Self {
        match e.kind() {
            // The load-memory ceiling's refusal, which is a policy decision
            // about this process rather than an I/O fault. Reporting it as
            // `FileIo` would tell an operator their disk misbehaved.
            std::io::ErrorKind::OutOfMemory => KgError::LoadMemoryLimit(e.to_string()),
            // Every other kind — including `NotFound`, whose path is not
            // recoverable from an `io::Error` — keeps the original error for
            // downstream inspection.
            _ => KgError::FileIo(e),
        }
    }
}

impl From<LeaseRefusal> for KgError {
    /// A contended acquisition becomes [`KgError::WriterLeaseHeld`] with the
    /// holder structured; any other refusal is plain I/O (nobody holds
    /// anything), so it stays [`KgError::FileIo`].
    fn from(refusal: LeaseRefusal) -> Self {
        match refusal.holder {
            Some(holder) => KgError::WriterLeaseHeld {
                message: refusal.error.to_string(),
                holder,
            },
            None => KgError::from(refusal.error),
        }
    }
}

impl KgError {
    /// A write refused because the handle is read-only.
    pub fn read_only(message: impl Into<String>) -> Self {
        KgError::ReadOnly {
            message: message.into(),
        }
    }
}

impl From<InternerCollision> for KgError {
    fn from(e: InternerCollision) -> Self {
        KgError::InternerCollision(e)
    }
}

impl From<crate::graph::constraints::ConstraintViolation> for KgError {
    /// Lift a core constraint violation to the public error type, splitting on
    /// whether a *write* or a *declaration* failed — the two carry different
    /// Neo4j status codes and call for different fixes.
    fn from(violation: crate::graph::constraints::ConstraintViolation) -> Self {
        let message = violation.to_string();
        let descriptor = violation.descriptor();
        // Entity-aware: one `ConstraintKind` serves both sides, and a
        // relationship key is spelled RELATIONSHIP KEY. No relationship
        // constraint can be a key today, so this changes no current message —
        // it stops the first one that can from reporting the node spelling.
        let kind = violation.kind.keyword_for(violation.entity);
        if violation.is_declaration_failure() {
            KgError::ConstraintCreationFailed {
                kind,
                node_type: violation.node_type,
                properties: violation.properties,
                descriptor,
                message,
            }
        } else {
            KgError::ConstraintViolation {
                kind,
                node_type: violation.node_type,
                properties: violation.properties,
                descriptor,
                message,
            }
        }
    }
}

fn entity_str(entity: crate::graph::constraints::EntityKind) -> &'static str {
    match entity {
        crate::graph::constraints::EntityKind::Node => "node",
        crate::graph::constraints::EntityKind::Relationship => "relationship",
    }
}

impl From<crate::graph::ontology::violation::OntologyViolation> for KgError {
    fn from(v: crate::graph::ontology::violation::OntologyViolation) -> Self {
        KgError::OntologyViolation {
            rule: v.rule.as_str(),
            entity: entity_str(v.entity),
            entity_type: v.entity_type,
            property: v.property,
            message: v.message,
            report: Vec::new(),
        }
    }
}

impl From<crate::graph::ontology::violation::OntologyDeclarationRefused> for KgError {
    /// The headline `rule`/`entity_type`/`property` are the first report
    /// entry's; the full breakdown rides in `report`.
    fn from(r: crate::graph::ontology::violation::OntologyDeclarationRefused) -> Self {
        let head = r.entries.first();
        KgError::OntologyViolation {
            rule: head.map_or("declaration", |e| e.rule.as_str()),
            entity: head.map_or("node", |e| entity_str(e.entity)),
            entity_type: head.map(|e| e.entity_type.clone()).unwrap_or_default(),
            property: head.and_then(|e| e.property.clone()),
            message: r.message,
            report: r.entries,
        }
    }
}

/// Public convenience alias for downstream Rust callers that prefer a
/// single KGLite result spelling. Engine internals use explicit result types
/// where the error boundary benefits from being visible.
#[allow(dead_code)]
pub type KgResult<T> = std::result::Result<T, KgError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_round_trip() {
        let e = KgError::CypherSyntax {
            message: "expected RETURN".to_string(),
            line: Some(3),
            col: Some(12),
        };
        assert_eq!(e.code(), KgErrorCode::CypherSyntax);
        assert_eq!(e.position(), Some((3, 12)));
    }

    #[test]
    fn display_includes_position() {
        let e = KgError::CypherSyntax {
            message: "expected RETURN".to_string(),
            line: Some(3),
            col: Some(12),
        };
        let s = format!("{}", e);
        assert!(s.contains("line 3"));
        assert!(s.contains("col 12"));
        assert!(s.contains("expected RETURN"));
    }

    #[test]
    fn display_without_position() {
        let e = KgError::CypherExecution {
            message: "div by zero".to_string(),
            position: None,
        };
        let s = format!("{}", e);
        assert!(s.contains("div by zero"));
        assert!(!s.contains("line"));
    }

    /// Position of each code in [`KgErrorCode::ALL`]. The match has no
    /// wildcard, so a new variant is a compile error here and its author must
    /// pick the next ordinal, which `all_lists_every_code` then holds `ALL` to.
    fn ordinal(code: KgErrorCode) -> usize {
        match code {
            KgErrorCode::CypherSyntax => 0,
            KgErrorCode::CypherTimeout => 1,
            KgErrorCode::CypherExecution => 2,
            KgErrorCode::CypherTypeMismatch => 3,
            KgErrorCode::Cancelled => 4,
            KgErrorCode::Schema => 5,
            KgErrorCode::Validation => 6,
            KgErrorCode::Expr => 7,
            KgErrorCode::ConstraintViolation => 8,
            KgErrorCode::ConstraintCreationFailed => 9,
            KgErrorCode::OntologyViolation => 10,
            KgErrorCode::TransactionConflict => 11,
            KgErrorCode::DurabilityFailed => 12,
            KgErrorCode::WriterLeaseHeld => 13,
            KgErrorCode::ReadOnly => 14,
            KgErrorCode::NodeNotFound => 15,
            KgErrorCode::ConnectionNotFound => 16,
            KgErrorCode::PropertyNotFound => 17,
            KgErrorCode::FileNotFound => 18,
            KgErrorCode::FileFormat => 19,
            KgErrorCode::FileIo => 20,
            KgErrorCode::LoadMemoryLimit => 21,
            KgErrorCode::InvalidArgument => 22,
            KgErrorCode::MissingArgument => 23,
            KgErrorCode::Internal => 24,
        }
    }

    #[test]
    fn all_lists_every_code() {
        let mut seen: Vec<usize> = KgErrorCode::ALL.iter().map(|c| ordinal(*c)).collect();
        seen.sort_unstable();
        assert_eq!(seen, (0..seen.len()).collect::<Vec<_>>());
        let max = seen.last().copied().unwrap();
        assert_eq!(ordinal(KgErrorCode::Internal), max, "Internal is last");
        for (i, code) in KgErrorCode::ALL.iter().enumerate() {
            assert_eq!(ordinal(*code), i, "{code:?} is out of declaration order");
        }
    }

    #[test]
    fn lease_and_read_only_identities() {
        assert_eq!(KgErrorCode::WriterLeaseHeld.as_str(), "WriterLeaseHeld");
        assert_eq!(KgErrorCode::WriterLeaseHeld.http_status_code(), 409);
        assert_eq!(KgErrorCode::ReadOnly.as_str(), "ReadOnly");
        assert_eq!(KgErrorCode::ReadOnly.http_status_code(), 403);
        assert_eq!(
            KgErrorCode::ReadOnly.neo4j_status_code(),
            "Neo.ClientError.General.ReadOnly"
        );
        assert!(KgErrorCode::WriterLeaseHeld
            .neo4j_status_code()
            .starts_with("Neo.TransientError."));
    }

    #[test]
    fn contended_refusal_becomes_writer_lease_held_and_io_refusal_stays_file_io() {
        let contended = LeaseRefusal {
            holder: Some(LeaseHolder {
                pid: Some(4242),
                since: Some("2026-10-09T00:00:00Z".into()),
                label: Some("tester".into()),
            }),
            error: std::io::Error::new(std::io::ErrorKind::WouldBlock, "held"),
        };
        match KgError::from(contended) {
            KgError::WriterLeaseHeld { message, holder } => {
                assert_eq!(message, "held");
                assert_eq!(holder.pid, Some(4242));
                assert_eq!(holder.label.as_deref(), Some("tester"));
            }
            other => panic!("expected WriterLeaseHeld, got {other:?}"),
        }
        let plain = LeaseRefusal {
            holder: None,
            error: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
        };
        assert_eq!(KgError::from(plain).code(), KgErrorCode::FileIo);
    }

    #[test]
    fn kgerror_code_as_str_stable() {
        // Wire-stable codes — any change here is a Bolt protocol breaking change.
        assert_eq!(KgErrorCode::CypherSyntax.as_str(), "CypherSyntax");
        assert_eq!(KgErrorCode::NodeNotFound.as_str(), "NodeNotFound");
        assert_eq!(KgErrorCode::FileFormat.as_str(), "FileFormat");
    }

    #[test]
    fn from_schema_error_preserves_kind_and_message() {
        use crate::graph::languages::cypher::planner::schema_check::SchemaErrorKind;
        let se = SchemaError {
            kind: SchemaErrorKind::UnknownProperty,
            message: "no such property 'foo'".to_string(),
        };
        let kg: KgError = se.into();
        assert_eq!(kg.code(), KgErrorCode::Schema);
        match kg {
            KgError::Schema { kind, message } => {
                assert_eq!(kind, SchemaErrorKindRepr::UnknownProperty);
                assert_eq!(message, "no such property 'foo'");
            }
            _ => panic!("expected Schema variant"),
        }
    }

    #[test]
    fn from_io_error_classifies_as_file_io() {
        let io = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let kg: KgError = io.into();
        assert_eq!(kg.code(), KgErrorCode::FileIo);
    }

    /// The load-memory ceiling's refusal must not arrive as an I/O fault: the
    /// disk is fine, and the operator's fix is a bigger ceiling, not a bigger
    /// disk.
    #[test]
    fn from_io_error_classifies_out_of_memory_as_the_load_ceiling() {
        let io = std::io::Error::new(std::io::ErrorKind::OutOfMemory, "estimated 900 MB");
        let kg: KgError = io.into();
        assert_eq!(kg.code(), KgErrorCode::LoadMemoryLimit);
        assert!(format!("{kg}").contains("estimated 900 MB"));
        assert_eq!(KgErrorCode::LoadMemoryLimit.as_str(), "LoadMemoryLimit");
        assert_eq!(KgErrorCode::LoadMemoryLimit.http_status_code(), 507);
        assert_eq!(
            KgErrorCode::LoadMemoryLimit.neo4j_status_code(),
            "Neo.TransientError.General.OutOfMemoryError"
        );
    }

    #[test]
    fn http_status_code_categorises_correctly() {
        assert_eq!(KgErrorCode::CypherSyntax.http_status_code(), 400);
        assert_eq!(KgErrorCode::CypherTypeMismatch.http_status_code(), 400);
        assert_eq!(KgErrorCode::InvalidArgument.http_status_code(), 400);
        assert_eq!(KgErrorCode::MissingArgument.http_status_code(), 400);

        assert_eq!(KgErrorCode::NodeNotFound.http_status_code(), 404);
        assert_eq!(KgErrorCode::ConnectionNotFound.http_status_code(), 404);
        assert_eq!(KgErrorCode::PropertyNotFound.http_status_code(), 404);
        assert_eq!(KgErrorCode::FileNotFound.http_status_code(), 404);

        assert_eq!(KgErrorCode::CypherTimeout.http_status_code(), 408);

        assert_eq!(KgErrorCode::Schema.http_status_code(), 422);
        assert_eq!(KgErrorCode::Validation.http_status_code(), 422);
        assert_eq!(KgErrorCode::Expr.http_status_code(), 422);
        assert_eq!(KgErrorCode::CypherExecution.http_status_code(), 422);

        assert_eq!(KgErrorCode::FileFormat.http_status_code(), 500);
        assert_eq!(KgErrorCode::FileIo.http_status_code(), 500);
        assert_eq!(KgErrorCode::Internal.http_status_code(), 500);
    }

    #[test]
    fn every_error_code_has_an_http_status() {
        // This list is maintained BY HAND and must name every `KgErrorCode`
        // variant. It previously omitted four of them and claimed to be
        // compile-time exhaustive, which it is not: adding a variant breaks
        // `http_status_code`'s match, not this loop, so a missing entry here
        // is silently untested.
        for code in [
            KgErrorCode::CypherSyntax,
            KgErrorCode::CypherTimeout,
            KgErrorCode::CypherExecution,
            KgErrorCode::CypherTypeMismatch,
            KgErrorCode::Cancelled,
            KgErrorCode::Schema,
            KgErrorCode::Validation,
            KgErrorCode::Expr,
            KgErrorCode::ConstraintViolation,
            KgErrorCode::ConstraintCreationFailed,
            KgErrorCode::OntologyViolation,
            KgErrorCode::TransactionConflict,
            KgErrorCode::DurabilityFailed,
            KgErrorCode::NodeNotFound,
            KgErrorCode::ConnectionNotFound,
            KgErrorCode::PropertyNotFound,
            KgErrorCode::FileNotFound,
            KgErrorCode::FileFormat,
            KgErrorCode::FileIo,
            KgErrorCode::LoadMemoryLimit,
            KgErrorCode::InvalidArgument,
            KgErrorCode::MissingArgument,
            KgErrorCode::Internal,
        ] {
            let code_val = code.http_status_code();
            assert!(
                (400..=599).contains(&code_val),
                "code {code:?} mapped to non-4xx-5xx http status: {code_val}"
            );
        }
    }
    #[test]
    fn ontology_violation_surfaces() {
        use crate::graph::constraints::EntityKind;
        use crate::graph::ontology::violation::{OntologyRule, OntologyViolation};
        let v = OntologyViolation::new(
            OntologyRule::RequiredProperty,
            EntityKind::Node,
            "Person",
            Some("name".into()),
            "Ontology violation: Person requires property name",
        );
        let kg = KgError::from(v);
        assert_eq!(kg.code(), KgErrorCode::OntologyViolation);
        assert_eq!(kg.code().as_str(), "OntologyViolation");
        assert_eq!(kg.code().http_status_code(), 422);
        assert_eq!(
            kg.code().neo4j_status_code(),
            "Neo.ClientError.Schema.ConstraintValidationFailed"
        );
        assert_eq!(
            kg.to_string(),
            "Ontology violation: Person requires property name"
        );
        match kg {
            KgError::OntologyViolation {
                rule,
                entity,
                entity_type,
                property,
                report,
                ..
            } => {
                assert_eq!(rule, "required_property");
                assert_eq!(entity, "node");
                assert_eq!(entity_type, "Person");
                assert_eq!(property.as_deref(), Some("name"));
                assert!(report.is_empty());
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn ontology_declaration_refusal_carries_report() {
        use crate::graph::constraints::EntityKind;
        use crate::graph::ontology::violation::{
            OntologyDeclarationRefused, OntologyReportEntry, OntologyRule,
        };
        let kg = KgError::from(OntologyDeclarationRefused {
            entries: vec![OntologyReportEntry {
                rule: OntologyRule::Domain,
                entity: EntityKind::Relationship,
                entity_type: "WORKS_AT".into(),
                property: None,
                count: 3,
            }],
            message: "declaration refused".into(),
        });
        assert_eq!(kg.code(), KgErrorCode::OntologyViolation);
        match kg {
            KgError::OntologyViolation {
                rule,
                entity,
                report,
                ..
            } => {
                assert_eq!((rule, entity), ("domain", "relationship"));
                assert_eq!(report[0].count, 3);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn ontology_violation_rides_the_pending_violation_side_channel() {
        use crate::graph::constraints::EntityKind;
        use crate::graph::ontology::violation::{OntologyRule, OntologyViolation};
        use crate::graph::schema::DirGraph;
        let mut graph = DirGraph::new();
        let message = graph.record_ontology_violation(OntologyViolation::new(
            OntologyRule::ClosedLabels,
            EntityKind::Node,
            "Ghost",
            None,
            "label Ghost is not declared",
        ));
        // A wrapped message no longer matches: the parked value is dropped,
        // never mis-attributed.
        assert!(graph
            .take_constraint_error("wrapped: label Ghost")
            .is_none());
        graph.record_ontology_violation(OntologyViolation::new(
            OntologyRule::ClosedLabels,
            EntityKind::Node,
            "Ghost",
            None,
            "label Ghost is not declared",
        ));
        let err = graph.take_constraint_error(&message).expect("parked");
        assert_eq!(err.code(), KgErrorCode::OntologyViolation);
        // The constraint-only drain does not hand back an ontology violation.
        graph.record_ontology_violation(OntologyViolation::new(
            OntologyRule::Domain,
            EntityKind::Relationship,
            "R",
            None,
            "m",
        ));
        assert!(graph.take_constraint_violation_for("m").is_none());
    }
}
