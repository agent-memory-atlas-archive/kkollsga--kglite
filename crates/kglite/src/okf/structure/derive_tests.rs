//! One fixture per rule in VAULT.md §7.1 `sections:` / `chunks:`.

use super::*;
use crate::okf::structure::block::parse_blocks;
use crate::okf::structure::profile::{ChunkRule, SectionRule};

fn sections_rule() -> SectionRule {
    SectionRule {
        label: "Section".to_string(),
        edge: "HAS_SECTION".to_string(),
        parent: "PARENT_SECTION".to_string(),
        next: "NEXT_SECTION".to_string(),
    }
}

fn chunks_rule(max_words: usize, max_chars: usize) -> ChunkRule {
    ChunkRule {
        label: "Chunk".to_string(),
        edge: "HAS_CHUNK".to_string(),
        next: "NEXT_CHUNK".to_string(),
        max_words,
        max_chars,
    }
}

fn sections_only() -> StructureProfile {
    StructureProfile {
        sections: Some(sections_rule()),
        ..StructureProfile::default()
    }
}

fn both(max_words: usize, max_chars: usize) -> StructureProfile {
    StructureProfile {
        sections: Some(sections_rule()),
        chunks: Some(chunks_rule(max_words, max_chars)),
        ..StructureProfile::default()
    }
}

fn run(body: &str, profile: &StructureProfile) -> Derived {
    derive(body, &parse_blocks(body), "The Note", "Article", profile)
}

/// `(suffix, label)` in derivation order.
fn nodes(d: &Derived) -> Vec<(&str, &str)> {
    d.nodes
        .iter()
        .map(|n| (n.suffix.as_str(), n.label.as_str()))
        .collect()
}

/// `(conn type, source suffix or "" for the note, target suffix)`, sorted.
fn edges(d: &Derived) -> Vec<(String, String, String)> {
    let mut out: Vec<(String, String, String)> = d
        .edges
        .iter()
        .map(|e| {
            (
                e.conn_type.clone(),
                e.source.clone().unwrap_or_default(),
                e.target.clone(),
            )
        })
        .collect();
    out.sort();
    out
}

fn text_of<'a>(d: &'a Derived, suffix: &str) -> &'a str {
    d.nodes
        .iter()
        .find(|n| n.suffix == suffix)
        .and_then(|n| n.text.as_deref())
        .unwrap_or_else(|| panic!("no node `{suffix}` in {:?}", nodes(d)))
}

fn prop(d: &Derived, suffix: &str, name: &str) -> Value {
    d.nodes
        .iter()
        .find(|n| n.suffix == suffix)
        .and_then(|n| n.props.iter().find(|(k, _)| k == name))
        .map(|(_, v)| v.clone())
        .unwrap_or(Value::Null)
}

#[test]
fn a_section_id_is_its_whole_heading_path() {
    let d = run("# A\n\ntext\n\n## B\n\nmore\n\n# C\n", &sections_only());
    assert_eq!(
        nodes(&d),
        vec![("#A", "Section"), ("#A#B", "Section"), ("#C", "Section")],
        "the id Obsidian links as `[[Note#A#B]]`"
    );
}

#[test]
fn a_sections_text_runs_to_the_next_same_or_higher_heading() {
    let body = "# A\n\nfirst\n\n## B\n\nsecond\n\n# C\n\nthird\n";
    let d = run(body, &sections_only());
    assert_eq!(
        text_of(&d, "#A"),
        "\nfirst\n\n## B\n\nsecond",
        "a parent section holds its children's prose too, verbatim and \
         trailing-trimmed"
    );
    assert_eq!(text_of(&d, "#A#B"), "\nsecond");
    assert_eq!(text_of(&d, "#C"), "\nthird");
}

#[test]
fn section_properties_are_the_declared_ones() {
    let d = run("# A\n\n## B\n\n## C\n", &sections_only());
    assert_eq!(prop(&d, "#A#C", "title"), Value::String("C".to_string()));
    assert_eq!(prop(&d, "#A#C", "level"), Value::Int64(2));
    assert_eq!(
        prop(&d, "#A#C", "ordinal"),
        Value::Int64(1),
        "second sibling"
    );
    assert_eq!(
        prop(&d, "#A#C", "path"),
        Value::List(vec![
            Value::String("A".to_string()),
            Value::String("C".to_string())
        ])
    );
}

