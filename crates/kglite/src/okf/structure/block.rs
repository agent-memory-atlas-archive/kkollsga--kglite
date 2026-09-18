//! The block tree: `parse_blocks(body) -> BlockTree`, a pure function.
//!
//! Every range is a byte range into the `body` that was passed in, so
//! `&body[node.range]` is the verbatim source of that node. Ranges are what
//! this module is *for*: the structure profile derives nodes whose `text` must
//! be the author's own markdown, and a parser that hands back rendered strings
//! cannot provide that.
//!
//! Parser options: **`ENABLE_TABLES` only.**
//! - `ENABLE_GFM` is off because it swallows callouts: `> [!note]\n> body`
//!   loses its marker line entirely, and `> [!warning] Be careful` is not
//!   recognised at all (GitHub alerts have no titles). Callouts are parsed here
//!   from the blockquote's own source range instead.
//! - `ENABLE_WIKILINKS` is off because link semantics live in `okf::links`.
//!   Turning it on changes only the *inline* event stream, and this module
//!   reads inline content by slicing ranges, so it would buy nothing while
//!   adding a second place where wikilink syntax is decided.
//! - `ENABLE_FOOTNOTES` is off: a footnote definition would become its own
//!   block kind and change paragraph boundaries, which no rule asks for yet.

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use regex::Regex;
use std::ops::Range;
use std::sync::OnceLock;

/// One note body's blocks, headings, block ids and comments.
///
/// `blocks` is in document order. A block's `heading` and `inside` are indices
/// into `headings` and `blocks` respectively, so attribution ("which section is
/// this in?") is a lookup rather than a second parse.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct BlockTree {
    pub headings: Vec<Heading>,
    pub blocks: Vec<Block>,
    pub block_ids: Vec<BlockId>,
    /// `%%…%%` spans, in document order. Kept apart from `blocks` because a
    /// comment can be *inline* — inside a paragraph, a list item or a table
    /// cell — so it is a range to exclude from scanning, not a sibling block.
    pub comments: Vec<Range<usize>>,
}

/// An ATX or setext heading.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Heading {
    pub level: u8,
    /// The heading's text with the optional ATX closing `#` sequence removed,
    /// per CommonMark. `### #` therefore has an **empty** `text` — the lone `#`
    /// is a closing sequence, not a title. `raw` keeps it.
    pub text: String,
    /// The verbatim inline source between the ATX opening marks and the end of
    /// the line (closing sequence *not* stripped); for a setext heading, the
    /// content lines above the underline. Equal to `text` in the common case.
    pub raw: String,
    /// This heading's `text` preceded by its ancestors' — the components
    /// Obsidian joins with `#` to address a nested heading (`[[Note#A#B]]`).
    pub path: Vec<String>,
    /// The whole heading line (both lines, for setext), trailing newline
    /// included.
    pub range: Range<usize>,
    /// From the end of the heading line to the start of the next heading of
    /// the same or a higher level, or to the end of the body.
    pub body_range: Range<usize>,
}

