//! `tables:` — a GFM table's rows as nodes, or as edges (VAULT.md §7.1).
//!
//! One rule list, read in order: the first rule whose `under_heading:` matches
//! the enclosing section's title reads the table, and a table under no
//! matching heading is prose like any other block.
//!
//! The two forms answer different questions and share everything else. A
//! **node** row is a thing the corpus holds (a parameter, a field, a return
//! value) and keys on one of its own columns; an **edge** row is a statement
//! *about* two notes — a role, a date range — and becomes a [`Link`] with its
//! other columns as edge properties, so it travels the resolver ladder every
//! prose link travels and an unresolvable target becomes the same
//! `_provisional` stub (VAULT.md §5.6).
//!
//! A cell's own `[[links]]` and `![images]` are scanned as prose wherever they
//! sit (VAULT.md §5.1): an edge table's target is *also* a `LINKS_TO` edge
//! from the note, and a picture in a cell is the note's attachment reference.
//! A row rule never swallows them — it reads the same cell for a different
//! purpose.

use super::block::{BlockKind, Cell};
use super::constructs::{bump, claim, Ctx, Place};
use super::derive::{Derived, DerivedEdge, DerivedNode, IdSpace};
use super::profile::TableRule;
use crate::datatypes::values::Value;
use crate::okf::links::{self, WikiRef};
use crate::okf::model::Link;
use std::collections::{BTreeMap, BTreeSet};

/// Read every table a rule claims (VAULT.md §7.1 `tables:`).
pub(super) fn derive_tables(
    ctx: &Ctx<'_>,
    rules: &[TableRule],
    ids: &mut IdSpace,
    out: &mut Derived,
) {
    let mut counters: BTreeMap<String, usize> = BTreeMap::new();
    for block in &ctx.tree.blocks {
        let BlockKind::Table(table) = &block.kind else {
            continue;
        };
        let place = ctx.place(block);
        // A table above the body's first heading sits under no section, so a
        // rule that names a heading cannot reach it.
        let Some(title) = place.section_title.as_deref() else {
            continue;
        };
        let Some(rule) = rules.iter().find(|r| r.under_heading.is_match(title)) else {
            continue;
        };
        let columns = Columns::read(&table.header);
        if rule.edges {
            edge_rows(rule, &place, &columns, &table.rows, out);
        } else {
            node_rows(rule, &place, &columns, &table.rows, ids, &mut counters, out);
        }
    }
}

/// The header row, as the property names its columns write under.
///
/// A header cell is taken **as written**, only trimmed (the block tree already
/// trims a cell) and with the `\|` escape removed. A blank one names no
/// property: GFM has no headerless table — a converter that had no header
/// wrote an empty one — and inventing `column3` would key a corpus's
/// properties to a position that moves the moment a column is inserted.
struct Columns {
    names: Vec<Option<String>>,
}

impl Columns {
    fn read(header: &[Cell]) -> Self {
        Columns {
            names: header
                .iter()
                .map(|cell| {
                    let name = unescape_pipes(&cell.text);
                    (!name.is_empty()).then_some(name)
                })
                .collect(),
        }
    }

    /// The index `key_column:` names, or `None` when it names a column the
    /// table does not carry.
    fn index_of(&self, name: &str) -> Option<usize> {
        self.names
            .iter()
            .position(|column| column.as_deref() == Some(name))
    }

    fn name(&self, index: usize) -> Option<&str> {
        self.names.get(index).and_then(|n| n.as_deref())
    }

    /// Every named column's `(name, value)`, skipping `except`, blank headers
    /// and empty cells — an empty cell writes no property (VAULT.md §7.1).
    fn pairs<'a>(
        &'a self,
        row: &'a [Cell],
        except: Option<usize>,
    ) -> impl Iterator<Item = (&'a str, String)> {
        row.iter().enumerate().filter_map(move |(index, cell)| {
            if except == Some(index) {
                return None;
            }
            let name = self.name(index)?;
            let value = unescape_pipes(&cell.text);
            (!value.is_empty()).then_some((name, value))
        })
    }
}