#[test]
fn section_edges_spell_the_nesting() {
    let d = run("# A\n\n## B\n\n## C\n\n# D\n", &sections_only());
    assert_eq!(
        edges(&d),
        vec![
            // The note joins its top-level sections; a section joins the
            // sections directly inside it.
            ("HAS_SECTION".into(), "".into(), "#A".into()),
            ("HAS_SECTION".into(), "".into(), "#D".into()),
            ("HAS_SECTION".into(), "#A".into(), "#A#B".into()),
            ("HAS_SECTION".into(), "#A".into(), "#A#C".into()),
            // `parent` only where there is one to name.
            ("NEXT_SECTION".into(), "#A".into(), "#D".into()),
            ("NEXT_SECTION".into(), "#A#B".into(), "#A#C".into()),
            ("PARENT_SECTION".into(), "#A#B".into(), "#A".into()),
            ("PARENT_SECTION".into(), "#A#C".into(), "#A".into()),
        ]
    );
}

/// CommonMark lets levels jump, so nesting is read from the levels rather than
/// from "the previous heading was one shallower".
#[test]
fn a_skipped_heading_level_still_nests() {
    let d = run("## A\n\n#### B\n", &sections_only());
    assert_eq!(nodes(&d), vec![("#A", "Section"), ("#A#B", "Section")]);
    assert!(edges(&d).contains(&("PARENT_SECTION".into(), "#A#B".into(), "#A".into())));
}

#[test]
fn a_duplicate_heading_path_takes_a_counter_and_warns() {
    let d = run("# A\n\n## B\n\n# C\n\n## B\n", &sections_only());
    assert_eq!(
        nodes(&d),
        vec![
            ("#A", "Section"),
            ("#A#B", "Section"),
            ("#C", "Section"),
            ("#C#B", "Section"),
        ],
        "two `B`s under different parents are two different paths"
    );
    assert!(d.warnings.is_empty(), "{:?}", d.warnings);

    let d = run("# A\n\n## B\n\n## B\n", &sections_only());
    assert_eq!(
        nodes(&d),
        vec![
            ("#A", "Section"),
            ("#A#B", "Section"),
            ("#A#B~2", "Section")
        ],
    );
    assert_eq!(d.warnings.len(), 1);
    assert!(
        d.warnings[0].contains("duplicate heading path `A#B`")
            && d.warnings[0].contains("^block-id"),
        "the warning names the heading and the fix: {:?}",
        d.warnings
    );
}

#[test]
fn chunks_pack_greedily_to_max_words() {
    // Three two-word paragraphs, five words to a chunk: two, then one.
    let body = "# A\n\none two\n\nthree four\n\nfive six\n";
    let d = run(body, &both(5, 6000));
    assert_eq!(
        nodes(&d),
        vec![
            ("#A", "Section"),
            ("#A~chunk1", "Chunk"),
            ("#A~chunk2", "Chunk"),
        ]
    );
    assert_eq!(text_of(&d, "#A~chunk1"), "one two\n\nthree four");
    assert_eq!(text_of(&d, "#A~chunk2"), "five six");
}

#[test]
fn max_chars_closes_a_chunk_too() {
    let body = "# A\n\none two\n\nthree four\n\nfive six\n";
    let d = run(body, &both(650, 12));
    assert_eq!(
        d.nodes.iter().filter(|n| n.label == "Chunk").count(),
        3,
        "every paragraph exceeds the combined-span limit on its own"
    );
}

/// A block over a limit is cut at its own line boundaries, and a block that
/// has none has nothing to cut — a `max_chars` bust would still hard-split it,
/// but a `max_words` bust on one line leaves the line whole.
#[test]
fn a_one_line_block_over_max_words_stays_whole() {
    let body = "# A\n\none two three four\n";
    let d = run(body, &both(2, 6000));
    assert_eq!(text_of(&d, "#A~chunk1"), "one two three four");
    assert_eq!(d.forced_splits, 0);
}

