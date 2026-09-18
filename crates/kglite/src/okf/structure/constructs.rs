//! The constructs a note's body names inside a section: callouts, fenced
//! examples and ordered lists (VAULT.md §7.1 `callouts:`, `code_fences:`,
//! `ordered_lists:`).
//!
//! Each reads [`BlockTree::blocks`] directly rather than the chunker's
//! grouping: a chunk is a *span* of prose and these are single blocks, so the
//! two passes see the same body through different lenses and neither consumes
//! the other's material — a caption paragraph stays in its chunk, and a
//! callout's prose is both a `Note` node and part of the chunk it sits in.
//!
//! Ids are suffixes, like everything in [`super::derive`]: the note's own id is
//! not settled until `resolve_ids` has run.

use super::block::{Block, BlockKind, BlockTree, List};
use super::derive::{Derived, DerivedEdge, DerivedNode, IdSpace};
use super::profile::{CalloutRule, FenceRule, OrderedListRule};
use crate::datatypes::values::Value;
use std::collections::{BTreeMap, HashMap};

/// What every construct rule needs of the note it is reading.
pub(super) struct Ctx<'a> {
    pub body: &'a str,
    pub tree: &'a BlockTree,
    /// Section suffix per heading index — `None` when `sections:` is not
    /// declared, in which case every construct hangs off the note itself.
    pub sections: Option<&'a [String]>,
    /// `title (the enclosing section's title, or the note's)` needs the note's.
    pub note_title: &'a str,
}

/// Where a block sits: the section node it belongs to (when there is one) and
/// that section's path and title, which every derived node carries.
struct Place {
    section: Option<String>,
    heading_path: Vec<String>,
    section_title: Option<String>,
}

impl Ctx<'_> {
    fn place(&self, block: &Block) -> Place {
        Place {
            section: self
                .sections
                .and_then(|s| block.heading.map(|h| s[h].clone())),
            heading_path: block
                .heading
                .map(|h| self.tree.headings[h].path.clone())
                .unwrap_or_default(),
            section_title: block.heading.map(|h| self.tree.headings[h].text.clone()),
        }
    }
}

/// `<n>` for the next node of one kind under `parent`, counting from 1
/// (VAULT.md §7.1). `ordinal` is `n - 1`.
fn bump(counters: &mut BTreeMap<String, usize>, parent: &str) -> usize {
    let count = counters.entry(parent.to_string()).or_insert(0);
    *count += 1;
    *count
}

/// Claim `<parent>~<kind><n>`, warning when a second construct wants it.
///
/// A duplicate is only reachable through a `^block-id` that already named
/// something, so the message points at the same fix the section one does.
fn claim(ids: &mut IdSpace, wanted: String, out: &mut Derived) -> String {
    let (suffix, duplicate) = ids.claim(&wanted);
    if duplicate {
        out.warnings.push(format!(
            "duplicate derived id `{wanted}`: the second one takes `{suffix}`"
        ));
    }
    suffix
}

// ---------------------------------------------------------------------------
// `callouts:`
// ---------------------------------------------------------------------------

