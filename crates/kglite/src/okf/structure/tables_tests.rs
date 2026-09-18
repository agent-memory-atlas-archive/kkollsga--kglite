//! One fixture per rule in VAULT.md §7.1 `tables:`, plus the shapes a real
//! converted corpus writes — a blank header row, a picture in a cell, and
//! Obsidian's `\|` cell escape.

use super::super::derive::{derive, Derived};
use super::super::profile::{SectionRule, StructureProfile, TableRule};
use crate::datatypes::values::Value;
use crate::okf::structure::block::parse_blocks;

fn sections_rule() -> SectionRule {
    SectionRule {
        label: "Section".to_string(),
        edge: "HAS_SECTION".to_string(),
        parent: "PARENT_SECTION".to_string(),
        next: "NEXT_SECTION".to_string(),
    }
}

/// Build the rules through the config parser, so every `under_heading:` is
/// compiled exactly as a vault's would be and the two forms' refusals are the
/// ones a vault meets.
fn rules(yaml: &str) -> Vec<TableRule> {
    let value = crate::okf::frontmatter::parse_yaml(yaml).expect("the fixture is YAML");
    super::super::profile::parse(&value)
        .expect("the rules the parser accepts")
        .tables
}

fn profile(yaml: &str) -> StructureProfile {
    StructureProfile {
        sections: Some(sections_rule()),
        tables: rules(yaml),
        ..StructureProfile::default()
    }
}

fn run(body: &str, profile: &StructureProfile) -> Derived {
    derive(body, &parse_blocks(body), "The Note", "Article", profile)
}

/// `(suffix, label)` of everything but the sections.
fn derived(d: &Derived) -> Vec<(&str, &str)> {
    d.nodes
        .iter()
        .filter(|n| n.label != "Section")
        .map(|n| (n.suffix.as_str(), n.label.as_str()))
        .collect()
}

/// One node's properties as `(name, rendered value)`, in declaration order.
fn props_of(d: &Derived, suffix: &str) -> Vec<(String, String)> {
    d.nodes
        .iter()
        .find(|n| n.suffix == suffix)
        .unwrap_or_else(|| panic!("no node `{suffix}` in {:?}", derived(d)))
        .props
        .iter()
        .map(|(k, v)| (k.clone(), rendered(v)))
        .collect()
}

fn rendered(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => crate::datatypes::values::raw_string(other),
    }
}

/// `(target, [(property, value)])` per edge-table link, in row order.
fn links(d: &Derived) -> Vec<(String, Vec<(String, String)>)> {
    d.links
        .iter()
        .map(|link| {
            (
                link.target.clone(),
                link.props
                    .iter()
                    .map(|(k, v)| (k.clone(), rendered(v)))
                    .collect(),
            )
        })
        .collect()
}

const PARAMETERS: &str = "\
# Api

## Parameters

| name | type | required |
|---|---|---|
| path | string | true |
| mode | string | |
";

#[test]
fn a_row_is_a_node_keyed_by_the_declared_key_column() {
    let d = run(
        PARAMETERS,
        &profile(
            "tables:\n  - {under_heading: '^Parameters$', label: ApiParameter, \
             key_column: name, edge: HAS_PARAMETER}\n",
        ),
    );
    assert_eq!(
        derived(&d),
        vec![
            ("#Api#Parameters~path", "ApiParameter"),
            ("#Api#Parameters~mode", "ApiParameter"),
        ]
    );
    // The key column is a property like every other one, and an empty cell
    // writes none at all (VAULT.md §7.1).
    assert_eq!(
        props_of(&d, "#Api#Parameters~path"),
        vec![
            ("name".to_string(), "path".to_string()),
            ("type".to_string(), "string".to_string()),
            ("required".to_string(), "true".to_string()),
        ]
    );
    assert_eq!(
        props_of(&d, "#Api#Parameters~mode"),
        vec![
            ("name".to_string(), "mode".to_string()),
            ("type".to_string(), "string".to_string()),
        ]
    );
    let edges: Vec<(&str, &str)> = d
        .edges
        .iter()
        .filter(|e| e.conn_type == "HAS_PARAMETER")
        .map(|e| (e.source.as_deref().unwrap_or(""), e.target.as_str()))
        .collect();
    assert_eq!(
        edges,
        vec![
            ("#Api#Parameters", "#Api#Parameters~path"),
            ("#Api#Parameters", "#Api#Parameters~mode"),
        ]
    );
    assert!(d.warnings.is_empty(), "{:?}", d.warnings);
}

#[test]
fn the_first_column_keys_a_row_when_no_key_column_is_declared() {
    let d = run(
        PARAMETERS,
        &profile("tables:\n  - {under_heading: 'Parameters', label: Row}\n"),
    );
    assert_eq!(
        derived(&d),
        vec![
            ("#Api#Parameters~path", "Row"),
            ("#Api#Parameters~mode", "Row"),
        ]
    );
    // An undeclared `edge:` is spelled from the label, as `ordered_lists:`
    // spells its container's.
    assert!(d.edges.iter().any(|e| e.conn_type == "HAS_ROW"));
}