/// The default: one node per body row (VAULT.md §7.1).
fn node_rows(
    rule: &TableRule,
    place: &Place,
    columns: &Columns,
    rows: &[Vec<Cell>],
    ids: &mut IdSpace,
    counters: &mut BTreeMap<String, usize>,
    out: &mut Derived,
) {
    let label = rule.label.clone().expect("a node rule declares its label");
    let key = match &rule.key_column {
        Some(name) => match columns.index_of(name) {
            Some(index) => index,
            None => {
                out.warnings.push(format!(
                    "table under `{}` has no column `{name}`; its rows key on the first column",
                    place.section_title.as_deref().unwrap_or_default()
                ));
                0
            }
        },
        None => 0,
    };
    let prefix = place.section.clone().unwrap_or_default();
    let mut used: BTreeSet<String> = BTreeSet::new();
    for (index, row) in rows.iter().enumerate() {
        let number = bump(counters, &prefix);
        let value = row.get(key).map(|c| unescape_pipes(&c.text));
        // A key that is empty or already spoken for cannot name the row, so
        // the row falls back to its position — which is what `~row<n>` is for
        // (VAULT.md §7.1). The warning names the table, because the fix is in
        // the table and not in the rule.
        let wanted = match value.filter(|v| !v.is_empty()) {
            Some(value) if used.insert(value.clone()) => format!("{prefix}~{value}"),
            other => {
                // The id is named as well as the row: `<n>` counts the rows
                // under the *section*, so a second table under one heading
                // starts where the first left off and "row 2" alone would
                // send a reader looking for a node that is not there.
                out.warnings.push(match other {
                    Some(value) => format!(
                        "table under `{}` repeats the key `{value}`; row {} keys on its \
                         position instead (`~row{number}`)",
                        place.section_title.as_deref().unwrap_or_default(),
                        index + 1
                    ),
                    None => format!(
                        "table under `{}` has an empty key in row {}; it keys on its \
                         position instead (`~row{number}`)",
                        place.section_title.as_deref().unwrap_or_default(),
                        index + 1
                    ),
                });
                format!("{prefix}~row{number}")
            }
        };
        let suffix = claim(ids, wanted, out);
        out.edges.push(DerivedEdge {
            conn_type: rule.edge.clone(),
            source: place.section.clone(),
            target: suffix.clone(),
        });
        // The key column is a property like every other one — "stored under
        // its own column name as well" (VAULT.md §7.1) — so the row is read
        // whole and nothing is excepted.
        let props: Vec<(String, Value)> = columns
            .pairs(row, None)
            .map(|(name, value)| (name.to_string(), Value::String(value)))
            .collect();
        out.nodes.push(DerivedNode {
            suffix,
            label: label.clone(),
            section: place.section.clone(),
            heading_path: place.heading_path.clone(),
            section_title: place.section_title.clone(),
            range: row_range(row),
            // A row's cells are its properties; it holds no prose of its own,
            // so no `embed_text:` is rendered for one.
            text: None,
            props,
        });
    }
}

/// A row's extent: its first cell's start to its last cell's end. The block
/// tree gives cells and not rows, and the pipes between them are the table's
/// own syntax — a tag can only ever be written inside a cell.
fn row_range(row: &[Cell]) -> std::ops::Range<usize> {
    match (row.first(), row.last()) {
        (Some(first), Some(last)) => first.range.start..last.range.end.max(first.range.start),
        _ => 0..0,
    }
}

/// `edges: true` — one edge per body row, no node (VAULT.md §7.1).
fn edge_rows(
    rule: &TableRule,
    place: &Place,
    columns: &Columns,
    rows: &[Vec<Cell>],
    out: &mut Derived,
) {
    let target = match &rule.key_column {
        Some(name) => columns.index_of(name),
        None => target_column(rows),
    };
    let Some(target) = target else {
        out.warnings.push(format!(
            "edge table under `{}` names no target column: declare `key_column:`, or \
             write the targets as `[[wikilinks]]`",
            place.section_title.as_deref().unwrap_or_default()
        ));
        return;
    };
    for (index, row) in rows.iter().enumerate() {
        let Some(cell) = row.get(target) else {
            continue;
        };
        let raw = unescape_pipes(&cell.text);
        // A wikilink states its own target, anchor and display text; a plain
        // cell is a name, resolved by the same ladder (VAULT.md §5.2).
        let link = links::first_wikilink(&cell.text);
        let (name, anchor, label) = match &link {
            Some(WikiRef {
                name,
                anchor,
                alias,
            }) => (name.to_string(), *anchor, *alias),
            None => (raw.clone(), None, None),
        };
        if name.is_empty() {
            continue;
        }
        let mut props: Vec<(String, Value)> = Vec::new();
        if let Some(section) = &place.section_title {
            props.push(("section".to_string(), Value::String(section.clone())));
        }
        if let Some(anchor) = anchor.filter(|a| !a.is_empty()) {
            props.push(("anchor".to_string(), Value::String(anchor.to_string())));
        }
        if let Some(label) = label {
            props.push(("label".to_string(), Value::String(label.to_string())));
        }
        // 1-based, so the property and the `~row<n>` a node form mints name
        // the same row — and so it reads as the row a person counts.
        props.push(("row".to_string(), Value::Int64(index as i64 + 1)));
        for (name, value) in columns.pairs(row, Some(target)) {
            if props.iter().any(|(k, _)| k == name) {
                out.warnings.push(format!(
                    "edge table under `{}` has a column `{name}`, which is the edge \
                     property the link itself carries; the column is dropped",
                    place.section_title.as_deref().unwrap_or_default()
                ));
                continue;
            }
            props.push((name.to_string(), Value::String(value)));
        }
        out.links.push(Link {
            target: name.trim_end_matches(".md").to_string(),
            conn_type: rule.edge.clone(),
            is_external: false,
            props,
            reverse: false,
        });
        out.edge_tables_hit.insert(rule.edge.clone());
    }
}

/// The first column whose body cells hold a `[[wikilink]]` (VAULT.md §7.1).
fn target_column(rows: &[Vec<Cell>]) -> Option<usize> {
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    (0..width).find(|&index| {
        rows.iter().any(|row| {
            row.get(index)
                .is_some_and(|c| links::first_wikilink(&c.text).is_some())
        })
    })
}

/// Obsidian's `\|` — the pipe a cell writes when it means a literal one
/// (VAULT.md §5.1). Every reader unescapes it: a value is what the author
/// meant, not what the table syntax forced them to type.
fn unescape_pipes(text: &str) -> String {
    if text.contains("\\|") {
        text.replace("\\|", "|")
    } else {
        text.to_string()
    }
}

#[cfg(test)]
#[path = "tables_tests.rs"]
mod tables_tests;
