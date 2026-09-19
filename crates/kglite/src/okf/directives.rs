//! `<!-- kglite <key>: <value> -->` → a property or a typed edge (VAULT.md
//! §5.8).
//!
//! The reader is here and not in `okf::structure` because the rule it applies
//! is §4.3's, not §7.1's: a value that is a wikilink is edges and everything
//! else is a property, typed the way a frontmatter value is typed. Only
//! *where* the property lands is structural — the enclosing section when the
//! vault derives them, the note otherwise — and the block tree already says
//! which heading a directive sits under.
//!
//! Runs after `structure::derive`, so the section a directive names already
//! exists as a `DerivedNode` and the property is set on that node rather than
//! patched into the graph afterwards.

use crate::datatypes::values::Value;
use crate::okf::frontmatter;
use crate::okf::links;
use crate::okf::model::{Link, Profile};
use crate::okf::structure::block::Directive;
use crate::okf::structure::profile::is_reserved_property;
use crate::okf::structure::{BlockTree, Derived};

/// The directive keys the format owns (VAULT.md §5.8): `chunk` closes the
/// open chunk and `heading` promotes the line below it. Neither states a
/// property, and both are acted on before this pass — `chunk` in the packer,
/// `heading` in the block tree — so both are skipped here rather than warned
/// about for carrying no value.
const MARKER_KEYS: [&str; 2] = ["chunk", "heading"];

/// The note-level half of what one note's directives write. The section-level
/// half lands on [`Derived`] itself.
pub(crate) struct NoteSide<'a> {
    pub profile: &'a Profile,
    pub source_dir: &'a str,
    pub props: &'a mut Vec<(String, Value)>,
    pub links: &'a mut Vec<Link>,
    pub errors: &'a mut Vec<String>,
}

/// Apply every directive in `tree` (VAULT.md §5.8).
///
/// `<!-- kglite -->` and the marker keys are handled elsewhere — the first is
/// warned about by `structure::derive`, `chunk` is a chunk boundary and
/// `heading` a synthetic heading — and are skipped here so none of them
/// becomes a property named after itself.
pub(crate) fn apply(tree: &BlockTree, derived: &mut Derived, note: &mut NoteSide<'_>) {
    for directive in &tree.directives {
        if directive.key.is_empty() || MARKER_KEYS.contains(&directive.key.as_str()) {
            continue;
        }
        if is_reserved_property(&directive.key) || directive.key == note.profile.body_property {
            note.errors.push(format!(
                "`<!-- kglite {key}: … -->` names `{key}`, which a note or a derived node \
                 defines itself (VAULT.md §4.1, §5.8, §7.1)",
                key = directive.key
            ));
            continue;
        }
        let raw = directive.raw_value.as_deref().unwrap_or("").trim();
        if raw.is_empty() {
            derived.warnings.push(format!(
                "`<!-- kglite {} -->` carries no value, so it states nothing (VAULT.md §5.8)",
                directive.key
            ));
            continue;
        }
        let target = target_of(tree, derived, directive);
        match read_value(raw, note.profile) {
            Read::Edges(targets) => emit_edges(&directive.key, targets, target, derived, note),
            Read::Property(value) => set_property(&directive.key, value, target, derived, note),
        }
    }
}

/// The section a directive sits under, or `None` for the note — which is also
/// the answer above the first heading and wherever `sections:` is not
/// declared, because then there is no node between the directive and the note
/// (VAULT.md §5.8).
fn target_of(tree: &BlockTree, derived: &Derived, directive: &Directive) -> Option<String> {
    let heading = tree.blocks[directive.block].heading?;
    derived.section_suffixes.get(heading).cloned()
}

/// What one directive's value states.
enum Read {
    /// Wikilink targets — the typed-edge rule (VAULT.md §4.3) fired.
    Edges(Vec<String>),
    Property(Value),
}

