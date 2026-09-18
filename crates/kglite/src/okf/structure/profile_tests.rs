//! `structure:`'s own schema (VAULT.md §7.1). Every refusal here **fails the
//! build** — the block is a compatibility boundary, and a rule read as
//! "derive nothing" is the reassuring-direction failure this closes.

use super::*;

fn parsed(yaml: &str) -> Result<StructureProfile, String> {
    let doc = crate::okf::frontmatter::parse_yaml(yaml).expect("the fixture is YAML");
    parse(&doc)
}

fn error(yaml: &str) -> String {
    parsed(yaml).expect_err("this shape is refused")
}

#[test]
fn a_rule_with_no_fields_is_the_rule_with_every_default() {
    let got = parsed("sections:\nchunks: {}\n").unwrap();
    let sections = got.sections.expect("declared");
    assert_eq!(
        (
            sections.label.as_str(),
            sections.edge.as_str(),
            sections.parent.as_str(),
            sections.next.as_str()
        ),
        ("Section", "HAS_SECTION", "PARENT_SECTION", "NEXT_SECTION")
    );
    let chunks = got.chunks.expect("declared");
    assert_eq!(
        (
            chunks.label.as_str(),
            chunks.edge.as_str(),
            chunks.next.as_str(),
            chunks.max_words,
            chunks.max_chars
        ),
        ("Chunk", "HAS_CHUNK", "NEXT_CHUNK", 650, 6000)
    );
}

#[test]
fn every_declared_name_is_read() {
    let got = parsed(
        "sections: {label: Heading, edge: HAS_HEADING, parent: UNDER, next: THEN}\n\
         chunks: {label: Passage, edge: HAS_PASSAGE, next: THEN_PASSAGE, max_words: 40, \
         max_chars: 200}\n",
    )
    .unwrap();
    let sections = got.sections.unwrap();
    assert_eq!(sections.label, "Heading");
    assert_eq!(sections.parent, "UNDER");
    let chunks = got.chunks.unwrap();
    assert_eq!((chunks.max_words, chunks.max_chars), (40, 200));
    assert_eq!(chunks.next, "THEN_PASSAGE");
}

/// A key this build has not shipped yet is refused by name, so a vault written
/// against a later spec fails loudly instead of quietly deriving less.
#[test]
fn an_unimplemented_or_invented_key_is_an_error() {
    for key in ["callouts: {}", "tables: []", "sctions: {}"] {
        let message = error(key);
        assert!(
            message.starts_with("unknown key `structure.")
                && message.contains("this build accepts sections, chunks, inherit, embed_text"),
            "{message}"
        );
    }
    assert!(error("sections: {lable: Section}").contains("unknown key `structure.sections.lable`"));
}

#[test]
fn a_rule_of_the_wrong_shape_is_an_error() {
    assert!(error("sections: [Section]").contains("`structure.sections` must be a mapping"));
    assert!(error("chunks: {label: 3}").contains("`structure.chunks.label` must be a string"));
    assert!(error("chunks: {max_words: many}").contains("must be an integer"));
    assert!(
        error("chunks: {max_chars: 0}").contains("must be a positive integer"),
        "a limit of zero would make every block its own chunk"
    );
    assert!(error("sections: {label: ''}").contains("must not be empty"));
}

/// The alternative is a note's frontmatter silently overwriting the structure
/// it was read from.
#[test]
fn inherit_may_not_name_a_property_a_derived_node_defines() {
    for name in ["text", "ordinal", "chunk_hash", "step_count"] {
        let message = error(&format!("inherit: [{name}]"));
        assert!(
            message.contains("names a property a derived node defines itself"),
            "{message}"
        );
    }
    for name in ["title", "tags", "id"] {
        let message = error(&format!("inherit: [{name}]"));
        assert!(
            message.contains("reserved") || message.contains("defines itself"),
            "{message}"
        );
    }
    assert_eq!(
        parsed("inherit: [corpus, category]").unwrap().inherit,
        vec!["corpus".to_string(), "category".to_string()]
    );
    assert!(error("inherit: corpus").contains("must be a list of strings"));
}

#[test]
fn an_unknown_embed_text_placeholder_is_an_error() {
    let message = error("embed_text: \"{title} | {heading} | {text}\"");
    assert!(
        message.contains("`{heading}`, which is not a placeholder")
            && message.contains("{section_title}"),
        "the message names the typo and the vocabulary: {message}"
    );
    assert!(parsed("embed_text: \"{title} {section_title} {heading_path} {text} {id}\"").is_ok());
}

#[test]
fn a_template_renders_each_placeholder_once() {
    let path = vec!["A".to_string(), "B".to_string()];
    assert_eq!(
        render_embed_text(
            "{title} | {heading_path} | {section_title} | {id}\n\n{text}",
            "Note title",
            "B",
            &path,
            "the prose",
            "note#A#B~chunk1",
        ),
        "Note title | A > B | B | note#A#B~chunk1\n\nthe prose"
    );
    assert_eq!(
        render_embed_text("{text}", "t", "s", &path, "holds {title} literally", "id"),
        "holds {title} literally",
        "substitution is one left-to-right pass, so a value is never re-read"
    );
}

#[test]
fn a_block_that_declares_no_rule_derives_nothing() {
    assert!(!parsed("inherit: [corpus]").unwrap().derives_anything());
    assert!(parsed("chunks: {}").unwrap().derives_anything());
}
