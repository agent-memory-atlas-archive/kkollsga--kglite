//! Declared cell grammars the lossless CSV export writes and a blueprint can
//! also declare by hand: `text`, `timestamp` and `map`.
//!
//! - `text` is a string whose empty cell is **null**. The empty string is
//!   written `\e`, and a string beginning with a backslash gets one more, so
//!   `\e` and `\\e` are unambiguous: a cell that is exactly `\e` is `""`;
//!   any other cell starting with `\` loses that one backslash.
//! - `timestamp` is ISO 8601 without a zone, `YYYY-MM-DDTHH:MM:SS[.fraction]`
//!   (a space may stand for the `T`).
//! - `map` is a JSON object; nested arrays and objects become lists and maps.

use crate::datatypes::values::{ColumnData, Value};
use crate::graph::io::export::typed_json;
use chrono::{Datelike, NaiveDateTime};

use super::super::table::{MisparseTally, RawCsv};

/// The text a `text` cell stands for, when it is not null.
pub fn decode_text(cell: &str) -> String {
    if cell == "\\e" {
        String::new()
    } else if let Some(rest) = cell.strip_prefix('\\') {
        rest.to_string()
    } else {
        cell.to_string()
    }
}

/// The `text` cell for a string; the inverse of [`decode_text`].
pub fn encode_text(text: &str) -> String {
    if text.is_empty() {
        "\\e".to_string()
    } else if text.starts_with('\\') {
        format!("\\{text}")
    } else {
        text.to_string()
    }
}

pub fn parse_timestamp(cell: &str) -> Option<NaiveDateTime> {
    let s = cell.trim();
    ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"]
        .iter()
        .find_map(|fmt| NaiveDateTime::parse_from_str(s, fmt).ok())
        .filter(|t| super::scalar::year_is_supported(t.year()))
}

pub fn text_column(raw: &RawCsv, src_idx: usize) -> ColumnData {
    ColumnData::String(
        raw.rows
            .iter()
            .enumerate()
            .map(|(r, row)| (!raw.nulls[r][src_idx]).then(|| decode_text(&row[src_idx])))
            .collect(),
    )
}

pub fn timestamp_column(
    raw: &RawCsv,
    src_idx: usize,
    column: &str,
    misparses: &mut MisparseTally,
) -> ColumnData {
    ColumnData::Timestamp(
        raw.rows
            .iter()
            .enumerate()
            .map(|(r, row)| {
                if raw.nulls[r][src_idx] {
                    return None;
                }
                let parsed = parse_timestamp(&row[src_idx]);
                if parsed.is_none() {
                    misparses.record_timestamp(column, raw.row_id(r), &row[src_idx]);
                }
                parsed
            })
            .collect(),
    )
}

pub fn map_column(raw: &RawCsv, src_idx: usize) -> ColumnData {
    ColumnData::Map(
        raw.rows
            .iter()
            .enumerate()
            .map(|(r, row)| {
                if raw.nulls[r][src_idx] {
                    return None;
                }
                match serde_json::from_str::<serde_json::Value>(&row[src_idx]) {
                    Ok(serde_json::Value::Object(obj)) => {
                        match typed_json::from_json(&serde_json::Value::Object(obj)) {
                            Value::Map(map) => Some(map),
                            _ => None,
                        }
                    }
                    _ => None,
                }
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_escape_round_trips_every_hostile_shape() {
        for s in ["", "\\", "\\e", "\\\\e", "a\\", "plain", " x ", "e", "\\n"] {
            assert_eq!(decode_text(&encode_text(s)), s, "{s:?}");
        }
        assert_eq!(encode_text(""), "\\e");
    }

    #[test]
    fn timestamps_keep_fractions() {
        let t = parse_timestamp("2020-01-02T03:04:05.5").unwrap();
        assert_eq!(
            t.format("%Y-%m-%dT%H:%M:%S%.f").to_string(),
            "2020-01-02T03:04:05.500"
        );
        assert!(parse_timestamp("2020-01-02").is_none());
    }

    #[test]
    fn declared_columns_read_null_empty_timestamp_and_map() {
        use crate::datatypes::values::DataFrame;
        use std::collections::HashMap;
        let cells = ["", "\\e", "\\\\x", "hi"];
        let raw = RawCsv {
            headers: vec!["t".into(), "ts".into(), "m".into()],
            rows: vec![
                vec![
                    cells[0].into(),
                    "2020-01-02T03:04:05.250".into(),
                    "{\"a\":[1,{\"b\":null}]}".into(),
                ],
                vec![cells[1].into(), "".into(), "".into()],
                vec![cells[2].into(), "bad".into(), "[1]".into()],
                vec![cells[3].into(), "".into(), "".into()],
            ],
            nulls: vec![
                vec![true, false, false],
                vec![false, true, true],
                vec![false, false, false],
                vec![false, true, true],
            ],
            row_ids: vec![1, 2, 3, 4],
        };
        let declared: HashMap<String, String> = [("t", "text"), ("ts", "timestamp"), ("m", "map")]
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        let keep: Vec<String> = ["t", "ts", "m"].iter().map(|s| s.to_string()).collect();
        let mut tally = crate::graph::blueprint::table::MisparseTally::default();
        let df: DataFrame = crate::graph::blueprint::typing::typed_dataframe(
            &raw,
            &keep,
            &declared,
            &HashMap::new(),
            &mut tally,
        )
        .unwrap();
        let t: Vec<Option<Value>> = (0..4).map(|r| df.get_value(r, "t")).collect();
        assert_eq!(t[0], None);
        assert_eq!(t[1], Some(Value::String(String::new())));
        assert_eq!(t[2], Some(Value::String("\\x".into())));
        assert_eq!(t[3], Some(Value::String("hi".into())));
        assert!(matches!(df.get_value(0, "ts"), Some(Value::Timestamp(_))));
        assert_eq!(df.get_value(2, "ts"), None);
        assert!(matches!(df.get_value(0, "m"), Some(Value::Map(_))));
        assert_eq!(df.get_value(2, "m"), None);
    }
}
