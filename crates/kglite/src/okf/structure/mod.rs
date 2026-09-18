//! `okf::structure` — the block model of a note body.
//!
//! The link pass (`okf::links`) reads a body one line at a time with a single
//! `in_fence` boolean, which is enough for links and tags and nothing else: it
//! has no notion of a list item, a table cell, a blockquote's extent, or where
//! one paragraph ends and the next begins. The structure profile needs all of
//! those, so this module parses the body once with `pulldown-cmark` and keeps
//! only the **block skeleton** — every node carrying a byte `Range` into the
//! original body, so a derived node's text is a verbatim slice and never a
//! re-render.
//!
//! Deliberately *not* here: link, tag and attachment semantics. Those stay in
//! `okf::links`, which keeps scanning the body's own text (VAULT.md §1.4/§5.1:
//! indented code and HTML blocks are scanned like prose). The two passes agree
//! because they share one heading model — this one — not because they
//! implement the same rules twice.

pub(crate) mod block;

use block::{BlockKind, Heading, List};
use std::collections::BTreeSet;
use std::ops::Range;

pub(crate) use block::{parse_blocks, BlockTree};

/// The heading whose section `offset` sits in, or `None` before the body's
/// first heading.
///
/// A heading line belongs to **its own** section: a link written on the
/// `## See also [[Alice]]` line carries `See also`, the same section as the
/// links below it (VAULT.md §5.4). Headings are in document order, so this is
/// a binary search rather than a scan.
pub(crate) fn heading_at(tree: &BlockTree, offset: usize) -> Option<&Heading> {
    let after = tree.headings.partition_point(|h| h.range.start <= offset);
    (after > 0).then(|| &tree.headings[after - 1])
}

/// The body regions a text scanner may read, in document order, with fenced
/// code blocks and `%%comments%%` removed (VAULT.md §5.1, §5.7).
///
/// Every region is a contiguous slice of one construct's source — one
/// heading line, one paragraph, one list item, one table cell — so a regex run
/// over it can span a hard-wrapped line without ever spanning a **paragraph**
/// boundary, and a match's section is decided once for the whole region.
/// Indented code and HTML blocks are deliberately still regions: VAULT.md
/// §1.4/§5.1 promise their links are read like prose.
pub(crate) fn scan_regions(body: &str, tree: &BlockTree) -> Vec<Range<usize>> {
    let mut cuts: BTreeSet<usize> = BTreeSet::from([0, body.len()]);
    // A region that is skipped rather than scanned. Its own bounds are cuts
    // too, so every interval below is either wholly inside one or wholly out.
    let mut skipped: Vec<Range<usize>> = Vec::new();
    for heading in &tree.headings {
        cuts.insert(heading.range.start);
        cuts.insert(heading.range.end);
    }
    for block in &tree.blocks {
        cuts.insert(block.range.start);
        cuts.insert(block.range.end);
        match &block.kind {
            BlockKind::Fence(_) => skipped.push(block.range.clone()),
            BlockKind::List(list) => list_cuts(list, &mut cuts),
            BlockKind::Table(table) => {
                for cell in table.header.iter().chain(table.rows.iter().flatten()) {
                    cuts.insert(cell.range.start);
                    cuts.insert(cell.range.end);
                }
            }
            _ => {}
        }
    }
    for comment in &tree.comments {
        cuts.insert(comment.start);
        cuts.insert(comment.end);
        skipped.push(comment.clone());
    }
    let cuts: Vec<usize> = cuts.into_iter().collect();
    cuts.windows(2)
        .map(|w| w[0]..w[1])
        .filter(|r| {
            r.start < r.end && !skipped.iter().any(|s| s.start <= r.start && r.end <= s.end)
        })
        .collect()
}

/// Cut at every list item, including nested ones, so one item's text can never
/// run into the next item's.
fn list_cuts(list: &List, cuts: &mut BTreeSet<usize>) {
    for item in &list.items {
        cuts.insert(item.range.start);
        cuts.insert(item.range.end);
        for child in &item.children {
            list_cuts(child, cuts);
        }
    }
}
