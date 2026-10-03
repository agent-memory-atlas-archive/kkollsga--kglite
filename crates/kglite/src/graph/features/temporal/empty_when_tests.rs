//! `empty_when: to_before_from` under `closed`: a `to` the day before the
//! `from` is kept as an empty interval — valid on no day — where `closed`
//! otherwise refuses an inverted row.

use std::collections::HashMap;

use super::declarations::{declare, list, TemporalTarget};
use super::endpoint_index::node_count_at;
use super::eval::{EmptyWhen, Instant, IntervalConvention};
use super::loader::declare_defaulted;
use super::{declare_loaded_with, EmptyWhen::ToBeforeFrom};
use crate::datatypes::Value;
use crate::graph::dir_graph::DirGraph;
use crate::graph::session::execute::{execute_mut, ExecuteOptions};

fn run(graph: &mut DirGraph, query: &str) -> Result<Vec<String>, String> {
    let params: HashMap<String, Value> = HashMap::new();
    execute_mut(graph, query, &ExecuteOptions::eager(&params))
        .map(|r| r.result.diagnostics.map(|d| d.warnings).unwrap_or_default())
        .map_err(|e| e.to_string())
}

fn span() -> TemporalTarget {
    TemporalTarget::Node("Span".into())
}

fn at(text: &str) -> Instant {
    Instant::Date(chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap())
}

/// A normal span, a one-day span, and one superseded the day it was
/// registered (`to` = the day before `from`).
const SPANS: &str = "CREATE (:Span {id: 1, vf: '2010-01-01', vt: '2010-12-31'}), \
                     (:Span {id: 2, vf: '2011-03-05', vt: '2011-03-05'}), \
                     (:Span {id: 3, vf: '2011-06-10', vt: '2011-06-09'})";

fn declared_over_rows() -> (DirGraph, Option<String>) {
    let mut g = DirGraph::new();
    run(&mut g, SPANS).unwrap();
    let report = declare_loaded_with(
        &mut g,
        &span(),
        ("vf", "vt", IntervalConvention::Closed, Some(ToBeforeFrom)),
        &[],
    )
    .unwrap();
    (g, report.warning)
}

#[test]
fn a_declaration_keeps_to_before_from_rows_counts_and_warns() {
    let (g, warning) = declared_over_rows();
    let warning = warning.expect("an empty row earns a warning");
    assert!(
        warning.starts_with(
            "1 of 3 rows of node label 'Span' have an empty interval under convention \
             'closed' with empty_when 'to_before_from'"
        ),
        "{warning}"
    );
    assert!(warning.contains("the first is node '3'"), "{warning}");
    let listed = list(&g);
    assert_eq!(listed[0].config.empty_when, Some(EmptyWhen::ToBeforeFrom));
    assert_eq!(listed[0].empty_rows, Some(1));
}

#[test]
fn the_empty_row_is_valid_on_no_day_and_outside_count_at() {
    let (g, _) = declared_over_rows();
    // Around the empty row's own days the count only holds the other rows.
    for day in ["2011-06-09", "2011-06-10", "2011-06-11"] {
        assert_eq!(node_count_at(&g, "Span", at(day)), Some(0), "{day}");
    }
    assert_eq!(node_count_at(&g, "Span", at("2010-06-01")), Some(1));
    assert_eq!(node_count_at(&g, "Span", at("2011-03-05")), Some(1));
    // `valid_at` agrees with the index, per element.
    let mut g = g;
    let params: HashMap<String, Value> = HashMap::new();
    for day in ["2011-06-09", "2011-06-10"] {
        let result = execute_mut(
            &mut g,
            &format!("MATCH (s:Span {{id: 3}}) RETURN valid_at(s, date('{day}')) AS v"),
            &ExecuteOptions::eager(&params),
        )
        .unwrap();
        assert_eq!(
            result.result.rows[0][0],
            Value::Boolean(false),
            "valid_at on {day}"
        );
    }
}

#[test]
fn a_write_keeps_to_before_from_with_the_closed_wording() {
    let (mut g, _) = declared_over_rows();
    let warnings = run(
        &mut g,
        "CREATE (:Span {id: 4, vf: '2012-01-02', vt: '2012-01-01'}), \
         (:Span {id: 5, vf: '2012-02-01', vt: '2012-02-28'})",
    )
    .unwrap();
    let empty: Vec<&String> = warnings
        .iter()
        .filter(|w| w.contains("empty interval"))
        .collect();
    assert_eq!(empty.len(), 1, "{warnings:?}");
    assert!(
        empty[0].starts_with(
            "1 of 2 rows written have an empty interval under convention 'closed' with \
             empty_when 'to_before_from'"
        ),
        "{}",
        empty[0]
    );
    assert_eq!(list(&g)[0].empty_rows, Some(2));
}

