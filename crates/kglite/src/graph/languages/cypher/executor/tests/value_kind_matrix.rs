//! The value-kind matrix: one table of representative temporal values pushed
//! through every path that decides equality, order and identity, so a new kind
//! cannot be added to `Value` and silently fall through one of them.
//!
//! The kinds are rows; the questions are the columns of the assertions below:
//! `==`, `Hash`, `Ord`, Cypher `=`/`<`/`>`/`IN`, `DISTINCT`, grouping,
//! `ORDER BY`, `min`/`max`, and the serde, WAL and overflow-bag round trips.
//! Each expectation is derived from the table's own `family` and `pos`
//! columns, written by hand, never from the engine under test. A new kind is
//! one more row (plus a family when it is incomparable with every existing one).

use super::*;
use crate::datatypes::values::Value;

/// A representative value and where it sits relative to its neighbours.
///
/// `family` groups kinds that compare with each other (`<`, `>`, `=`);
/// kinds in different families are incomparable (`=` false, ordering null).
/// `pos` is the rank inside the family: equal `pos` means equal. `ordered` is
/// false for a family with `=` but no `<`/`>` (durations).
struct Row {
    name: &'static str,
    family: &'static str,
    pos: i32,
    ordered: bool,
    literal: &'static str,
}

/// Declaration order is the expected `ORDER BY` block order.
const ROWS: &[Row] = &[
    Row {
        name: "date_a",
        family: "datetime",
        pos: 10,
        ordered: true,
        literal: "date('2020-01-02')",
    },
    Row {
        name: "ldt_a",
        family: "datetime",
        pos: 20,
        ordered: true,
        literal: "localdatetime('2020-01-02T03:04:05.25')",
    },
    Row {
        name: "date_b",
        family: "datetime",
        pos: 30,
        ordered: true,
        literal: "date('2021-06-15')",
    },
    Row {
        name: "ldt_b",
        family: "datetime",
        pos: 40,
        ordered: true,
        literal: "localdatetime('2021-06-15T00:00:00.5')",
    },
    Row {
        name: "lt_a",
        family: "localtime",
        pos: 1,
        ordered: true,
        literal: "localtime('10:00')",
    },
    Row {
        name: "lt_b",
        family: "localtime",
        pos: 2,
        ordered: true,
        literal: "localtime('10:00:00.000000001')",
    },
    // 10:00+01:00 and 09:00Z are one instant of day: equal, offsets ignored.
    Row {
        name: "time_a",
        family: "time",
        pos: 1,
        ordered: true,
        literal: "time('10:00+01:00')",
    },
    Row {
        name: "time_a_utc",
        family: "time",
        pos: 1,
        ordered: true,
        literal: "time('09:00Z')",
    },
    Row {
        name: "time_b",
        family: "time",
        pos: 2,
        ordered: true,
        literal: "time('09:00:00.000000001Z')",
    },
    Row {
        name: "dur_a",
        family: "duration",
        pos: 1,
        ordered: false,
        literal: "duration('PT1S')",
    },
    Row {
        name: "dur_fine",
        family: "duration",
        pos: 2,
        ordered: false,
        literal: "duration({seconds: 1, nanoseconds: 5})",
    },
    // Zoned datetimes share the datetime family: a zoned value is its instant
    // and a local datetime is read as UTC, so these tie with, or fall between,
    // the date and local-datetime rows above by instant alone. The `datetime`
    // kinds are appended last so earlier row indexes hold.
    Row {
        name: "zdt_a",
        family: "datetime",
        pos: 20,
        ordered: true,
        literal: "datetime('2020-01-02T03:04:05.25Z')",
    },
    // The same instant written at +02:00: equal, offset ignored.
    Row {
        name: "zdt_a_off",
        family: "datetime",
        pos: 20,
        ordered: true,
        literal: "datetime('2020-01-02T05:04:05.25+02:00')",
    },
    Row {
        name: "zdt_mid",
        family: "datetime",
        pos: 25,
        ordered: true,
        literal: "datetime('2020-01-02T12:00:00Z')",
    },
    // Midnight UTC of the second date, written at +02:00: ties with `date_b`.
    Row {
        name: "zdt_b",
        family: "datetime",
        pos: 30,
        ordered: true,
        literal: "datetime('2021-06-15T02:00:00+02:00')",
    },
    Row {
        name: "zdt_c",
        family: "datetime",
        pos: 40,
        ordered: true,
        literal: "datetime('2021-06-15T00:00:00.5Z')",
    },
    // Named zones (feature `iana-zones`): identity is still the instant, so the
    // zone never splits or merges a value. Oslo is +02:00 in June, so this is
    // the instant of `zdt_b` (midnight UTC) and ties with it.
    #[cfg(feature = "iana-zones")]
    Row {
        name: "zdt_oslo_tie",
        family: "datetime",
        pos: 30,
        ordered: true,
        literal: "datetime('2021-06-15T02:00[Europe/Oslo]')",
    },
    // 02:30 on 2021-03-28 is skipped in Oslo: 03:30+02:00, which is 01:30Z.
    #[cfg(feature = "iana-zones")]
    Row {
        name: "zdt_oslo_gap",
        family: "datetime",
        pos: 27,
        ordered: true,
        literal: "datetime('2021-03-28T02:30[Europe/Oslo]')",
    },
    // 02:30 on 2021-10-31 is repeated in Oslo; the earlier is +02:00 = 00:30Z.
    #[cfg(feature = "iana-zones")]
    Row {
        name: "zdt_oslo_fold_early",
        family: "datetime",
        pos: 60,
        ordered: true,
        literal: "datetime('2021-10-31T02:30[Europe/Oslo]')",
    },
    // The same instant written in Chicago (CDT, -05:00): equal across zones.
    #[cfg(feature = "iana-zones")]
    Row {
        name: "zdt_chicago_fold_early",
        family: "datetime",
        pos: 60,
        ordered: true,
        literal: "datetime('2021-10-30T19:30[America/Chicago]')",
    },
    // The later occurrence, 02:30+01:00 = 01:30Z.
    #[cfg(feature = "iana-zones")]
    Row {
        name: "zdt_oslo_fold_late",
        family: "datetime",
        pos: 70,
        ordered: true,
        literal: "datetime('2021-10-31T02:30+01:00[Europe/Oslo]')",
    },
];