/// One node per callout (VAULT.md §7.1 `callouts:`, §5.7).
///
/// The edge comes from the enclosing **callout** where callouts nest, from the
/// enclosing section otherwise, and from the note where there is no section.
/// `section_id` names the section either way: a nested callout is inside
/// another callout *and* inside the same section.
pub(super) fn derive_callouts(
    ctx: &Ctx<'_>,
    rule: &CalloutRule,
    ids: &mut IdSpace,
    out: &mut Derived,
) {
    // Block index → the suffix that block's callout took, so a nested one can
    // find its parent. Blocks are in document order and a container opens
    // before its contents, so a parent is always already here.
    let mut minted: HashMap<usize, String> = HashMap::new();
    let mut counters: BTreeMap<String, usize> = BTreeMap::new();
    for (index, block) in ctx.tree.blocks.iter().enumerate() {
        let BlockKind::BlockQuote(quote) = &block.kind else {
            continue;
        };
        let Some(callout) = &quote.callout else {
            continue;
        };
        let place = ctx.place(block);
        let parent = enclosing_callout(ctx.tree, &minted, block);
        let container = parent.or_else(|| place.section.clone());
        let prefix = container.clone().unwrap_or_default();
        let n = bump(&mut counters, &prefix);
        let suffix = claim(ids, format!("{prefix}~note{n}"), out);

        out.edges.push(DerivedEdge {
            conn_type: rule.edge.clone(),
            source: container,
            target: suffix.clone(),
        });
        let mut props = vec![
            ("kind".to_string(), Value::String(callout.kind.clone())),
            ("ordinal".to_string(), Value::Int64(n as i64 - 1)),
        ];
        if let Some(title) = &callout.title {
            props.push(("title".to_string(), Value::String(title.clone())));
        }
        if let Some(fold) = callout.fold {
            props.push(("fold".to_string(), Value::String(fold.to_string())));
        }
        out.nodes.push(DerivedNode {
            suffix: suffix.clone(),
            label: rule.label.clone(),
            section: place.section,
            heading_path: place.heading_path,
            section_title: place.section_title,
            text: Some(strip_quote_markers(
                &ctx.body[quote.inner_text_range.clone()],
                quote_depth(ctx.tree, index),
            )),
            props,
        });
        minted.insert(index, suffix);
    }
}

/// The suffix of the nearest enclosing callout, if any. Walks `inside`, so a
/// callout inside a list inside a callout still finds it.
fn enclosing_callout(
    tree: &BlockTree,
    minted: &HashMap<usize, String>,
    block: &Block,
) -> Option<String> {
    let mut at = block.inside;
    while let Some(index) = at {
        if let Some(suffix) = minted.get(&index) {
            return Some(suffix.clone());
        }
        at = tree.blocks[index].inside;
    }
    None
}

/// How many blockquotes deep a block sits, itself included.
fn quote_depth(tree: &BlockTree, block: usize) -> usize {
    let mut depth = 1;
    let mut at = tree.blocks[block].inside;
    while let Some(index) = at {
        if matches!(tree.blocks[index].kind, BlockKind::BlockQuote(_)) {
            depth += 1;
        }
        at = tree.blocks[index].inside;
    }
    depth
}

/// Drop this callout's own `>` markers from every line (VAULT.md §7.1: "the
/// callout body verbatim with the `>` markers stripped").
///
/// Exactly `depth` of them, no more: a callout nested two deep is written
/// `> > text`, and both markers are quote syntax from *its* point of view —
/// while from its parent's only the outer one is, so the parent's own `text`
/// keeps the `> [!tip]` line that says a callout is nested inside it.
fn strip_quote_markers(text: &str, depth: usize) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let mut rest = line;
        for _ in 0..depth {
            let trimmed = rest.trim_start_matches([' ', '\t']);
            let Some(after) = trimmed.strip_prefix('>') else {
                break;
            };
            // A single space after the marker is the marker's own padding;
            // anything further is the author's indentation.
            rest = after.strip_prefix(' ').unwrap_or(after);
        }
        out.push_str(rest);
    }
    out.trim_end().to_string()
}

// ---------------------------------------------------------------------------
// `code_fences:`
// ---------------------------------------------------------------------------

