//! How a bulk load tells one version of a declared temporal relationship from
//! another. A row is the relationship already stored between its endpoints
//! only when it is the same image of it — every property equal, numbers and
//! instants compared by what they name ([`StartKey::image`]); anything else
//! is a new, parallel relationship, so no load ever rewrites a stored version. The
//! batch flush buckets candidates on the start ([`StartKey::read`]) before
//! comparing images.

use std::borrow::Borrow;

use chrono::NaiveTime;

use super::eval::{parse_instant, Instant};
use crate::datatypes::values::Value;
use crate::graph::core::filtering::may_parse_as_temporal;
use crate::graph::schema::id_integer;
use crate::graph::schema::{InternedKey, TemporalConfig};

/// The `from` properties rows of one declared relationship type key on, in
/// declaration order: one for a source-keyed or single declaration, several
/// for a legacy type holding more than one unkeyed declaration. A row keys on
/// the first of them it carries. `bounds` holds every `from` and `to`
/// property of those declarations, which an image compares as instants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StartKey {
    from: Box<[InternedKey]>,
    bounds: Box<[InternedKey]>,
}

/// A relationship's properties as a declared-type load compares them: sorted
/// by key, NULLs dropped, each bound as its [`StartValue`] and every other
/// value as [`compared_value`] reads it.
pub(crate) type Image = Vec<(InternedKey, StartValue)>;

/// A row's or stored relationship's start as the load buckets it: the
/// `from` property it was read from and the value, `None` when none is
/// carried or all are NULL.
pub(crate) type Start = Option<(InternedKey, StartValue)>;

/// A start value, normalised so one instant compares equal however it was
/// written: a date, a datetime at midnight and an ISO string of either are
/// the same day; any other datetime is exact. A value the evaluator cannot
/// read keys on itself — rows loaded onto a declared type are not validated.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum StartValue {
    At(Instant),
    Raw(Value),
}

impl StartValue {
    /// `None` for NULL.
    pub(crate) fn of(value: &Value) -> Option<Self> {
        if matches!(value, Value::Null) {
            return None;
        }
        Some(match parse_instant(value) {
            Ok(Instant::Timestamp(ts)) if ts.time() == NaiveTime::MIN => {
                StartValue::At(Instant::Date(ts.date()))
            }
            Ok(instant) => StartValue::At(instant),
            Err(_) => StartValue::Raw(value.clone()),
        })
    }
}

/// A property other than a bound as an image compares it, so a redelivery
/// whose column came back as another kind is still the same value: a whole
/// float and the integer it names exactly are one number (`1.0` and `1`, the
/// id rule of [`id_integer`]), and a date, a datetime and a date-like string
/// compare as the instant they name, as a bound does — so two strings naming
/// one day are one value too. `None` for NULL.
fn compared_value(value: &Value) -> Option<StartValue> {
    match value {
        Value::Null => None,
        Value::DateTime(_) | Value::Timestamp(_) => StartValue::of(value),
        Value::String(text) if may_parse_as_temporal(text) => StartValue::of(value),
        _ => Some(StartValue::Raw(
            id_integer(value).map_or_else(|| value.clone(), Value::Int64),
        )),
    }
}

impl StartKey {
    pub(super) fn of<'c>(configs: impl IntoIterator<Item = &'c TemporalConfig>) -> Option<Self> {
        fn push(keys: &mut Vec<InternedKey>, name: &str) {
            let key = InternedKey::from_str(name);
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        let mut from: Vec<InternedKey> = Vec::new();
        let mut bounds: Vec<InternedKey> = Vec::new();
        for config in configs {
            push(&mut from, &config.valid_from);
            push(&mut bounds, &config.valid_from);
            push(&mut bounds, &config.valid_to);
        }
        (!from.is_empty()).then(|| StartKey {
            from: from.into_boxed_slice(),
            bounds: bounds.into_boxed_slice(),
        })
    }

    /// The image of `properties`, leaving out the keys in `ignore` (the
    /// provenance stamps a load writes afresh on every row).
    pub(crate) fn image<V: Borrow<Value>>(
        &self,
        properties: impl IntoIterator<Item = (InternedKey, V)>,
        ignore: &[InternedKey],
    ) -> Image {
        let mut image: Image = properties
            .into_iter()
            .filter(|(key, _)| !ignore.contains(key))
            .filter_map(|(key, value)| {
                let value = value.borrow();
                let compared = if self.bounds.contains(&key) {
                    StartValue::of(value)
                } else {
                    compared_value(value)
                };
                compared.map(|compared| (key, compared))
            })
            .collect();
        image.sort_by_key(|(key, _)| *key);
        image
    }

    /// The start `cell` gives: the first key whose value is present and not
    /// NULL.
    pub(crate) fn read<V: Borrow<Value>>(
        &self,
        mut cell: impl FnMut(InternedKey) -> Option<V>,
    ) -> Start {
        self.from.iter().find_map(|&key| {
            let value = cell(key)?;
            StartValue::of(value.borrow()).map(|start| (key, start))
        })
    }

    /// The start of a row given as interned property pairs.
    pub(crate) fn of_properties(&self, properties: &[(InternedKey, Value)]) -> Start {
        self.read(|key| {
            properties
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, value)| value)
        })
    }
}