fn value_of(row: &Row) -> Value {
    super::temporal_functions::cell(&format!("RETURN {}", row.literal))
}

/// Cypher `=`: the same family and the same position.
fn same_kind_and_pos(a: &Row, b: &Row) -> bool {
    a.family == b.family && a.pos == b.pos
}

/// `Value` identity (`==`, `DISTINCT`, grouping): `=` and the same kind, so a
/// date and the zoned value at its midnight stay two values.
fn same_identity(a: &Row, b: &Row) -> bool {
    kind_of(a) == kind_of(b) && same_kind_and_pos(a, b)
}

fn kind_of(row: &Row) -> &str {
    row.name.split('_').next().unwrap_or(row.name)
}

fn hash_of(value: &Value) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

#[test]
fn eq_hash_and_ord_agree_for_every_pair() {
    let values: Vec<Value> = ROWS.iter().map(value_of).collect();
    for (i, (a, row_a)) in values.iter().zip(ROWS).enumerate() {
        assert_eq!(a, &a.clone(), "{} is not equal to its clone", row_a.name);
        assert_eq!(hash_of(a), hash_of(&a.clone()), "{} hash", row_a.name);
        for (j, (b, row_b)) in values.iter().zip(ROWS).enumerate() {
            let equal = a == b;
            assert_eq!(
                equal,
                a.cmp(b).is_eq(),
                "{} vs {}: Eq and Ord",
                row_a.name,
                row_b.name
            );
            assert_eq!(
                a.cmp(b),
                b.cmp(a).reverse(),
                "{} vs {}: antisymmetry",
                row_a.name,
                row_b.name
            );
            if equal {
                assert_eq!(
                    hash_of(a),
                    hash_of(b),
                    "{} vs {}: equal values hash alike",
                    row_a.name,
                    row_b.name
                );
            }
            // Within one kind, the table says which pairs are equal. The date
            // and datetime rows are two kinds of one family and never equal
            // here (no midnight datetime in the table).
            if i != j && kind_of(row_a) == kind_of(row_b) {
                assert_eq!(
                    equal,
                    same_kind_and_pos(row_a, row_b),
                    "{} vs {}",
                    row_a.name,
                    row_b.name
                );
            }
            // The Cypher order (total_order) is a total order consistent with it.
            let total = crate::graph::core::filtering::total_order(a, b);
            assert_eq!(
                total,
                crate::graph::core::filtering::total_order(b, a).reverse(),
                "{} vs {}: total_order antisymmetry",
                row_a.name,
                row_b.name
            );
        }
    }
    let mut sorted = values.clone();
    sorted.sort_by(crate::graph::core::filtering::total_order);
    assert!(sorted
        .windows(2)
        .all(|w| crate::graph::core::filtering::total_order(&w[0], &w[1]).is_le()));
}

