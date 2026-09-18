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
    derive(body, &parse_blocks(body), "The Note", profile)
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

#[test]
fn a_block_over_a_limit_on_its_own_is_one_chunk() {
    let body = "# A\n\none two three four\n";
    let d = run(body, &both(2, 6000));
    assert_eq!(text_of(&d, "#A~chunk1"), "one two three four");
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
