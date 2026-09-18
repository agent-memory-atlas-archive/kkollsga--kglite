//! One fixture per rule in VAULT.md §7.1 `callouts:` / `code_fences:` /
//! `ordered_lists:`.

use super::super::derive::{derive, Derived};
use super::super::profile::{CalloutRule, FenceRule, SectionRule, StructureProfile};
use crate::datatypes::values::Value;
use crate::okf::structure::block::parse_blocks;

fn sections_rule() -> SectionRule {
    SectionRule {
        label: "Section".to_string(),
        edge: "HAS_SECTION".to_string(),
        parent: "PARENT_SECTION".to_string(),
        next: "NEXT_SECTION".to_string(),
    }
}

/// `sections:` plus one construct rule — the shape a vault declares them in,
/// so every id below carries the section prefix the spec's table shows.
fn with(rule: impl FnOnce(&mut StructureProfile)) -> StructureProfile {
    let mut profile = StructureProfile {
        sections: Some(sections_rule()),
        ..StructureProfile::default()
    };
    rule(&mut profile);
    profile
}

fn callouts() -> StructureProfile {
    with(|p| {
        p.callouts = Some(CalloutRule {
            label: "Note".to_string(),
            edge: "HAS_NOTE".to_string(),
        })
    })
}

fn fences(langs: Option<&[&str]>) -> StructureProfile {
    with(|p| {
        p.code_fences = Some(FenceRule {
            label: "Example".to_string(),
            edge: "HAS_EXAMPLE".to_string(),
            langs: langs.map(|l| l.iter().map(|s| s.to_string()).collect()),
        })
    })
}

fn lists(min_items: usize, under_heading: Option<&str>) -> StructureProfile {
    let rule = super::super::profile::parse(&yaml_lists(min_items, under_heading))
        .expect("the rule the parser accepts");
    with(|p| p.ordered_lists = rule.ordered_lists)
}

/// Build the `ordered_lists:` mapping through the config parser, so the
/// `under_heading` regex is compiled exactly as a vault's would be.
fn yaml_lists(min_items: usize, under_heading: Option<&str>) -> Value {
    let mut rule = crate::datatypes::PropMap::new();
    rule.insert("label".to_string(), Value::String("Step".to_string()));
    rule.insert(
        "container".to_string(),
        Value::String("Procedure".to_string()),
    );
    rule.insert("min_items".to_string(), Value::Int64(min_items as i64));
    if let Some(pattern) = under_heading {
        rule.insert(
            "under_heading".to_string(),
            Value::String(pattern.to_string()),
        );
    }
    let mut block = crate::datatypes::PropMap::new();
    block.insert("ordered_lists".to_string(), Value::Map(rule));
    Value::Map(block)
}

fn run(body: &str, profile: &StructureProfile) -> Derived {
    derive(body, &parse_blocks(body), "The Note", profile)
}

/// `(suffix, label)` in derivation order, sections dropped — every fixture
/// below declares them and none of them is about a section.
fn derived(d: &Derived) -> Vec<(&str, &str)> {
    d.nodes
        .iter()
        .filter(|n| n.label != "Section")
        .map(|n| (n.suffix.as_str(), n.label.as_str()))
        .collect()
}

/// `(conn type, source suffix or "" for the note, target suffix)`, sorted,
/// with the section rule's own edges dropped.
fn edges(d: &Derived) -> Vec<(String, String, String)> {
    let mut out: Vec<(String, String, String)> = d
        .edges
        .iter()
        .filter(|e| {
            !matches!(
                e.conn_type.as_str(),
                "HAS_SECTION" | "PARENT_SECTION" | "NEXT_SECTION"
            )
        })
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
        .unwrap_or_else(|| panic!("no node `{suffix}` with text in {:?}", derived(d)))
}

fn prop(d: &Derived, suffix: &str, name: &str) -> Value {
    d.nodes
        .iter()
        .find(|n| n.suffix == suffix)
        .and_then(|n| n.props.iter().find(|(k, _)| k == name))
        .map(|(_, v)| v.clone())
        .unwrap_or(Value::Null)
}

fn section_of(d: &Derived, suffix: &str) -> Option<String> {
    d.nodes
        .iter()
        .find(|n| n.suffix == suffix)
        .and_then(|n| n.section.clone())
}