#[test]
fn a_section_boundary_closes_the_open_chunk() {
    let body = "# A\n\none\n\n# B\n\ntwo\n";
    let d = run(body, &both(650, 6000));
    assert_eq!(text_of(&d, "#A~chunk1"), "one");
    assert_eq!(text_of(&d, "#B~chunk1"), "two");
    let edges = edges(&d);
    assert!(
        !edges.iter().any(|(conn, _, _)| conn == "NEXT_CHUNK"),
        "a chunk never follows one in another section: {edges:?}"
    );
}

#[test]
fn chunk_properties_carry_the_ordinal_and_the_hash() {
    let d = run("# A\n\none\n\ntwo\n", &both(1, 6000));
    assert_eq!(prop(&d, "#A~chunk2", "ordinal"), Value::Int64(1));
    assert_eq!(
        prop(&d, "#A~chunk1", "chunk_hash"),
        Value::String(
            // sha256("one")
            "7692c3ad3540bb803c020b3aee66cd8887123234ea0c6e7143c0add73ff431ed".to_string()
        ),
        "the SHA-256 of the text and nothing else — the id is what moves"
    );
    assert!(d
        .edges
        .iter()
        .any(|e| e.conn_type == "NEXT_CHUNK" && e.target == "#A~chunk2"));
}

#[test]
fn a_paragraph_a_block_id_names_is_a_chunk_of_its_own() {
    let body = "# A\n\none\n\ntwo ^keep-me\n\nthree\n";
    let d = run(body, &both(650, 6000));
    assert_eq!(
        nodes(&d),
        vec![
            ("#A", "Section"),
            ("#A~chunk1", "Chunk"),
            ("#^keep-me", "Chunk"),
            ("#A~chunk3", "Chunk"),
        ],
        "the block id closes the open chunk and keys its own"
    );
    assert_eq!(text_of(&d, "#^keep-me"), "two ^keep-me");
}

/// Obsidian's own-line placement addresses the block *above* it, and the `^id`
/// line is not prose a reader sees (VAULT.md §5.7).
#[test]
fn an_own_line_block_id_keys_the_block_above_it() {
    let body = "# A\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n\n^table-1\n\nafter\n";
    let d = run(body, &both(650, 6000));
    assert_eq!(
        nodes(&d),
        vec![
            ("#A", "Section"),
            ("#^table-1", "Chunk"),
            ("#A~chunk2", "Chunk"),
        ]
    );
    assert!(
        !text_of(&d, "#^table-1").contains("^table-1")
            && !text_of(&d, "#A~chunk2").contains("^table-1"),
        "the anchor line is not text"
    );
}

#[test]
fn without_sections_every_chunk_hangs_off_the_note() {
    let body = "# A\n\none\n\n# B\n\ntwo\n";
    let profile = StructureProfile {
        chunks: Some(chunks_rule(650, 6000)),
        ..StructureProfile::default()
    };
    let d = run(body, &profile);
    assert_eq!(nodes(&d), vec![("~chunk1", "Chunk"), ("~chunk2", "Chunk")]);
    assert_eq!(
        edges(&d),
        vec![
            ("HAS_CHUNK".into(), "".into(), "~chunk1".into()),
            ("HAS_CHUNK".into(), "".into(), "~chunk2".into()),
        ],
        "one counter for the note, and no chunk ordering across the boundary"
    );
}

#[test]
fn prose_above_the_first_heading_is_the_notes_own_chunk() {
    let d = run("intro\n\n# A\n\nunder\n", &both(650, 6000));
    assert_eq!(
        nodes(&d),
        vec![
            ("#A", "Section"),
            ("~chunk1", "Chunk"),
            ("#A~chunk1", "Chunk"),
        ]
    );
    assert!(edges(&d).contains(&("HAS_CHUNK".into(), "".into(), "~chunk1".into())));
}

/// A list is one block. Counting its items again — they are blocks inside it —
/// would put the same prose in two chunks.
#[test]
fn a_nested_block_is_not_chunked_twice() {
    let body = "# A\n\n- one\n- two\n\n> quoted\n";
    let d = run(body, &both(650, 6000));
    assert_eq!(text_of(&d, "#A~chunk1"), "- one\n- two\n\n> quoted");

    // The packed range is a span, so counting an inner block again produces
    // the same text and shows up only at the limit: these three words fit one
    // chunk, and six would not.
    let d = run("# A\n\n> one two three\n", &both(3, 6000));
    assert_eq!(nodes(&d), vec![("#A", "Section"), ("#A~chunk1", "Chunk")]);
    assert_eq!(text_of(&d, "#A~chunk1"), "> one two three");
}

