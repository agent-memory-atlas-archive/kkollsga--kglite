//! Block-tree fixtures — one per construct, plus the golden vault bodies.
//!
//! Two rules run through every case. **Ranges must slice back to the source**:
//! a derived node's text is `&body[range]`, so a range that is merely "about
//! right" is a wrong answer that no rendering test would catch. And **the
//! heading model is shared** with `okf::links` — the golden-body case pins the
//! tree's heading count against what the link pass counts today, which is the
//! consistency the structure pass is allowed to assume.

use super::*;
use crate::okf::structure::scan_regions;
use std::path::{Path, PathBuf};

fn slice<'a>(body: &'a str, range: &Range<usize>) -> &'a str {
    &body[range.clone()]
}

fn kinds(tree: &BlockTree) -> Vec<&'static str> {
    tree.blocks
        .iter()
        .map(|b| match b.kind {
            BlockKind::Paragraph => "para",
            BlockKind::Fence(_) => "fence",
            BlockKind::BlockQuote(_) => "quote",
            BlockKind::List(_) => "list",
            BlockKind::Table(_) => "table",
            BlockKind::HtmlBlock => "html",
            BlockKind::IndentedCode => "indented",
        })
        .collect()
}

fn only_list(tree: &BlockTree) -> &List {
    tree.blocks
        .iter()
        .find_map(|b| match &b.kind {
            BlockKind::List(l) => Some(l),
            _ => None,
        })
        .expect("a list block")
}

fn only_quote(tree: &BlockTree) -> &BlockQuote {
    tree.blocks
        .iter()
        .find_map(|b| match &b.kind {
            BlockKind::BlockQuote(q) => Some(q),
            _ => None,
        })
        .expect("a blockquote block")
}

#[test]
fn headings_carry_level_path_and_a_body_range_that_slices_back() {
    let body = "# Top\n\nintro\n\n## A\n\nunder a\n\n### A1\n\ndeep\n\n## B\n\nunder b\n";
    let tree = parse_blocks(body);

    let levels: Vec<u8> = tree.headings.iter().map(|h| h.level).collect();
    assert_eq!(
        levels,
        vec![1, 2, 3, 2],
        "H1 is level 1, not a 0-based index"
    );

    let paths: Vec<Vec<String>> = tree.headings.iter().map(|h| h.path.clone()).collect();
    assert_eq!(
        paths,
        vec![
            vec!["Top"],
            vec!["Top", "A"],
            vec!["Top", "A", "A1"],
            vec!["Top", "B"],
        ],
        "a nested heading's path is what Obsidian joins with `#`"
    );

    assert_eq!(slice(body, &tree.headings[0].range), "# Top\n");
    // `## A` closes at `## B`, and takes its `### A1` subsection with it.
    assert_eq!(
        slice(body, &tree.headings[1].body_range),
        "\nunder a\n\n### A1\n\ndeep\n\n"
    );
    assert_eq!(slice(body, &tree.headings[2].body_range), "\ndeep\n\n");
    assert_eq!(slice(body, &tree.headings[3].body_range), "\nunder b\n");
}

#[test]
fn duplicate_heading_texts_differ_by_path() {
    let body = "## A\n\n### Notes\n\nx\n\n## B\n\n### Notes\n\ny\n";
    let tree = parse_blocks(body);
    let notes: Vec<Vec<String>> = tree
        .headings
        .iter()
        .filter(|h| h.text == "Notes")
        .map(|h| h.path.clone())
        .collect();
    assert_eq!(
        notes,
        vec![vec!["A", "Notes"], vec!["B", "Notes"]],
        "two `Notes` headings are distinguishable only by their ancestors"
    );
}

#[test]
fn setext_headings_are_headings_and_keep_both_lines_in_range() {
    let body = "Title\n=====\n\npara\n\nSub\n---\n\nmore\n";
    let tree = parse_blocks(body);
    assert_eq!(
        tree.headings
            .iter()
            .map(|h| (h.level, h.text.as_str()))
            .collect::<Vec<_>>(),
        vec![(1, "Title"), (2, "Sub")]
    );
    assert_eq!(slice(body, &tree.headings[0].range), "Title\n=====\n");
    assert_eq!(tree.headings[1].path, vec!["Title", "Sub"]);
}

