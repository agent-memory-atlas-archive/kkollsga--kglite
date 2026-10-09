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

/// Cycle breaker for the recursive `KgValue` alias below; named so the emitted
/// TypeScript refers to the alias itself.
pub struct KgValue;

impl TypeName for KgValue {
    fn type_name() -> &'static str {
        "KgValue"
    }
    fn value_type() -> napi::ValueType {
        napi::ValueType::Unknown
    }
}

impl ValidateNapiValue for KgValue {}

impl ToNapiValue for KgValue {
    unsafe fn to_napi_value(_env: sys::napi_env, _val: Self) -> napi::Result<sys::napi_value> {
        Err(napi::Error::from_reason("KgValue is a type-only marker"))
    }
}

impl FromNapiValue for KgValue {
    unsafe fn from_napi_value(_env: sys::napi_env, _val: sys::napi_value) -> napi::Result<Self> {
        Err(napi::Error::from_reason("KgValue is a type-only marker"))
    }
}

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
    HashMap<String, KgValue>,
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

/// The `Error` every rejection carries.
#[napi(object, js_name = "KgliteError")]
pub struct KgliteErrorShape {
    /// Engine error code (`CypherSyntax`, `CypherTimeout`, `ConstraintViolation`,
    /// `TransactionConflict`, ...), or `INTERNAL` / `WRITER_LEASE_HELD` / `QUEUE_FULL`.
    pub code: String,
    /// Always `"KgliteError"`.
    pub name: String,
    pub message: String,
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
    /// Not supported yet; `true` rejects.
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