#[test]
fn nothing_is_derived_without_a_rule() {
    let d = run("# A\n\ntext\n", &StructureProfile::default());
    assert_eq!(d, Derived::default());
}

// ---------------------------------------------------------------------------
// `key_from_heading:` (VAULT.md §7.1)
// ---------------------------------------------------------------------------

/// The rule as a vault declares it, so `when_matches:` is the compiled default
/// unless a fixture overrides it.
fn symbols(yaml: &str) -> StructureProfile {
    let value = crate::okf::frontmatter::parse_yaml(yaml).expect("the fixture is YAML");
    let parsed = crate::okf::structure::profile::parse(&value).expect("the rule the parser takes");
    StructureProfile {
        sections: Some(sections_rule()),
        key_from_heading: parsed.key_from_heading,
        ..StructureProfile::default()
    }
}

fn labelled(body: &str, note_label: &str, profile: &StructureProfile) -> Derived {
    derive(body, &parse_blocks(body), "The Note", note_label, profile)
}

/// `(suffix, label, qualified_name, signature)` per node.
fn symbol_rows(d: &Derived) -> Vec<(String, String, String, String)> {
    d.nodes
        .iter()
        .map(|n| {
            let prop = |name: &str| match n.props.iter().find(|(k, _)| k == name) {
                Some((_, Value::String(s))) => s.clone(),
                _ => String::new(),
            };
            (
                n.suffix.clone(),
                n.label.clone(),
                prop("qualified_name"),
                prop("signature"),
            )
        })
        .collect()
}

#[test]
fn a_symbol_heading_is_relabelled_and_split_into_name_and_signature() {
    let body = "# Api\n\n## rmsapi.Project.open(path) → Project\n\ntext\n";
    let d = labelled(
        body,
        "Api",
        &symbols("key_from_heading: {label: ApiSymbol, under_label: Api}\n"),
    );
    assert_eq!(
        symbol_rows(&d),
        vec![
            (
                "#Api".to_string(),
                "Section".to_string(),
                String::new(),
                String::new()
            ),
            (
                "#Api#rmsapi.Project.open(path) → Project".to_string(),
                "ApiSymbol".to_string(),
                "rmsapi.Project.open".to_string(),
                "(path) → Project".to_string(),
            ),
        ],
        "the section is relabelled in place, id and all"
    );
    // Its own properties and its section edges are unchanged (VAULT.md §7.1):
    // one `HAS_SECTION` per heading, and the symbol still has its parent.
    let symbol = &d.nodes[1];
    assert_eq!(
        symbol
            .props
            .iter()
            .filter(|(k, _)| k == "title" || k == "level")
            .count(),
        2
    );
    assert_eq!(
        d.edges
            .iter()
            .filter(|e| e.conn_type == "HAS_SECTION")
            .count(),
        2
    );
    assert!(d
        .edges
        .iter()
        .any(|e| e.conn_type == "PARENT_SECTION" && e.target == "#Api"));
}

#[test]
fn the_default_pattern_takes_a_return_annotation_and_refuses_a_prose_heading() {
    let body = "# Api\n\n## Introduction\n\n## rmsapi.grid.get() → Grid\n\n## rmsapi.grid\n";
    let d = labelled(
        body,
        "Api",
        &symbols("key_from_heading: {label: ApiSymbol, under_label: Api}\n"),
    );
    let labels: Vec<&str> = d.nodes.iter().map(|n| n.label.as_str()).collect();
    assert_eq!(labels, vec!["Section", "Section", "ApiSymbol", "ApiSymbol"]);
}

#[test]
fn a_heading_with_no_dot_or_paren_is_never_a_symbol() {
    // The gate the format states outright, whatever `when_matches:` says: a
    // pattern that matches everything still cannot relabel `Overview`.
    let body = "# Api\n\n## Overview\n\n## open(path)\n";
    let d = labelled(
        body,
        "Api",
        &symbols("key_from_heading: {label: ApiSymbol, under_label: Api, when_matches: '.*'}\n"),
    );
    let labels: Vec<&str> = d.nodes.iter().map(|n| n.label.as_str()).collect();
    assert_eq!(labels, vec!["Section", "Section", "ApiSymbol"]);
}

