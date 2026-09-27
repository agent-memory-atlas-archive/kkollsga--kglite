//! The endpoint index against the evaluator it stands in for: every mask
//! bit, count and timeless verdict must equal what `eval::interval_contains`
//! answers row by row, over an exhaustive small domain of bound kinds and
//! instants, and over generated rows.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use fixedbitset::FixedBitSet;
use petgraph::graph::NodeIndex;

use super::*;
use crate::graph::core::graph_filter::{GraphFilter, GuardBounds, NodeGuard};
use crate::graph::features::temporal::declarations::{declare, list};
use crate::graph::session::execute::{execute_mut, ExecuteOptions};

const CLOSED: IntervalConvention = IntervalConvention::Closed;
const HALF_OPEN: IntervalConvention = IntervalConvention::HalfOpen;

fn day(n: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2020, 1, n).unwrap()
}

fn at(n: u32, h: u32, m: u32, s: u32, micro: u32) -> NaiveDateTime {
    day(n).and_time(NaiveTime::from_hms_micro_opt(h, m, s, micro).unwrap())
}

/// Every bound spelling on days 2..=4: NULL, a date, datetimes at midnight,
/// 08:00 and the day's last microsecond, and ISO strings of a date and a
/// datetime.
fn bound_domain() -> Vec<Value> {
    let mut values = vec![Value::Null];
    for n in 2..=4 {
        values.push(Value::DateTime(day(n)));
        values.push(Value::Timestamp(at(n, 0, 0, 0, 0)));
        values.push(Value::Timestamp(at(n, 8, 0, 0, 0)));
        values.push(Value::Timestamp(at(n, 23, 59, 59, 999_999)));
        values.push(Value::String(format!("2020-01-0{n}")));
        values.push(Value::String(format!("2020-01-0{n}T08:00:00")));
    }
    values
}

/// Every date on days 1..=5, and datetimes at midnight, 08:00, noon and the
/// last microsecond of each.
fn instant_domain() -> Vec<Instant> {
    let mut instants = Vec::new();
    for n in 1..=5 {
        instants.push(Instant::Date(day(n)));
        for (h, m, s, us) in [
            (0, 0, 0, 0),
            (8, 0, 0, 0),
            (12, 0, 0, 0),
            (23, 59, 59, 999_999),
        ] {
            instants.push(Instant::Timestamp(at(n, h, m, s, us)));
        }
    }
    instants
}

/// Build an index over `rows` (slot = position) the way a target walk does.
fn build(rows: &[(Value, Value)], convention: IntervalConvention) -> (TargetCounts, EndpointIndex) {
    let mut scan = Scan {
        convention,
        counts: TargetCounts::default(),
        builder: Ok(Builder::new(convention, usize::MAX)),
    };
    for (slot, (from, to)) in rows.iter().enumerate() {
        scan.visit(slot, from, to);
    }
    let index = scan
        .builder
        .map(Builder::finish)
        .expect("every row is indexable");
    (scan.counts, index)
}

fn contains(row: &(Value, Value), t: Instant, convention: IntervalConvention) -> bool {
    eval::interval_contains(&row.0, &row.1, t, convention).unwrap()
}

/// The index's answer for every row at `t`: a mask over the rows.
fn mask_at(index: &EndpointIndex, rows: usize, t: Instant) -> FixedBitSet {
    let mut mask = FixedBitSet::with_capacity(rows);
    mask.insert_range(..);
    index.clear_invalid(index.segment_of(t).unwrap(), &mut mask);
    mask
}