#[test]
fn membership_finds_exactly_the_equal_values_hashed_or_linear() {
    use crate::graph::core::membership::MembershipSet;
    let values: Vec<Value> = ROWS.iter().map(value_of).collect();
    // The table is longer than the linear threshold, so the hashed index is live.
    let set = MembershipSet::new(values.clone());
    let short = MembershipSet::new(vec![values[6].clone(), Value::Int64(1)]);
    for (probe, row) in values.iter().zip(ROWS) {
        assert!(set.matches(probe), "{} is in the full set", row.name);
    }
    // 10:00+01:00 is found by 09:00Z, and a local time is not found by a time.
    assert!(short.matches(&values[7]));
    assert!(!short.matches(&values[4]));
    // A zoned instant is found by the same instant at another offset and by the
    // local datetime read as UTC (and a date by its midnight), never by a
    // neighbouring instant.
    let zoned = |name: &str| {
        let at = ROWS.iter().position(|r| r.name == name).unwrap();
        values[at].clone()
    };
    let zoned_set = MembershipSet::new(vec![zoned("zdt_a"), Value::Int64(1)]);
    assert!(zoned_set.matches(&zoned("zdt_a_off")));
    assert!(zoned_set.matches(&zoned("ldt_a")));
    assert!(!zoned_set.matches(&zoned("zdt_mid")));
    assert!(!zoned_set.matches(&zoned("date_a")));
    let date_set = MembershipSet::new((0..12).map(Value::Int64).chain([zoned("date_b")]).collect());
    assert!(date_set.matches(&zoned("zdt_b")));
    assert!(!date_set.matches(&zoned("zdt_c")));
    // A value of every other family is absent from a one-family set.
    let only_times = MembershipSet::new(values[6..9].to_vec());
    for (probe, row) in values.iter().zip(ROWS) {
        assert_eq!(
            only_times.matches(probe),
            row.family == "time",
            "{}",
            row.name
        );
    }
}

