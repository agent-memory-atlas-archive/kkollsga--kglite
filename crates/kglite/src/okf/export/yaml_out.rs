//! Frontmatter emission (VAULT.md §10.3, §10.4).
//!
//! A deliberately small YAML writer rather than a general one. It has to emit
//! exactly the scalar set the reader recognises and *only* that, because the
//! round trip is the contract: a value that comes back a different type has
//! changed the graph. A general emitter chooses quoting for readability; this
//! one chooses it so that re-parsing gives the value back.
//!
//! Two consequences worth knowing before editing:
//!
//! - **Nothing here quotes for taste.** [`needs_quoting`] is the whole rule,
//!   and its one job is to stop a string from re-parsing as something else.
//! - **A key with a dot is a nested map.** The reader flattens `metadata: {a: 1}`
//!   into the property `metadata.a` (§4.2), so the writer puts it back; a key
//!   whose dotted path would collide with a scalar already written there is
//!   emitted literally instead of silently losing one of the two.

use crate::datatypes::values::Value;
use std::collections::BTreeMap;

/// A frontmatter document under construction: sorted keys, nested where the
/// reader's flattening made them dotted.
#[derive(Debug, Default)]
pub(super) struct Tree {
    root: BTreeMap<String, Node>,
}

#[derive(Debug)]
enum Node {
    Leaf(Value),
    /// A list of `[[wikilink]]` targets — kept apart from a `Value::List` of
    /// strings because these are written as wikilinks and must not be quoted
    /// into ordinary strings.
    Links(Vec<String>),
    Branch(BTreeMap<String, Node>),
}

impl Tree {
    /// Add one property, expanding a dotted key into nested maps.
    pub(super) fn insert(&mut self, key: &str, value: Value) {
        insert_path(&mut self.root, key, Node::Leaf(value));
    }

    /// Add one edge key: a list of target names, written as wikilinks.
    pub(super) fn insert_wikilinks(&mut self, key: &str, targets: Vec<String>) {
        insert_path(&mut self.root, key, Node::Links(targets));
    }

    fn is_empty(&self) -> bool {
        self.root.is_empty()
    }
}

/// Place `node` at a dotted path, creating the branches it names.
///
/// A path that would have to grow *through* an existing leaf, or replace a
/// branch with one, keeps the literal dotted key instead: both values were
/// real properties of the node, and choosing one of them would be a silent
/// loss on a round trip.
fn insert_path(map: &mut BTreeMap<String, Node>, key: &str, node: Node) {
    let Some((head, rest)) = key.split_once('.') else {
        if matches!(map.get(key), Some(Node::Branch(_))) {
            return;
        }
        map.insert(key.to_string(), node);
        return;
    };
    if head.is_empty() || rest.is_empty() {
        map.insert(key.to_string(), node);
        return;
    }
    match map
        .entry(head.to_string())
        .or_insert_with(|| Node::Branch(BTreeMap::new()))
    {
        Node::Branch(child) => insert_path(child, rest, node),
        // A leaf already sits where this key wants a map.
        _ => {
            map.insert(key.to_string(), node);
        }
    }
}

/// The frontmatter body — everything that goes *between* the `---` fences,
/// with a trailing newline. Empty when the note has no keys at all.
pub(super) fn render_frontmatter(tree: &Tree) -> String {
    if tree.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    render_map(&tree.root, 0, &mut out);
    out
}

fn render_map(map: &BTreeMap<String, Node>, indent: usize, out: &mut String) {
    let pad = "  ".repeat(indent);
    for (key, node) in map {
        let key = render_key(key);
        match node {
            Node::Leaf(Value::List(items)) => {
                if items.is_empty() {
                    out.push_str(&format!("{pad}{key}: []\n"));
                    continue;
                }
                out.push_str(&format!("{pad}{key}:\n"));
                for item in items {
                    out.push_str(&format!("{pad}  - {}\n", render_inline(item)));
                }
            }
            Node::Links(targets) => {
                out.push_str(&format!("{pad}{key}:\n"));
                for target in targets {
                    // A wikilink is quoted because `[[A]]` unquoted is a YAML
                    // flow sequence holding a sequence (VAULT.md §10.4).
                    out.push_str(&format!("{pad}  - {}\n", quote(&format!("[[{target}]]"))));
                }
            }
            Node::Leaf(Value::Map(entries)) => {
                let nested: BTreeMap<String, Node> = entries
                    .iter()
                    .map(|(k, v)| (k.to_string(), Node::Leaf(v.clone())))
                    .collect();
                if nested.is_empty() {
                    out.push_str(&format!("{pad}{key}: {{}}\n"));
                    continue;
                }
                out.push_str(&format!("{pad}{key}:\n"));
                render_map(&nested, indent + 1, out);
            }
            Node::Leaf(value) => {
                out.push_str(&format!("{pad}{key}: {}\n", render_inline(value)));
            }
            Node::Branch(child) => {
                out.push_str(&format!("{pad}{key}:\n"));
                render_map(child, indent + 1, out);
            }
        }
    }
}