#[test]
fn the_mask_count_and_timeless_verdict_equal_the_evaluator_on_every_row() {
    let values = bound_domain();
    let rows: Vec<(Value, Value)> = values
        .iter()
        .flat_map(|from| values.iter().map(move |to| (from.clone(), to.clone())))
        .collect();
    for convention in [CLOSED, HALF_OPEN] {
        let (counts, index) = build(&rows, convention);
        assert_eq!(counts.rows, rows.len());
        assert_eq!(index.rows(), rows.len());
        let empty = rows
            .iter()
            .filter(|row| {
                instant_domain()
                    .iter()
                    .all(|&t| !contains(row, t, convention))
            })
            .count();
        assert!(counts.empty_rows > 0, "the domain has empty rows");
        // An empty row is valid on no instant, so it is never in any mask; a
        // row the domain's instants all miss need not be empty.
        assert!(counts.empty_rows <= empty);
        for t in instant_domain() {
            let mask = mask_at(&index, rows.len(), t);
            let mut valid = 0;
            for (slot, row) in rows.iter().enumerate() {
                let expected = contains(row, t, convention);
                valid += usize::from(expected);
                assert_eq!(
                    mask.contains(slot),
                    expected,
                    "{row:?} at {t:?} under {convention:?}"
                );
            }
            assert_eq!(index.count_at(t), Some(valid), "{t:?} {convention:?}");
            assert_eq!(index.timeless_at(t), Some(valid == rows.len()));
        }
    }
}

#[test]
fn every_single_row_is_timeless_exactly_where_the_evaluator_admits_it() {
    // `timeless` is "every row valid", so on a one-row target it is the
    // evaluator's own verdict — the empty rows included, which are never.
    let values = bound_domain();
    for convention in [CLOSED, HALF_OPEN] {
        for from in &values {
            for to in &values {
                let row = (from.clone(), to.clone());
                let (_, index) = build(std::slice::from_ref(&row), convention);
                for t in instant_domain() {
                    assert_eq!(
                        index.timeless_at(t),
                        Some(contains(&row, t, convention)),
                        "{row:?} at {t:?} under {convention:?}"
                    );
                }
            }
        }
    }
}

/// A small deterministic generator (64-bit LCG), so the brute-force test
/// needs no dependency and reproduces exactly.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, below: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % below
    }

    fn bound(&mut self) -> Value {
        let date = NaiveDate::from_ymd_opt(2000, 1, 1).unwrap() + chrono::Days::new(self.next(90));
        let time = NaiveTime::from_hms_opt(self.next(24) as u32, self.next(60) as u32, 0).unwrap();
        match self.next(5) {
            0 => Value::Null,
            1 | 2 => Value::DateTime(date),
            3 => Value::Timestamp(date.and_time(time)),
            _ => Value::String(date.format("%Y-%m-%d").to_string()),
        }
    }

    fn instant(&mut self) -> Instant {
        let date =
            NaiveDate::from_ymd_opt(1999, 12, 20).unwrap() + chrono::Days::new(self.next(110));
        if self.next(2) == 0 {
            Instant::Date(date)
        } else {
            let time =
                NaiveTime::from_hms_opt(self.next(24) as u32, self.next(60) as u32, 0).unwrap();
            Instant::Timestamp(date.and_time(time))
        }
    }
}

#[test]
fn count_at_equals_a_brute_force_count_over_generated_rows() {
    let mut gen = Lcg(0x5eed);
    for convention in [CLOSED, HALF_OPEN] {
        let rows: Vec<(Value, Value)> = (0..2_000).map(|_| (gen.bound(), gen.bound())).collect();
        let (counts, index) = build(&rows, convention);
        assert!(counts.empty_rows > 0);
        for _ in 0..300 {
            let t = gen.instant();
            let brute = rows
                .iter()
                .filter(|row| contains(row, t, convention))
                .count();
            assert_eq!(index.count_at(t), Some(brute), "{t:?} {convention:?}");
        }
    }
}

