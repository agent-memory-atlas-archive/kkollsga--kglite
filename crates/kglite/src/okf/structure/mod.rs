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

// The block tree is complete before its first consumer: the structure pass
// that reads it lands in the next phase, and `block_tests.rs` exercises every
// field meanwhile. Scoped to this module so the rest of okf keeps the lint.
#![allow(dead_code)]

pub(crate) mod block;