// ---------------------------------------------------------------------------
// `callouts:`
// ---------------------------------------------------------------------------

#[test]
fn a_callout_carries_its_kind_title_fold_and_stripped_body() {
    let body =
        "# A\n\n> [!warning]+ Check the datum\n> Depths are metres below MSL.\n> Twice over.\n";
    let d = run(body, &callouts());
    assert_eq!(derived(&d), vec![("#A~note1", "Note")]);
    assert_eq!(
        prop(&d, "#A~note1", "kind"),
        Value::String("warning".into())
    );
    assert_eq!(
        prop(&d, "#A~note1", "title"),
        Value::String("Check the datum".into())
    );
    assert_eq!(prop(&d, "#A~note1", "fold"), Value::String("+".into()));
    assert_eq!(prop(&d, "#A~note1", "ordinal"), Value::Int64(0));
    assert_eq!(
        text_of(&d, "#A~note1"),
        "Depths are metres below MSL.\nTwice over.",
        "the `>` markers are the quote's, not the author's text"
    );
    assert_eq!(
        edges(&d),
        vec![(
            "HAS_NOTE".to_string(),
            "#A".to_string(),
            "#A~note1".to_string()
        )]
    );
}

#[test]
fn a_callout_kind_is_arbitrary_and_lowercased_and_a_title_may_be_absent() {
    let body = "# A\n\n> [!VersionAdded]\n> Since 2.0.\n";
    let d = run(body, &callouts());
    assert_eq!(
        prop(&d, "#A~note1", "kind"),
        Value::String("versionadded".into()),
        "Obsidian styles its 13 and renders the rest; a corpus keeps its own vocabulary"
    );
    assert_eq!(prop(&d, "#A~note1", "title"), Value::Null);
    assert_eq!(prop(&d, "#A~note1", "fold"), Value::Null);
}

#[test]
fn a_nested_callout_hangs_off_the_callout_and_keeps_the_section() {
    let body = "# A\n\n> [!note] Outer\n> Before.\n>\n> > [!tip] Inner\n> > Deeper.\n";
    let d = run(body, &callouts());
    assert_eq!(
        derived(&d),
        vec![("#A~note1", "Note"), ("#A~note1~note1", "Note")],
        "the nested one is keyed under its parent callout, not under the section"
    );
    assert_eq!(
        edges(&d),
        vec![
            (
                "HAS_NOTE".to_string(),
                "#A".to_string(),
                "#A~note1".to_string()
            ),
            (
                "HAS_NOTE".to_string(),
                "#A~note1".to_string(),
                "#A~note1~note1".to_string()
            ),
        ]
    );
    assert_eq!(
        section_of(&d, "#A~note1~note1"),
        Some("#A".to_string()),
        "`section_id` is the heading it sits under either way"
    );
    assert_eq!(
        text_of(&d, "#A~note1~note1"),
        "Deeper.",
        "both markers are quote syntax from the inner callout's own depth"
    );
    assert_eq!(
        text_of(&d, "#A~note1"),
        "Before.\n\n> [!tip] Inner\n> Deeper.",
        "and only the outer one is from its parent's"
    );
}

#[test]
fn a_callout_above_the_first_heading_hangs_off_the_note() {
    let d = run("> [!note] Lead\n> Text.\n\n# A\n", &callouts());
    assert_eq!(derived(&d), vec![("~note1", "Note")]);
    assert_eq!(section_of(&d, "~note1"), None);
    assert_eq!(
        edges(&d),
        vec![("HAS_NOTE".to_string(), String::new(), "~note1".to_string())],
        "`\"\"` is the note itself"
    );
}

#[test]
fn a_plain_blockquote_is_not_a_callout() {
    let d = run("# A\n\n> Just a quotation.\n", &callouts());
    assert!(derived(&d).is_empty());
}

// ---------------------------------------------------------------------------
// `code_fences:`
// ---------------------------------------------------------------------------