/// Read a directive's value with the grammar §4.2 gives a frontmatter value,
/// plus the three things being written **inline in prose** changes.
///
/// 1. **A bare `[[Target]]` needs no quotes**, and nor does a comma-separated
///    run of them. In frontmatter `depends_on: [[X]]` is a YAML flow sequence
///    holding a sequence and the quotes are what make it a wikilink; in a
///    comment beside a sentence, `[[X]]` is a wikilink and a nested flow
///    sequence is not something anyone writes. The whole-string wikilink is
///    tried before the comma split, so `[[A, B]]` stays one target.
/// 2. **A value YAML reads as a mapping is the raw text.** `:` is ordinary
///    punctuation in a sentence (`Task pane: Wells -> Annotations`), and a
///    single node property cannot hold a mapping in any case — frontmatter
///    flattens one into dotted keys, which a value with no key of its own
///    cannot do.
/// 3. **A value YAML refuses outright is the raw text**, for the same reason:
///    the author wrote a sentence, not a document.
fn read_value(raw: &str, profile: &Profile) -> Read {
    if let Some(targets) = bare_wikilinks(raw) {
        return Read::Edges(targets);
    }
    let parsed = match frontmatter::parse_yaml(raw) {
        Ok(Value::Map(_) | Value::Null) | Err(_) => Value::String(raw.to_string()),
        Ok(value) => value,
    };
    if let Some(targets) = links::wikilink_targets(&parsed) {
        return Read::Edges(targets);
    }
    Read::Property(if profile.infer_temporal {
        frontmatter::infer_temporal(parsed)
    } else {
        parsed
    })
}

/// `[[A]]`, or `[[A]], [[B]]` — unquoted, as a note writes them.
fn bare_wikilinks(raw: &str) -> Option<Vec<String>> {
    let one = |text: &str| links::wikilink_targets(&Value::String(text.trim().to_string()));
    if let Some(single) = one(raw) {
        return Some(single);
    }
    let parts: Vec<&str> = raw.split(',').collect();
    if parts.len() < 2 {
        return None;
    }
    parts
        .into_iter()
        .map(|part| one(part).filter(|t| t.len() == 1).map(|mut t| t.remove(0)))
        .collect()
}

/// `upper_snake(key)` edges from the target node to each wikilink named
/// (VAULT.md §4.3, §5.8). A key that normalises to nothing states no edge.
fn emit_edges(
    key: &str,
    targets: Vec<String>,
    target: Option<String>,
    derived: &mut Derived,
    note: &mut NoteSide<'_>,
) {
    let conn_type = links::upper_snake(key);
    if conn_type.is_empty() {
        derived.warnings.push(format!(
            "`<!-- kglite {key}: … -->` names no edge type, so its wikilinks state \
             nothing (VAULT.md §5.3)"
        ));
        return;
    }
    for name in targets {
        if note.profile.path_safety {
            links::record_wikilink_path_error(note.errors, &name, note.source_dir);
        }
        let link = Link::plain(name, conn_type.clone(), false);
        match &target {
            Some(suffix) => derived.links_from.push((suffix.clone(), link)),
            None => links::push_unique(note.links, link),
        }
    }
}

/// Set `key` on the target node, replacing whatever it held.
///
/// **Last wins, and says so.** Two directives naming one key on one node, or
/// a directive naming a key the note's frontmatter already carries, are an
/// ambiguity only the author can resolve; taking the later one silently would
/// hide it. Edges do not follow this rule — two wikilink directives sharing a
/// key are two edges, exactly as a frontmatter list of wikilinks is.
fn set_property(
    key: &str,
    value: Value,
    target: Option<String>,
    derived: &mut Derived,
    note: &mut NoteSide<'_>,
) {
    let props = match &target {
        Some(suffix) => match derived.nodes.iter_mut().find(|n| n.suffix == *suffix) {
            Some(node) => &mut node.props,
            None => return,
        },
        None => &mut *note.props,
    };
    let replaced = match props.iter_mut().find(|(k, _)| k == key) {
        Some(slot) => {
            slot.1 = value;
            true
        }
        None => {
            props.push((key.to_string(), value));
            false
        }
    };
    if replaced {
        derived.warnings.push(format!(
            "`{key}` is stated more than once on the same node; the last \
             `<!-- kglite {key}: … -->` wins (VAULT.md §5.8)"
        ));
    }
}