#[test]
fn a_bound_or_instant_finer_than_a_microsecond_has_no_key() {
    let fine = day(2).and_hms_nano_opt(8, 0, 0, 1_500).unwrap();
    let rows = [(Value::Timestamp(fine), Value::Null)];
    let mut scan = Scan {
        convention: CLOSED,
        counts: TargetCounts::default(),
        builder: Ok(Builder::new(CLOSED, usize::MAX)),
    };
    scan.visit(0, &rows[0].0, &rows[0].1);
    assert!(matches!(scan.builder, Err(Unindexed::Unrepresentable)));
    assert_eq!(scan.counts.rows, 1);
    let (_, index) = build(&[(Value::DateTime(day(2)), Value::Null)], CLOSED);
    assert_eq!(index.segment_of(Instant::Timestamp(fine)), None);
    assert_eq!(
        index.count_at(Instant::Timestamp(at(2, 8, 0, 0, 1))),
        Some(1)
    );
}

#[test]
fn keys_span_the_whole_date_range() {
    // A date key is a day's midnight in microseconds; chrono's extreme
    // dates stay inside i64 on both sides of the day.
    for date in [NaiveDate::MIN, NaiveDate::MAX, day(1)] {
        let start = day_start(date);
        assert!(start.checked_add(DAY_US - 1).is_some());
    }
    assert_eq!(day_start(NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()), 0);
    let rows = [
        (
            Value::DateTime(NaiveDate::MIN),
            Value::DateTime(NaiveDate::MAX),
        ),
        (Value::Null, Value::DateTime(day(1))),
    ];
    let (_, index) = build(&rows, CLOSED);
    assert_eq!(index.count_at(Instant::Date(NaiveDate::MAX)), Some(1));
    assert_eq!(index.count_at(Instant::Date(NaiveDate::MIN)), Some(2));
}

// --- Through a graph: the walk, the cache and resolve ------------------

fn run(graph: &mut DirGraph, query: &str) {
    let params: HashMap<String, Value> = HashMap::new();
    execute_mut(graph, query, &ExecuteOptions::eager(&params))
        .unwrap_or_else(|e| panic!("{query}: {e}"));
}

fn graph(queries: &[&str]) -> DirGraph {
    let mut graph = DirGraph::new();
    for query in queries {
        run(&mut graph, query);
    }
    graph
}

fn node(label: &str) -> TemporalTarget {
    TemporalTarget::Node(label.into())
}

fn date(text: &str) -> Instant {
    Instant::Date(NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap())
}

/// Three well versions, one open-ended, and a licence period per well.
const WELLS: &[&str] = &[
    "UNWIND [
        {id: 1, vf: date('2000-01-01'), vt: date('2009-12-31')},
        {id: 2, vf: date('2010-01-01'), vt: date('2019-12-31')},
        {id: 3, vf: date('2020-01-01'), vt: null}
    ] AS r CREATE (:Well {id: r.id, vf: r.vf, vt: r.vt})",
    "MATCH (w:Well) CREATE (w)-[:LICENSED {lf: date('2005-01-01'), lt: date('2015-12-31')}]->(:Company {id: w.id})",
];

fn licensed() -> TemporalTarget {
    TemporalTarget::Relationship {
        rel_type: "LICENSED".into(),
        source_type: None,
    }
}

fn declared_wells() -> DirGraph {
    let mut g = graph(WELLS);
    declare(&mut g, &node("Well"), "vf", "vt", CLOSED).unwrap();
    declare(&mut g, &licensed(), "lf", "lt", CLOSED).unwrap();
    g
}

fn well_config(g: &DirGraph) -> TemporalConfig {
    g.temporal.node("Well").unwrap().clone()
}

fn template(g: &DirGraph) -> GuardTemplate {
    let mut template = GuardTemplate::default();
    template.nodes.push(NodeGuard {
        label: "Well".into(),
        bounds: GuardBounds::of(&well_config(g)),
    });
    for config in g.temporal.edges("LICENSED") {
        template
            .edges
            .push(crate::graph::core::graph_filter::EdgeGuard {
                rel_type: "LICENSED".into(),
                rel_key: InternedKey::from_str("LICENSED"),
                source_type: None,
                source_type_key: None,
                bounds: GuardBounds::of(config),
            });
    }
    template
}