#[test]
fn an_atx_closing_sequence_empties_the_title_but_not_the_raw_text() {
    // `### #` is a real glossary heading in the RMS vault: CommonMark reads the
    // lone `#` as the closing sequence, so the title is empty. `raw` is the
    // escape hatch for a caller that would rather show the `#`.
    let body = "### #\n\nbody\n\n## Done ##\n\n## C#\n";
    let tree = parse_blocks(body);
    let seen: Vec<(&str, &str)> = tree
        .headings
        .iter()
        .map(|h| (h.text.as_str(), h.raw.as_str()))
        .collect();
    assert_eq!(
        seen,
        vec![("", "#"), ("Done", "Done ##"), ("C#", "C#")],
        "a `#` run only closes when whitespace precedes it — `C#` keeps its hash"
    );
}

#[test]
fn crlf_line_endings_do_not_leak_into_heading_text() {
    let body = "# H\r\n\r\npara one\r\n\r\n## H2\r\n\r\ntail\r\n";
    let tree = parse_blocks(body);
    assert_eq!(
        tree.headings
            .iter()
            .map(|h| h.text.as_str())
            .collect::<Vec<_>>(),
        vec!["H", "H2"]
    );
    assert_eq!(slice(body, &tree.headings[0].range), "# H\r\n");
    let paras: Vec<&str> = tree
        .blocks
        .iter()
        .filter(|b| matches!(b.kind, BlockKind::Paragraph))
        .map(|b| slice(body, &b.range))
        .collect();
    assert_eq!(paras, vec!["para one\r\n", "tail\r\n"]);
}

#[test]
fn every_block_records_the_heading_it_sits_under() {
    let body = "before\n\n# Top\n\nunder top\n\n## Sub\n\nunder sub\n";
    let tree = parse_blocks(body);
    let attributed: Vec<(Option<usize>, &str)> = tree
        .blocks
        .iter()
        .map(|b| (b.heading, slice(body, &b.range)))
        .collect();
    assert_eq!(
        attributed,
        vec![
            (None, "before\n"),
            (Some(0), "under top\n"),
            (Some(1), "under sub\n"),
        ],
        "a block before the first heading has no section"
    );
}

#[test]
fn a_fence_keeps_its_info_string_and_a_content_only_range() {
    let body = "# H\n\n```python title=\"x\"\nprint(1)\nprint(2)\n```\n\nafter\n";
    let tree = parse_blocks(body);
    let fence = tree
        .blocks
        .iter()
        .find_map(|b| match &b.kind {
            BlockKind::Fence(f) => Some(f),
            _ => None,
        })
        .expect("a fence");
    assert_eq!(fence.info, "python title=\"x\"");
    assert_eq!(fence.lang.as_deref(), Some("python"));
    assert_eq!(slice(body, &fence.code_range), "print(1)\nprint(2)\n");
}

#[test]
fn a_tilde_fence_inside_a_backtick_fence_is_content_not_a_second_fence() {
    // The line scanner toggles `in_fence` on any ``` or ~~~ line, so this body
    // turns scanning back on mid-code and then off again for the rest of the
    // note. The block tree sees one fence.
    let body = "```\nnot a fence:\n~~~\nstill code\n```\n\nafter\n";
    let tree = parse_blocks(body);
    assert_eq!(kinds(&tree), vec!["fence", "para"]);
    let fence = match &tree.blocks[0].kind {
        BlockKind::Fence(f) => f,
        other => panic!("expected a fence, got {other:?}"),
    };
    assert_eq!(
        slice(body, &fence.code_range),
        "not a fence:\n~~~\nstill code\n"
    );
}

#[test]
fn an_indented_code_block_and_an_html_block_keep_their_own_ranges() {
    let body = "para\n\n    indented code\n    more\n\n<div>\n<p>hi [[x]]</p>\n</div>\n\ntail\n";
    let tree = parse_blocks(body);
    assert_eq!(kinds(&tree), vec!["para", "indented", "html", "para"]);
    assert_eq!(
        slice(body, &tree.blocks[1].range),
        "indented code\n    more\n"
    );
    assert_eq!(
        slice(body, &tree.blocks[2].range),
        "<div>\n<p>hi [[x]]</p>\n</div>\n"
    );
}