/// One node per qualifying fenced block (VAULT.md §7.1 `code_fences:`).
pub(super) fn derive_fences(ctx: &Ctx<'_>, rule: &FenceRule, ids: &mut IdSpace, out: &mut Derived) {
    let mut counters: BTreeMap<String, usize> = BTreeMap::new();
    for (index, block) in ctx.tree.blocks.iter().enumerate() {
        let BlockKind::Fence(fence) = &block.kind else {
            continue;
        };
        let lang = fence.lang.as_ref().map(|l| l.to_lowercase());
        if let Some(declared) = &rule.langs {
            match &lang {
                Some(lang) if declared.iter().any(|d| d == lang) => {}
                _ => continue,
            }
        }
        let place = ctx.place(block);
        let prefix = place.section.clone().unwrap_or_default();
        let n = bump(&mut counters, &prefix);
        let suffix = claim(ids, format!("{prefix}~example{n}"), out);

        out.edges.push(DerivedEdge {
            conn_type: rule.edge.clone(),
            source: place.section.clone(),
            target: suffix.clone(),
        });
        let mut props = vec![
            (
                "code".to_string(),
                Value::String(dedent(
                    &ctx.body[fence.code_range.clone()],
                    indent_of(ctx.body, block.range.start),
                )),
            ),
            ("ordinal".to_string(), Value::Int64(n as i64 - 1)),
        ];
        if let Some(lang) = lang {
            props.push(("lang".to_string(), Value::String(lang)));
        }
        if let Some(caption) = caption_above(ctx, index) {
            props.push(("caption".to_string(), Value::String(caption)));
        }
        out.nodes.push(DerivedNode {
            suffix,
            label: rule.label.clone(),
            section: place.section,
            heading_path: place.heading_path,
            section_title: place.section_title,
            // `code` is the fence's own property (VAULT.md §7.1); an Example
            // carries no `text`, so no `embed_text` is rendered for one.
            text: None,
            props,
        });
    }
}

/// How many bytes of whitespace precede `offset` on its own line — the
/// container indentation a fence inside a list item sits at.
fn indent_of(body: &str, offset: usize) -> usize {
    let start = body[..offset].rfind('\n').map_or(0, |n| n + 1);
    body[start..offset]
        .bytes()
        .take_while(|b| matches!(b, b' ' | b'\t'))
        .count()
}

/// Remove up to `indent` leading spaces or tabs from every line.
///
/// `Fence::code_range` spans the source, so a fence inside a list item keeps
/// that item's indentation on every line after the first. Removing *at most*
/// the container's own indentation keeps the code's own relative indentation
/// exactly as written, and a top-level fence (`indent == 0`) is untouched.
fn dedent(code: &str, indent: usize) -> String {
    if indent == 0 {
        return code.to_string();
    }
    let mut out = String::with_capacity(code.len());
    for line in code.split_inclusive('\n') {
        let strip = line
            .bytes()
            .take_while(|b| matches!(b, b' ' | b'\t'))
            .count()
            .min(indent);
        out.push_str(&line[strip..]);
    }
    out
}

/// The paragraph immediately above a fence, when its text ends with `:`
/// (VAULT.md §7.1 `code_fences:`). It stays in its chunk as well: nothing is
/// taken out of the prose.
fn caption_above(ctx: &Ctx<'_>, fence: usize) -> Option<String> {
    let previous = ctx.tree.blocks.get(fence.checked_sub(1)?)?;
    if !matches!(previous.kind, BlockKind::Paragraph)
        || previous.inside != ctx.tree.blocks[fence].inside
    {
        return None;
    }
    let text = ctx.body[previous.range.clone()].trim();
    text.ends_with(':').then(|| text.to_string())
}

// ---------------------------------------------------------------------------
// `ordered_lists:`
// ---------------------------------------------------------------------------