fn resolved(g: &DirGraph, t: Instant) -> ResolvedFilter {
    GraphFilter {
        template: Arc::new(template(g)),
        selector: ValidTimeSelector::AsOf(t),
    }
    .resolve(g)
}

fn well_slot(g: &DirGraph, id: i64) -> usize {
    g.type_indices
        .get("Well")
        .unwrap()
        .iter()
        .find(|&idx| g.graph.get_node_id(idx) == Some(Value::Int64(id)))
        .map(NodeIndex::index)
        .unwrap()
}

fn well_count(g: &DirGraph, t: &str) -> Option<usize> {
    node_count_at(g, "Well", date(t))
}

#[test]
fn a_graph_target_counts_and_masks_what_the_evaluator_admits() {
    let g = declared_wells();
    assert_eq!(well_count(&g, "2005-06-01"), Some(1));
    assert_eq!(well_count(&g, "1999-06-01"), Some(0));
    assert_eq!(well_count(&g, "2025-06-01"), Some(1));
    let r = resolved(&g, date("2012-06-01"));
    assert!(r.guarded.is_empty());
    assert!(!r.timeless);
    let masks = r.masks.expect("an indexed, non-timeless filter has masks");
    assert!(!masks.nodes.contains(well_slot(&g, 1)));
    assert!(masks.nodes.contains(well_slot(&g, 2)));
    assert!(!masks.nodes.contains(well_slot(&g, 3)));
    // Companies are governed by no declaration.
    for idx in g.type_indices.get("Company").unwrap().iter() {
        assert!(masks.nodes.contains(idx.index()));
    }
    // Every licence runs 2005..2015.
    assert_eq!(masks.edges.count_ones(..), 3);
    let r = resolved(&g, date("2016-06-01"));
    assert_eq!(r.masks.unwrap().edges.count_ones(..), 0);
}

#[test]
fn two_instants_in_one_segment_share_a_key_and_a_mask() {
    let g = declared_wells();
    let a = resolved(&g, date("2011-03-01"));
    let b = resolved(&g, date("2012-09-30"));
    assert_eq!(a.key, b.key);
    // The second resolve is served the first one's cached masks.
    assert!(Arc::ptr_eq(
        a.masks.as_ref().unwrap(),
        b.masks.as_ref().unwrap()
    ));
    // Across the LICENSED end (2015-12-31) the key moves and the mask with it.
    let c = resolved(&g, date("2016-01-01"));
    assert_ne!(a.key, c.key);
    assert_ne!(a.masks.unwrap().edges, c.masks.unwrap().edges);
    // The last day of a closed interval is still inside it.
    assert_eq!(resolved(&g, date("2015-12-31")).key, a.key);
}

#[test]
fn a_write_moves_the_version_and_the_next_lookup_sees_it() {
    let mut g = declared_wells();
    assert_eq!(well_count(&g, "2025-06-01"), Some(1));
    let before = resolved(&g, date("2025-06-01"));
    run(
        &mut g,
        "CREATE (:Well {id: 4, vf: date('2021-01-01'), vt: null})",
    );
    assert_eq!(well_count(&g, "2025-06-01"), Some(2));
    run(
        &mut g,
        "MATCH (w:Well {id: 3}) SET w.vt = date('2024-12-31')",
    );
    assert_eq!(well_count(&g, "2025-06-01"), Some(1));
    let after = resolved(&g, date("2025-06-01"));
    assert!(!after
        .masks
        .as_ref()
        .unwrap()
        .nodes
        .contains(well_slot(&g, 3)));
    assert!(after.masks.unwrap().nodes.contains(well_slot(&g, 4)));
    assert!(before.masks.unwrap().nodes.contains(well_slot(&g, 3)));
}

#[test]
fn a_fork_does_not_see_the_cache_its_origin_filled() {
    let g = declared_wells();
    assert_eq!(well_count(&g, "2005-06-01"), Some(1));
    assert!(read_cache(&g).is_some());
    let fork = g.clone();
    assert!(read_cache(&fork).is_none());
    assert_eq!(well_count(&fork, "2005-06-01"), Some(1));
}