#[test]
fn the_rule_reaches_only_the_notes_carrying_under_label() {
    let body = "# Api\n\n## rmsapi.grid.get()\n";
    let rule = symbols("key_from_heading: {label: ApiSymbol, under_label: Api}\n");
    assert_eq!(labelled(body, "Api", &rule).nodes[1].label, "ApiSymbol");
    assert_eq!(labelled(body, "Article", &rule).nodes[1].label, "Section");
}

#[test]
fn the_property_the_name_is_stored_under_is_declared() {
    let body = "# Api\n\n## rmsapi.grid.get()\n";
    let d = labelled(
        body,
        "Api",
        &symbols("key_from_heading: {label: Sym, under_label: Api, property: symbol}\n"),
    );
    assert!(d.nodes[1]
        .props
        .iter()
        .any(|(k, v)| k == "symbol" && v == &Value::String("rmsapi.grid.get".to_string())));
}

#[test]
fn a_heading_that_is_only_a_return_annotation_splits_at_the_arrow() {
    // No parentheses at all: the split cuts at whichever of `(` and `→` comes
    // first, and here only the arrow is there.
    let body = "# Api\n\n## rmsapi.grid.count → int\n";
    let d = labelled(
        body,
        "Api",
        &symbols("key_from_heading: {label: ApiSymbol, under_label: Api}\n"),
    );
    assert_eq!(
        symbol_rows(&d)[1],
        (
            "#Api#rmsapi.grid.count → int".to_string(),
            "ApiSymbol".to_string(),
            "rmsapi.grid.count".to_string(),
            "→ int".to_string(),
        )
    );
}

#[test]
fn only_a_section_is_relabelled_never_another_construct_under_it() {
    // A `Procedure` carries its section's title as its own, so a symbol-named
    // heading with a list under it offers the rule a second node with exactly
    // the matching `title` — and relabelling that would move a procedure into
    // the API surface.
    let body = "# Api\n\n## rmsapi.grid.get()\n\n1. Open it.\n2. Close it.\n";
    let mut profile = symbols("key_from_heading: {label: ApiSymbol, under_label: Api}\n");
    profile.ordered_lists = crate::okf::structure::profile::parse(
        &crate::okf::frontmatter::parse_yaml("ordered_lists:\n").unwrap(),
    )
    .unwrap()
    .ordered_lists;
    let d = labelled(body, "Api", &profile);
    let labels: Vec<(&str, &str)> = d
        .nodes
        .iter()
        .map(|n| (n.suffix.as_str(), n.label.as_str()))
        .collect();
    assert_eq!(
        labels,
        vec![
            ("#Api", "Section"),
            ("#Api#rmsapi.grid.get()", "ApiSymbol"),
            ("#Api#rmsapi.grid.get()~list1", "Procedure"),
            ("#Api#rmsapi.grid.get()~list1~step1", "ProcedureStep"),
            ("#Api#rmsapi.grid.get()~list1~step2", "ProcedureStep"),
        ]
    );
}

// ---------------------------------------------------------------------------
// A block bigger than the cap splits inside itself (VAULT.md §7.1)
// ---------------------------------------------------------------------------

/// Chunk texts in derivation order.
fn chunk_texts(d: &Derived) -> Vec<&str> {
    d.nodes
        .iter()
        .filter(|n| n.label == "Chunk")
        .map(|n| n.text.as_deref().unwrap_or(""))
        .collect()
}

/// The `ordinal` of every chunk, in derivation order.
fn chunk_ordinals(d: &Derived) -> Vec<i64> {
    d.nodes
        .iter()
        .filter(|n| n.label == "Chunk")
        .map(|n| match n.props.iter().find(|(k, _)| k == "ordinal") {
            Some((_, Value::Int64(i))) => *i,
            other => panic!("no int ordinal: {other:?}"),
        })
        .collect()
}

/// The `NEXT_CHUNK` chain, walked from the first chunk: every chunk once, in
/// derivation order, or the chain is broken.
fn next_chunk_chain(d: &Derived) -> Vec<String> {
    let mut chain = Vec::new();
    let Some(first) = d.nodes.iter().find(|n| n.label == "Chunk") else {
        return chain;
    };
    let mut at = first.suffix.clone();
    loop {
        chain.push(at.clone());
        let Some(edge) = d
            .edges
            .iter()
            .find(|e| e.conn_type == "NEXT_CHUNK" && e.source.as_deref() == Some(at.as_str()))
        else {
            return chain;
        };
        at = edge.target.clone();
    }
}