/// A block-level node, in document order.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Block {
    pub kind: BlockKind,
    pub range: Range<usize>,
    /// Index into [`BlockTree::headings`] of the heading whose section this
    /// block sits in; `None` before the body's first heading.
    pub heading: Option<usize>,
    /// Index into [`BlockTree::blocks`] of the enclosing container block
    /// (blockquote, list, table); `None` at the top level.
    pub inside: Option<usize>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum BlockKind {
    Paragraph,
    Fence(Fence),
    BlockQuote(BlockQuote),
    /// Only lists that are not themselves inside another list get a block;
    /// nested lists hang off [`ListItem::children`].
    List(List),
    Table(Table),
    /// A `<div>…` block. Its source text is still scanned by `okf::links`
    /// (VAULT.md §1.4) — the range is here so the structure pass can tell
    /// prose from markup.
    HtmlBlock,
    /// A four-space-indented code block. Also still scanned by `okf::links`
    /// (VAULT.md §5.1).
    IndentedCode,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Fence {
    /// The info string verbatim (`python`, `cypher title="x"`, or empty).
    pub info: String,
    /// The info string's first whitespace-delimited token, case preserved.
    pub lang: Option<String>,
    /// The fence's content, without the opening and closing fence lines.
    ///
    /// Caveat for a fence indented inside a list item: the slice keeps the
    /// container's indentation on every line after the first, because the
    /// range spans the source between the first and last content bytes.
    pub code_range: Range<usize>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct BlockQuote {
    pub callout: Option<Callout>,
    /// The quote's body with the callout marker line removed; identical to the
    /// block's `range` when there is no callout. The `>` prefixes of every
    /// line are still in the slice — stripping them is presentation, and the
    /// caller decides whether it wants the source or the rendered text.
    pub inner_text_range: Range<usize>,
}

/// `> [!type]` — Obsidian's callout marker. The kind is arbitrary and
/// lowercased (Obsidian matches its 13 built-ins case-insensitively, and a
/// converter is free to emit its own, e.g. `[!versionadded]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Callout {
    pub kind: String,
    pub title: Option<String>,
    /// `'+'` (starts expanded) or `'-'` (starts folded); `None` for a callout
    /// that is not foldable.
    pub fold: Option<char>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct List {
    pub ordered: bool,
    /// The first number of an ordered list (`3.` → `Some(3)`); `None` when
    /// unordered.
    pub start: Option<u64>,
    pub items: Vec<ListItem>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ListItem {
    pub range: Range<usize>,
    /// The item's own first paragraph — the step text, before any nested list
    /// or fenced block. `None` for an item that opens directly with one.
    pub text_range: Option<Range<usize>>,
    pub children: Vec<List>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Table {
    pub header: Vec<Cell>,
    pub rows: Vec<Vec<Cell>>,
}

/// One table cell. `text` is the raw source, trimmed — `[[Note]]`, `![[x.png]]`
/// and `` `code` `` are left exactly as written, because resolving them is the
/// link pass's job and a cell's value is whatever the author typed.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Cell {
    pub text: String,
    pub range: Range<usize>,
}

/// `^block-id` — Obsidian's addressable anchor (`[[Note#^id]]`).
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct BlockId {
    /// Latin letters, digits and dashes only, per Obsidian. `^my_id` is not a
    /// block id.
    pub id: String,
    /// Index into [`BlockTree::blocks`] of the block the id addresses.
    pub attaches_to: Option<usize>,
    /// The `^id` token itself, caret included.
    pub range: Range<usize>,
    /// True when the id sits alone on its own line — Obsidian's placement for
    /// a table, a code block or a quote, which addresses the block *before*
    /// it. False when it trails a paragraph or a list item, which it addresses
    /// directly. The paragraph an own-line id forms is still in `blocks`, so a
    /// caller assembling section text skips it by this flag.
    pub own_line: bool,
}

fn callout_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\s*>\s*\[!([A-Za-z0-9_-]+)\]([+-])?\s*(.*)$").unwrap())
}

fn block_id_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // Obsidian: "Block identifiers can only consist of Latin letters, numbers,
    // and dashes." The leading space is required — `x^id` is not an anchor.
    RE.get_or_init(|| Regex::new(r"(?:^|\s)(\^([A-Za-z0-9-]+))\s*$").unwrap())
}

/// Parse a note body into its block tree. Pure: no I/O, no configuration, and
/// the same body always yields the same tree.
pub(crate) fn parse_blocks(body: &str) -> BlockTree {
    let mut tree = Walker::default().run(body);
    tree.comments = scan_comments(body, &tree.blocks);
    tree.block_ids = scan_block_ids(body, &tree.blocks);
    tree
}

// ---------------------------------------------------------------------------
// The walk
// ---------------------------------------------------------------------------

/// A list under construction. Lists nest, so these stack; only the outermost
/// one owns a [`Block`].
#[derive(Default)]
struct ListBuilder {
    ordered: bool,
    start: Option<u64>,
    items: Vec<ListItem>,
    open_item: Option<OpenItem>,
}

struct OpenItem {
    range: Range<usize>,
    text_range: Option<Range<usize>>,
    /// Set once the item's leading text is over — at its first child block.
    /// Without it, a nested list's own text would extend the parent item's.
    text_done: bool,
    children: Vec<List>,
}

/// A code block under construction. The info string comes from the source, the
/// content range from the `Text` events, which exclude the fence lines.
struct CodeBuilder {
    fenced: bool,
    range: Option<Range<usize>>,
}

#[derive(Default)]
struct TableBuilder {
    header: Vec<Cell>,
    rows: Vec<Vec<Cell>>,
    row: Vec<Cell>,
    in_head: bool,
}

#[derive(Default)]
struct Walker {
    headings: Vec<Heading>,
    /// (level, index) of the headings still open, outermost first.
    heading_stack: Vec<(u8, usize)>,
    blocks: Vec<Block>,
    /// Indices of blocks whose `End` event has not arrived yet.
    open_blocks: Vec<usize>,
    lists: Vec<ListBuilder>,
    table: Option<TableBuilder>,
    code: Option<CodeBuilder>,
}

impl Walker {
    fn run(mut self, body: &str) -> BlockTree {
        let mut opts = Options::empty();
        opts.insert(Options::ENABLE_TABLES);
        for (event, range) in Parser::new_ext(body, opts).into_offset_iter() {
            match event {
                Event::Start(tag) => self.start(body, tag, range),
                Event::End(tag) => self.end(body, tag, range),
                Event::Text(_) if self.code.is_some() => self.extend_code(range),
                Event::Rule => self.item_text_done(),
                _ => self.extend_item_text(range),
            }
        }
        BlockTree {
            headings: self.headings,
            blocks: self.blocks,
            ..BlockTree::default()
        }
    }

    /// Reserve a block slot at `Start`, so that document order and the indices
    /// children use for `inside` are both settled before the block's contents
    /// are known.
    fn open(&mut self, range: Range<usize>) -> usize {
        let index = self.blocks.len();
        self.blocks.push(Block {
            kind: BlockKind::Paragraph,
            range,
            heading: self.heading_stack.last().map(|&(_, i)| i),
            inside: self.open_blocks.last().copied(),
        });
        self.open_blocks.push(index);
        index
    }

    fn close(&mut self, kind: BlockKind) {
        if let Some(index) = self.open_blocks.pop() {
            self.blocks[index].kind = kind;
        }
    }

    fn start(&mut self, body: &str, tag: Tag<'_>, range: Range<usize>) {
        match tag {
            Tag::Paragraph => {
                self.claim_item_text(&range);
                self.open(range);
            }
            Tag::Heading { level, .. } => {
                self.item_text_done();
                self.push_heading(body, level as u8, range);
            }
            Tag::CodeBlock(kind) => {
                self.item_text_done();
                self.code = Some(CodeBuilder {
                    fenced: matches!(kind, CodeBlockKind::Fenced(_)),
                    range: None,
                });
                self.open(range);
            }
            Tag::BlockQuote(_) | Tag::HtmlBlock => {
                self.item_text_done();
                self.open(range);
            }
            Tag::Table(_) => {
                self.item_text_done();
                self.table = Some(TableBuilder::default());
                self.open(range);
            }
            Tag::List(first) => {
                self.item_text_done();
                if self.lists.is_empty() {
                    self.open(range);
                }
                self.lists.push(ListBuilder {
                    ordered: first.is_some(),
                    start: first,
                    ..ListBuilder::default()
                });
            }
            Tag::Item => {
                if let Some(list) = self.lists.last_mut() {
                    list.open_item = Some(OpenItem {
                        range,
                        text_range: None,
                        text_done: false,
                        children: Vec::new(),
                    });
                }
            }
            Tag::TableHead => {
                if let Some(t) = self.table.as_mut() {
                    t.in_head = true;
                }
            }
            Tag::TableRow => {
                if let Some(t) = self.table.as_mut() {
                    t.row = Vec::new();
                }
            }
            Tag::TableCell => {
                let cell = trimmed_cell(body, range);
                if let Some(t) = self.table.as_mut() {
                    t.row.push(cell);
                }
            }
            // Emphasis, links, images: inline, and part of a tight list item's
            // text — their range spans the whole construct, including the
            // syntax the `Text` events drop.
            _ => self.extend_item_text(range),
        }
    }

    fn end(&mut self, body: &str, tag: TagEnd, range: Range<usize>) {
        match tag {
            TagEnd::Paragraph => self.close(BlockKind::Paragraph),
            TagEnd::HtmlBlock => self.close(BlockKind::HtmlBlock),
            TagEnd::CodeBlock => self.close_code_block(body, &range),
            TagEnd::BlockQuote(_) => {
                let kind = block_quote(body, &range);
                self.close(kind);
            }
            TagEnd::Table => self.close_table(),
            TagEnd::List(_) => self.close_list(),
            TagEnd::Item => self.close_item(range),
            TagEnd::TableHead => {
                if let Some(t) = self.table.as_mut() {
                    t.header = std::mem::take(&mut t.row);
                    t.in_head = false;
                }
            }
            TagEnd::TableRow => {
                if let Some(t) = self.table.as_mut() {
                    let row = std::mem::take(&mut t.row);
                    if !t.in_head {
                        t.rows.push(row);
                    }
                }
            }
            _ => {}
        }
    }

    fn extend_code(&mut self, range: Range<usize>) {
        if let Some(code) = self.code.as_mut() {
            code.range = Some(match code.range.take() {
                Some(r) => r.start..range.end,
                None => range,
            });
        }
    }

    /// Grow the open list item's text span. Only a *tight* item needs this:
    /// its text arrives with no enclosing `Paragraph` event to give it a range.
    fn extend_item_text(&mut self, range: Range<usize>) {
        let Some(item) = self.lists.last_mut().and_then(|l| l.open_item.as_mut()) else {
            return;
        };
        if item.text_done {
            return;
        }
        item.text_range = Some(match item.text_range.take() {
            // `max`, not the new end: an inline construct's own `Start` spans
            // the whole of it, and the `Text` events nested inside it end
            // earlier — `[Docs](url)` would otherwise shrink back to `[Docs`.
            Some(r) => r.start..r.end.max(range.end),
            None => range,
        });
    }

    fn claim_item_text(&mut self, range: &Range<usize>) {
        if let Some(item) = self.lists.last_mut().and_then(|l| l.open_item.as_mut()) {
            if !item.text_done {
                item.text_range = Some(range.clone());
                item.text_done = true;
            }
        }
    }

    fn item_text_done(&mut self) {
        if let Some(item) = self.lists.last_mut().and_then(|l| l.open_item.as_mut()) {
            item.text_done = true;
        }
    }

    fn push_heading(&mut self, body: &str, level: u8, range: Range<usize>) {
        while let Some(&(open_level, index)) = self.heading_stack.last() {
            if open_level < level {
                break;
            }
            self.headings[index].body_range.end = range.start;
            self.heading_stack.pop();
        }
        let (text, raw) = heading_source(&body[range.clone()]);
        let mut path: Vec<String> = self
            .heading_stack
            .iter()
            .map(|&(_, i)| self.headings[i].text.clone())
            .collect();
        path.push(text.clone());
        self.heading_stack.push((level, self.headings.len()));
        self.headings.push(Heading {
            level,
            text,
            raw,
            path,
            body_range: range.end..body.len(),
            range,
        });
    }

    fn close_code_block(&mut self, body: &str, range: &Range<usize>) {
        let built = self.code.take();
        let code_range = built
            .as_ref()
            .and_then(|c| c.range.clone())
            .unwrap_or(range.end..range.end);
        let kind = if built.is_some_and(|c| c.fenced) {
            let info = fence_info(&body[range.clone()]);
            let lang = info.split_whitespace().next().map(str::to_string);
            BlockKind::Fence(Fence {
                info,
                lang,
                code_range,
            })
        } else {
            BlockKind::IndentedCode
        };
        self.close(kind);
    }

    fn close_table(&mut self) {
        let built = self.table.take().unwrap_or_default();
        self.close(BlockKind::Table(Table {
            header: built.header,
            rows: built.rows,
        }));
    }

    fn close_list(&mut self) {
        let Some(built) = self.lists.pop() else {
            return;
        };
        let list = List {
            ordered: built.ordered,
            start: built.start,
            items: built.items,
        };
        match self.lists.last_mut().and_then(|l| l.open_item.as_mut()) {
            Some(parent) => parent.children.push(list),
            None => self.close(BlockKind::List(list)),
        }
    }

    fn close_item(&mut self, range: Range<usize>) {
        let Some(list) = self.lists.last_mut() else {
            return;
        };
        let item = match list.open_item.take() {
            Some(open) => ListItem {
                range: open.range,
                text_range: open.text_range,
                children: open.children,
            },
            None => ListItem {
                range,
                text_range: None,
                children: Vec::new(),
            },
        };
        list.items.push(item);
    }
}

// ---------------------------------------------------------------------------
// Source-level rules the parser does not implement
// ---------------------------------------------------------------------------

/// Split a heading line's source into `(text, raw)`.
///
/// CommonMark's ATX closing sequence is the reason these differ: in `### #`
/// the lone `#` closes the heading rather than titling it, so `text` is empty
/// while `raw` keeps the `#` for a caller that would rather show something.
fn heading_source(src: &str) -> (String, String) {
    let line = src.trim_end();
    let atx = line.trim_start();
    if !atx.starts_with('#') {
        // Setext: the content is everything above the `===`/`---` underline.
        let content = match line.rfind('\n') {
            Some(cut) => &line[..cut],
            None => line,
        };
        let text = content.trim().to_string();
        return (text.clone(), text);
    }
    let hashes = atx.len() - atx.trim_start_matches('#').len();
    let raw = atx[hashes..].trim().to_string();
    (strip_closing_sequence(&raw), raw)
}

/// Drop a trailing run of `#`s that is preceded by whitespace or is the whole
/// text — CommonMark's optional closing sequence. `C#` keeps its `#`.
fn strip_closing_sequence(raw: &str) -> String {
    let trimmed = raw.trim_end();
    let before = trimmed.trim_end_matches('#');
    if before.len() == trimmed.len() {
        return trimmed.to_string();
    }
    if before.is_empty() || before.ends_with(char::is_whitespace) {
        before.trim_end().to_string()
    } else {
        trimmed.to_string()
    }
}

/// The info string of a fenced code block: whatever follows the opening run of
/// backticks or tildes on the fence's first line.
fn fence_info(src: &str) -> String {
    let first = src.trim_start().lines().next().unwrap_or("");
    first
        .trim_start_matches(['`', '~'])
        .trim()
        .replace('\r', "")
}

/// A blockquote's callout marker and body extent, read from its first line.
fn block_quote(body: &str, range: &Range<usize>) -> BlockKind {
    let src = &body[range.clone()];
    let first_line = src.lines().next().unwrap_or("");
    let Some(caps) = callout_re().captures(first_line) else {
        return BlockKind::BlockQuote(BlockQuote {
            callout: None,
            inner_text_range: range.clone(),
        });
    };
    let title = caps[3].trim();
    let callout = Callout {
        kind: caps[1].to_ascii_lowercase(),
        title: (!title.is_empty()).then(|| title.to_string()),
        fold: caps.get(2).and_then(|m| m.as_str().chars().next()),
    };
    // The marker line is the callout's own declaration, not its text; the body
    // starts on the line below it (and is empty for a marker-only callout).
    let after_marker = match src.find('\n') {
        Some(nl) => range.start + nl + 1,
        None => range.end,
    };
    BlockKind::BlockQuote(BlockQuote {
        callout: Some(callout),
        inner_text_range: after_marker.min(range.end)..range.end,
    })
}

fn trimmed_cell(body: &str, range: Range<usize>) -> Cell {
    let raw = &body[range.clone()];
    let start = range.start + (raw.len() - raw.trim_start().len());
    let end = (range.end - (raw.len() - raw.trim_end().len())).max(start);
    Cell {
        text: body[start..end].to_string(),
        range: start..end,
    }
}

/// `%%…%%` spans (Obsidian comments), inline or spanning lines.
///
/// Not the parser's concern — `%%` is ordinary text to CommonMark — so this is
/// a scan of the raw body. Spans that open inside a code block are skipped:
/// `%%` in a shell snippet is code, not a comment. The shortest closing `%%`
/// wins, matching Obsidian's non-greedy behaviour.
fn scan_comments(body: &str, blocks: &[Block]) -> Vec<Range<usize>> {
    let code: Vec<&Range<usize>> = blocks
        .iter()
        .filter(|b| matches!(b.kind, BlockKind::Fence(_) | BlockKind::IndentedCode))
        .map(|b| &b.range)
        .collect();
    let mut out = Vec::new();
    let mut cursor = 0;
    while let Some(offset) = body[cursor..].find("%%") {
        let start = cursor + offset;
        let Some(close) = body[start + 2..].find("%%") else {
            break;
        };
        let end = start + 2 + close + 2;
        if !code.iter().any(|r| r.contains(&start)) {
            out.push(start..end);
        }
        cursor = end;
    }
    out
}

/// `^block-id` anchors, in the three placements Obsidian allows: trailing a
/// paragraph, trailing a list item, or alone on a line after a block that
/// cannot carry one itself (table, code block, quote).
fn scan_block_ids(body: &str, blocks: &[Block]) -> Vec<BlockId> {
    let mut out = Vec::new();
    for (index, block) in blocks.iter().enumerate() {
        match &block.kind {
            BlockKind::Paragraph => {
                let Some(found) = trailing_block_id(body, &block.range) else {
                    continue;
                };
                let own_line = body[block.range.clone()].trim() == &body[found.range.clone()];
                let attaches_to = if own_line {
                    preceding_sibling(blocks, index)
                } else {
                    Some(index)
                };
                out.push(BlockId {
                    attaches_to,
                    own_line,
                    ..found
                });
            }
            BlockKind::List(list) => collect_item_ids(body, list, index, &mut out),
            _ => {}
        }
    }
    out.sort_by_key(|b| b.range.start);
    out
}

fn collect_item_ids(body: &str, list: &List, block: usize, out: &mut Vec<BlockId>) {
    for item in &list.items {
        if let Some(range) = &item.text_range {
            if let Some(found) = trailing_block_id(body, range) {
                out.push(BlockId {
                    attaches_to: Some(block),
                    own_line: false,
                    ..found
                });
            }
        }
        for child in &item.children {
            collect_item_ids(body, child, block, out);
        }
    }
}

/// The nearest preceding block at the same nesting level — what an own-line
/// `^id` addresses.
fn preceding_sibling(blocks: &[Block], index: usize) -> Option<usize> {
    blocks[..index]
        .iter()
        .enumerate()
        .rev()
        .find(|(_, b)| b.inside == blocks[index].inside)
        .map(|(i, _)| i)
}

/// A `^id` at the end of `range`'s last non-blank line, with `attaches_to` and
/// `own_line` left for the caller to decide.
fn trailing_block_id(body: &str, range: &Range<usize>) -> Option<BlockId> {
    let src = body[range.clone()].trim_end();
    let caps = block_id_re().captures(src)?;
    let token = caps.get(1)?;
    Some(BlockId {
        id: caps[2].to_string(),
        attaches_to: None,
        range: range.start + token.start()..range.start + token.end(),
        own_line: false,
    })
}

#[cfg(test)]
#[path = "block_tests.rs"]
mod block_tests;