#[test]
fn setting_the_version_directly_empties_the_cache() {
    let mut g = declared_wells();
    assert_eq!(well_count(&g, "2005-06-01"), Some(1));
    let version = g.version();
    g.set_version(version);
    assert!(read_cache(&g).is_none());
}

#[test]
fn an_unreadable_bound_leaves_the_target_to_property_guards_and_is_counted() {
    let mut g = declared_wells();
    run(&mut g, "MATCH (w:Well {id: 2}) SET w.vt = 2019");
    let config = well_config(&g);
    assert_eq!(
        index(&g, &node("Well"), &config).unwrap_err(),
        Unindexed::Unreadable
    );
    let counts = target_counts(&g, &node("Well"), &config);
    assert_eq!(counts.rows, 3);
    assert_eq!(counts.unreadable_rows, 1);
    assert_eq!(well_count(&g, "2005-06-01"), None);
    let r = resolved(&g, date("2005-06-01"));
    assert_eq!(r.guarded, vec![node("Well")]);
    assert!(!r.timeless);
    // The relationship target is still indexed.
    assert_eq!(r.key.len(), 1);
    let listed = list(&g);
    assert_eq!(listed[0].unreadable_rows, Some(1));
    assert_eq!(listed[0].empty_rows, Some(0));
}

#[test]
fn an_empty_row_written_after_the_declaration_is_counted_and_valid_nowhere() {
    let mut g = declared_wells();
    run(
        &mut g,
        "MATCH (w:Well {id: 1}) SET w.vt = date('1990-01-01')",
    );
    let listed = list(&g);
    assert_eq!(listed[0].target, node("Well"));
    assert_eq!(listed[0].empty_rows, Some(1));
    assert_eq!(listed[0].unreadable_rows, Some(0));
    assert_eq!(well_count(&g, "2005-06-01"), Some(0));
    let r = resolved(&g, date("1995-01-01"));
    assert!(!r.masks.unwrap().nodes.contains(well_slot(&g, 1)));
}

#[test]
fn a_build_over_the_byte_cap_is_refused_and_counted_all_the_same() {
    let g = declared_wells();
    set_byte_cap(&g, 16);
    let config = well_config(&g);
    assert_eq!(
        index(&g, &node("Well"), &config).unwrap_err(),
        Unindexed::OverBudget
    );
    assert_eq!(target_counts(&g, &node("Well"), &config).rows, 3);
    let r = resolved(&g, date("2005-06-01"));
    assert_eq!(r.guarded.len(), 2);
    assert!(r.masks.is_none());
    assert!(!r.timeless);
}

#[test]
fn masks_are_evicted_oldest_first_to_fit_the_cap() {
    let g = declared_wells();
    let first = resolved(&g, date("2005-06-01"));
    let arrays = read_cache(&g).as_ref().unwrap().array_bytes();
    let one_mask = first.masks.as_ref().unwrap().bytes();
    set_byte_cap(&g, arrays + one_mask);
    let second = resolved(&g, date("2016-06-01"));
    let cache = read_cache(&g);
    let cache = cache.as_ref().unwrap();
    assert_eq!(cache.masks.len(), 1);
    assert_eq!(cache.masks[0].0, second.key);
}