#[test]
fn a_lazy_blockquote_continuation_stays_inside_the_quote() {
    let body = "> quoted\nlazy continuation\n\nafter\n";
    let tree = parse_blocks(body);
    assert_eq!(kinds(&tree), vec!["quote", "para", "para"]);
    assert_eq!(
        slice(body, &tree.blocks[0].range),
        "> quoted\nlazy continuation\n",
        "the unprefixed second line is part of the quote"
    );
    assert_eq!(tree.blocks[1].inside, Some(0), "the quote's own paragraph");
    assert_eq!(tree.blocks[2].inside, None);
    let quote = only_quote(&tree);
    assert_eq!(quote.callout, None);
    assert_eq!(quote.inner_text_range, tree.blocks[0].range);
}

#[test]
fn a_titled_folded_callout_reports_kind_title_fold_and_a_body_after_the_marker() {
    let body = "> [!Warning]- Be careful\n> Body line one.\n> more\n\nafter\n";
    let tree = parse_blocks(body);
    let quote = only_quote(&tree);
    assert_eq!(
        quote.callout,
        Some(Callout {
            kind: "warning".to_string(),
            title: Some("Be careful".to_string()),
            fold: Some('-'),
        }),
        "the kind is lowercased; Obsidian matches it case-insensitively"
    );
    assert_eq!(
        slice(body, &quote.inner_text_range),
        "> Body line one.\n> more\n",
        "the marker line declares the callout and is not its text"
    );
}

