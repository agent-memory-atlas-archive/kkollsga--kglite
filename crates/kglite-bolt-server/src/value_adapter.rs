//! `kglite::api::Value` ↔ `boltr::types::BoltValue` adapter.
//!
//! `to_bolt` handles ALL outbound variants (scalars, collections,
//! temporal/spatial, Node, Relationship, Path). `Value::NodeRef` and malformed
//! paths return errors: internal placeholders and invalid graph structure must
//! not reach the boundary. `from_bolt` rejects inbound Node/Rel/Path (drivers
//! don't pass those as parameters), inbound time-of-day variants
//! (kglite has no standalone time-of-day variant), inbound Bytes (no Value
//! variant), and inbound Point3D (kglite has only Point2D).
//!
//! # Mapping table (the implementer's spec)
//!
//! | kglite `Value`                          | `boltr::BoltValue`            | Notes                                              |
//! |-----------------------------------------|-------------------------------|----------------------------------------------------|
//! | `Null`                                  | `Null`                        |                                                    |
//! | `Boolean(b)`                            | `Boolean(b)`                  |                                                    |
//! | `Int64(n)`                              | `Integer(n)`                  |                                                    |
//! | `UniqueId(n)`                           | `Integer(n as i64)`           | u32 → i64 widen (always non-negative)              |
//! | `Float64(f)`                            | `Float(f)`                    | IEEE 754 double; both sides preserve NaN bit-pat   |
//! | `String(s)`                             | `String(s.clone())`           | UTF-8                                              |
//! | `List(items)`                           | `List(items.map(to_bolt))`    | Recursive                                          |
//! | `Map(entries)`                          | `Dict(entries.map(...))`      | `BoltDict = HashMap<String, BoltValue>`            |
//! | `DateTime(NaiveDate)`                   | `Date(BoltDate { days })`     | Days since Unix epoch (1970-01-01)                 |
//! | `Timestamp(NaiveDateTime)`             | `LocalDateTime`               | Epoch seconds and fractional nanoseconds preserved |
//! | `ZonedDateTime { utc, offset_secs, zone: None }` | `DateTime(BoltDateTime { seconds, nanoseconds, tz_offset_seconds })` | `seconds` is the UTC epoch reading (Bolt 5 `DateTime`); inbound offset beyond ±18 h is `Protocol` |
//! | `ZonedDateTime { .., zone: Some(z) }`   | `DateTimeZoneId(BoltDateTimeZoneId { seconds, nanoseconds, tz_id })` | `seconds` is the UTC epoch reading, `tz_id` the IANA name; inbound an unknown name, or any name on a build without `iana-zones`, is `Protocol` |
//! | `LocalTime(nanos)`                      | `LocalTime(BoltLocalTime { nanoseconds })` | Nanoseconds since midnight; inbound outside one day is `Protocol` |
//! | `Time { nanos, offset_secs }`           | `Time(BoltTime { nanoseconds, tz_offset_seconds })` | Local clock plus offset; inbound offset beyond ±18 h is `Protocol` |
//! | `Duration { months, days, seconds, nanos }` | `Duration(BoltDuration {..})` | All i64; inbound `nanoseconds` may be negative or exceed 1e9 and is normalised into `seconds` plus 0..1e9 nanos |
//! | `Point { lat, lon }`                    | `Point2D(BoltPoint2D { .. })` | srid=4326 for WGS84; Bolt convention x=lon, y=lat  |
//! | `Node { id, labels, properties }`       | `Node(BoltNode { id, labels, properties, element_id })` | `element_id = id.to_string()` |
//! | `Relationship { id, start_id, end_id, rel_type, properties }` | `Relationship(BoltRelationship { ... })` | `element_id` / `start_element_id` / `end_element_id` = stringified ids |
//! | `Path { nodes, rels }`                  | `Path(BoltPath { nodes, rels: UnboundRel*, indices })` | `indices`: 1-based signed rel idx + 0-based node idx pairs |
//! | `NodeRef(_)`                            | `Err(BoltError::Backend)`     | Internal placeholder — leaking here is an executor bug |
//!
//! `from_bolt` is the inbound direction for parameters. The graph-structure
//! variants (`Node`/`Relationship`/`Path`) on input would mean a driver
//! passed a node *as a parameter* — Neo4j drivers don't do this, and
//! `from_bolt` rejects them with `BoltError::Protocol`.

use std::collections::HashMap;

use boltr::error::BoltError;
use boltr::types::{
    BoltDate, BoltDateTime, BoltDict, BoltDuration, BoltLocalDateTime, BoltLocalTime, BoltNode,
    BoltPath, BoltPoint2D, BoltRelationship, BoltTime, BoltUnboundRelationship, BoltValue,
};

use kglite::api::Value;