#[test]
fn closed_without_the_option_still_refuses_the_inversion() {
    let mut g = DirGraph::new();
    run(&mut g, SPANS).unwrap();
    let err = declare(&mut g, &span(), "vf", "vt", IntervalConvention::Closed).unwrap_err();
    assert!(err.contains("is after the to bound"), "{err}");
    assert!(!err.contains("empty_when"), "{err}");
    assert!(list(&g).is_empty());

    let mut g = DirGraph::new();
    run(
        &mut g,
        "CREATE (:Span {id: 1, vf: '2010-01-01', vt: '2010-12-31'})",
    )
    .unwrap();
    declare(&mut g, &span(), "vf", "vt", IntervalConvention::Closed).unwrap();
    let err = run(
        &mut g,
        "CREATE (:Span {id: 3, vf: '2011-06-10', vt: '2011-06-09'})",
    )
    .unwrap_err();
    assert!(err.contains("is after the to bound"), "{err}");
}

#[test]
fn only_a_one_day_date_inversion_is_accepted() {
    let mut g = DirGraph::new();
    run(
        &mut g,
        "CREATE (:Span {id: 1, vf: '2010-01-01', vt: '2010-12-31'})",
    )
    .unwrap();
    declare_loaded_with(
        &mut g,
        &span(),
        ("vf", "vt", IntervalConvention::Closed, Some(ToBeforeFrom)),
        &[],
    )
    .unwrap();
    // Two days before: refused, and the option is named in the message.
    let err = run(
        &mut g,
        "CREATE (:Span {id: 2, vf: '2011-06-10', vt: '2011-06-08'})",
    )
    .unwrap_err();
    assert!(err.contains("is after the to bound"), "{err}");
    assert!(err.contains("empty_when 'to_before_from'"), "{err}");
    // A timestamp bound on either side: refused, even a day apart.
    for (vf, vt) in [
        ("datetime('2011-06-10T00:00:00')", "date('2011-06-09')"),
        ("date('2011-06-10')", "datetime('2011-06-09T23:59:59')"),
        (
            "datetime('2011-06-10T08:00:00')",
            "datetime('2011-06-09T08:00:00')",
        ),
    ] {
        let err = run(
            &mut g,
            &format!("CREATE (:Span {{id: 9, vf: {vf}, vt: {vt}}})"),
        )
        .unwrap_err();
        assert!(err.contains("is after the to bound"), "{vf} {vt}: {err}");
    }
    // The valid shape in a Cypher date value is accepted.
    run(
        &mut g,
        "CREATE (:Span {id: 3, vf: date('2011-06-10'), vt: date('2011-06-09')})",
    )
    .unwrap();
}

#[test]
fn half_open_with_the_option_is_refused_and_stores_nothing() {
    let mut g = DirGraph::new();
    run(
        &mut g,
        "CREATE (:Span {id: 1, vf: '2010-01-01', vt: '2010-12-31'})",
    )
    .unwrap();
    let err = declare_loaded_with(
        &mut g,
        &span(),
        ("vf", "vt", IntervalConvention::HalfOpen, Some(ToBeforeFrom)),
        &[],
    )
    .unwrap_err();
    assert!(err.contains("applies to convention 'closed'"), "{err}");
    assert!(err.contains("needs no option"), "{err}");
    assert!(list(&g).is_empty());
}

#[test]
fn redeclaring_keeps_the_option_and_a_different_form_conflicts() {
    let (mut g, _) = declared_over_rows();
    // No convention and no option named: the declaration's own form stands.
    let report = declare_defaulted(&mut g, &span(), "vf", "vt", (None, None)).unwrap();
    assert!(!report.changed);
    // The same form named again is a no-op.
    let report = declare_defaulted(
        &mut g,
        &span(),
        "vf",
        "vt",
        (Some(IntervalConvention::Closed), Some(ToBeforeFrom)),
    )
    .unwrap();
    assert!(!report.changed);
    // Naming the convention alone drops the option: a different declaration.
    let err = declare_defaulted(
        &mut g,
        &span(),
        "vf",
        "vt",
        (Some(IntervalConvention::Closed), None),
    )
    .unwrap_err();
    assert!(err.contains("already declared"), "{err}");
    assert!(err.contains("empty_when 'to_before_from'"), "{err}");
}