#[test]
fn an_untitled_callout_has_no_title_and_an_unfolded_one_no_marker() {
    let body = "> [!note]\n> body\n\n> [!tip]+ Open\n> t\n\n> [!versionadded] 15.1\n> v\n";
    let tree = parse_blocks(body);
    let callouts: Vec<Option<Callout>> = tree
        .blocks
        .iter()
        .filter_map(|b| match &b.kind {
            BlockKind::BlockQuote(q) => Some(q.callout.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        callouts,
        vec![
            Some(Callout {
                kind: "note".to_string(),
                title: None,
                fold: None
            }),
            Some(Callout {
                kind: "tip".to_string(),
                title: Some("Open".to_string()),
                fold: Some('+')
            }),
            // Arbitrary kinds are allowed: a converter emits Sphinx's.
            Some(Callout {
                kind: "versionadded".to_string(),
                title: Some("15.1".to_string()),
                fold: None
            }),
        ]
    );
    let first = match &tree.blocks[0].kind {
        BlockKind::BlockQuote(q) => q,
        other => panic!("expected a quote, got {other:?}"),
    };
    assert_eq!(slice(body, &first.inner_text_range), "> body\n");
}

#[test]
fn nested_lists_keep_item_text_and_a_fence_inside_an_item() {
    let body =
        "1. step one\n\n   ```python\n   x=1\n   ```\n\n   - sub a\n   - sub b\n2. step two\n";
    let tree = parse_blocks(body);
    let list = only_list(&tree);
    assert!(list.ordered);
    assert_eq!(list.start, Some(1));
    assert_eq!(list.items.len(), 2);

    let first = &list.items[0];
    assert_eq!(
        slice(body, first.text_range.as_ref().expect("step text")),
        "step one\n",
        "the item's text stops before the fence it contains"
    );
    assert_eq!(first.children.len(), 1, "the nested bullet list");
    let sub: Vec<&str> = first.children[0]
        .items
        .iter()
        .map(|i| slice(body, i.text_range.as_ref().unwrap()))
        .collect();
    assert_eq!(sub, vec!["sub a", "sub b"]);
    assert!(!first.children[0].ordered);

    // The fence is a block in its own right, filed inside the list.
    let fence = tree
        .blocks
        .iter()
        .position(|b| matches!(b.kind, BlockKind::Fence(_)))
        .expect("a fence block");
    assert_eq!(tree.blocks[fence].inside, Some(0), "inside the outer list");
    assert_eq!(
        slice(body, &list.items[1].range),
        "2. step two\n",
        "item ranges are verbatim"
    );
}

#[test]
fn a_tight_items_text_spans_the_inline_syntax_it_contains() {
    let body = "- plain item\n- see `code` in [Docs](https://x/y)\n";
    let tree = parse_blocks(body);
    let list = only_list(&tree);
    let texts: Vec<&str> = list
        .items
        .iter()
        .map(|i| slice(body, i.text_range.as_ref().unwrap()))
        .collect();
    assert_eq!(
        texts,
        vec!["plain item", "see `code` in [Docs](https://x/y)"],
        "a tight item has no Paragraph event, and its text is still the source"
    );
}

#[test]
fn an_items_text_is_its_first_paragraph_not_its_last() {
    let body = "1. step one\n\n   more prose about step one\n2. step two\n";
    let tree = parse_blocks(body);
    let list = only_list(&tree);
    assert_eq!(
        slice(body, list.items[0].text_range.as_ref().expect("step text")),
        "step one\n",
        "a second paragraph is the step's detail, not its name"
    );
}

#[test]
fn an_ordered_lists_start_number_survives() {
    let body = "3. three\n4. four\n";
    let list_start = only_list(&parse_blocks(body)).start;
    assert_eq!(list_start, Some(3));
}

#[test]
fn table_cells_are_raw_source_including_wikilinks_and_images() {
    let body = "| Name | Shot |\n|---|---|\n| [[Atlas]] | ![[map.png]] |\n| `int` | plain |\n";
    let tree = parse_blocks(body);
    let table = tree
        .blocks
        .iter()
        .find_map(|b| match &b.kind {
            BlockKind::Table(t) => Some(t),
            _ => None,
        })
        .expect("a table");
    assert_eq!(
        table
            .header
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>(),
        vec!["Name", "Shot"]
    );
    assert_eq!(
        table
            .rows
            .iter()
            .map(|r| r.iter().map(|c| c.text.as_str()).collect::<Vec<_>>())
            .collect::<Vec<_>>(),
        vec![vec!["[[Atlas]]", "![[map.png]]"], vec!["`int`", "plain"]],
        "nothing in a cell is resolved — that is the link pass's job"
    );
    for cell in table.header.iter().chain(table.rows.iter().flatten()) {
        assert_eq!(
            slice(body, &cell.range),
            cell.text,
            "cell ranges slice back"
        );
    }
}

#[test]
fn block_ids_attach_to_the_paragraph_item_or_preceding_block() {
    let body = "A paragraph. ^abc-123\n\n- item one ^bid2\n  - nested ^bid3\n- two\n\n\
| a |\n|---|\n| b |\n\n^tableid\n";
    let tree = parse_blocks(body);
    let seen: Vec<(&str, Option<usize>, bool, &str)> = tree
        .block_ids
        .iter()
        .map(|b| {
            (
                b.id.as_str(),
                b.attaches_to,
                b.own_line,
                slice(body, &b.range),
            )
        })
        .collect();
    assert_eq!(kinds(&tree), vec!["para", "list", "table", "para"]);
    assert_eq!(
        seen,
        vec![
            ("abc-123", Some(0), false, "^abc-123"),
            ("bid2", Some(1), false, "^bid2"),
            ("bid3", Some(1), false, "^bid3"),
            // Obsidian's placement for a block that cannot carry an inline id:
            // its own line, addressing the table above it.
            ("tableid", Some(2), true, "^tableid"),
        ]
    );
}

#[test]
fn an_underscore_is_not_a_block_id() {
    // Obsidian: "Block identifiers can only consist of Latin letters, numbers,
    // and dashes." A `^my_id` is prose.
    let tree = parse_blocks("A paragraph. ^my_id\n");
    assert_eq!(tree.block_ids, vec![]);
}

#[test]
fn comments_are_found_inline_and_across_lines_but_not_inside_code() {
    let body =
        "Pre %%inline%% post\n\n%%\nblock comment\n%%\n\n```sh\necho %%not a comment%%\n```\n";
    let tree = parse_blocks(body);
    let found: Vec<&str> = tree.comments.iter().map(|r| slice(body, r)).collect();
    assert_eq!(
        found,
        vec!["%%inline%%", "%%\nblock comment\n%%"],
        "`%%` inside a fence is code, and the shortest close wins"
    );
}

// ---------------------------------------------------------------------------
// One heading model
// ---------------------------------------------------------------------------

fn golden_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/okf/golden")
}

fn golden_bodies(vault: &str) -> Vec<(String, String)> {
    let root = golden_root().join(vault);
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(&root).sort_by_file_name() {
        let entry = entry.expect("walking the golden vault");
        if entry.path().extension().is_none_or(|e| e != "md") {
            continue;
        }
        let text = std::fs::read_to_string(entry.path()).expect("reading a golden note");
        let (_yaml, body) = crate::okf::frontmatter::split(&text);
        out.push((entry.path().display().to_string(), body));
    }
    assert!(!out.is_empty(), "golden vault {vault} has notes");
    out
}

/// VAULT.md §5.7: a heading inside a `%%comment%%` is not a heading, so the
/// ones around it keep the paths and extents they would have had without it.
#[test]
fn a_heading_inside_a_comment_is_not_in_the_tree() {
    // The hidden heading is a level 1, so keeping it would both close `# Top`'s
    // section early and reparent `## Real` underneath itself.
    let body = "# Top\n\n%%\n# Hidden\n%%\n\ntext\n\n## Real\n\nmore\n";
    let tree = parse_blocks(body);
    assert_eq!(
        tree.headings
            .iter()
            .map(|h| h.text.as_str())
            .collect::<Vec<_>>(),
        vec!["Top", "Real"]
    );
    assert_eq!(tree.headings[1].path, vec!["Top", "Real"]);
    assert_eq!(
        tree.headings[0].body_range.end,
        body.len(),
        "the hidden heading does not close the section it sits in"
    );
    let text = tree
        .blocks
        .iter()
        .find(|b| slice(body, &b.range).trim() == "more")
        .expect("the paragraph under `## Real`");
    assert_eq!(
        text.heading,
        Some(1),
        "blocks re-point at the kept headings"
    );
}

/// An independent oracle for the tree's headings: the line rule the link pass
/// used before P2 re-seated it on this tree (one to six `#` then a space, a
/// tab or the end of the line, outside a fence). Kept as a *replica*, not a
/// call into the shipped code, so the two cannot agree by construction.
fn scanned_heading_lines(body: &str) -> usize {
    let mut fenced = false;
    let mut count = 0;
    for line in body.lines() {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let hashes = t.len() - t.trim_start_matches('#').len();
        if (1..=6).contains(&hashes) {
            let rest = &t[hashes..];
            if rest.is_empty() || rest.starts_with([' ', '\t']) {
                count += 1;
            }
        }
    }
    count
}

/// The structure pass may assume the link pass agrees about headings, so the
/// count has to match what the pre-P2 line scanner produced — 12 across the
/// three golden vaults, all ATX (Phase 0 census).
#[test]
fn the_golden_vaults_headings_match_what_the_link_pass_counts() {
    let mut total = 0;
    for vault in ["okf", "obsidian", "vault"] {
        for (path, body) in golden_bodies(vault) {
            let tree = parse_blocks(&body);
            let scanned = scanned_heading_lines(&body);
            assert_eq!(
                tree.headings.len(),
                scanned,
                "{path}: the tree and the line scanner must see the same headings"
            );
            for heading in &tree.headings {
                assert!(
                    slice(&body, &heading.range).contains(&heading.text),
                    "{path}: heading range must contain its own text"
                );
                assert!(heading.body_range.end <= body.len());
            }
            total += tree.headings.len();
        }
    }
    assert_eq!(total, 12, "the golden corpus's heading count (Phase 0)");
}

/// Ranges are the module's contract: concatenating every top-level block and
/// heading of a golden note must not invent or reorder a byte.
#[test]
fn every_golden_range_slices_back_into_its_own_body() {
    for vault in ["okf", "obsidian", "vault"] {
        for (path, body) in golden_bodies(vault) {
            let tree = parse_blocks(&body);
            let mut previous = 0;
            for block in &tree.blocks {
                assert!(
                    block.range.end <= body.len() && block.range.start <= block.range.end,
                    "{path}: block range out of bounds"
                );
                if block.inside.is_none() {
                    assert!(
                        block.range.start >= previous,
                        "{path}: top-level blocks are in document order"
                    );
                    previous = block.range.end;
                }
                // Slicing must not split a UTF-8 boundary.
                let _ = slice(&body, &block.range);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// `<!-- kglite … -->` directives (VAULT.md §5.8)
// ---------------------------------------------------------------------------

fn directives(tree: &BlockTree) -> Vec<(&str, Option<&str>)> {
    tree.directives
        .iter()
        .map(|d| (d.key.as_str(), d.raw_value.as_deref()))
        .collect()
}

#[test]
fn an_own_line_directive_is_recorded_with_its_key_value_and_range() {
    let body = "# H\n\npara\n\n<!-- kglite address: Data tree -> Wells | Task pane -->\n\ntail\n";
    let tree = parse_blocks(body);
    assert_eq!(
        directives(&tree),
        vec![("address", Some("Data tree -> Wells | Task pane"))]
    );
    let directive = &tree.directives[0];
    assert_eq!(
        slice(body, &directive.range),
        "<!-- kglite address: Data tree -> Wells | Task pane -->\n"
    );
    assert_eq!(
        tree.blocks[directive.block].kind,
        BlockKind::HtmlBlock,
        "a directive names the HTML block it is"
    );
}

#[test]
fn a_valueless_directive_records_no_value() {
    let body = "para\n\n<!--   kglite   chunk   -->\n\ntail\n";
    let tree = parse_blocks(body);
    assert_eq!(directives(&tree), vec![("chunk", None)]);
}

#[test]
fn an_inline_kglite_comment_stays_prose() {
    let body = "A paragraph <!-- kglite address: nope --> and more.\n";
    let tree = parse_blocks(body);
    assert_eq!(kinds(&tree), vec!["para"]);
    assert!(tree.directives.is_empty());
}

#[test]
fn a_non_kglite_html_comment_is_not_a_directive() {
    let body = "para\n\n<!-- just a note -->\n\n<!-- kglitex address: no -->\n\ntail\n";
    let tree = parse_blocks(body);
    assert!(tree.directives.is_empty(), "{:?}", directives(&tree));
    assert_eq!(kinds(&tree), vec!["para", "html", "html", "para"]);
}

/// A comment block that only *starts* like a directive is prose: the whole
/// block must be the comment, or the author wrote something else.
#[test]
fn a_directive_with_trailing_text_on_its_line_is_not_one() {
    let body = "para\n\n<!-- kglite chunk --> trailing\n\ntail\n";
    let tree = parse_blocks(body);
    assert!(tree.directives.is_empty(), "{:?}", directives(&tree));
}

/// A key that is not spelled like a frontmatter key names nothing, so the
/// comment stays the prose it is.
#[test]
fn a_directive_whose_key_is_not_a_key_is_not_one() {
    let body = "<!-- kglite two words -->\n\n<!-- kglite 9lives: x -->\n";
    let tree = parse_blocks(body);
    assert!(tree.directives.is_empty(), "{:?}", directives(&tree));
}

/// `<!-- kglite -->` names no key. It is still recorded — the author plainly
/// reached for a directive — with an empty key, which is what the build warns
/// about (VAULT.md §5.8, §9).
#[test]
fn a_keyless_directive_is_recorded_with_an_empty_key() {
    let body = "para\n\n<!-- kglite -->\n\ntail\n";
    let tree = parse_blocks(body);
    assert_eq!(directives(&tree), vec![("", None)]);
}

#[test]
fn consecutive_directive_lines_are_two_directives() {
    let body = "<!-- kglite a: 1 -->\n<!-- kglite b: 2 -->\n\npara\n";
    let tree = parse_blocks(body);
    assert_eq!(directives(&tree), vec![("a", Some("1")), ("b", Some("2"))]);
    assert_eq!(
        slice(body, &tree.directives[0].range),
        "<!-- kglite a: 1 -->\n"
    );
    assert_eq!(
        slice(body, &tree.directives[1].range),
        "<!-- kglite b: 2 -->\n"
    );
}

/// A directive is metadata, not prose: it is never scanned, so a wikilink or
/// a tag written inside one mints nothing on its own (VAULT.md §5.1, §5.8).
#[test]
fn a_directive_is_not_a_scan_region() {
    let body = "para\n\n<!-- kglite address: [[Wells]] #here -->\n\ntail\n";
    let tree = parse_blocks(body);
    let directive = tree.directives[0].range.clone();
    assert!(
        !scan_regions(body, &tree)
            .iter()
            .any(|r| r.start >= directive.start && r.end <= directive.end),
        "the directive's own bytes are skipped"
    );
}

// ---------------------------------------------------------------------------
// `<!-- kglite heading -->` — synthetic headings (VAULT.md §5.8)
// ---------------------------------------------------------------------------

fn heading_facts(tree: &BlockTree) -> Vec<(u8, &str, &str)> {
    tree.headings
        .iter()
        .map(|h| (h.level, h.text.as_str(), h.raw.as_str()))
        .collect()
}

/// The API-page shape: a converter emitted a bold signature line where a
/// heading belonged, and the marker says so without changing what the file
/// renders as.
#[test]
fn a_heading_directive_promotes_the_paragraph_below_it() {
    let body = "### rmsapi.Project\n\n<!-- kglite heading -->\n\
                **open(filename)**\nOpens a project.\n\nMore prose.\n";
    let tree = parse_blocks(body);
    assert_eq!(
        heading_facts(&tree),
        vec![
            (3, "rmsapi.Project", "rmsapi.Project"),
            (4, "open(filename)", "**open(filename)**"),
        ],
        "the enclosing heading's level + 1, with the bold markers off the text"
    );
    assert_eq!(
        slice(body, &tree.headings[1].range),
        "**open(filename)**\n",
        "the promoted line is the heading's own range, as an ATX line would be"
    );
    assert_eq!(
        tree.headings[1].path,
        vec!["rmsapi.Project", "open(filename)"]
    );
    assert_eq!(
        slice(body, &tree.headings[1].body_range),
        "Opens a project.\n\nMore prose.\n",
        "everything below the promoted line is the new section"
    );
    let paragraphs: Vec<&str> = tree
        .blocks
        .iter()
        .filter(|b| matches!(b.kind, BlockKind::Paragraph) && !b.range.is_empty())
        .map(|b| slice(body, &b.range).trim())
        .collect();
    assert_eq!(
        paragraphs,
        vec!["Opens a project.", "More prose."],
        "the promoted paragraph's remaining lines are a paragraph of their own"
    );
}

/// No blank line between the marker and the line it promotes — the spelling a
/// converter writes — and a paragraph that is only the promoted line.
#[test]
fn a_promoted_paragraph_of_one_line_leaves_no_paragraph_behind() {
    let body = "<!-- kglite heading -->\n**Alone**\n\ntail\n";
    let tree = parse_blocks(body);
    assert_eq!(
        heading_facts(&tree),
        vec![(1, "Alone", "**Alone**")],
        "with no enclosing heading the promoted one is level 1"
    );
    let prose: Vec<&str> = tree
        .blocks
        .iter()
        .filter(|b| matches!(b.kind, BlockKind::Paragraph) && !b.range.is_empty())
        .map(|b| slice(body, &b.range).trim())
        .collect();
    assert_eq!(prose, vec!["tail"]);
}

/// Each marker under one heading promotes to the **same** level: the level is
/// read from the heading the author wrote, not from the synthetic one above.
#[test]
fn two_markers_under_one_heading_are_siblings() {
    let body = "## Methods\n\n<!-- kglite heading -->\n**open()**\n\n\
                <!-- kglite heading -->\n**close()**\n";
    let tree = parse_blocks(body);
    assert_eq!(
        heading_facts(&tree),
        vec![
            (2, "Methods", "Methods"),
            (3, "open()", "**open()**"),
            (3, "close()", "**close()**"),
        ]
    );
    assert_eq!(tree.headings[2].path, vec!["Methods", "close()"]);
}

/// Only the surrounding `**` come off. A code span or a link inside the line
/// is the heading's own text, exactly as it is in an ATX heading.
#[test]
fn only_surrounding_bold_markers_are_stripped() {
    let cases = [
        ("**a** and **b**", "**a** and **b**"),
        ("`code`", "`code`"),
        ("**`code`**", "`code`"),
        ("__also bold__", "__also bold__"),
        // `***`+ on a line of its own is a thematic break, so `**` is the
        // shortest line that is only markers.
        ("**", "**"),
    ];
    for (written, expected) in cases {
        let body = format!("<!-- kglite heading -->\n{written}\n");
        let tree = parse_blocks(&body);
        assert_eq!(
            tree.headings.first().map(|h| h.text.as_str()),
            Some(expected),
            "{written}"
        );
    }
}

/// A marker with nothing to promote says so (VAULT.md §9): silence would look
/// like a reader that had understood it.
#[test]
fn a_heading_directive_with_no_paragraph_below_warns() {
    for body in [
        "## A\n\n<!-- kglite heading -->\n",
        "<!-- kglite heading -->\n\n## A real heading\n\npara\n",
        "<!-- kglite heading -->\n\n- one\n- two\n",
        "<!-- kglite heading -->\n\n```\ncode\n```\n",
    ] {
        let tree = parse_blocks(body);
        assert!(
            tree.headings.iter().all(|h| !h.raw.starts_with("**")),
            "nothing was promoted in {body:?}"
        );
        assert_eq!(
            tree.warnings.len(),
            1,
            "{body:?} warned {:?}",
            tree.warnings
        );
        assert!(
            tree.warnings[0].contains("kglite heading"),
            "{:?}",
            tree.warnings
        );
    }
}