#[test]
fn an_empty_or_repeated_key_falls_back_to_the_row_number() {
    let body = "\
# Api

## Parameters

| name | type |
|---|---|
| path | string |
| path | int |
|  | bool |
";
    let d = run(
        body,
        &profile("tables:\n  - {under_heading: 'Parameters', label: Row, key_column: name}\n"),
    );
    assert_eq!(
        derived(&d),
        vec![
            ("#Api#Parameters~path", "Row"),
            ("#Api#Parameters~row2", "Row"),
            ("#Api#Parameters~row3", "Row"),
        ]
    );
    assert_eq!(
        d.warnings,
        vec![
            "table under `Parameters` repeats the key `path`; row 2 keys on its position \
             instead (`~row2`)"
                .to_string(),
            "table under `Parameters` has an empty key in row 3; it keys on its position \
             instead (`~row3`)"
                .to_string(),
        ]
    );
}

#[test]
fn a_key_column_the_table_does_not_carry_warns_and_keys_on_the_first() {
    let d = run(
        PARAMETERS,
        &profile("tables:\n  - {under_heading: 'Parameters', label: Row, key_column: parameter}\n"),
    );
    assert_eq!(derived(&d)[0], ("#Api#Parameters~path", "Row"));
    assert_eq!(
        d.warnings,
        vec![
            "table under `Parameters` has no column `parameter`; its rows key on the first \
             column"
                .to_string()
        ]
    );
}

/// 223 of one converted corpus's 1 089 tables carry no `<th>`, so the
/// converter writes a blank header row: GFM has no headerless table. A blank
/// header names no property rather than inventing a positional one — and the
/// row still exists, keyed by its first cell.
#[test]
fn a_blank_header_names_no_column_and_its_cells_are_dropped() {
    let body = "\
# Petrel

## Properties

|  |  |
|---|---|
| Zone | The stratigraphic interval |
| Facies | ![icon](img/facies.png) |
";
    let d = run(
        body,
        &profile("tables:\n  - {under_heading: 'Properties', label: Row}\n"),
    );
    assert_eq!(
        derived(&d),
        vec![
            ("#Petrel#Properties~Zone", "Row"),
            ("#Petrel#Properties~Facies", "Row"),
        ]
    );
    assert_eq!(props_of(&d, "#Petrel#Properties~Zone"), vec![]);
    // A picture in a cell is the note's attachment reference (VAULT.md §6.1),
    // scanned as prose — the row rule neither reads it nor swallows it.
    assert!(d.links.is_empty());
}

#[test]
fn a_table_under_no_matching_heading_is_prose() {
    let body = "\
# Api

## Returns

| name | type |
|---|---|
| path | string |
";
    let d = run(
        body,
        &profile("tables:\n  - {under_heading: '^Parameters$', label: Row}\n"),
    );
    assert_eq!(derived(&d), vec![]);
}

#[test]
fn the_first_rule_that_matches_reads_the_table() {
    let d = run(
        PARAMETERS,
        &profile(
            "tables:\n  - {under_heading: '^Parameters$', label: Narrow}\n  \
             - {under_heading: 'Param', label: Broad}\n",
        ),
    );
    assert_eq!(
        derived(&d)
            .iter()
            .map(|(_, label)| *label)
            .collect::<Vec<_>>(),
        vec!["Narrow", "Narrow"]
    );
}

// ---------------------------------------------------------------------------
// `edges: true`
// ---------------------------------------------------------------------------

const WORKED_AT: &str = "\
# People

## Worked at