/// A container node per qualifying top-level ordered list, and a node per item
/// (VAULT.md §7.1 `ordered_lists:`).
///
/// Only outermost lists carry a [`Block`] — a nested one hangs off its item —
/// so every `BlockKind::List` here is already "top-level" in the spec's sense
/// of "not nested inside another list item".
pub(super) fn derive_lists(
    ctx: &Ctx<'_>,
    rule: &OrderedListRule,
    ids: &mut IdSpace,
    out: &mut Derived,
) {
    let mut counters: BTreeMap<String, usize> = BTreeMap::new();
    for block in &ctx.tree.blocks {
        let BlockKind::List(list) = &block.kind else {
            continue;
        };
        if !list.ordered || list.items.len() < rule.min_items {
            continue;
        }
        let place = ctx.place(block);
        if let Some(matcher) = &rule.under_heading {
            // A list above the first heading sits under no heading at all, so
            // a narrowing that names one cannot reach it.
            match &place.section_title {
                Some(title) if matcher.is_match(title) => {}
                _ => continue,
            }
        }
        let prefix = place.section.clone().unwrap_or_default();
        let n = bump(&mut counters, &prefix);
        let suffix = claim(ids, format!("{prefix}~list{n}"), out);

        out.edges.push(DerivedEdge {
            conn_type: format!("HAS_{}", crate::okf::links::upper_snake(&rule.container)),
            source: place.section.clone(),
            target: suffix.clone(),
        });
        out.nodes.push(DerivedNode {
            suffix: suffix.clone(),
            label: rule.container.clone(),
            section: place.section.clone(),
            heading_path: place.heading_path.clone(),
            section_title: place.section_title.clone(),
            text: None,
            props: vec![
                (
                    "title".to_string(),
                    Value::String(
                        place
                            .section_title
                            .clone()
                            .unwrap_or_else(|| ctx.note_title.to_string()),
                    ),
                ),
                ("ordinal".to_string(), Value::Int64(n as i64 - 1)),
                // The steps the container itself holds — the `edge` rows that
                // leave it. A sub-step is its own step's.
                (
                    "step_count".to_string(),
                    Value::Int64(list.items.len() as i64),
                ),
            ],
        });
        StepWalk {
            ctx,
            rule,
            place: &place,
        }
        .emit(list, &suffix, 0, ids, out);
    }
}

/// One node per item, and the same again for an ordered list nested inside one
/// (VAULT.md §7.1: "nested = sub-steps").
///
/// The rule and the enclosing section are fixed for the whole walk; only the
/// parent and the level change as it recurses.
struct StepWalk<'a> {
    ctx: &'a Ctx<'a>,
    rule: &'a OrderedListRule,
    place: &'a Place,
}

impl StepWalk<'_> {
    fn emit(&self, list: &List, parent: &str, level: usize, ids: &mut IdSpace, out: &mut Derived) {
        let mut previous: Option<String> = None;
        for (index, item) in list.items.iter().enumerate() {
            let suffix = claim(ids, format!("{parent}~step{}", index + 1), out);
            out.edges.push(DerivedEdge {
                conn_type: self.rule.edge.clone(),
                source: Some(parent.to_string()),
                target: suffix.clone(),
            });
            if let Some(previous) = previous.replace(suffix.clone()) {
                out.edges.push(DerivedEdge {
                    conn_type: self.rule.next.clone(),
                    source: Some(previous),
                    target: suffix.clone(),
                });
            }
            out.nodes.push(DerivedNode {
                suffix: suffix.clone(),
                label: self.rule.label.clone(),
                section: self.place.section.clone(),
                heading_path: self.place.heading_path.clone(),
                section_title: self.place.section_title.clone(),
                text: Some(step_text(self.ctx.body, item)),
                props: vec![
                    ("ordinal".to_string(), Value::Int64(index as i64)),
                    ("level".to_string(), Value::Int64(level as i64)),
                ],
            });
            for child in &item.children {
                // An *unordered* list nested in a step is the step's own
                // content, never a procedure (VAULT.md §7.1).
                if child.ordered {
                    self.emit(child, &suffix, level + 1, ids, out);
                }
            }
        }
    }
}

/// "the item's own content, excluding any list nested inside it" (VAULT.md
/// §7.1) — from the item's text, which starts after its `1.` marker, to
/// whichever nested list opens first.
fn step_text(body: &str, item: &super::block::ListItem) -> String {
    let start = item
        .text_range
        .as_ref()
        .map_or(item.range.start, |range| range.start);
    let end = item
        .children
        .iter()
        .filter_map(|child| child.items.first())
        .map(|first| first.range.start)
        .min()
        .unwrap_or(item.range.end)
        .max(start);
    body[start..end].trim_end().to_string()
}

#[cfg(test)]
#[path = "constructs_tests.rs"]
mod constructs_tests;
