//! The table one declared edge type writes, and where it goes (VAULT.md §10.6).

use super::*;

fn row(target: &str, props: &[(&str, Value)]) -> Row {
    Row::new(
        target.to_string(),
        &props
            .iter()
            .map(|(name, value)| (name.to_string(), value.clone()))
            .collect::<Vec<_>>(),
    )
}

/// The shape a reader has to be able to read back: the target as a wikilink
/// carrying its display text, one column per other property, and the four the
/// table itself implies left out.
#[test]
fn a_row_writes_its_target_and_every_column_but_the_implied_four() {
    let rows = vec![
        row(
            "chunky",
            &[
                ("section", Value::String("Worked on by".into())),
                ("label", Value::String("The chunky note".into())),
                ("row", Value::Int64(1)),
                ("role", Value::String("author".into())),
                ("since", Value::String("2024".into())),
            ],
        ),
        row(
            "nobody",
            &[
                ("section", Value::String("Worked on by".into())),
                ("row", Value::Int64(2)),
                ("role", Value::String("reviewer".into())),
                ("since", Value::String("2025".into())),
            ],
        ),
    ];
    assert_eq!(
        render("person", &rows),
        "| person | role | since |\n\
         | --- | --- | --- |\n\
         | [[chunky\\|The chunky note]] | author | 2024 |\n\
         | [[nobody]] | reviewer | 2025 |"
    );
}

/// `row` is the order the author's own table had, so it is the order the
/// export writes back — not the order the graph happens to hold the edges in.
#[test]
fn rows_come_back_in_the_order_the_row_property_names() {
    let rows = vec![
        row("zulu", &[("row", Value::Int64(2))]),
        row("alpha", &[("row", Value::Int64(3))]),
        row("mike", &[("row", Value::Int64(1))]),
    ];
    let table = render("target", &rows);
    let targets: Vec<&str> = table.lines().skip(2).map(|line| line.trim()).collect();
    assert_eq!(
        targets,
        vec!["| [[mike]] |", "| [[zulu]] |", "| [[alpha]] |"]
    );
}

/// A graph that was never a vault carries no `row`, and two exports of it still
/// have to write the same bytes (§10.8).
#[test]
fn rows_without_an_order_sort_by_target_and_then_by_their_columns() {
    let rows = vec![
        row("beta", &[("role", Value::String("second".into()))]),
        row("alpha", &[("role", Value::String("later".into()))]),
        row("alpha", &[("role", Value::String("earlier".into()))]),
    ];
    assert_eq!(
        render("target", &rows),
        "| target | role |\n\
         | --- | --- |\n\
         | [[alpha]] | earlier |\n\
         | [[alpha]] | later |\n\
         | [[beta]] | second |"
    );
}

/// A cell is one line and a literal `|` in it is `\|` — the same escape the
/// reader unescapes (VAULT.md §5.1). A property no row carries is an empty
/// cell, not a missing column.
#[test]
fn a_cell_escapes_its_pipes_and_flattens_its_newlines() {
    let rows = vec![
        row(
            "alpha",
            &[
                ("row", Value::Int64(1)),
                ("note", Value::String("a | b".into())),
                ("lines", Value::String("one\ntwo".into())),
            ],
        ),
        row("beta", &[("row", Value::Int64(2))]),
    ];
    assert_eq!(
        render("target", &rows),
        "| target | lines | note |\n\
         | --- | --- | --- |\n\
         | [[alpha]] | one two | a \\| b |\n\
         | [[beta]] |  |  |"
    );
}

/// A cell is text and `types:` names node labels, so a non-string property
/// comes back a string — written down here rather than discovered later.
#[test]
fn a_non_string_property_is_written_as_its_text() {
    let rows = vec![row(
        "alpha",
        &[("weight", Value::Int64(3)), ("keep", Value::Boolean(true))],
    )];
    assert_eq!(
        render("target", &rows),
        "| target | keep | weight |\n| --- | --- | --- |\n| [[alpha]] | true | 3 |"
    );
}

/// The anchor rides the wikilink, because that is where the reader takes it
/// from — a fragment is not a column.
#[test]
fn an_anchored_target_keeps_its_fragment_in_the_link() {
    let rows = vec![row(
        "chunky",
        &[
            ("anchor", Value::String("Deep dive".into())),
            ("label", Value::String("there".into())),
        ],
    )];
    assert_eq!(
        render("target", &rows),
        "| target |\n| --- |\n| [[chunky#Deep dive\\|there]] |"
    );
}

const BODY_WITH_TABLE: &str = "# Note\n\nProse above.\n\n\
     ## Worked on by\n\n\
     | person | role |\n|---|---|\n| [[old]] | author |\n\n\
     ## After\n\nProse below.\n";

