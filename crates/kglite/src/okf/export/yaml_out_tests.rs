//! The frontmatter writer's own rules (VAULT.md §10.3, §10.4).

use super::*;

fn one(key: &str, value: Value) -> String {
    let mut tree = Tree::default();
    tree.insert(key, value);
    render_frontmatter(&tree)
}

#[test]
fn an_empty_document_writes_nothing() {
    assert_eq!(render_frontmatter(&Tree::default()), "");
}

#[test]
fn each_quoting_class_is_quoted_and_a_plain_word_is_not() {
    for bare in [
        "plain",
        "two words",
        "a-b",
        "mid#hash",
        "1.2.3",
        "POINT(1 2)",
    ] {
        assert!(!needs_quoting(bare), "{bare} needs no quoting");
    }
    for quoted in [
        "",
        " padded",
        "padded ",
        "12",
        "-3",
        "1.5",
        "1e9",
        "0x1f",
        "0o17",
        "true",
        "False",
        "null",
        "~",
        "yes",
        "no",
        "on",
        "off",
        "[[A]]",
        "a: b",
        "a #b",
        "- dash",
        "trailing:",
        "line\nbreak",
        "#leading",
        "*anchor",
        "2026-01-15",
        "2026-01-15T09:30:00Z",
    ] {
        assert!(needs_quoting(quoted), "{quoted:?} must be quoted");
    }
}

#[test]
fn a_whole_float_keeps_its_point() {
    assert_eq!(render_float(1.0), "1.0");
    assert_eq!(render_float(1.5), "1.5");
    assert_eq!(render_float(f64::INFINITY), ".inf");
    assert_eq!(render_float(f64::NAN), ".nan");
}

#[test]
fn scalars_render_as_the_reader_reads_them_back() {
    assert_eq!(one("k", Value::Int64(7)), "k: 7\n");
    assert_eq!(one("k", Value::Boolean(false)), "k: false\n");
    assert_eq!(
        one("k", Value::Point { lat: 1.0, lon: 2.0 }),
        "k: POINT(2 1)\n"
    );
    assert_eq!(
        one(
            "k",
            Value::DateTime(chrono::NaiveDate::from_ymd_opt(2026, 1, 15).unwrap())
        ),
        "k: 2026-01-15\n"
    );
    assert_eq!(
        one(
            "k",
            Value::Timestamp(
                chrono::NaiveDate::from_ymd_opt(2026, 1, 15)
                    .unwrap()
                    .and_hms_opt(9, 30, 0)
                    .unwrap()
            )
        ),
        "k: 2026-01-15T09:30:00Z\n"
    );
}

#[test]
fn an_empty_collection_keeps_its_shape() {
    assert_eq!(one("k", Value::List(Vec::new())), "k: []\n");
    assert_eq!(
        one("k", Value::Map(crate::datatypes::prop_map::PropMap::new())),
        "k: {}\n"
    );
}

#[test]
fn a_collection_inside_a_list_is_written_as_flow_json() {
    assert_eq!(
        one(
            "k",
            Value::List(vec![Value::List(vec![Value::Int64(1), Value::Int64(2)])])
        ),
        "k:\n  - [1,2]\n"
    );
}

#[test]
fn dotted_keys_nest_and_a_clash_keeps_the_literal_key() {
    let mut tree = Tree::default();
    tree.insert("meta.a", Value::Int64(1));
    tree.insert("meta.b", Value::Int64(2));
    assert_eq!(render_frontmatter(&tree), "meta:\n  a: 1\n  b: 2\n");

    // A node really carrying both `meta` and `meta.a` keeps both, whichever
    // order they arrive in: the dotted key cannot grow through a scalar, and a
    // scalar cannot replace the branch a dotted key already built.
    let mut scalar_first = Tree::default();
    scalar_first.insert("meta", Value::Int64(9));
    scalar_first.insert("meta.a", Value::Int64(1));
    assert_eq!(render_frontmatter(&scalar_first), "meta: 9\nmeta.a: 1\n");

    let mut branch_first = Tree::default();
    branch_first.insert("meta.a", Value::Int64(1));
    branch_first.insert("meta", Value::Int64(9));
    assert_eq!(render_frontmatter(&branch_first), "meta:\n  a: 1\n");
}

#[test]
fn a_wikilink_list_is_quoted_so_it_is_not_a_flow_sequence() {
    let mut tree = Tree::default();
    tree.insert_wikilinks("depends_on", vec!["A".to_string(), "B/c".to_string()]);
    assert_eq!(
        render_frontmatter(&tree),
        "depends_on:\n  - \"[[A]]\"\n  - \"[[B/c]]\"\n"
    );
}

#[test]
fn lower_snake_inverts_upper_snake() {
    for conn in [
        "LINKS_TO",
        "HAS_KEYWORD",
        "RELATED_TO",
        "CHILD_OF",
        "EMBEDS",
    ] {
        let key = lower_snake(conn);
        assert_eq!(crate::okf::links::upper_snake(&key), conn, "{conn}");
    }
}