#[test]
fn omitting_langs_takes_every_fence_including_one_with_no_info_string() {
    let body = "# A\n\n```python\none\n```\n\n```\ntwo\n```\n\n~~~\nthree\n~~~\n";
    let d = run(body, &fences(None));
    assert_eq!(
        derived(&d),
        vec![
            ("#A~example1", "Example"),
            ("#A~example2", "Example"),
            ("#A~example3", "Example"),
        ],
        "the setting a corpus needs when its converter dropped the languages"
    );
    assert_eq!(
        prop(&d, "#A~example1", "lang"),
        Value::String("python".into())
    );
    assert_eq!(prop(&d, "#A~example2", "lang"), Value::Null);
    assert_eq!(
        prop(&d, "#A~example3", "code"),
        Value::String("three\n".into())
    );
    assert_eq!(prop(&d, "#A~example2", "ordinal"), Value::Int64(1));
}

#[test]
fn langs_matches_the_info_strings_first_word_case_insensitively() {
    let body = "# A\n\n```Python title=\"x\"\none\n```\n\n```rust\ntwo\n```\n\n```\nthree\n```\n";
    let d = run(body, &fences(Some(&["python"])));
    assert_eq!(derived(&d), vec![("#A~example1", "Example")]);
    assert_eq!(
        prop(&d, "#A~example1", "lang"),
        Value::String("python".into()),
        "the first word, lowercased — not the whole info string"
    );
    assert_eq!(
        prop(&d, "#A~example1", "code"),
        Value::String("one\n".into())
    );
}

#[test]
fn a_caption_is_the_paragraph_above_only_when_it_ends_in_a_colon() {
    let body = "# A\n\nRead it like this:\n\n```py\none\n```\n\nJust prose.\n\n```py\ntwo\n```\n";
    let d = run(body, &fences(None));
    assert_eq!(
        prop(&d, "#A~example1", "caption"),
        Value::String("Read it like this:".into())
    );
    assert_eq!(prop(&d, "#A~example2", "caption"), Value::Null);
}

#[test]
fn a_fence_indented_inside_a_list_item_is_dedented_to_its_own_column() {
    // `Fence::code_range` spans the source, so every line after the first
    // still carries the item's four-space indentation.
    let body = "# A\n\n1. Step\n\n    ```py\n    if x:\n        y()\n    ```\n";
    let d = run(body, &fences(None));
    assert_eq!(
        prop(&d, "#A~example1", "code"),
        Value::String("if x:\n    y()\n".into()),
        "the container's indentation goes; the code's own stays"
    );
}

// ---------------------------------------------------------------------------
// `ordered_lists:`
// ---------------------------------------------------------------------------

#[test]
fn a_qualifying_list_becomes_a_container_and_one_node_per_item() {
    let body = "# A\n\n1. Open it.\n2. Close it.\n";
    let d = run(body, &lists(2, None));
    assert_eq!(
        derived(&d),
        vec![
            ("#A~list1", "Procedure"),
            ("#A~list1~step1", "Step"),
            ("#A~list1~step2", "Step"),
        ]
    );
    assert_eq!(
        prop(&d, "#A~list1", "title"),
        Value::String("A".into()),
        "the enclosing section's title"
    );
    assert_eq!(prop(&d, "#A~list1", "step_count"), Value::Int64(2));
    assert_eq!(prop(&d, "#A~list1~step2", "ordinal"), Value::Int64(1));
    assert_eq!(prop(&d, "#A~list1~step2", "level"), Value::Int64(0));
    assert_eq!(text_of(&d, "#A~list1~step1"), "Open it.");
    assert_eq!(
        edges(&d),
        vec![
            (
                "HAS_PROCEDURE".to_string(),
                "#A".to_string(),
                "#A~list1".to_string()
            ),
            (
                "HAS_STEP".to_string(),
                "#A~list1".to_string(),
                "#A~list1~step1".to_string()
            ),
            (
                "HAS_STEP".to_string(),
                "#A~list1".to_string(),
                "#A~list1~step2".to_string()
            ),
            (
                "NEXT_STEP".to_string(),
                "#A~list1~step1".to_string(),
                "#A~list1~step2".to_string()
            ),
        ],
        "`HAS_<UPPER_SNAKE(container)>` is not declared — it is spelled from the label"
    );
}

