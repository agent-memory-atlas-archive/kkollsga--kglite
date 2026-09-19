//! `okf::structure` — the block model of a note body.
//!
//! One parse of the body with `pulldown-cmark`, kept as the **block
//! skeleton** — every node carrying a byte `Range` into the original body, so
//! a derived node's text is a verbatim slice and never a re-render. Both the
//! structure profile and the link pass read it: list items, table cells, a
//! blockquote's extent and where one paragraph ends are all things a line
//! scanner cannot see, and a second scanner would be a second answer.
//!
//! Deliberately *not* here: link, tag and attachment semantics. Those stay in
//! `okf::links`, which keeps scanning the body's own text (VAULT.md §1.4/§5.1:
//! indented code and HTML blocks are scanned like prose). The two passes agree
//! because they share one heading model — this one — not because they
//! implement the same rules twice.

pub(crate) mod block;
mod constructs;
pub(crate) mod derive;
pub(crate) mod profile;
mod tables;

use block::{BlockKind, Heading, List};
use std::borrow::Cow;
use std::collections::BTreeSet;
use std::ops::Range;

pub(crate) use block::{parse_blocks, BlockTree};
pub(crate) use derive::{derive, Derived, DerivedNode};
pub(crate) use profile::StructureProfile;

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

/// The characters a link, a tag or an attachment is spelled with. Inside an
/// inline code span they are literal text, so they are the ones the mask hides.
const SPELLING: &[u8] = b"[]()!#";

/// The body with every link-, tag- and attachment-spelling character inside an
/// inline code span replaced by NUL (VAULT.md §5.1).
///
/// A code span is rendered literally — `` `[[Note]]` `` is the five characters
/// a reader sees, not a link — but it is *inline* content, so cutting it out of
/// the scan the way a fence or a comment is cut would break the constructs
/// written **around** it: ``[`file.md`](file.md)`` is one link whose display
/// text happens to be code, and a help corpus writes tens of thousands of them.
/// Masking keeps the region contiguous, and because every replaced byte is
/// ASCII the mask is byte-for-byte the same length as the body, so a match's
/// offsets still index the author's own text (which is what the label, the
/// target and the title are read from).
///
/// Only the spelling characters are replaced — the backticks are not among
/// them and stay where the author put them, so the text around a span reads
/// exactly as it did, `#` glued to a closing backtick included. The
/// replacement is NUL because no note writes one, so a masked position can
/// never be mistaken for something the author typed.
pub(crate) fn mask_code_spans<'a>(body: &'a str, tree: &BlockTree) -> Cow<'a, str> {
    if tree.code_spans.is_empty() {
        return Cow::Borrowed(body);
    }
    let mut bytes = body.as_bytes().to_vec();
    for span in &tree.code_spans {
        for byte in &mut bytes[span.clone()] {
            if SPELLING.contains(byte) {
                *byte = 0;
            }
        }
    }
    Cow::Owned(
        String::from_utf8(bytes).expect("only ASCII bytes were replaced, so UTF-8 still holds"),
    )
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
    // A `<!-- kglite … -->` directive is metadata and not prose (VAULT.md
    // §5.8), so it is skipped like a comment: the wikilink in
    // `<!-- kglite address: [[Wells]] -->` is the directive's value and must
    // not also become a plain link from the note.
    for directive in &tree.directives {
        cuts.insert(directive.range.start);
        cuts.insert(directive.range.end);
        skipped.push(directive.range.clone());
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