/// kglite → Bolt. Called by `execute`'s record emission.
///
/// Returns `Err(BoltError::Backend)` rather than panicking so a query
/// that touches an unimplemented variant doesn't orphan the tokio
/// connection task; the client gets a clean Bolt FAILURE instead.
pub fn to_bolt(value: &Value) -> Result<BoltValue, BoltError> {
    match value {
        // ---- Scalars -----------------------------------------------------
        Value::Null => Ok(BoltValue::Null),
        Value::Boolean(b) => Ok(BoltValue::Boolean(*b)),
        Value::Int64(n) => Ok(BoltValue::Integer(*n)),
        Value::UniqueId(n) => Ok(BoltValue::Integer(i64::from(*n))),
        Value::Float64(f) => Ok(BoltValue::Float(*f)),
        Value::String(s) => Ok(BoltValue::String(s.clone())),

        // ---- Recursive containers ---------------------------------------
        Value::List(items) => items
            .iter()
            .map(to_bolt)
            .collect::<Result<Vec<_>, _>>()
            .map(BoltValue::List),
        Value::Map(entries) => entries
            .iter()
            .map(|(k, v)| to_bolt(v).map(|bv| (k.to_string(), bv)))
            .collect::<Result<HashMap<_, _>, _>>()
            .map(BoltValue::Dict),

        // ---- Temporal / spatial / duration ------------------------------
        Value::DateTime(date) => {
            // kglite's `DateTime` is a `NaiveDate` (date only);
            // Bolt's BoltDate is also days-since-Unix-epoch.
            // `signed_duration_since` is unambiguous; the bare `-` op
            // resolves to the wrong impl in some chrono versions.
            // SAFETY: 1970-01-01 is a valid Gregorian date — the only way
            // `from_ymd_opt` returns None is on out-of-range/invalid input
            // (month 13, day 32, etc.). This expect is infallible.
            let epoch =
                chrono::NaiveDate::from_ymd_opt(1970, 1, 1).expect("1970-01-01 is a valid date");
            Ok(BoltValue::Date(BoltDate {
                days: date.signed_duration_since(epoch).num_days(),
            }))
        }
        Value::Timestamp(dt) => {
            // Floor seconds plus a nonnegative fraction also preserves instants
            // before the epoch; Duration::num_seconds would truncate toward zero.
            let utc = dt.and_utc();
            if utc.timestamp_subsec_nanos() >= 1_000_000_000 {
                return Err(BoltError::Backend(
                    "Timestamp leap seconds cannot be represented as Bolt LocalDateTime".into(),
                ));
            }
            Ok(BoltValue::LocalDateTime(BoltLocalDateTime {
                seconds: utc.timestamp(),
                nanoseconds: i64::from(utc.timestamp_subsec_nanos()),
            }))
        }
        // The Bolt 5 `DateTime` structure reads `seconds` as the UTC epoch reading
        // and carries the offset beside it, so the instant maps one to one.
        Value::ZonedDateTime {
            utc,
            offset_secs,
            zone,
        } => {
            let (seconds, nanoseconds) = kglite::api::zoned_datetime::epoch_parts(*utc);
            Ok(match zone {
                None => BoltValue::DateTime(BoltDateTime {
                    seconds,
                    nanoseconds: i64::from(nanoseconds),
                    tz_offset_seconds: i64::from(*offset_secs),
                }),
                Some(zone) => BoltValue::DateTimeZoneId(boltr::types::BoltDateTimeZoneId {
                    seconds,
                    nanoseconds: i64::from(nanoseconds),
                    tz_id: zone.name().to_string(),
                }),
            })
        }
        // Nanoseconds since local midnight, below one day, so the `i64` never saturates.
        Value::LocalTime(nanos) => Ok(BoltValue::LocalTime(BoltLocalTime {
            nanoseconds: *nanos as i64,
        })),
        Value::Time { nanos, offset_secs } => Ok(BoltValue::Time(BoltTime {
            nanoseconds: *nanos as i64,
            tz_offset_seconds: i64::from(*offset_secs),
        })),
        Value::Point { lat, lon } => Ok(BoltValue::Point2D(BoltPoint2D {
            // SRID 4326 = WGS84 (geographic lat/lon). Bolt convention
            // is x=longitude, y=latitude. kglite stores them named, so
            // the cross-naming is intentional, not a bug.
            srid: 4326,
            x: *lon,
            y: *lat,
        })),
        // The engine value is `seconds + nanos / 1e9` with `nanos` in 0..1e9, a
        // pair Bolt's `seconds` + `nanoseconds` fields carry as they are.
        Value::Duration {
            months,
            days,
            seconds,
            nanos,
        } => Ok(BoltValue::Duration(BoltDuration {
            months: i64::from(*months),
            days: i64::from(*days),
            seconds: *seconds,
            nanoseconds: i64::from(*nanos),
        })),

        // ---- Node / Relationship / Path ---------------------------------
        Value::Node(node) => {
            let properties = props_to_bolt_dict(&node.properties)?;
            Ok(BoltValue::Node(BoltNode {
                id: i64::from(node.id),
                labels: node.labels.clone(),
                properties,
                // Bolt 5.x element_id: a stable string identifier.
                // Neo4j uses a UUID-like string; for kglite we use the
                // numeric id stringified — it's stable within one
                // server lifetime, which is the contract drivers care
                // about. (Across reloads the id may change; drivers
                // shouldn't persist element_ids long-term.)
                element_id: node.id.to_string(),
            }))
        }
        Value::Relationship(rel) => {
            let properties = props_to_bolt_dict(&rel.properties)?;
            Ok(BoltValue::Relationship(BoltRelationship {
                id: i64::from(rel.id),
                start_node_id: i64::from(rel.start_id),
                end_node_id: i64::from(rel.end_id),
                rel_type: rel.rel_type.clone(),
                properties,
                element_id: rel.id.to_string(),
                start_element_id: rel.start_id.to_string(),
                end_element_id: rel.end_id.to_string(),
            }))
        }
        Value::Path(path) => path_to_bolt_path(path.as_ref()).map(BoltValue::Path),

        // ---- Executor bug ----------------------------------------------
        Value::NodeRef(_) => Err(BoltError::Backend(
            "internal Value::NodeRef leaked through projection — please file a \
             bug against kglite (this variant is supposed to be materialized \
             before reaching the Bolt boundary)"
                .into(),
        )),
    }
}