fn long_list(items: usize) -> String {
    let mut body = String::from("# A\n\n");
    for i in 0..items {
        body.push_str(&format!(
            "- item {i} with enough words to make the line wide\n"
        ));
    }
    body
}

#[test]
fn a_list_over_max_chars_splits_at_item_edges() {
    let body = long_list(400);
    let d = run(&body, &both(usize::MAX, 6000));
    let texts = chunk_texts(&d);
    assert!(texts.len() > 1, "a 20 kB list is not one 6 000-char chunk");
    for text in &texts {
        assert!(
            text.len() <= 6000,
            "a chunk of {} chars under a 6 000 cap",
            text.len()
        );
        for line in text.lines() {
            assert!(
                line.starts_with("- item "),
                "a chunk boundary inside an item: {line:?}"
            );
        }
    }
    assert_eq!(
        texts.join("\n"),
        body["# A\n\n".len()..].trim_end(),
        "the pieces are the list, in order and whole"
    );
    assert_eq!(
        chunk_ordinals(&d),
        (0..texts.len() as i64).collect::<Vec<_>>(),
        "ordinals stay contiguous"
    );
    assert_eq!(
        next_chunk_chain(&d).len(),
        texts.len(),
        "the NEXT_CHUNK chain covers every piece"
    );
}

#[test]
fn a_table_over_max_chars_splits_at_row_edges_and_the_header_stays_in_the_first_piece() {
    let mut body = String::from("# A\n\n| name | type |\n| --- | --- |\n");
    for i in 0..60 {
        body.push_str(&format!("| row number {i} | string |\n"));
    }
    let d = run(&body, &both(usize::MAX, 200));
    let texts = chunk_texts(&d);
    assert!(texts.len() > 3, "the table is one chunk again");
    assert!(texts[0].starts_with("| name | type |\n| --- | --- |\n| row number 0 |"));
    assert_eq!(
        texts
            .iter()
            .filter(|t| t.contains("| name | type |"))
            .count(),
        1,
        "chunks are text ranges: the header row is never duplicated"
    );
    for text in &texts {
        assert!(text.len() <= 200, "{} chars under a 200 cap", text.len());
        for line in text.lines() {
            assert!(
                line.starts_with('|') && line.ends_with('|'),
                "a chunk boundary inside a row: {line:?}"
            );
        }
    }
}

#[test]
fn a_single_line_over_max_chars_hard_splits_on_char_boundaries() {
    // Two bytes per character, so a byte-indexed cut that ignored char
    // boundaries would panic or produce invalid UTF-8.
    let line: String = "é".repeat(5_000);
    let body = format!("# A\n\n{line}\n");
    let d = run(&body, &both(usize::MAX, 1_000));
    let texts = chunk_texts(&d);
    assert_eq!(texts.len(), 10, "10 000 bytes under a 1 000-byte cap");
    for text in &texts {
        assert!(
            text.len() <= 1_000,
            "{} bytes under a 1 000 cap",
            text.len()
        );
    }
    assert_eq!(texts.concat(), line, "the pieces reassemble to the line");
}

#[test]
fn forced_splits_counts_the_boundaries_the_cap_added_inside_blocks() {
    let under = run("# A\n\none\n\ntwo\n", &both(1, 6000));
    assert_eq!(
        under.forced_splits, 0,
        "two blocks packed apart is the packer, not a forced split"
    );

    let d = run(&long_list(400), &both(usize::MAX, 6000));
    let pieces = chunk_texts(&d).len();
    assert!(pieces > 1, "nothing was split, so the count proves nothing");
    assert_eq!(
        d.forced_splits,
        pieces - 1,
        "one forced boundary per extra piece of the one block"
    );
}