/// A mapping key. Quoted under the same rule as a value, so a key that reads
/// as a number or carries a colon still round-trips.
fn render_key(key: &str) -> String {
    if needs_quoting(key) {
        quote(key)
    } else {
        key.to_string()
    }
}

/// One scalar, or a nested collection in flow style.
///
/// A list or map *inside* a list is written as JSON, which is valid YAML flow
/// syntax and needs no indentation bookkeeping. The reader parses it back to
/// the same `Value`, which is the only property that matters here.
fn render_inline(value: &Value) -> String {
    match value {
        Value::String(s) => {
            if needs_quoting(s) {
                quote(s)
            } else {
                s.clone()
            }
        }
        Value::Int64(n) => n.to_string(),
        Value::UniqueId(n) => n.to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::Float64(f) => render_float(*f),
        // `NaiveDate` / `NaiveDateTime` re-parse as the same temporal values
        // under §4.2's inference, so they are written bare.
        Value::DateTime(d) => d.format("%Y-%m-%d").to_string(),
        Value::Timestamp(t) => format!("{}Z", t.format("%Y-%m-%dT%H:%M:%S%.f")),
        // WKT, the spelling `point()` and the RDF loader both read (§10.3).
        Value::Point { lat, lon } => format!("POINT({lon} {lat})"),
        Value::Null => "null".to_string(),
        Value::List(_) | Value::Map(_) => {
            serde_json::to_string(&crate::param::kglite_value_to_json(value))
                .unwrap_or_else(|_| "null".to_string())
        }
        other => quote(&crate::datatypes::values::raw_string(other)),
    }
}

/// A float that still reads as a float. `1.0` formats as `1` by default, which
/// YAML hands back as an integer.
fn render_float(f: f64) -> String {
    if f.is_nan() {
        return ".nan".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 { ".inf" } else { "-.inf" }.to_string();
    }
    let text = f.to_string();
    if text.contains(['.', 'e', 'E']) {
        text
    } else {
        format!("{text}.0")
    }
}

/// A double-quoted YAML scalar. JSON string syntax is a subset of YAML's
/// double-quoted style, so the encoder already in the tree emits one every
/// YAML parser accepts — the same reasoning `skills::yaml_quoted` uses.
fn quote(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| format!("\"{}\"", text.replace('"', "'")))
}

/// Whether a string has to be quoted to come back as a string (VAULT.md §10.4).
///
/// The rule, in the order it is checked: empty or padded with whitespace;
/// starting with a YAML indicator character (which is what quotes a `[[A]]`);
/// containing `: `, a ` #` comment opener, or a newline; ending in
/// `:`; spelling a boolean or null; parsing as an integer or a float; or
/// matching a date or datetime the reader would infer (§4.2) — which is asked
/// of [`crate::okf::frontmatter::infer_temporal`] itself, so the two can never
/// disagree about what a bare `2026-01-15` means.
fn needs_quoting(text: &str) -> bool {
    if text.is_empty() || text != text.trim() {
        return true;
    }
    let first = text.chars().next().unwrap_or(' ');
    // `[` is one of these, which is what quotes a `[[wikilink]]` string: bare,
    // it is a flow sequence holding a flow sequence, not the name it spells.
    if "-?:,[]{}#&*!|>'\"%@`".contains(first) {
        return true;
    }
    if text.contains(": ") || text.contains(" #") || text.contains('\n') || text.ends_with(':') {
        return true;
    }
    if YAML_KEYWORDS.contains(&text) {
        return true;
    }
    if text.parse::<i64>().is_ok() || text.parse::<f64>().is_ok() {
        return true;
    }
    if text.starts_with("0x") || text.starts_with("0o") {
        return true;
    }
    !matches!(
        crate::okf::frontmatter::infer_temporal(Value::String(text.to_string())),
        Value::String(_)
    )
}

/// The bare words a YAML reader may take for a boolean or a null. The 1.1
/// spellings (`yes`, `on`, …) are here too: YAML 1.2 keeps them strings, but a
/// vault is read by other tools as well, and quoting a word that was already a
/// string costs nothing while guessing wrong loses the value.
const YAML_KEYWORDS: [&str; 22] = [
    "true", "True", "TRUE", "false", "False", "FALSE", "null", "Null", "NULL", "~", "y", "Y", "n",
    "N", "yes", "Yes", "YES", "no", "No", "NO", "on", "off",
];

/// `HAS_KEYWORD` → `has_keyword`: the inverse of
/// [`crate::okf::links::upper_snake`], which is what turns the key back into
/// the edge type on the next import. Exact for the `UPPER_SNAKE` types this
/// format writes; a type spelled any other way round-trips to its upper-snake
/// form, which is what the reader would have produced from it anyway.
pub(super) fn lower_snake(conn_type: &str) -> String {
    let mut out = String::with_capacity(conn_type.len());
    let mut pending_sep = false;
    for ch in conn_type.chars() {
        if ch.is_alphanumeric() {
            if pending_sep && !out.is_empty() {
                out.push('_');
            }
            pending_sep = false;
            out.extend(ch.to_lowercase());
        } else {
            pending_sep = true;
        }
    }
    out
}

#[cfg(test)]
#[path = "yaml_out_tests.rs"]
mod yaml_out_tests;
