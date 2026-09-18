//! Declared edge tables — an edge's properties written back as prose
//! (VAULT.md §7.3, §10.6).
//!
//! A frontmatter list carries targets and nothing else, so every property an
//! edge carries is a documented loss (§10.9 loss 1). A vault that wants them
//! back declares `export: {edge_tables: {TYPE: "Heading"}}`, and the exporter
//! writes that type's edges as a GFM table under that heading — the one place
//! an export adds prose to a note, and only because the vault asked for it by
//! name.
//!
//! **The declared heading's first table is the exporter's.** It rewrites that
//! table whole — header row, delimiter row and body rows — keeping only the
//! name the author gave its first column, and appends a heading and a table at
//! the end of the body when the heading is absent. That is what makes an
//! exported vault a fixed point: the second export finds its own table and
//! replaces it rather than appending a second one. Everything else in the body
//! is human prose and is never rewritten (§10.5).

use super::{LinkIndex, Note, Target};
use crate::datatypes::values::{raw_string, Value};
use crate::okf::structure::block::BlockKind;
use crate::okf::structure::parse_blocks;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

/// The header the first column gets where the exporter writes the table from
/// nothing. A table the author already wrote keeps their own name for it.
const TARGET_COLUMN: &str = "target";

/// One edge, as the row that states it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Row {
    /// The link body: a stem, or the folder-qualified path (VAULT.md §5.1).
    target: String,
    anchor: Option<String>,
    label: Option<String>,
    /// The `row` property, which is the order the author's own table had.
    order: Option<i64>,
    /// Every other property, as the text its column carries.
    props: BTreeMap<String, String>,
}

impl Row {
    /// One outgoing edge as a row. `link` is the wikilink body naming its
    /// target — [`LinkIndex::wikilink`] for a note, the unresolved name for a
    /// stub (VAULT.md §10.6).
    pub(super) fn new(link: String, props: &[(String, Value)]) -> Row {
        let mut row = Row {
            target: link,
            anchor: None,
            label: None,
            order: None,
            props: BTreeMap::new(),
        };
        for (name, value) in props {
            match name.as_str() {
                // The heading states it, and the reader reads it back off the
                // heading, so writing it as a column would state it twice.
                "section" => {}
                "anchor" => row.anchor = text(value),
                "label" => row.label = text(value),
                "row" => row.order = as_int(value),
                _ => {
                    row.props.insert(name.clone(), string_of(value));
                }
            }
        }
        row
    }

    /// The first cell: the target as the `[[wikilink]]` the reader resolves,
    /// carrying its own anchor and display text.
    fn target_cell(&self) -> String {
        let mut out = format!("[[{}", self.target);
        if let Some(anchor) = &self.anchor {
            out.push('#');
            out.push_str(anchor);
        }
        if let Some(label) = &self.label {
            out.push('|');
            out.push_str(label);
        }
        out.push_str("]]");
        out
    }

    /// The sort key (VAULT.md §10.8). `row` first, so a table the author wrote
    /// comes back in the order they wrote it; a type whose edges carry no
    /// `row` — a graph that was never a vault — falls back to the target and
    /// then to the row's own text, which is total.
    fn sort_key(&self) -> (i64, String, String) {
        (
            self.order.unwrap_or(i64::MAX),
            self.target_cell(),
            self.props
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("\u{1f}"),
        )
    }
}

/// A property's text, empty strings dropped: an empty cell writes no property
/// on the way in (VAULT.md §7.1), so writing one back would be a column the
/// reader throws away.
fn text(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if s.is_empty() => None,
        Value::Null => None,
        other => Some(string_of(other)),
    }
}

fn as_int(value: &Value) -> Option<i64> {
    match value {
        Value::Int64(n) => Some(*n),
        _ => None,
    }
}

/// A property as the text its cell holds. A cell is text and `types:` names
/// node labels only, so a non-string property comes back a string — stated in
/// §10.6 rather than silently rounded.
fn string_of(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => raw_string(other),
    }
}