#[test]
fn timeless_holds_only_when_every_row_is_valid() {
    let mut g = graph(&[
        "CREATE (:Site {id: 1, vf: null, vt: null}), (:Site {id: 2, vf: date('2000-01-01'), vt: null}), \
         (:Site {id: 3, vf: date('1990-01-01'), vt: date('2100-01-01')})",
    ]);
    declare(&mut g, &node("Site"), "vf", "vt", HALF_OPEN).unwrap();
    let mut template = GuardTemplate::default();
    template.nodes.push(NodeGuard {
        label: "Site".into(),
        bounds: GuardBounds::of(g.temporal.node("Site").unwrap()),
    });
    let at = |t: &str| {
        GraphFilter {
            template: Arc::new(template.clone()),
            selector: ValidTimeSelector::AsOf(date(t)),
        }
        .resolve(&g)
    };
    let before = at("1999-12-31");
    assert!(!before.timeless);
    assert!(before.masks.is_some());
    let after = at("2000-01-01");
    assert!(after.timeless);
    assert!(after.masks.is_none());
    // A range selector resolves nothing from the index.
    let range = GraphFilter {
        template: Arc::new(template.clone()),
        selector: ValidTimeSelector::Overlap(date("2000-01-01"), date("2001-01-01")),
    }
    .resolve(&g);
    assert_eq!(range.guarded, vec![node("Site")]);
    assert!(!range.timeless);
}

#[test]
fn a_node_carrying_two_declared_labels_must_be_valid_under_both() {
    let mut g = graph(&[
        "CREATE (:Asset {id: 1, af: date('2000-01-01'), at: date('2010-12-31')})",
        "MATCH (a:Asset {id: 1}) SET a:Field, a.ff = date('2005-01-01'), a.ft = null",
    ]);
    declare(&mut g, &node("Asset"), "af", "at", CLOSED).unwrap();
    declare(&mut g, &node("Field"), "ff", "ft", CLOSED).unwrap();
    let mut template = GuardTemplate::default();
    for label in ["Asset", "Field"] {
        template.nodes.push(NodeGuard {
            label: label.into(),
            bounds: GuardBounds::of(g.temporal.node(label).unwrap()),
        });
    }
    let slot = g
        .type_indices
        .get("Asset")
        .unwrap()
        .iter()
        .next()
        .unwrap()
        .index();
    let valid = |t: &str| {
        GraphFilter {
            template: Arc::new(template.clone()),
            selector: ValidTimeSelector::AsOf(date(t)),
        }
        .resolve(&g)
        .masks
        .is_none_or(|m| m.nodes.contains(slot))
    };
    assert!(!valid("2003-01-01"), "Field has not started");
    assert!(valid("2007-01-01"));
    assert!(!valid("2012-01-01"), "Asset has ended");
}

/// Bytes the build can hold at its current capacities, by the bound the
/// module states: 48 per reserved pair slot, 8 per reserved empty slot.
fn working_bytes(builder: &Builder) -> usize {
    48 * builder.from.capacity().max(builder.to.capacity()) + 8 * builder.empty.capacity()
}

#[test]
fn the_build_is_refused_before_an_allocation_would_pass_its_budget() {
    let d = |n| Some(Instant::Date(day(n)));
    for (budget, every_nth_empty) in [(48 * 40 + 7, 0), (48 * 25 + 8 * 9, 3), (47, 0)] {
        let mut builder = Builder::new(CLOSED, budget);
        let mut accepted = 0;
        let refused_at = loop {
            let empty = every_nth_empty != 0 && accepted % every_nth_empty == 0;
            let capacities = (
                builder.from.capacity(),
                builder.to.capacity(),
                builder.empty.capacity(),
            );
            match builder.push(accepted, d(1), d(2), empty) {
                Ok(()) => {
                    accepted += 1;
                    assert!(
                        working_bytes(&builder) <= budget,
                        "{budget}: {accepted} rows"
                    );
                }
                Err(reason) => {
                    assert_eq!(reason, Unindexed::OverBudget);
                    // Refused before reserving anything more.
                    let after = (
                        builder.from.capacity(),
                        builder.to.capacity(),
                        builder.empty.capacity(),
                    );
                    assert_eq!(after, capacities, "{budget}");
                    break capacities;
                }
            }
        };
        // Not refused early: the next row could not have fit in any growth.
        let pairs = builder.from.len();
        let empties = builder.empty.len();
        let (pair_cap, empty_cap) = (refused_at.0, refused_at.2);
        let pair_full = pairs == pair_cap && 48 * (pair_cap + 1) + 8 * empty_cap > budget;
        let empty_full = empties == empty_cap && 48 * pair_cap + 8 * (empty_cap + 1) > budget;
        assert!(pair_full || empty_full, "{budget}: refused with room left");
        if accepted > 0 {
            assert!(builder.finish().bytes() <= budget);
        }
    }
}

