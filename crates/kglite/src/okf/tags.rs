//! Inline `#tag`s read as *placed* rather than as a note-wide list: the `tags`
//! property on the derived node holding one, and the `tag_labels:` rule that
//! models one as a node of its own (VAULT.md §5.5, §7.1).
//!
//! Beside `okf::directives` and for the same reason: both take something
//! written **in the prose** and decide which node it states something about,
//! and both need the block tree's answer to that question after
//! `structure::derive` has produced the nodes. The hub's view of the same
//! tags — one `Tag` per distinct name, joined from the note — is untouched
//! here and stays in `build::hubs`.

use crate::datatypes::values::Value;
use crate::okf::links::TagRef;
use crate::okf::model::{tag_label_rule, Profile};
use crate::okf::structure::{Derived, DerivedNode};

/// The list property a derived node carries its own tags in (VAULT.md §7.1).
/// A derived node defines it, so no vault-side declaration may write it —
/// `structure::profile::is_reserved_property` is where that is enforced.
const TAGS_PROPERTY: &str = "tags";

/// One tag a `tag_labels:` rule claimed, in the shape the builder needs to
/// mint its node and join it (VAULT.md §5.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TypedTag {
    /// The rule's pattern as `vault.yaml` wrote it (`intent/*`) — the key
    /// `Profile::tag_labels` holds its label and edge under.
    pub rule: String,
    /// The tag text after the prefix, spelled as the note spelled it. The
    /// node's id folds this to lowercase; its title keeps the commonest
    /// spelling, exactly as a hub node's does.
    pub name: String,
    /// The derived node the tag was written in, as a suffix of the note's id;
    /// `None` for the note itself.
    pub source: Option<String>,
}

/// Attribute one note's inline tags, and read its frontmatter ones against the
/// same rules.
///
/// Returns what `tag_labels:` claimed; the `tags` properties are written into
/// `derived` in place. Both forms §5.5 names feed one hub, so a rule has to
/// see both — but only an inline tag has an **offset**, so a frontmatter one
/// is the note's by construction.
pub(crate) fn apply(
    spans: &[TagRef],
    props: &[(String, Value)],
    derived: &mut Derived,
    profile: &Profile,
) -> Vec<TypedTag> {
    let mut out: Vec<TypedTag> = Vec::new();
    for span in spans {
        let node = innermost(&derived.nodes, span.range.start);
        if let Some(index) = node {
            push_tag(&mut derived.nodes[index].props, &span.name);
        }
        if let Some((rule, _, rest)) = tag_label_rule(&profile.tag_labels, &span.name) {
            out.push(TypedTag {
                rule: rule.to_string(),
                name: rest.to_string(),
                source: node.map(|index| derived.nodes[index].suffix.clone()),
            });
        }
    }
    for name in frontmatter_tags(props) {
        if let Some((rule, _, rest)) = tag_label_rule(&profile.tag_labels, name) {
            out.push(TypedTag {
                rule: rule.to_string(),
                name: rest.to_string(),
                source: None,
            });
        }
    }
    out
}

/// The derived node with the **smallest** range containing `offset`, by index.
///
/// Smallest and not innermost-by-containment, because the derived nodes are
/// not a tree: a chunk and the callout it packs are two lenses on the same
/// blockquote and neither is the other's parent. Where two ranges are equal —
/// a chunk that is exactly one callout — the later node wins, and the order
/// `derive` mints them in puts the named construct after the chunk.
fn innermost(nodes: &[DerivedNode], offset: usize) -> Option<usize> {
    let span = |node: &DerivedNode| node.range.end - node.range.start;
    let mut best: Option<usize> = None;
    for (index, node) in nodes.iter().enumerate() {
        if !node.range.contains(&offset) {
            continue;
        }
        if best.is_none_or(|current| span(node) <= span(&nodes[current])) {
            best = Some(index);
        }
    }
    best
}

/// Append `name` to a node's `tags`, in first-use order and without repeating
/// a spelling. Spelled as the note wrote it: folding case is the hub's rule
/// for its *identity*, and this property is what the prose says.
fn push_tag(props: &mut Vec<(String, Value)>, name: &str) {
    let at = match props.iter().position(|(key, _)| key == TAGS_PROPERTY) {
        Some(at) => at,
        None => {
            props.push((TAGS_PROPERTY.to_string(), Value::List(Vec::new())));
            props.len() - 1
        }
    };
    if let (_, Value::List(items)) = &mut props[at] {
        let value = Value::String(name.to_string());
        if !items.contains(&value) {
            items.push(value);
        }
    }
}

/// The note's `tags:` frontmatter entries. A scalar `tags:` joins no hub
/// (VAULT.md §7), so it names no rule either.
fn frontmatter_tags(props: &[(String, Value)]) -> impl Iterator<Item = &str> {
    props
        .iter()
        .filter(|(key, _)| key == TAGS_PROPERTY)
        .filter_map(|(_, value)| match value {
            Value::List(items) => Some(items),
            _ => None,
        })
        .flatten()
        .filter_map(|item| match item {
            Value::String(name) => Some(name.as_str()),
            _ => None,
        })
}