#[test]
fn serde_wal_and_overflow_round_trips_keep_every_value() {
    use crate::graph::storage::overflow::{decode_blob, encode_value};
    for row in ROWS {
        let value = value_of(row);
        // JSON and postcard, compared on Debug so an offset that `==` ignores
        // is still held to.
        let json: Value = serde_json::from_str(&serde_json::to_string(&value).unwrap()).unwrap();
        assert_eq!(
            format!("{json:?}"),
            format!("{value:?}"),
            "{} json",
            row.name
        );
        let bytes = crate::serde_codec::encode_versioned(
            crate::serde_codec::CURRENT_CODEC,
            &value,
            u64::MAX,
        )
        .unwrap();
        let back: Value = crate::serde_codec::decode_exact_with(
            crate::serde_codec::CURRENT_CODEC,
            &bytes,
            bytes.len() as u64,
            crate::serde_codec::DecodeLimits::new(u64::MAX, u64::MAX),
        )
        .unwrap();
        assert_eq!(
            format!("{back:?}"),
            format!("{value:?}"),
            "{} postcard",
            row.name
        );

        // A WAL frame carrying it as a node property.
        let frame = crate::graph::wal::WalFrame {
            lsn: 1,
            ops: vec![crate::graph::wal::MutationOp::UpsertNode {
                node_type: "M".into(),
                id: Value::Int64(1),
                title: Value::String("t".into()),
                properties: vec![("v".into(), value.clone())],
            }],
        };
        let frame_bytes = crate::serde_codec::encode_versioned(
            crate::serde_codec::CURRENT_CODEC,
            &frame,
            u64::MAX,
        )
        .unwrap();
        let frame_back: crate::graph::wal::WalFrame = crate::serde_codec::decode_exact_with(
            crate::serde_codec::CURRENT_CODEC,
            &frame_bytes,
            frame_bytes.len() as u64,
            crate::serde_codec::DecodeLimits::new(u64::MAX, u64::MAX),
        )
        .unwrap();
        let crate::graph::wal::MutationOp::UpsertNode { properties, .. } = &frame_back.ops[0]
        else {
            panic!("the op kind changed");
        };
        assert_eq!(
            format!("{:?}", properties[0].1),
            format!("{value:?}"),
            "{} wal",
            row.name
        );

        // The overflow bag, which stores a Duration, LocalTime and Time as
        // themselves (a Date and a LocalDateTime have their own tags).
        let mut blob = 1u16.to_le_bytes().to_vec();
        let key = crate::graph::schema::InternedKey::from_str("v");
        encode_value(&mut blob, key, &value);
        let decoded = decode_blob(&blob);
        assert_eq!(decoded.len(), 1, "{} overflow", row.name);
        assert_eq!(
            format!("{:?}", decoded[0].1),
            format!("{value:?}"),
            "{} overflow",
            row.name
        );
    }
}

/// Build the graph: one `:M` node per table row, the literal stored as `v`.
fn matrix_graph() -> DirGraph {
    let mut graph = DirGraph::new();
    for row in ROWS {
        let query = parser::parse_cypher(&format!(
            "CREATE (:M {{k: '{}', v: {}}})",
            row.name, row.literal
        ))
        .unwrap();
        execute_mutable(
            &mut graph,
            &query,
            HashMap::new(),
            crate::graph::algorithms::Interrupt::default(),
        )
        .unwrap_or_else(|e| panic!("CREATE failed for {}: {e}", row.name));
    }
    graph
}

fn column(graph: &DirGraph, query: &str) -> Vec<Value> {
    let parsed = parser::parse_cypher(query).unwrap();
    let params = HashMap::new();
    let result = CypherExecutor::with_params(graph, &params, None)
        .execute(&parsed)
        .unwrap_or_else(|e| panic!("query failed: {query}\n  error: {e}"));
    result.rows.into_iter().map(|mut r| r.remove(0)).collect()
}

fn names(graph: &DirGraph, query: &str) -> Vec<String> {
    let mut out: Vec<String> = column(graph, query)
        .into_iter()
        .map(|v| match v {
            Value::String(s) => s,
            other => panic!("expected a name, got {other:?}"),
        })
        .collect();
    out.sort();
    out
}

fn expected(rows: impl Iterator<Item = &'static Row>) -> Vec<String> {
    let mut out: Vec<String> = rows.map(|r| r.name.to_string()).collect();
    out.sort();
    out
}