#[test]
fn a_nested_ordered_list_makes_sub_steps_under_its_own_step() {
    let body =
        "# A\n\n1. Open it.\n2. Pick one.\n   1. The first.\n   2. The second.\n3. Close it.\n";
    let d = run(body, &lists(2, None));
    assert_eq!(
        derived(&d),
        vec![
            ("#A~list1", "Procedure"),
            ("#A~list1~step1", "Step"),
            ("#A~list1~step2", "Step"),
            ("#A~list1~step2~step1", "Step"),
            ("#A~list1~step2~step2", "Step"),
            ("#A~list1~step3", "Step"),
        ]
    );
    assert_eq!(prop(&d, "#A~list1~step2~step2", "level"), Value::Int64(1));
    assert_eq!(prop(&d, "#A~list1~step2~step2", "ordinal"), Value::Int64(1));
    assert_eq!(
        prop(&d, "#A~list1", "step_count"),
        Value::Int64(3),
        "the steps the container itself holds; a sub-step is its own step's"
    );
    let edges = edges(&d);
    assert!(edges.contains(&(
        "HAS_STEP".to_string(),
        "#A~list1~step2".to_string(),
        "#A~list1~step2~step1".to_string()
    )));
    assert!(
        edges.contains(&(
            "NEXT_STEP".to_string(),
            "#A~list1~step2~step1".to_string(),
            "#A~list1~step2~step2".to_string()
        )),
        "`next` joins consecutive steps at one level"
    );
    assert!(
        !edges.contains(&(
            "NEXT_STEP".to_string(),
            "#A~list1~step2~step2".to_string(),
            "#A~list1~step3".to_string()
        )),
        "and never across levels"
    );
    assert_eq!(
        text_of(&d, "#A~list1~step2"),
        "Pick one.",
        "the item's own content, excluding the list nested inside it"
    );
}

#[test]
fn min_items_excludes_a_short_list_and_an_unordered_one_is_never_a_procedure() {
    let d = run("# A\n\n1. Alone.\n\n# B\n\n- one\n- two\n", &lists(2, None));
    assert!(derived(&d).is_empty());
    let d = run("# A\n\n1. Alone.\n", &lists(1, None));
    assert_eq!(
        derived(&d).len(),
        2,
        "`min_items: 1` is the old corpus rule"
    );
}

#[test]
fn under_heading_reads_only_the_lists_under_a_matching_heading() {
    let body = "# Steps\n\n1. One.\n2. Two.\n\n# Notes\n\n1. Three.\n2. Four.\n";
    let d = run(body, &lists(2, Some("^(Procedure|Steps|To .*)")));
    assert_eq!(
        derived(&d),
        vec![
            ("#Steps~list1", "Procedure"),
            ("#Steps~list1~step1", "Step"),
            ("#Steps~list1~step2", "Step"),
        ]
    );
}

#[test]
fn a_list_above_the_first_heading_titles_its_container_after_the_note() {
    let d = run("1. One.\n2. Two.\n\n# A\n", &lists(2, None));
    assert_eq!(
        prop(&d, "~list1", "title"),
        Value::String("The Note".into()),
        "the enclosing section's title, or the note's"
    );
    assert_eq!(section_of(&d, "~list1"), None);
    // The pattern matches the *note's* title, and the list still does not
    // qualify: `under_heading` reads a section's title, and a list above the
    // first heading sits under no section at all.
    let d = run("1. One.\n2. Two.\n\n# A\n", &lists(2, Some("^The Note")));
    assert!(
        derived(&d).is_empty(),
        "a narrowing that names a heading cannot reach a list that sits under none"
    );
}

// ---------------------------------------------------------------------------
// Ids across rules
// ---------------------------------------------------------------------------

#[test]
fn each_kind_counts_its_own_sequence_under_its_own_parent() {
    let body = "# A\n\n> [!note] One\n> x\n\n```py\nz\n```\n\n## B\n\n> [!note] Two\n> y\n\n```py\nw\n```\n";
    let mut profile = callouts();
    profile.code_fences = fences(None).code_fences;
    let d = run(body, &profile);
    assert_eq!(
        derived(&d),
        vec![
            ("#A~note1", "Note"),
            ("#A#B~note1", "Note"),
            ("#A~example1", "Example"),
            ("#A#B~example1", "Example"),
        ],
        "`<n>` counts that kind under that parent from 1, so the two sections \
         each start at 1 and the two kinds never share a counter"
    );
}