#[test]
fn a_block_id_on_an_over_cap_block_keys_its_first_piece() {
    let body = "# A\n\naa bb\ncc dd\nee ^big\n";
    let d = run(body, &both(usize::MAX, 12));
    let suffixes: Vec<&str> = d
        .nodes
        .iter()
        .filter(|n| n.label == "Chunk")
        .map(|n| n.suffix.as_str())
        .collect();
    assert_eq!(
        suffixes,
        vec!["#^big", "#A~chunk2"],
        "the id keys the first piece; the rest take chunk ordinals"
    );
    assert_eq!(chunk_texts(&d), vec!["aa bb\ncc dd", "ee ^big"]);
}

#[test]
fn a_block_over_max_words_splits_at_its_line_boundaries_too() {
    let body = "# A\n\none two three\nfour five six\nseven eight nine\n";
    let d = run(body, &both(4, 6000));
    assert_eq!(
        chunk_texts(&d),
        vec!["one two three", "four five six", "seven eight nine"],
        "three words a line, four to a chunk"
    );
    assert_eq!(d.forced_splits, 2);
}

// ---------------------------------------------------------------------------
// `<!-- kglite … -->` directives are metadata, not prose (VAULT.md §5.8)
// ---------------------------------------------------------------------------

#[test]
fn a_directive_is_cut_out_of_the_chunk_it_sits_in() {
    let body = concat!(
        "# Annotation Table\n",
        "\n",
        "To open the **Annotation Table** dialog box, click the button.\n",
        "\n",
        "<!-- kglite address: Data tree -> Wells | Task pane -->\n",
        "\n",
        "Then pick a well.\n",
    );
    let d = run(body, &both(650, 6000));
    let text = text_of(&d, "#Annotation Table~chunk1");
    assert!(
        !text.contains("kglite"),
        "the directive is not chunk text: {text:?}"
    );
    assert!(text.starts_with("To open the **Annotation Table** dialog box, click the button."));
    assert!(text.ends_with("Then pick a well."));
}

#[test]
fn a_directive_is_cut_out_of_the_section_text_around_it() {
    let body = concat!(
        "# One\n",
        "\n",
        "before\n",
        "\n",
        "<!-- kglite address: somewhere -->\n",
        "\n",
        "after\n",
    );
    let d = run(body, &sections_only());
    // Both blank lines that separated the directive from its neighbours stay:
    // the cut is the directive's own range and nothing more.
    assert_eq!(text_of(&d, "#One"), "\nbefore\n\n\nafter");
}

#[test]
fn a_directive_above_the_first_heading_is_cut_out_too() {
    let body = "<!-- kglite owner: docs -->\n\nlead-in prose\n\n# One\n\nbody\n";
    let d = run(body, &both(650, 6000));
    assert_eq!(text_of(&d, "~chunk1"), "lead-in prose");
}

/// Two directives around one paragraph leave the paragraph and nothing else.
#[test]
fn directives_on_both_sides_of_a_paragraph_leave_only_the_paragraph() {
    let body = "<!-- kglite a: 1 -->\n\npara\n\n<!-- kglite b: 2 -->\n";
    let d = run(body, &both(650, 6000));
    assert_eq!(text_of(&d, "~chunk1"), "para");
}

/// `<!-- kglite -->` names no key, so it can carry no meaning — but it is
/// still the author reaching for a directive, and the build says so (§9).
#[test]
fn a_keyless_directive_warns() {
    let body = "# One\n\npara\n\n<!-- kglite -->\n";
    let d = run(body, &both(650, 6000));
    assert_eq!(
        d.warnings,
        vec!["`<!-- kglite -->` names no key; nothing was recorded (VAULT.md §5.8)"]
    );
    assert_eq!(text_of(&d, "#One~chunk1"), "para");
}

// ---------------------------------------------------------------------------
// `<!-- kglite chunk -->` (VAULT.md §5.8, §7.1)
// ---------------------------------------------------------------------------