fn check_graph(graph: &DirGraph, pass: &str) {
    for probe in ROWS {
        let lit = probe.literal;
        let query = |op: &str| format!("MATCH (n:M) WHERE n.v {op} {lit} RETURN n.k");
        assert_eq!(
            names(graph, &query("=")),
            expected(ROWS.iter().filter(|r| same_kind_and_pos(r, probe))),
            "{pass}: = {}",
            probe.name
        );
        assert_eq!(
            names(graph, &query("<>")),
            // `<>` is `NOT (=)`: true across families, where `=` is false.
            expected(ROWS.iter().filter(|r| !same_kind_and_pos(r, probe))),
            "{pass}: <> {}",
            probe.name
        );
        let greater = ROWS
            .iter()
            .filter(|r| probe.ordered && r.family == probe.family && r.pos > probe.pos);
        assert_eq!(
            names(graph, &query(">")),
            expected(greater),
            "{pass}: > {}",
            probe.name
        );
        let less = ROWS
            .iter()
            .filter(|r| probe.ordered && r.family == probe.family && r.pos < probe.pos);
        assert_eq!(
            names(graph, &query("<")),
            expected(less),
            "{pass}: < {}",
            probe.name
        );
        assert_eq!(
            names(
                graph,
                &format!("MATCH (n:M) WHERE n.v IN [{lit}, 1, 'x'] RETURN n.k")
            ),
            expected(ROWS.iter().filter(|r| same_kind_and_pos(r, probe))),
            "{pass}: IN {}",
            probe.name
        );
    }

    // DISTINCT and grouping both merge the equal values and nothing else.
    let distinct_count = ROWS
        .iter()
        .enumerate()
        .filter(|(i, r)| !ROWS[..*i].iter().any(|earlier| same_identity(earlier, r)))
        .count();
    assert_eq!(
        column(graph, "MATCH (n:M) RETURN DISTINCT n.v").len(),
        distinct_count,
        "{pass}: DISTINCT"
    );
    assert_eq!(
        column(graph, "MATCH (n:M) WITH n.v AS v, count(*) AS c RETURN c").len(),
        distinct_count,
        "{pass}: GROUP BY"
    );
    let two_equal = column(
        graph,
        "MATCH (n:M) WITH n.v AS v, count(*) AS c WHERE c = 2 RETURN c",
    );
    assert_eq!(
        two_equal.len(),
        ROWS.len() - distinct_count,
        "{pass}: a group holds each pair of equal values (the two times, the two instants)"
    );

    // ORDER BY lays the families out as contiguous blocks in rank order, and
    // each ordered family ascending by pos.
    let ordered = column(graph, "MATCH (n:M) RETURN n.k ORDER BY n.v");
    let by_name = |n: &Value| {
        ROWS.iter()
            .find(|r| Value::String(r.name.into()) == *n)
            .unwrap()
    };
    let mut blocks: Vec<&str> = Vec::new();
    for v in &ordered {
        let family = by_name(v).family;
        if blocks.last() != Some(&family) {
            blocks.push(family);
        }
    }
    assert_eq!(
        blocks,
        ["datetime", "localtime", "time", "duration"],
        "{pass}: ORDER BY blocks"
    );
    for family in ["datetime", "localtime", "time"] {
        let positions: Vec<i32> = ordered
            .iter()
            .map(by_name)
            .filter(|r| r.family == family)
            .map(|r| r.pos)
            .collect();
        assert!(
            positions.windows(2).all(|w| w[0] <= w[1]),
            "{pass}: {family} ascending, got {positions:?}"
        );
    }

    // min / max take the order's extremes: the first date, the last duration.
    let extremes = column(
        graph,
        "MATCH (n:M) WITH min(n.v) AS lo, max(n.v) AS hi RETURN [lo, hi]",
    );
    let Value::List(pair) = &extremes[0] else {
        panic!("{pass}: min/max list")
    };
    assert_eq!(pair[0], value_of(&ROWS[0]), "{pass}: min");
    assert_eq!(pair[1], value_of(&ROWS[10]), "{pass}: max");
    // The same instant at two offsets is one DISTINCT value that keeps the
    // first offset it met.
    let distinct_zoned = column(
        graph,
        "MATCH (n:M) WHERE n.k IN ['zdt_a', 'zdt_a_off'] RETURN DISTINCT n.v",
    );
    assert_eq!(
        distinct_zoned.len(),
        1,
        "{pass}: DISTINCT merges one instant"
    );
    assert!(
        format!("{:?}", distinct_zoned[0]).contains("offset_secs: 0"),
        "{pass}: DISTINCT keeps the first offset met, got {:?}",
        distinct_zoned[0]
    );
}

#[test]
fn stored_values_answer_every_question_like_their_literals() {
    let mut graph = matrix_graph();
    check_graph(&graph, "scan");
    // The same questions with an equality index on the property: an index the
    // new kinds cannot key must decline to a scan, never answer for them.
    graph.create_index("M", "v");
    check_graph(&graph, "equality index");
    graph.create_range_index("M", "v");
    check_graph(&graph, "range index");
}
