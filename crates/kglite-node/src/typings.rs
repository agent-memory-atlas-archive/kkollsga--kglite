//! Type-only declarations that give `index.d.ts` its public option, result and
//! value shapes. Options are parsed and results built by hand (see `graph` and
//! `values`), so none of these is ever constructed in Rust.
use std::collections::HashMap;

use napi::bindgen_prelude::{
    BigInt, Date, Either3, Either4, Either5, FromNapiValue, Null, ToNapiValue, TypeName,
    ValidateNapiValue,
};
use napi::sys;
use napi_derive::napi;

use crate::classes::{Duration, KgFloat, LocalDate, LocalDateTime, Point};

/// Declares a type-only marker whose `type_name` is the TypeScript it should emit.
macro_rules! ts_marker {
    ($(#[$doc:meta])* $name:ident, $ts:literal) => {
        $(#[$doc])*
        pub struct $name;

        impl TypeName for $name {
            fn type_name() -> &'static str {
                $ts
            }
            fn value_type() -> napi::ValueType {
                napi::ValueType::Unknown
            }
        }

        impl ValidateNapiValue for $name {}

        impl ToNapiValue for $name {
            unsafe fn to_napi_value(_env: sys::napi_env, _val: Self) -> napi::Result<sys::napi_value> {
                Err(napi::Error::from_reason(concat!(stringify!($name), " is a type-only marker")))
            }
        }

        impl FromNapiValue for $name {
            unsafe fn from_napi_value(_env: sys::napi_env, _val: sys::napi_value) -> napi::Result<Self> {
                Err(napi::Error::from_reason(concat!(stringify!($name), " is a type-only marker")))
            }
        }
    };
}

ts_marker!(
    /// The map member of `KgValue`, declared as an interface by `dtsHeader` in
    /// `package.json`: `Record<string, KgValue>` inside the alias is circular to
    /// TypeScript (error TS2456) and degrades `KgValue` to `any`; an interface is not.
    /// The emitted TypeScript uses this struct's identifier, not its `type_name`.
    KgMap,
    "KgMap"
);

ts_marker!(
    /// Cycle breaker for the recursive `KgValue` alias below; named so the emitted
    /// TypeScript refers to the alias itself.
    KgValue,
    "KgValue"
);

/// Any value a query can return.
///
/// `Int64` is `number` when exact and `bigint` beyond 2^53 - 1 (`integers: 'bigint'`
/// makes every integer a `bigint`). Dates, datetimes, durations and points are the
/// exported classes; nodes, relationships and paths are plain objects.
///
/// `JSON.stringify` throws on a `bigint` (a JavaScript rule, not a kglite one): pass a
/// replacer, or define `BigInt.prototype.toJSON`, when a result may hold one.
#[napi(js_name = "KgValue")]
pub type KgValueType = Either5<
    Either5<Null, bool, f64, BigInt, String>,
    Either4<LocalDate, LocalDateTime, Duration, Point>,
    Either3<KgNode, KgRelationship, KgPath>,
    Vec<KgValue>,
    KgMap,
>;

/// A value accepted as a query parameter.
///
/// A `number` that is a safe integer is sent as an integer, any other as a float
/// (use `KgFloat` to force a float); a `bigint` outside the signed 64-bit range
/// rejects. `Date` is sent as a UTC datetime. `undefined` map entries are omitted.
#[napi(js_name = "ParamValue")]
pub type ParamValueType = Either3<KgValueType, KgFloat, Date<'static>>;

/// Named parameters, referenced in Cypher as `$name`.
#[napi(js_name = "Params")]
pub type ParamsType = HashMap<String, ParamValueType>;

/// Who holds a writer lease. `pid`, `since` and `label` are what the holder published
/// (null when it had not yet, or an older build wrote the record); `self` is true when
/// the holder is this very process, i.e. a handle that was opened and never closed.
#[napi(object)]
pub struct LeaseHolderInfo {
    pub pid: Option<u32>,
    pub since: Option<String>,
    pub label: Option<String>,
    #[napi(js_name = "self")]
    pub is_self: bool,
}

/// The `Error` every rejection carries.
#[napi(object, js_name = "KgliteError")]
pub struct KgliteErrorShape {
    /// Engine error code (`CypherSyntax`, `CypherTimeout`, `ConstraintViolation`,
    /// `TransactionConflict`, ...) or a binding code: `Internal`, `QueueFull`,
    /// `WriterLeaseHeld`, `Closed`, `ReadOnly`, `NotDurable`, `TransactionClosed`.
    pub code: String,
    /// Always `"KgliteError"`.
    pub name: String,
    pub message: String,
    /// Present on `WriterLeaseHeld`.
    pub holder: Option<LeaseHolderInfo>,
    /// Present on `OntologyViolation`: the rule that fired (`required_property`,
    /// `property_type`, `closed_labels`, `domain`, `range`). For a refused
    /// declaration, the first report entry's.
    pub rule: Option<String>,
    /// Present on `OntologyViolation`.
    #[napi(ts_type = "'node' | 'relationship'")]
    pub entity: Option<String>,
    /// Present on `OntologyViolation`: the label or relationship type.
    pub entity_type: Option<String>,
    /// Present on `OntologyViolation`: the offending property, `null` for a rule that has none.
    #[napi(ts_type = "string | null")]
    pub property: Option<String>,
    /// Present on `OntologyViolation`: the per-rule breakdown of a refused declaration; empty for a refused write.
    pub report: Option<Vec<OntologyReportEntry>>,
}

/// One line of a refused declaration's report: `count` stored entities already break `rule`.
#[napi(object)]
pub struct OntologyReportEntry {
    pub rule: String,
    #[napi(ts_type = "'node' | 'relationship'")]
    pub entity: String,
    pub entity_type: String,
    #[napi(ts_type = "string | null")]
    pub property: Option<String>,
    pub count: f64,
}

/// What `declareOntology` returns.
#[napi(object)]
pub struct OntologyDeclared {
    /// `warn`-level findings of the declaration over the stored data; empty when there are none.
    pub warnings: Vec<String>,
}

/// What `backup` captured.
#[napi(object)]
pub struct BackupReport {
    /// The destination file.
    pub path: String,
    /// Size of the published file.
    pub bytes: f64,
    pub nodes: f64,
    pub relationships: f64,
    /// The graph's in-memory commit count at the snapshot.
    pub graph_version: f64,
    /// Newest write-ahead-log position the file contains (`null` without a log). `number`
    /// when exact, `bigint` beyond 2^53 - 1 (always `bigint` with `integers: 'bigint'`).
    #[napi(ts_type = "number | bigint | null")]
    pub lsn: f64,
    /// How long writers were held off to fix the point in time.
    pub lock_hold_ms: f64,
    /// The whole call.
    pub elapsed_ms: f64,
    /// The snapshot needed a private copy first (costs a fork of the graph).
    pub prepared_copy: bool,
}

/// A node as returned in a row.
#[napi(object)]
pub struct KgNode {
    pub id: f64,
    pub labels: Vec<String>,
    pub properties: HashMap<String, KgValue>,
}

/// A relationship as returned in a row.
#[napi(object)]
pub struct KgRelationship {
    pub id: f64,
    #[napi(js_name = "type")]
    pub rel_type: String,
    pub start_id: f64,
    pub end_id: f64,
    pub properties: HashMap<String, KgValue>,
}

/// A path as returned in a row.
#[napi(object)]
pub struct KgPath {
    pub nodes: Vec<KgNode>,
    pub relationships: Vec<KgRelationship>,
}

#[napi(object)]
pub struct OpenOptions {
    /// Default `'full'`. Refused for `storage: 'disk'` when set explicitly.
    #[napi(ts_type = "'full' | 'normal' | 'off'")]
    pub durability: Option<String>,
    #[napi(ts_type = "'memory' | 'mapped' | 'disk'")]
    pub storage: Option<String>,
    /// Load the last checkpoint and take no lease; writes reject `ReadOnly`. Not
    /// combinable with `durability`, `storage` or `lockTimeoutMs`.
    pub read_only: Option<bool>,
    /// How long to wait for another writer to release the path. Default 0 (fail fast).
    pub lock_timeout_ms: Option<f64>,
    /// Per-query default; `0` disables the deadline. Default 180000.
    pub timeout_ms: Option<f64>,
    /// Per-query default cap on returned rows.
    pub row_limit: Option<f64>,
    /// `'safe'` (default): `number` when exact, `bigint` otherwise. `'bigint'`: always `bigint`.
    #[napi(ts_type = "'safe' | 'bigint'")]
    pub integers: Option<String>,
    /// Which instant an unprefixed statement reads on a graph with declared validity intervals: `'today'` (default), `'all'` (no valid-time filtering) or a fixed `'YYYY-MM-DD'` day. Runtime only; not saved with the graph.
    pub valid_time_default: Option<String>,
}

#[napi(object)]
pub struct EmbedderOptions {
    /// Vector width. Omitted: learned from the first vector the function returns (one probe call when a statement needs it up front).
    pub dimension: Option<f64>,
    /// Model identity stamped on stored embeddings; default is the registration name.
    pub model_id: Option<String>,
    /// How long one call may wait for the function before it fails. Default 120000.
    pub timeout_ms: Option<f64>,
}

#[napi(object)]
pub struct QueryOptions {
    /// Deadline in milliseconds; `0` disables it.
    pub timeout_ms: Option<f64>,
    /// Cap on rows returned; the rest are dropped and `truncated` says how many.
    pub row_limit: Option<f64>,
    /// Work budget (not a row cap); exceeding it fails the query.
    pub max_work_units: Option<f64>,
}

#[napi(object)]
pub struct MutationStats {
    pub nodes_created: f64,
    pub relationships_created: f64,
    pub properties_set: f64,
    pub nodes_deleted: f64,
    pub relationships_deleted: f64,
    pub properties_removed: f64,
    pub indexes_added: f64,
    pub indexes_removed: f64,
    pub constraints_added: f64,
    pub constraints_removed: f64,
}

#[napi(object)]
pub struct Truncated {
    pub row_limit: f64,
    pub total_rows: f64,
}

#[napi(object)]
pub struct QueryResult {
    pub columns: Vec<String>,
    /// One object per row, keyed by column name.
    #[napi(ts_type = "Array<Record<string, KgValue>>")]
    pub rows: Vec<String>,
    /// Present for statements that write.
    pub stats: Option<MutationStats>,
    /// Advisory warnings for this query; never printed.
    pub warnings: Vec<String>,
    /// Present only when `rowLimit` dropped rows.
    pub truncated: Option<Truncated>,
}