/// kglite → Bolt, consuming the value: strings, labels and containers move
/// into the Bolt value instead of being cloned, and the engine's copy is freed
/// as the Bolt copy is built. Same mapping as [`to_bolt`], which it falls back
/// to for the variants that own no heap data.
pub fn to_bolt_owned(value: Value) -> Result<BoltValue, BoltError> {
    match value {
        Value::String(s) => Ok(BoltValue::String(s)),
        Value::List(items) => items
            .into_iter()
            .map(to_bolt_owned)
            .collect::<Result<Vec<_>, _>>()
            .map(BoltValue::List),
        Value::Map(entries) => props_into_bolt_dict(entries).map(BoltValue::Dict),
        Value::Node(node) => {
            let kglite::api::NodeValue {
                id,
                labels,
                properties,
            } = *node;
            Ok(BoltValue::Node(BoltNode {
                id: i64::from(id),
                labels,
                properties: props_into_bolt_dict(properties)?,
                element_id: id.to_string(),
            }))
        }
        Value::Relationship(rel) => {
            let rel = *rel;
            Ok(BoltValue::Relationship(BoltRelationship {
                id: i64::from(rel.id),
                start_node_id: i64::from(rel.start_id),
                end_node_id: i64::from(rel.end_id),
                element_id: rel.id.to_string(),
                start_element_id: rel.start_id.to_string(),
                end_element_id: rel.end_id.to_string(),
                rel_type: rel.rel_type,
                properties: props_into_bolt_dict(rel.properties)?,
            }))
        }
        Value::Path(path) => path_into_bolt_path(*path).map(BoltValue::Path),
        other => to_bolt(&other),
    }
}

fn props_into_bolt_dict(props: kglite::datatypes::PropMap) -> Result<BoltDict, BoltError> {
    let pairs = props.into_pairs();
    let mut dict = HashMap::with_capacity(pairs.len());
    for (key, value) in pairs {
        dict.insert(key.to_string(), to_bolt_owned(value)?);
    }
    Ok(dict)
}

/// Convert a kglite property map to a Bolt dict. Recursive through
/// `to_bolt` so nested lists / maps round-trip; surface conversion
/// failure on the first bad value.
fn props_to_bolt_dict(props: &kglite::datatypes::PropMap) -> Result<BoltDict, BoltError> {
    props
        .iter()
        .map(|(k, v)| to_bolt(v).map(|bv| (k.to_string(), bv)))
        .collect::<Result<HashMap<_, _>, _>>()
}