#[test]
fn a_mask_that_cannot_fit_evicts_nothing_and_leaves_its_targets_guarded() {
    let g = declared_wells();
    let first = resolved(&g, date("2005-06-01"));
    let arrays = read_cache(&g).as_ref().unwrap().array_bytes();
    let one_mask = first.masks.as_ref().unwrap().bytes();
    set_byte_cap(&g, arrays + one_mask - 1);
    let second = resolved(&g, date("2016-06-01"));
    assert!(second.masks.is_none());
    assert_eq!(second.guarded, vec![node("Well"), licensed()]);
    assert!(!second.timeless);
    let cache = read_cache(&g);
    let cache = cache.as_ref().unwrap();
    assert_eq!(cache.masks.len(), 1, "the cached mask is kept");
    assert_eq!(cache.masks[0].0, first.key);
}

#[test]
fn the_duplicate_id_map_holds_only_repeated_ids_and_counts_against_the_cap() {
    let wells = declared_wells();
    assert_eq!(duplicate_ids(&wells, "Well").unwrap().bytes(), 0);
    let versions = &["CREATE (:M {id: 1}), (:M {id: 1}), (:M {id: 2}), (:M {id: 1})"];
    let g = graph(versions);
    let order = g.type_indices.get("M").unwrap().to_vec();
    let hit = g.lookup_by_id_readonly("M", &Value::Int64(1)).unwrap();
    assert_eq!(hit, order[3], "the id index keeps the last node");
    let map = duplicate_ids(&g, "M").unwrap();
    assert_eq!(
        map.latest_first(hit).collect::<Vec<_>>(),
        [order[3], order[1], order[0]]
    );
    assert_eq!(map.latest_first(order[2]).count(), 0);
    // Three entries in the first reserved capacity of sixteen.
    let bytes = 16 * crate::graph::features::temporal::duplicate_ids::ENTRY_BYTES;
    assert_eq!(map.bytes(), bytes);
    assert_eq!(read_cache(&g).as_ref().unwrap().array_bytes(), bytes);
    let capped = graph(versions);
    set_byte_cap(&capped, bytes - 1);
    assert!(duplicate_ids(&capped, "M").is_none());
}

// --- Pinned masks and covering keys (valid-time views) -----------------

/// Twelve one-year well versions (twelve segments of their own) and a
/// licence on each, so resolves at different years never share a key.
fn yearly_wells() -> DirGraph {
    let mut g = graph(&[
        "UNWIND range(0, 11) AS i CREATE (:Well {id: i, \
         vf: date({year: 2000 + i, month: 1, day: 1}), \
         vt: date({year: 2000 + i, month: 12, day: 31})})",
        "MATCH (w:Well) CREATE (w)-[:LICENSED {lf: date('2003-01-01'), lt: date('2040-01-01')}]->(:Company {id: w.id})",
    ]);
    declare(&mut g, &node("Well"), "vf", "vt", CLOSED).unwrap();
    declare(&mut g, &licensed(), "lf", "lt", CLOSED).unwrap();
    g
}

/// `template(g)` without its relationship target: the key of a query that
/// reaches wells only.
fn wells_only(g: &DirGraph) -> GuardTemplate {
    let mut template = template(g);
    template.edges.clear();
    template
}

fn resolve_with(g: &DirGraph, template: &GuardTemplate, year: i32) -> ResolvedFilter {
    GraphFilter {
        template: Arc::new(template.clone()),
        selector: ValidTimeSelector::AsOf(Instant::Date(
            NaiveDate::from_ymd_opt(year, 6, 1).unwrap(),
        )),
    }
    .resolve(g)
}