| company | role | from |
|---|---|---|
| [[acme]] | author | 2024 |
| [[globex#Team]] | editor | |
";

#[test]
fn an_edge_table_states_one_edge_per_row_and_no_node() {
    let d = run(
        WORKED_AT,
        &profile("tables:\n  - {under_heading: 'Worked at', edge: WORKED_AT, edges: true}\n"),
    );
    assert_eq!(derived(&d), vec![], "a row states an edge, not a node");
    assert_eq!(
        links(&d),
        vec![
            (
                "acme".to_string(),
                vec![
                    ("section".to_string(), "Worked at".to_string()),
                    ("row".to_string(), "1".to_string()),
                    ("role".to_string(), "author".to_string()),
                    ("from".to_string(), "2024".to_string()),
                ]
            ),
            (
                "globex".to_string(),
                vec![
                    ("section".to_string(), "Worked at".to_string()),
                    // The fragment is the edge's anchor, as a prose link's is,
                    // and never part of the target (VAULT.md §5.4).
                    ("anchor".to_string(), "Team".to_string()),
                    ("row".to_string(), "2".to_string()),
                    ("role".to_string(), "editor".to_string()),
                ]
            ),
        ]
    );
    assert_eq!(
        d.edge_tables_hit.iter().collect::<Vec<_>>(),
        vec!["WORKED_AT"]
    );
}

#[test]
fn the_target_column_is_the_first_holding_a_wikilink_or_the_one_declared() {
    // `company` holds the links, though it is not the first column here.
    let body = "\
# People

## Worked at

| role | company |
|---|---|
| author | [[acme]] |
";
    let d = run(
        body,
        &profile("tables:\n  - {under_heading: 'Worked at', edge: WORKED_AT, edges: true}\n"),
    );
    assert_eq!(links(&d)[0].0, "acme");
    // A declared `key_column:` overrides it, and a plain cell is a name the
    // resolver ladder reads like any other (VAULT.md §5.2).
    let d = run(
        body,
        &profile(
            "tables:\n  - {under_heading: 'Worked at', edge: WORKED_AT, edges: true, \
             key_column: role}\n",
        ),
    );
    assert_eq!(links(&d)[0].0, "author");
}

#[test]
fn an_edge_table_with_no_target_column_warns_and_states_nothing() {
    let body = "\
# People

## Worked at

| role | from |
|---|---|
| author | 2024 |
";
    let d = run(
        body,
        &profile("tables:\n  - {under_heading: 'Worked at', edge: WORKED_AT, edges: true}\n"),
    );
    assert!(d.links.is_empty());
    assert_eq!(
        d.warnings,
        vec![
            "edge table under `Worked at` names no target column: declare `key_column:`, \
             or write the targets as `[[wikilinks]]`"
                .to_string()
        ]
    );
}

#[test]
fn a_column_named_like_the_links_own_property_is_dropped_with_a_warning() {
    let body = "\
# People

## Worked at

| company | section |
|---|---|
| [[acme]] | Engineering |
";
    let d = run(
        body,
        &profile("tables:\n  - {under_heading: 'Worked at', edge: WORKED_AT, edges: true}\n"),
    );
    assert_eq!(
        links(&d)[0].1,
        vec![
            ("section".to_string(), "Worked at".to_string()),
            ("row".to_string(), "1".to_string()),
        ]
    );
    assert_eq!(
        d.warnings,
        vec![
            "edge table under `Worked at` has a column `section`, which is the edge property \
             the link itself carries; the column is dropped"
                .to_string()
        ]
    );
}

/// Obsidian's documented spelling for a wikilink inside a table cell. Before
/// this was read, the target was a note named `Usage\` — 398 dangling links on
/// one converted corpus — and the display text was dropped with it.
#[test]
fn an_escaped_pipe_in_a_cell_names_the_note_and_keeps_its_display_text() {
    let body = "\
# Guides

## Worked at

| company | note |
|---|---|
| [[acme\\|Acme Corp]] | a \\| b |
";
    let d = run(
        body,
        &profile("tables:\n  - {under_heading: 'Worked at', edge: WORKED_AT, edges: true}\n"),
    );
    assert_eq!(
        links(&d),
        vec![(
            "acme".to_string(),
            vec![
                ("section".to_string(), "Worked at".to_string()),
                ("label".to_string(), "Acme Corp".to_string()),
                ("row".to_string(), "1".to_string()),
                // A cell's own `\|` is the pipe the author meant, in a
                // property as much as in a link.
                ("note".to_string(), "a | b".to_string()),
            ]
        )]
    );
}

#[test]
fn a_node_rows_cells_are_unescaped_too() {
    let body = "\
# Guides

## Parameters

| name | note |
|---|---|
| a \\| b | see [[chunky\\|the guide]] |
";
    let d = run(
        body,
        &profile("tables:\n  - {under_heading: 'Parameters', label: Row, key_column: name}\n"),
    );
    assert_eq!(derived(&d), vec![("#Guides#Parameters~a | b", "Row")]);
    assert_eq!(
        props_of(&d, "#Guides#Parameters~a | b"),
        vec![
            ("name".to_string(), "a | b".to_string()),
            ("note".to_string(), "see [[chunky|the guide]]".to_string()),
        ]
    );
}

/// `![[diagram.png]]` is an attachment, not a link (VAULT.md §6.1), so a
/// column of pictures is not the column a row's target lives in — reading one
/// as a target would mint an edge to a note named after an image file.
#[test]
fn a_column_of_embeds_is_not_the_target_column() {
    let body = "\
# People

## Worked at

| picture | company |
|---|---|
| ![[acme-logo.png]] | [[acme]] |
";
    let d = run(
        body,
        &profile("tables:\n  - {under_heading: 'Worked at', edge: WORKED_AT, edges: true}\n"),
    );
    assert_eq!(links(&d)[0].0, "acme");
}