/// Build a `BoltPath` from a kglite `PathValue`. Encodes the
/// Neo4j Bolt-protocol `indices` scheme: pairs of (signed-rel-index,
/// next-node-index) where the rel index is 1-based with sign
/// (+ = traversed in the rel's natural direction, - = traversed in
/// reverse) and the node index is 0-based into `nodes`.
///
/// kglite's `PathValue` stores parallel `nodes` (k+1) + `rels` (k)
/// vectors with no deduplication and no direction sign — direction is
/// inferred per rel by comparing `rel.start_id` / `rel.end_id` against
/// the node ids before and after the rel in the traversal order. We
/// emit nodes 1:1 (no dedup) and one (signed_rel, next_node) pair
/// per rel.
fn path_to_bolt_path(p: &kglite::api::PathValue) -> Result<BoltPath, BoltError> {
    let indices = path_indices(p)?;
    let nodes: Vec<BoltNode> = p
        .nodes
        .iter()
        .map(|nv| {
            Ok::<BoltNode, BoltError>(BoltNode {
                id: i64::from(nv.id),
                labels: nv.labels.clone(),
                properties: props_to_bolt_dict(&nv.properties)?,
                element_id: nv.id.to_string(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let rels: Vec<BoltUnboundRelationship> = p
        .rels
        .iter()
        .map(|rv| {
            Ok::<BoltUnboundRelationship, BoltError>(BoltUnboundRelationship {
                id: i64::from(rv.id),
                rel_type: rv.rel_type.clone(),
                properties: props_to_bolt_dict(&rv.properties)?,
                element_id: rv.id.to_string(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(BoltPath {
        nodes,
        rels,
        indices,
    })
}

/// [`path_to_bolt_path`] consuming the path.
fn path_into_bolt_path(p: kglite::api::PathValue) -> Result<BoltPath, BoltError> {
    let indices = path_indices(&p)?;
    let nodes = p
        .nodes
        .into_iter()
        .map(|nv| {
            Ok::<BoltNode, BoltError>(BoltNode {
                id: i64::from(nv.id),
                element_id: nv.id.to_string(),
                labels: nv.labels,
                properties: props_into_bolt_dict(nv.properties)?,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let rels = p
        .rels
        .into_iter()
        .map(|rv| {
            Ok::<BoltUnboundRelationship, BoltError>(BoltUnboundRelationship {
                id: i64::from(rv.id),
                element_id: rv.id.to_string(),
                rel_type: rv.rel_type,
                properties: props_into_bolt_dict(rv.properties)?,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(BoltPath {
        nodes,
        rels,
        indices,
    })
}

/// Validate a path's shape and compute its Bolt `indices`.
fn path_indices(p: &kglite::api::PathValue) -> Result<Vec<i64>, BoltError> {
    // Sanity check: kglite paths are linear, so |nodes| = |rels| + 1.
    if p.nodes.len() != p.rels.len() + 1 {
        return Err(BoltError::Backend(format!(
            "kglite PathValue invariant violated: {} nodes vs {} rels (expected {} vs {})",
            p.nodes.len(),
            p.rels.len(),
            p.rels.len() + 1,
            p.rels.len()
        )));
    }
    let mut indices: Vec<i64> = Vec::with_capacity(p.rels.len() * 2);
    for (i, rel) in p.rels.iter().enumerate() {
        let node_before = &p.nodes[i];
        let node_after = &p.nodes[i + 1];
        // 1-based rel index; sign indicates traversal direction.
        let rel_idx_1based = (i + 1) as i64;
        let signed_rel: i64 = if rel.start_id == node_before.id && rel.end_id == node_after.id {
            rel_idx_1based // outgoing in path's traversal
        } else if rel.start_id == node_after.id && rel.end_id == node_before.id {
            -rel_idx_1based // incoming (traversed in reverse)
        } else {
            return Err(BoltError::Backend(format!(
                "kglite PathValue relationship {} ({} -> {}) does not connect path nodes {} -> {}",
                rel.id, rel.start_id, rel.end_id, node_before.id, node_after.id
            )));
        };
        indices.push(signed_rel);
        indices.push((i + 1) as i64); // 0-based next-node index
    }
    Ok(indices)
}

/// Bolt → kglite. Called by `execute`'s parameter decoding.
///
/// Returns `BoltError::Protocol` on inbound variants that don't make
/// sense in a parameter context: graph structures (Node/Relationship/
/// Path — drivers never pass these), time-of-day temporals (no standalone
/// time-of-day `Value` variant), Bytes (no `Value` variant), or Point3D
/// (kglite has only 2D points).
pub fn from_bolt(value: &BoltValue) -> Result<Value, BoltError> {
    match value {
        // ---- Scalars -----------------------------------------------------
        BoltValue::Null => Ok(Value::Null),
        BoltValue::Boolean(b) => Ok(Value::Boolean(*b)),
        BoltValue::Integer(n) => Ok(Value::Int64(*n)),
        // PackStream carries NaN and ±Infinity as ordinary float64 values and
        // every result path returns them unchanged, so a parameter takes
        // the same value a result would.
        BoltValue::Float(f) => Ok(Value::Float64(*f)),
        BoltValue::String(s) => Ok(Value::String(s.clone())),

        // ---- Recursive containers ---------------------------------------
        BoltValue::List(items) => items
            .iter()
            .map(from_bolt)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::List),
        BoltValue::Dict(entries) => entries
            .iter()
            .map(|(k, v)| from_bolt(v).map(|kv| (k.as_str(), kv)))
            .collect::<Result<kglite::datatypes::PropMap, _>>()
            .map(Value::Map),

        // ---- Temporal / spatial ----------------------------------------
        BoltValue::Date(d) => {
            // SAFETY: 1970-01-01 is a valid Gregorian date — the only way
            // `from_ymd_opt` returns None is on out-of-range/invalid input
            // (month 13, day 32, etc.). This expect is infallible.
            let epoch =
                chrono::NaiveDate::from_ymd_opt(1970, 1, 1).expect("1970-01-01 is a valid date");
            let date = epoch
                .checked_add_signed(chrono::Duration::days(d.days))
                .ok_or_else(|| {
                    BoltError::Protocol(format!(
                        "Bolt Date out of range for kglite NaiveDate: days={}",
                        d.days
                    ))
                })?;
            Ok(Value::DateTime(date))
        }
        BoltValue::Duration(d) => {
            let (bolt_seconds, bolt_nanos) = normalise_bolt_seconds(d.seconds, d.nanoseconds)?;
            Ok(Value::Duration {
                months: i32::try_from(d.months).map_err(|_| {
                    BoltError::Protocol(format!(
                        "Bolt Duration.months out of i32 range: {}",
                        d.months
                    ))
                })?,
                days: i32::try_from(d.days).map_err(|_| {
                    BoltError::Protocol(format!("Bolt Duration.days out of i32 range: {}", d.days))
                })?,
                seconds: bolt_seconds,
                nanos: bolt_nanos,
            })
        }
        BoltValue::Point2D(p) => {
            // SRID 4326 is WGS84 (geographic lat/lon). Other SRIDs (e.g.
            // 7203 for Cartesian) aren't representable as kglite's
            // `Point { lat, lon }`.
            if p.srid != 4326 {
                return Err(BoltError::Protocol(format!(
                    "Bolt Point2D with SRID {} not supported — kglite \
                     only represents WGS84 lat/lon (SRID 4326)",
                    p.srid
                )));
            }
            // Bolt convention: x=longitude, y=latitude.
            Ok(Value::Point { lat: p.y, lon: p.x })
        }

        // ---- Variants kglite can't represent ----------------------------
        BoltValue::Bytes(_) => Err(BoltError::Protocol(
            "Bolt Bytes parameter not supported — kglite has no byte-string Value variant".into(),
        )),
        BoltValue::LocalDateTime(dt) => {
            let stamp = u32::try_from(dt.nanoseconds)
                .ok()
                .filter(|nanos| *nanos < 1_000_000_000)
                .and_then(|nanos| chrono::DateTime::from_timestamp(dt.seconds, nanos))
                .ok_or_else(|| {
                    BoltError::Protocol(format!(
                        "Bolt LocalDateTime out of range: seconds={}, nanoseconds={}",
                        dt.seconds, dt.nanoseconds
                    ))
                })?;
            Ok(Value::Timestamp(stamp.naive_utc()))
        }
        BoltValue::LocalTime(t) => {
            let nanos = u64::try_from(t.nanoseconds)
                .ok()
                .filter(|n| kglite::api::time_of_day::is_valid_nanos(*n))
                .ok_or_else(|| {
                    BoltError::Protocol(format!(
                        "Bolt LocalTime out of range: nanoseconds={}",
                        t.nanoseconds
                    ))
                })?;
            Ok(Value::LocalTime(nanos))
        }
        BoltValue::Time(t) => {
            let nanos = u64::try_from(t.nanoseconds)
                .ok()
                .filter(|n| kglite::api::time_of_day::is_valid_nanos(*n))
                .ok_or_else(|| {
                    BoltError::Protocol(format!(
                        "Bolt Time out of range: nanoseconds={}",
                        t.nanoseconds
                    ))
                })?;
            let offset_secs = i32::try_from(t.tz_offset_seconds)
                .ok()
                .filter(|o| o.abs() <= kglite::api::time_of_day::MAX_OFFSET_SECS)
                .ok_or_else(|| {
                    BoltError::Protocol(format!(
                        "Bolt Time offset out of range: tz_offset_seconds={}",
                        t.tz_offset_seconds
                    ))
                })?;
            Ok(Value::Time { nanos, offset_secs })
        }
        BoltValue::DateTime(dt) => {
            let utc = u32::try_from(dt.nanoseconds)
                .ok()
                .filter(|nanos| *nanos < 1_000_000_000)
                .and_then(|nanos| kglite::api::zoned_datetime::from_epoch(dt.seconds, nanos))
                .ok_or_else(|| {
                    BoltError::Protocol(format!(
                        "Bolt DateTime out of range: seconds={}, nanoseconds={}",
                        dt.seconds, dt.nanoseconds
                    ))
                })?;
            let offset_secs = i32::try_from(dt.tz_offset_seconds)
                .ok()
                .filter(|o| o.abs() <= kglite::api::time_of_day::MAX_OFFSET_SECS)
                .ok_or_else(|| {
                    BoltError::Protocol(format!(
                        "Bolt DateTime offset out of range: tz_offset_seconds={}",
                        dt.tz_offset_seconds
                    ))
                })?;
            Ok(Value::zoned(utc, offset_secs))
        }
        BoltValue::DateTimeZoneId(dt) => {
            let utc = u32::try_from(dt.nanoseconds)
                .ok()
                .filter(|nanos| *nanos < 1_000_000_000)
                .and_then(|nanos| kglite::api::zoned_datetime::from_epoch(dt.seconds, nanos))
                .ok_or_else(|| {
                    BoltError::Protocol(format!(
                        "Bolt DateTimeZoneId out of range: seconds={}, nanoseconds={}",
                        dt.seconds, dt.nanoseconds
                    ))
                })?;
            let zone = kglite::api::zoned_datetime::zone_by_name(&dt.tz_id)
                .map_err(|e| BoltError::Protocol(format!("Bolt DateTimeZoneId: {e}")))?;
            Ok(Value::zoned_in(utc, zone))
        }
        BoltValue::Point3D(_) => Err(BoltError::Protocol(
            "Bolt Point3D parameter not supported — kglite represents only 2D points".into(),
        )),

        // ---- Inbound graph structures (drivers don't pass these) -------
        BoltValue::Node(_) | BoltValue::Relationship(_) | BoltValue::Path(_) => {
            Err(BoltError::Protocol(
                "Bolt Node/Relationship/Path is not a valid parameter type — \
                 drivers should serialize property values instead"
                    .into(),
            ))
        }
        BoltValue::UnboundRelationship(_) => Err(BoltError::Protocol(
            "Bolt UnboundRelationship only appears inside Path structures — \
             cannot be a standalone parameter"
                .into(),
        )),
    }
}

/// Fold a Bolt duration's `seconds` and (possibly negative or oversized)
/// `nanoseconds` into the engine's signed seconds plus 0..1e9 nanos.
fn normalise_bolt_seconds(seconds: i64, nanoseconds: i64) -> Result<(i64, i32), BoltError> {
    const NANOS: i128 = 1_000_000_000;
    let total = i128::from(seconds) * NANOS + i128::from(nanoseconds);
    let whole = i64::try_from(total.div_euclid(NANOS)).map_err(|_| {
        BoltError::Protocol(format!(
            "Bolt Duration seconds out of i64 range: {seconds}s + {nanoseconds}ns"
        ))
    })?;
    // rem_euclid is below 1e9, so it fits an i32.
    Ok((whole, total.rem_euclid(NANOS) as i32))
}

#[cfg(test)]
mod tests {

    use kglite::api::{NodeValue, PathValue, RelValue};

    use super::*;

    fn node(id: u32) -> NodeValue {
        NodeValue {
            id,
            labels: vec!["N".into()],
            properties: kglite::datatypes::PropMap::new(),
        }
    }

    fn rel(id: u32, start_id: u32, end_id: u32) -> RelValue {
        RelValue {
            incarnation: None,
            id,
            start_id,
            end_id,
            rel_type: "R".into(),
            properties: kglite::datatypes::PropMap::new(),
        }
    }

    fn bolt_duration(seconds: i64, nanoseconds: i64) -> BoltValue {
        BoltValue::Duration(BoltDuration {
            months: 1,
            days: 2,
            seconds,
            nanoseconds,
        })
    }

    #[test]
    fn duration_nanoseconds_cross_the_wire_both_ways() {
        let fine = Value::Duration {
            months: 1,
            days: 2,
            seconds: -2,
            nanos: 500_000_000,
        };
        assert_eq!(to_bolt(&fine).unwrap(), bolt_duration(-2, 500_000_000));
        assert_eq!(
            to_bolt_owned(fine.clone()).unwrap(),
            bolt_duration(-2, 500_000_000)
        );
        assert_eq!(from_bolt(&bolt_duration(-2, 500_000_000)).unwrap(), fine);
    }

    #[test]
    fn inbound_duration_nanoseconds_are_normalised() {
        // (seconds, nanoseconds) -> (seconds, 0..1e9 nanos): negative, oversized
        // and mixed-sign pairs all describe the same total.
        for ((seconds, nanoseconds), expected) in [
            ((0, -1), (-1, 999_999_999)),
            ((1, -500_000_000), (0, 500_000_000)),
            ((-1, -500_000_000), (-2, 500_000_000)),
            ((0, 2_500_000_000), (2, 500_000_000)),
            ((3, 7), (3, 7)),
        ] {
            let Ok(Value::Duration {
                seconds: s,
                nanos: n,
                ..
            }) = from_bolt(&bolt_duration(seconds, nanoseconds))
            else {
                panic!("({seconds}, {nanoseconds}) must bind");
            };
            assert_eq!((s, n), expected, "({seconds}, {nanoseconds})");
        }
        assert!(from_bolt(&bolt_duration(i64::MAX, 1_000_000_000)).is_err());
    }

    #[test]
    fn non_finite_float_parameters_bind_unchanged() {
        for f in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.0] {
            let Ok(Value::Float64(bound)) = from_bolt(&BoltValue::Float(f)) else {
                panic!("{f} must bind as a Float64 parameter");
            };
            assert_eq!(bound.to_bits(), f.to_bits());
        }
    }

    #[test]
    fn path_adapter_preserves_reverse_traversal_direction() {
        let path = PathValue {
            nodes: vec![node(2), node(1)],
            rels: vec![rel(7, 1, 2)],
        };

        let encoded = path_to_bolt_path(&path).expect("valid reverse path");
        assert_eq!(encoded.indices, vec![-1, 1]);
    }

    #[test]
    fn path_adapter_rejects_relationship_disconnected_from_segment() {
        let path = PathValue {
            nodes: vec![node(1), node(2)],
            rels: vec![rel(7, 3, 4)],
        };

        let error = path_to_bolt_path(&path).expect_err("disconnected relationship must fail");
        assert!(error
            .to_string()
            .contains("does not connect path nodes 1 -> 2"));
    }

    #[test]
    fn path_adapter_rejects_non_linear_shape() {
        let path = PathValue {
            nodes: vec![node(1)],
            rels: vec![rel(7, 1, 2)],
        };

        let error =
            path_to_bolt_path(&path).expect_err("invalid node/relationship count must fail");
        assert!(error.to_string().contains("invariant violated"));
    }
}

#[cfg(test)]
mod fractional_timestamp_contract_tests {
    use super::*;
    #[test]
    fn local_datetime_roundtrip_preserves_fraction_and_negative_epoch_floor() {
        for (text, seconds, nanos) in [
            ("1970-01-01T00:00:00", 0, 0),
            ("1970-01-01T00:00:00.123456789", 0, 123456789),
            ("1969-12-31T23:59:59.500", -1, 500000000),
        ] {
            let value = Value::Timestamp(
                chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f").unwrap(),
            );
            let bolt = BoltValue::LocalDateTime(BoltLocalDateTime {
                seconds,
                nanoseconds: nanos,
            });
            assert_eq!(to_bolt(&value).unwrap(), bolt);
            assert_eq!(from_bolt(&bolt).unwrap(), value);
            let nested = Value::List(vec![value]);
            assert_eq!(from_bolt(&to_bolt(&nested).unwrap()).unwrap(), nested);
        }
    }
}

#[cfg(test)]
mod time_of_day_tests {
    use super::*;

    #[test]
    fn local_time_and_time_round_trip_with_their_nanoseconds_and_offset() {
        let local = Value::LocalTime(36_000_500_000_001);
        let zoned = Value::Time {
            nanos: 86_399_999_999_999,
            offset_secs: -19_800,
        };
        assert_eq!(
            to_bolt(&local).unwrap(),
            BoltValue::LocalTime(BoltLocalTime {
                nanoseconds: 36_000_500_000_001
            })
        );
        assert_eq!(
            to_bolt(&zoned).unwrap(),
            BoltValue::Time(BoltTime {
                nanoseconds: 86_399_999_999_999,
                tz_offset_seconds: -19_800
            })
        );
        for value in [local, zoned] {
            let back = from_bolt(&to_bolt(&value).unwrap()).unwrap();
            // `==` ignores a Time's offset; the debug form carries it.
            assert_eq!(format!("{back:?}"), format!("{value:?}"));
            assert_eq!(
                to_bolt_owned(value.clone()).unwrap(),
                to_bolt(&value).unwrap()
            );
        }
        let nested = Value::List(vec![Value::Time {
            nanos: 1,
            offset_secs: 3600,
        }]);
        assert_eq!(
            format!("{:?}", from_bolt(&to_bolt(&nested).unwrap()).unwrap()),
            format!("{nested:?}")
        );
    }

    #[test]
    fn out_of_range_inbound_times_are_protocol_errors() {
        for bad in [
            BoltValue::LocalTime(BoltLocalTime { nanoseconds: -1 }),
            BoltValue::LocalTime(BoltLocalTime {
                nanoseconds: 86_400_000_000_000,
            }),
            BoltValue::Time(BoltTime {
                nanoseconds: 86_400_000_000_000,
                tz_offset_seconds: 0,
            }),
            BoltValue::Time(BoltTime {
                nanoseconds: 0,
                tz_offset_seconds: 64_801,
            }),
            BoltValue::Time(BoltTime {
                nanoseconds: 0,
                tz_offset_seconds: i64::MAX,
            }),
        ] {
            assert!(
                matches!(from_bolt(&bad), Err(BoltError::Protocol(_))),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn zoned_datetimes_round_trip_keeping_the_instant_and_the_offset() {
        // 2015-07-21T21:40:32.142+01:00 is 2015-07-21T20:40:32Z.
        let utc = chrono::DateTime::from_timestamp(1_437_511_232, 142_000_000)
            .unwrap()
            .naive_utc();
        let value = Value::zoned(utc, 3600);
        let bolt = BoltValue::DateTime(boltr::types::BoltDateTime {
            seconds: 1_437_511_232,
            nanoseconds: 142_000_000,
            tz_offset_seconds: 3600,
        });
        assert_eq!(to_bolt(&value).unwrap(), bolt);
        assert_eq!(to_bolt_owned(value.clone()).unwrap(), bolt);
        // `==` ignores the offset; the debug form carries it.
        assert_eq!(
            format!("{:?}", from_bolt(&bolt).unwrap()),
            format!("{value:?}")
        );
        let before_epoch = Value::zoned(
            chrono::DateTime::from_timestamp(-1, 999_999_999)
                .unwrap()
                .naive_utc(),
            -64_800,
        );
        let nested = Value::List(vec![value, before_epoch]);
        assert_eq!(
            format!("{:?}", from_bolt(&to_bolt(&nested).unwrap()).unwrap()),
            format!("{nested:?}")
        );
    }

    /// A named zone travels as `DateTimeZoneId`: UTC seconds plus the IANA name,
    /// and comes back with the zone and the offset the zone has at the instant.
    #[test]
    fn named_zones_travel_as_date_time_zone_id() {
        // 2026-03-29T01:30Z is 03:30+02:00 in Oslo (the clock skipped 02:00-03:00).
        let utc = chrono::DateTime::from_timestamp(1_774_747_800, 0)
            .unwrap()
            .naive_utc();
        let zone = kglite::api::time_zone::ZoneId::from_name("Europe/Oslo").unwrap();
        let value = Value::zoned_in(utc, zone);
        let bolt = BoltValue::DateTimeZoneId(boltr::types::BoltDateTimeZoneId {
            seconds: 1_774_747_800,
            nanoseconds: 0,
            tz_id: "Europe/Oslo".into(),
        });
        assert_eq!(to_bolt(&value).unwrap(), bolt);
        assert_eq!(to_bolt_owned(value.clone()).unwrap(), bolt);
        let back = from_bolt(&bolt).unwrap();
        assert_eq!(format!("{back:?}"), format!("{value:?}"));
        let Value::ZonedDateTime {
            offset_secs,
            zone: Some(back_zone),
            ..
        } = back
        else {
            panic!("DateTimeZoneId decodes a named zoned datetime");
        };
        assert_eq!((offset_secs, back_zone.name()), (7200, "Europe/Oslo"));
    }

    #[test]
    fn out_of_range_inbound_datetimes_and_named_zones_are_protocol_errors() {
        for bad in [
            BoltValue::DateTime(boltr::types::BoltDateTime {
                seconds: 0,
                nanoseconds: 1_000_000_000,
                tz_offset_seconds: 0,
            }),
            BoltValue::DateTime(boltr::types::BoltDateTime {
                seconds: 0,
                nanoseconds: -1,
                tz_offset_seconds: 0,
            }),
            BoltValue::DateTime(boltr::types::BoltDateTime {
                seconds: i64::MAX,
                nanoseconds: 0,
                tz_offset_seconds: 0,
            }),
            BoltValue::DateTime(boltr::types::BoltDateTime {
                seconds: 0,
                nanoseconds: 0,
                tz_offset_seconds: 64_801,
            }),
            BoltValue::DateTimeZoneId(boltr::types::BoltDateTimeZoneId {
                seconds: 0,
                nanoseconds: 0,
                tz_id: "Mars/Base".into(),
            }),
            BoltValue::DateTimeZoneId(boltr::types::BoltDateTimeZoneId {
                seconds: 0,
                nanoseconds: 1_000_000_000,
                tz_id: "UTC".into(),
            }),
        ] {
            assert!(
                matches!(from_bolt(&bad), Err(BoltError::Protocol(_))),
                "{bad:?}"
            );
        }
    }
}

#[cfg(test)]
mod timestamp_representability_tests {
    use super::*;
    #[test]
    fn chrono_leap_second_is_refused_without_silent_normalization() {
        let leap = chrono::NaiveDate::from_ymd_opt(2016, 12, 31)
            .unwrap()
            .and_hms_nano_opt(23, 59, 59, 1_500_000_000)
            .unwrap();
        let value = Value::Timestamp(leap);
        assert!(
            matches!(to_bolt(&value), Err(BoltError::Backend(message)) if message.contains("leap seconds"))
        );
        assert!(matches!(
            to_bolt(&Value::List(vec![value])),
            Err(BoltError::Backend(_))
        ));
        let incompatible = BoltValue::LocalDateTime(BoltLocalDateTime {
            seconds: leap.and_utc().timestamp(),
            nanoseconds: 1_500_000_000,
        });
        assert!(matches!(
            from_bolt(&incompatible),
            Err(BoltError::Protocol(_))
        ));
    }
}

#[cfg(test)]
mod owned_conversion_tests {
    use kglite::api::{NodeValue, PathValue, RelValue};
    use kglite::datatypes::PropMap;

    use super::*;

    fn props() -> PropMap {
        PropMap::from_pairs(vec![
            ("name".into(), Value::String("Ada".into())),
            (
                "tags".into(),
                Value::List(vec![Value::Int64(1), Value::Null]),
            ),
            (
                "nested".into(),
                Value::Map(PropMap::from_pairs(vec![(
                    "k".into(),
                    Value::Boolean(true),
                )])),
            ),
        ])
    }

    fn node(id: u32) -> NodeValue {
        NodeValue {
            id,
            labels: vec!["Person".into(), "Admin".into()],
            properties: props(),
        }
    }

    fn rel(id: u32, start_id: u32, end_id: u32) -> RelValue {
        RelValue {
            incarnation: None,
            id,
            start_id,
            end_id,
            rel_type: "KNOWS".into(),
            properties: props(),
        }
    }

    /// `to_bolt_owned` must produce exactly what `to_bolt` does, for every variant.
    #[test]
    fn owned_conversion_matches_borrowed_conversion() {
        let values = vec![
            Value::Null,
            Value::Boolean(true),
            Value::Int64(-7),
            Value::UniqueId(9),
            Value::Float64(1.5),
            Value::String("text".into()),
            Value::List(vec![
                Value::String("a".into()),
                Value::List(vec![Value::Int64(1)]),
            ]),
            Value::Map(props()),
            Value::DateTime(chrono::NaiveDate::from_ymd_opt(2024, 2, 29).unwrap()),
            Value::Timestamp(
                chrono::NaiveDate::from_ymd_opt(2024, 2, 29)
                    .unwrap()
                    .and_hms_nano_opt(1, 2, 3, 456)
                    .unwrap(),
            ),
            Value::Point { lat: 1.0, lon: 2.0 },
            Value::Duration {
                months: 1,
                days: 2,
                seconds: 3,
                nanos: 0,
            },
            Value::Node(Box::new(node(4))),
            Value::Relationship(Box::new(rel(5, 4, 6))),
            Value::Path(Box::new(PathValue {
                nodes: vec![node(2), node(1), node(3)],
                rels: vec![rel(7, 1, 2), rel(8, 1, 3)],
            })),
        ];
        for value in values {
            assert_eq!(
                to_bolt_owned(value.clone()).unwrap(),
                to_bolt(&value).unwrap(),
                "{value:?}"
            );
        }
    }

    #[test]
    fn owned_conversion_refuses_what_borrowed_conversion_refuses() {
        let leap = chrono::NaiveDate::from_ymd_opt(2016, 12, 31)
            .unwrap()
            .and_hms_nano_opt(23, 59, 59, 1_500_000_000)
            .unwrap();
        for value in [
            Value::NodeRef(1),
            Value::List(vec![Value::Timestamp(leap)]),
            Value::Path(Box::new(PathValue {
                nodes: vec![node(1)],
                rels: vec![rel(7, 1, 2)],
            })),
            Value::Path(Box::new(PathValue {
                nodes: vec![node(1), node(2)],
                rels: vec![rel(7, 3, 4)],
            })),
        ] {
            assert!(to_bolt(&value).is_err(), "{value:?}");
            assert!(to_bolt_owned(value.clone()).is_err(), "{value:?}");
        }
    }
}