/// Where a declared type's table goes in a body.
#[derive(Debug, PartialEq, Eq)]
enum Placement {
    /// The heading carries a table already: this is its range and the name the
    /// author gave its first column, and the exporter owns it.
    Replace(Range<usize>, String),
    /// The heading is there without a table — the author's prose under it
    /// stays, and the table is written at the end of what the heading itself
    /// holds, before the next heading of any level.
    Insert(usize),
    /// No such heading: the exporter appends its own at the end of the body.
    Append,
}

/// Find the declared heading's first table (VAULT.md §10.6).
///
/// The heading is matched on its **text**, at any level: a vault that writes
/// `### Worked on by` under a symbol heading declares `"Worked on by"`, not the
/// level it happens to sit at. A table under a *nested* heading belongs to that
/// heading, not to this one, which is why the block's own heading index is
/// compared rather than the range.
fn locate(body: &str, heading: &str) -> Placement {
    let tree = parse_blocks(body);
    let wanted = heading.trim();
    let Some(at) = tree.headings.iter().position(|h| h.text.trim() == wanted) else {
        return Placement::Append;
    };
    let table = tree
        .blocks
        .iter()
        .find(|b| b.heading == Some(at) && matches!(b.kind, BlockKind::Table(_)));
    match table {
        // The author named the first column; every other column is a property
        // name, and the delimiter row is the exporter's. A blank name stays
        // blank — a header cell naming no property is a legitimate thing for a
        // converter to have written (VAULT.md §7.1).
        Some(block) => {
            let first = match &block.kind {
                BlockKind::Table(table) => table
                    .header
                    .first()
                    .map(|cell| cell.text.clone())
                    .unwrap_or_default(),
                _ => String::new(),
            };
            Placement::Replace(block.range.clone(), first)
        }
        // The next heading in document order, whatever its level: what this
        // heading itself holds ends there.
        None => Placement::Insert(
            tree.headings
                .get(at + 1)
                .map(|next| next.range.start)
                .unwrap_or(body.len()),
        ),
    }
}

/// The body with every declared heading's first table cut out — the prose the
/// note states on its own, as against the region the exporter owns.
///
/// Used to decide which declared edges the body *already* states elsewhere: a
/// row for one of those would make a second edge on the next import, exactly as
/// a frontmatter list would (VAULT.md §10.6).
pub(super) fn prose_outside_owned_tables(body: &str, headings: &[&str]) -> String {
    let mut ranges: Vec<Range<usize>> = headings
        .iter()
        .filter_map(|heading| match locate(body, heading) {
            Placement::Replace(range, _) => Some(range),
            Placement::Insert(_) | Placement::Append => None,
        })
        .collect();
    if ranges.is_empty() {
        return body.to_string();
    }
    ranges.sort_by_key(|r| r.start);
    let mut out = String::with_capacity(body.len());
    let mut at = 0usize;
    for range in ranges {
        if range.start >= at {
            out.push_str(&body[at..range.start]);
            at = range.end;
        }
    }
    out.push_str(&body[at..]);
    out
}

/// Write each declared type's rows into `body` (VAULT.md §10.6).
///
/// `tables` is `(heading, rows)` in declaration order. A type with no rows
/// removes the table the exporter owns — its edges are gone from the graph, so
/// leaving the table would make them again on the next import — and appends
/// nothing where there is none.
pub(super) fn apply(body: &str, tables: &[(&str, Vec<Row>)]) -> String {
    let mut out = body.to_string();
    for (heading, rows) in tables {
        out = write_one(&out, heading, rows);
    }
    out
}