/// The fixed point §10.6 turns on: the declared heading's first table is the
/// exporter's, so a second export replaces it instead of appending another —
/// and keeps the name the author gave its first column.
#[test]
fn the_declared_headings_first_table_is_replaced_not_appended() {
    let rows = vec![row("new", &[("role", Value::String("editor".into()))])];
    let once = apply(BODY_WITH_TABLE, &[("Worked on by", rows.clone())]);
    assert_eq!(
        once,
        "# Note\n\nProse above.\n\n\
         ## Worked on by\n\n\
         | person | role |\n| --- | --- |\n| [[new]] | editor |\n\n\
         ## After\n\nProse below.\n",
        "the author's column name survives; their rows do not"
    );
    assert_eq!(
        apply(&once, &[("Worked on by", rows)]),
        once,
        "and the second pass is the fixed point"
    );
}

/// The heading is matched on its text at any level — a vault writing
/// `### Worked on by` under a symbol heading declares the text, not the level.
#[test]
fn the_heading_is_matched_at_whatever_level_it_was_written() {
    let body = "# Api\n\n## Symbol\n\n### Worked on by\n\n| p |\n|---|\n| [[old]] |\n";
    assert_eq!(
        apply(body, &[("Worked on by", vec![row("new", &[])])]),
        "# Api\n\n## Symbol\n\n### Worked on by\n\n| p |\n| --- |\n| [[new]] |\n"
    );
}

/// A table under a *nested* heading belongs to that heading. Writing into it
/// would rewrite prose the vault never declared.
#[test]
fn a_table_under_a_nested_heading_is_not_the_declared_ones() {
    let body = "## Worked on by\n\nIntro.\n\n### Details\n\n| a |\n|---|\n| [[x]] |\n";
    assert_eq!(
        apply(body, &[("Worked on by", vec![row("new", &[])])]),
        "## Worked on by\n\nIntro.\n\n| target |\n| --- |\n| [[new]] |\n\n\
         ### Details\n\n| a |\n|---|\n| [[x]] |\n",
        "the table goes at the end of what the declared heading itself holds"
    );
}

/// No such heading: the export appends one, which §10.5 permits for exactly
/// this — the vault asked for it by name.
#[test]
fn a_missing_heading_is_appended_with_its_table() {
    let body = "Just prose.\n";
    assert_eq!(
        apply(body, &[("Worked on by", vec![row("new", &[])])]),
        "Just prose.\n\n## Worked on by\n\n| target |\n| --- |\n| [[new]] |\n"
    );
}

/// A note with no body at all still gets the table, and no leading blank line.
#[test]
fn an_empty_body_gets_the_heading_alone() {
    assert_eq!(
        apply("", &[("Worked on by", vec![row("new", &[])])]),
        "## Worked on by\n\n| target |\n| --- |\n| [[new]] |\n"
    );
}

/// The edges are gone from the graph, so the table has to go with them: leaving
/// it would make them again on the next import.
#[test]
fn a_type_with_no_rows_removes_the_table_the_export_owns() {
    assert_eq!(
        apply(BODY_WITH_TABLE, &[("Worked on by", Vec::new())]),
        "# Note\n\nProse above.\n\n\
         ## Worked on by\n\n\
         ## After\n\nProse below.\n",
        "the heading and the prose around it stay; the table does not"
    );
}

/// And a heading that never had one is left exactly as it is.
#[test]
fn a_type_with_no_rows_appends_nothing() {
    let body = "# Note\n\nProse.\n";
    assert_eq!(apply(body, &[("Worked on by", Vec::new())]), body);
}

/// What [`super::declared_tables`] asks of the body when it decides which edges
/// the note already states: the exporter's own region is not the note's word.
#[test]
fn the_prose_outside_the_owned_table_excludes_it() {
    let prose = prose_outside_owned_tables(BODY_WITH_TABLE, &["Worked on by"]);
    assert!(!prose.contains("[[old]]"), "{prose}");
    assert!(prose.contains("Prose above."), "{prose}");
    assert!(prose.contains("Prose below."), "{prose}");
}

/// A heading with no table of its own leaves the body whole — there is no
/// region to exclude, and cutting the prose under it would hide real links, so
/// an edge the author stated there would be written as a row as well.
///
/// The fixture carries a heading *after* the declared one on purpose: with
/// nothing following it, a rule that cut from the heading to the end of the
/// body would cut nothing at all and the assertion could not tell the two
/// apart.
#[test]
fn prose_outside_the_owned_table_keeps_a_heading_that_carries_none() {
    let body = "## Worked on by\n\n[[stated]] in prose.\n\n## After\n\n[[later]] too.\n";
    assert_eq!(prose_outside_owned_tables(body, &["Worked on by"]), body);
}