#[test]
fn a_covering_key_serves_a_query_without_a_build() {
    let g = yearly_wells();
    let view = resolve_with(&g, &template(&g), 2005);
    let view_masks = view.masks.clone().unwrap();
    assert_eq!(view.key.len(), 2);
    let query = resolve_with(&g, &wells_only(&g), 2005);
    assert_eq!(query.key.len(), 1);
    assert!(is_covered_by(&query.key, &view.key));
    assert!(!is_covered_by(&view.key, &query.key));
    assert!(Arc::ptr_eq(query.masks.as_ref().unwrap(), &view_masks));
    assert_eq!(
        read_cache(&g).as_ref().unwrap().masks.len(),
        1,
        "nothing built"
    );
    // A key in another segment is not covered.
    let other = resolve_with(&g, &wells_only(&g), 2006);
    assert!(!Arc::ptr_eq(other.masks.as_ref().unwrap(), &view_masks));
}

#[test]
fn a_pinned_mask_outlives_the_lru_and_is_counted_once() {
    let g = yearly_wells();
    let view = resolve_with(&g, &template(&g), 2005);
    let pinned = view.masks.clone().unwrap();
    pin_masks(&g, &view.key, &pinned);
    // Nine resolves at other years overflow the eight-entry LRU.
    for year in (2000..2012).filter(|&y| y != 2005).take(9) {
        resolve_with(&g, &template(&g), year);
    }
    {
        let cache = read_cache(&g);
        let cache = cache.as_ref().unwrap();
        assert_eq!(cache.masks.len(), MAX_CACHED_MASKS);
        assert!(!cache.masks.iter().any(|(_, m)| Arc::ptr_eq(m, &pinned)));
        let lru: usize = cache.masks.iter().map(|(_, m)| m.bytes()).sum();
        assert_eq!(
            cache.mask_bytes(),
            lru + pinned.bytes(),
            "the pin counted once"
        );
    }
    // A query the view's key covers is served the pinned masks, not a build.
    let query = resolve_with(&g, &wells_only(&g), 2005);
    assert!(Arc::ptr_eq(query.masks.as_ref().unwrap(), &pinned));
    let same = resolve_with(&g, &template(&g), 2005);
    assert!(Arc::ptr_eq(same.masks.as_ref().unwrap(), &pinned));
    // Pinning the same masks twice registers them once.
    pin_masks(&g, &view.key, &pinned);
    assert_eq!(read_cache(&g).as_ref().unwrap().pinned.len(), 1);
    // Once the view lets go, the pin is gone and a resolve builds afresh.
    let weak = Arc::downgrade(&pinned);
    drop((view, pinned, query, same));
    assert!(weak.upgrade().is_none());
    let rebuilt = resolve_with(&g, &wells_only(&g), 2005);
    assert!(rebuilt.masks.is_some());
    assert_eq!(read_cache(&g).as_ref().unwrap().mask_bytes() % 8, 0);
}

#[test]
fn a_write_drops_the_pins_with_the_masks() {
    let mut g = yearly_wells();
    let view = resolve_with(&g, &template(&g), 2005);
    let pinned = view.masks.clone().unwrap();
    pin_masks(&g, &view.key, &pinned);
    run(
        &mut g,
        "MATCH (w:Well {id: 5}) SET w.vt = date('2005-03-01')",
    );
    let after = resolve_with(&g, &wells_only(&g), 2005);
    assert!(!Arc::ptr_eq(after.masks.as_ref().unwrap(), &pinned));
    assert!(read_cache(&g).as_ref().unwrap().pinned.is_empty());
    // The held masks still answer for the state they were built on.
    assert!(pinned.nodes.contains(well_slot(&g, 5)));
    assert!(!after.masks.unwrap().nodes.contains(well_slot(&g, 5)));
}