#[test]
fn a_chunk_marker_closes_the_open_chunk_between_two_paragraphs() {
    let body = concat!(
        "# One\n",
        "\n",
        "First paragraph.\n",
        "\n",
        "<!-- kglite chunk -->\n",
        "\n",
        "Second paragraph.\n",
    );
    let d = run(body, &both(650, 6000));
    assert_eq!(
        nodes(&d),
        vec![
            ("#One", "Section"),
            ("#One~chunk1", "Chunk"),
            ("#One~chunk2", "Chunk"),
        ]
    );
    assert_eq!(text_of(&d, "#One~chunk1"), "First paragraph.");
    assert_eq!(text_of(&d, "#One~chunk2"), "Second paragraph.");
    assert_eq!(prop(&d, "#One~chunk1", "ordinal"), Value::Int64(0));
    assert_eq!(prop(&d, "#One~chunk2", "ordinal"), Value::Int64(1));
    assert!(edges(&d).contains(&(
        "NEXT_CHUNK".to_string(),
        "#One~chunk1".to_string(),
        "#One~chunk2".to_string()
    )));
    assert_eq!(
        d.forced_splits, 0,
        "an authored boundary is a choice, not a cap the packer had to break"
    );
}

#[test]
fn a_chunk_marker_first_or_last_in_a_section_splits_nothing() {
    let body = concat!(
        "# One\n",
        "\n",
        "<!-- kglite chunk -->\n",
        "\n",
        "Only paragraph.\n",
        "\n",
        "<!-- kglite chunk -->\n",
    );
    let d = run(body, &both(650, 6000));
    assert_eq!(
        nodes(&d),
        vec![("#One", "Section"), ("#One~chunk1", "Chunk")]
    );
    assert_eq!(text_of(&d, "#One~chunk1"), "Only paragraph.");
    assert!(d.warnings.is_empty(), "{:?}", d.warnings);
}

/// Only a top-level marker names a boundary: inside a list item there is no
/// chunk of its own to close, and silently doing nothing would look like a
/// packer bug to the author.
#[test]
fn a_chunk_marker_inside_a_list_warns_and_splits_nothing() {
    let body = concat!(
        "# One\n",
        "\n",
        "- first item\n",
        "\n",
        "  <!-- kglite chunk -->\n",
        "\n",
        "- second item\n",
    );
    let d = run(body, &both(650, 6000));
    assert_eq!(
        d.warnings,
        vec!["`<!-- kglite chunk -->` inside a list has no chunk to split (VAULT.md §7.1)"]
    );
    assert_eq!(
        nodes(&d),
        vec![("#One", "Section"), ("#One~chunk1", "Chunk")]
    );
    assert!(!text_of(&d, "#One~chunk1").contains("kglite"));
}

/// A marker between two paragraphs of one *note* with no `sections:` rule
/// divides the note's own chunk sequence just the same.
#[test]
fn a_chunk_marker_above_the_first_heading_divides_the_notes_own_chunks() {
    let body = "alpha\n\n<!-- kglite chunk -->\n\nbeta\n";
    let d = run(body, &both(650, 6000));
    assert_eq!(nodes(&d), vec![("~chunk1", "Chunk"), ("~chunk2", "Chunk")]);
    assert_eq!(text_of(&d, "~chunk1"), "alpha");
    assert_eq!(text_of(&d, "~chunk2"), "beta");
}

/// A synthetic heading (VAULT.md §5.8) is a heading: it derives a section with
/// the same id shape an ATX one would, so `[[Note#Name]]` reaches it.
#[test]
fn a_promoted_heading_derives_a_section_like_any_other() {
    let body = "## Methods\n\nIntro.\n\n<!-- kglite heading -->\n\
                **open(filename)**\n\nOpens a project.\n";
    let derived = run(body, &both(650, 6000));
    assert_eq!(
        nodes(&derived),
        vec![
            ("#Methods", "Section"),
            ("#Methods#open(filename)", "Section"),
            ("#Methods~chunk1", "Chunk"),
            ("#Methods#open(filename)~chunk1", "Chunk"),
        ],
        "the promoted line is a section of its own and closes the chunk above it"
    );
    let section = derived
        .nodes
        .iter()
        .find(|n| n.suffix == "#Methods#open(filename)")
        .expect("the promoted section");
    assert_eq!(
        section.props,
        vec![
            (
                "title".to_string(),
                Value::String("open(filename)".to_string())
            ),
            ("level".to_string(), Value::Int64(3)),
            ("ordinal".to_string(), Value::Int64(0)),
            (
                "path".to_string(),
                Value::List(vec![
                    Value::String("Methods".to_string()),
                    Value::String("open(filename)".to_string()),
                ])
            ),
        ],
        "the bold markers are off the title, and the level is the parent's + 1"
    );
    assert_eq!(section.text.as_deref(), Some("\nOpens a project."));
}
