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
    for key in ["paragraphs: {}", "sctions: {}"] {
        let message = error(key);
        assert!(
            message.starts_with("unknown key `structure.")
                && message.contains(
                    "this build accepts sections, chunks, callouts, code_fences, \
                     ordered_lists, tables, key_from_heading, inherit, embed_text"
                ),
            "{message}"
        );
    }
    assert!(error("sections: {lable: Section}").contains("unknown key `structure.sections.lable`"));
    assert!(error("callouts: {lable: Note}").contains("unknown key `structure.callouts.lable`"));
    assert!(error("code_fences: {lang: [py]}").contains("unknown key `structure.code_fences.lang`"));
    assert!(
        error("ordered_lists: {steps: 2}").contains("unknown key `structure.ordered_lists.steps`")
    );
}

/// The three rules P4 shipped, with their defaults and their refusals.
#[test]
fn the_construct_rules_default_to_the_names_the_spec_writes() {
    let got = parsed("callouts:\ncode_fences:\nordered_lists:\n").unwrap();
    let callouts = got.callouts.unwrap();
    assert_eq!(
        (callouts.label.as_str(), callouts.edge.as_str()),
        ("Note", "HAS_NOTE")
    );
    let fences = got.code_fences.unwrap();
    assert_eq!(
        (fences.label.as_str(), fences.edge.as_str()),
        ("Example", "HAS_EXAMPLE")
    );
    assert_eq!(fences.langs, None, "omitting `langs:` is every fence");
    let lists = got.ordered_lists.unwrap();
    assert_eq!(
        (
            lists.label.as_str(),
            lists.container.as_str(),
            lists.edge.as_str(),
            lists.next.as_str(),
            lists.min_items
        ),
        ("ProcedureStep", "Procedure", "HAS_STEP", "NEXT_STEP", 2)
    );
    assert!(
        lists.under_heading.is_none(),
        "the heading gate is the opt-in"
    );
}

#[test]
fn a_construct_rule_of_the_wrong_shape_is_an_error() {
    assert!(error("code_fences: {langs: python}").contains("must be a list of strings"));
    assert!(error("code_fences: {langs: [3]}").contains("must be a list of strings"));
    assert!(error("ordered_lists: {min_items: 0}").contains("must be a positive integer"));
    assert!(error("ordered_lists: {under_heading: 3}").contains("must be a string"));
    assert!(
        error("ordered_lists: {under_heading: \"^(\"}").contains("is not a regular expression"),
        "compiled once at load, so a broken pattern fails the build rather than every note"
    );
}

#[test]
fn langs_are_lowercased_so_a_declaration_and_a_fence_agree() {
    let got = parsed("code_fences: {langs: [Python, CYPHER]}").unwrap();
    assert_eq!(
        got.code_fences.unwrap().langs.unwrap(),
        vec!["python".to_string(), "cypher".to_string()]
    );
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

// ---------------------------------------------------------------------------
// `tables:` and `key_from_heading:` (VAULT.md §7.1)
// ---------------------------------------------------------------------------

#[test]
fn a_table_rule_names_its_heading_and_one_of_the_two_forms() {
    let got = parsed(
        "tables:\n  - {under_heading: '^Parameters$', label: ApiParameter, key_column: name, \
         edge: HAS_PARAMETER}\n  - {under_heading: 'Worked at', edge: WORKED_AT, edges: true}\n",
    )
    .unwrap();
    let node = &got.tables[0];
    assert!(node.under_heading.is_match("Parameters") && !node.under_heading.is_match("Returns"));
    assert_eq!(
        (
            node.label.as_deref(),
            node.key_column.as_deref(),
            node.edge.as_str(),
            node.edges
        ),
        (Some("ApiParameter"), Some("name"), "HAS_PARAMETER", false)
    );
    let edge = &got.tables[1];
    assert_eq!(
        (edge.label.as_deref(), edge.edge.as_str(), edge.edges),
        (None, "WORKED_AT", true)
    );
    // An undeclared node-form `edge:` is spelled from the label.
    assert_eq!(
        parsed("tables:\n  - {under_heading: 'Parameters', label: Api Parameter}\n")
            .unwrap()
            .tables[0]
            .edge,
        "HAS_API_PARAMETER"
    );
}

#[test]
fn a_table_rule_refuses_the_shapes_that_could_not_be_read() {
    for (yaml, wanted) in [
        (
            "tables:\n  - {label: Row}\n",
            "needs an `under_heading:` naming the heading its tables sit under",
        ),
        (
            "tables:\n  - {under_heading: 'P'}\n",
            "needs a `label:` for the row nodes, or `edges: true` and an `edge:`",
        ),
        (
            "tables:\n  - {under_heading: 'P', label: Row, edges: true, edge: HAS_ROW}\n",
            "states an edge, not a node",
        ),
        (
            "tables:\n  - {under_heading: '(', label: Row}\n",
            "is not a regular expression",
        ),
        (
            "tables:\n  - {under_heading: 'P', label: Row, rows: 2}\n",
            "unknown key `structure.tables.rows`",
        ),
        ("tables: {under_heading: 'P'}\n", "must be a list of rules"),
    ] {
        let message = error(yaml);
        assert!(message.contains(wanted), "{yaml:?} gave {message}");
    }
}

#[test]
fn the_symbol_rule_needs_its_under_label_and_defaults_the_rest() {
    let got = parsed("key_from_heading: {under_label: Api}\n")
        .unwrap()
        .key_from_heading
        .expect("declared");
    assert_eq!(
        (
            got.label.as_str(),
            got.property.as_str(),
            got.under_label.as_str()
        ),
        ("ApiSymbol", "qualified_name", "Api")
    );
    // The default pattern: a dotted name, a call, and the trailing `→ type` a
    // converter writes into the same heading — and nothing that is merely a
    // word (the `.`/`(` gate refuses those too, in `derive`).
    for heading in [
        "rmsapi.grid.get",
        "rmsapi.grid.get(a, b)",
        "rmsapi.Project.open(path) → Project",
    ] {
        assert!(got.when_matches.is_match(heading), "{heading}");
    }
    for heading in ["Introduction", "Read this first", "A. B"] {
        assert!(!got.when_matches.is_match(heading), "{heading}");
    }
    assert!(error("key_from_heading: {label: ApiSymbol}").contains("needs an `under_label:`"));
    assert!(
        error("key_from_heading: {under_label: Api, when_matches: '('}")
            .contains("is not a regular expression")
    );
    assert!(error("key_from_heading: {under_label: Api, propery: x}")
        .contains("unknown key `structure.key_from_heading.propery`"));
}