fn write_one(body: &str, heading: &str, rows: &[Row]) -> String {
    let placement = locate(body, heading);
    if rows.is_empty() {
        return match placement {
            Placement::Replace(range, _) => cut(body, range),
            Placement::Insert(_) | Placement::Append => body.to_string(),
        };
    }
    match placement {
        Placement::Replace(range, first) => {
            let mut table = render(&first, rows);
            if body[range.clone()].ends_with('\n') {
                table.push('\n');
            }
            format!("{}{table}{}", &body[..range.start], &body[range.end..])
        }
        Placement::Insert(at) => splice(body, at, &render(TARGET_COLUMN, rows)),
        Placement::Append => splice(
            body,
            body.len(),
            &format!("## {heading}\n\n{}", render(TARGET_COLUMN, rows)),
        ),
    }
}

/// The table itself: header, delimiter, one line per row, no trailing newline.
fn render(first_column: &str, rows: &[Row]) -> String {
    let mut sorted: Vec<&Row> = rows.iter().collect();
    sorted.sort_by_key(|row| row.sort_key());
    let columns: Vec<&str> = sorted
        .iter()
        .flat_map(|row| row.props.keys())
        .map(String::as_str)
        .collect::<BTreeSet<&str>>()
        .into_iter()
        .collect();
    let mut out = String::new();
    out.push_str(&line(
        std::iter::once(escape(first_column)).chain(columns.iter().map(|name| escape(name))),
    ));
    out.push_str(&line(std::iter::repeat_n(
        "---".to_string(),
        columns.len() + 1,
    )));
    for row in sorted {
        out.push_str(&line(std::iter::once(escape(&row.target_cell())).chain(
            columns.iter().map(|name| {
                row.props
                    .get(*name)
                    .map(|value| escape(value))
                    .unwrap_or_default()
            }),
        )));
    }
    out.pop();
    out
}

fn line(cells: impl Iterator<Item = String>) -> String {
    let mut out = String::from("|");
    for cell in cells {
        out.push(' ');
        out.push_str(&cell);
        out.push_str(" |");
    }
    out.push('\n');
    out
}

/// A cell holds one line, and a literal `|` in it is Obsidian's `\|` — which
/// is also how a wikilink writes its display text inside a table (VAULT.md
/// §5.1), so the alias separator is escaped here like any other pipe.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '|' => out.push_str("\\|"),
            '\n' | '\r' => out.push(' '),
            other => out.push(other),
        }
    }
    out.trim().to_string()
}

/// Put `block` at `at`, with one blank line on each side of it and no trailing
/// whitespace left behind.
fn splice(body: &str, at: usize, block: &str) -> String {
    let (before, after) = body.split_at(at);
    let mut out = before.trim_end().to_string();
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(block.trim_end());
    let rest = after.trim_start_matches('\n');
    if rest.is_empty() {
        out.push('\n');
    } else {
        out.push_str("\n\n");
        out.push_str(rest);
    }
    out
}

/// Remove the table the exporter owns, leaving its heading and the prose
/// around it — and no double blank line where it stood.
fn cut(body: &str, range: Range<usize>) -> String {
    let before = body[..range.start].trim_end();
    let after = body[range.end..].trim_start_matches('\n');
    if before.is_empty() {
        return after.to_string();
    }
    if after.is_empty() {
        return format!("{before}\n");
    }
    format!("{before}\n\n{after}")
}

/// Every declared type's edges from one note, as rows — and the properties the
/// rows carry, which the export therefore does not drop (VAULT.md §10.9).
pub(super) fn rows_for(
    edges: &[super::OutEdge],
    conn_type: &str,
    notes: &[Note],
    index: &LinkIndex,
    skip: impl Fn(&super::OutEdge) -> bool,
) -> Vec<Row> {
    edges
        .iter()
        .filter(|edge| edge.conn_type == conn_type && !skip(edge))
        .map(|edge| {
            let link = match &edge.target {
                Target::Note(at) => index.wikilink(&notes[*at]),
                Target::Stub(name) => name.clone(),
            };
            Row::new(link, &edge.props)
        })
        .collect()
}

#[cfg(test)]
#[path = "edge_tables_tests.rs"]
mod edge_tables_tests;
