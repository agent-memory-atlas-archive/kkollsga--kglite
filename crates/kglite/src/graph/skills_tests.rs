//! Unit tests for the `skills` module.

use super::*;
use crate::graph::storage::mode::{new_dir_graph_in_mode, StorageMode};

fn graph() -> DirGraph {
    new_dir_graph_in_mode(StorageMode::Memory, None).expect("create graph")
}

fn record(name: &str) -> SkillRecord {
    SkillRecord {
        name: name.to_string(),
        description: format!("what {name} is for"),
        body: format!("# {name}\n\nmethodology"),
        references_tools: vec!["cypher_query".to_string(), "explore".to_string()],
        delivery: Delivery::Lazy,
    }
}

// ── Validation (D4) ────────────────────────────────────────────────────────

#[test]
fn an_empty_name_is_refused() {
    let mut r = record("s");
    r.name = "  ".to_string();
    assert!(matches!(
        validate(&r),
        Err(KgError::InvalidArgument { ref argument, .. }) if argument == "name"
    ));
}

#[test]
fn a_name_that_is_not_a_path_safe_token_is_refused() {
    // `name` becomes `<name>.md` on export; a separator would escape the
    // directory the caller named.
    for bad in ["a/b", "a\\b", "two words", "..", "a:b"] {
        let mut r = record("s");
        r.name = bad.to_string();
        assert!(
            validate(&r).is_err(),
            "{bad:?} must not be accepted as a skill name"
        );
    }
}

#[test]
fn an_empty_description_is_refused() {
    let mut r = record("s");
    r.description = String::new();
    assert!(matches!(
        validate(&r),
        Err(KgError::InvalidArgument { ref argument, .. }) if argument == "description"
    ));
}

#[test]
fn a_body_over_the_ceiling_is_refused_and_one_at_it_is_not() {
    let mut r = record("s");
    r.body = "x".repeat(MAX_BODY_BYTES);
    assert!(validate(&r).is_ok(), "the ceiling itself is legal");
    r.body.push('x');
    assert!(matches!(
        validate(&r),
        Err(KgError::InvalidArgument { ref argument, .. }) if argument == "body"
    ));
}

#[test]
fn an_unknown_delivery_is_refused_rather_than_defaulted() {
    assert_eq!(Delivery::parse("eager").unwrap(), Delivery::Eager);
    assert_eq!(Delivery::parse("lazy").unwrap(), Delivery::Lazy);
    assert!(Delivery::parse("later").is_err());
    assert_eq!(Delivery::default(), Delivery::Lazy);
}

// ── CRUD ───────────────────────────────────────────────────────────────────

#[test]
fn set_creates_then_updates_the_same_node() {
    let mut g = graph();
    assert_eq!(set(&mut g, &record("alpha")).unwrap(), SetOutcome::Created);

    let mut changed = record("alpha");
    changed.description = "revised".to_string();
    changed.delivery = Delivery::Eager;
    assert_eq!(set(&mut g, &changed).unwrap(), SetOutcome::Updated);

    assert_eq!(list(&g).len(), 1, "MERGE keys on name — one node per name");
    let stored = get(&g, "alpha").unwrap();
    assert_eq!(stored.description, "revised");
    assert_eq!(stored.delivery, Delivery::Eager);
    assert_eq!(stored, changed);
}

/// The planner's typo-guard rejects a CREATE property the type's metadata has
/// never seen, so an operator's hand-written `CREATE (:KgliteSkill {name})`
/// could have made every later full upsert illegal. It does not — `SET +=` is
/// outside the guard — and this pins that, because the alternative is a graph
/// whose skills can never be completed.
#[test]
fn a_hand_created_skill_node_can_still_be_completed_by_set() {
    let mut g = graph();
    let params = std::collections::HashMap::new();
    execute_mut(
        &mut g,
        "CREATE (s:KgliteSkill {name: 'alpha'})",
        &skill_opts(&params),
    )
    .expect("hand-written create");

    assert_eq!(set(&mut g, &record("alpha")).unwrap(), SetOutcome::Updated);
    assert_eq!(list(&g).len(), 1, "the upsert found the existing node");
    let stored = get(&g, "alpha").unwrap();
    assert_eq!(stored, record("alpha"), "all five properties landed");
}

#[test]
fn get_of_an_unknown_name_is_node_not_found() {
    let g = graph();
    assert!(matches!(
        get(&g, "nope"),
        Err(KgError::NodeNotFound { ref node_type, ref id })
            if node_type == SKILL_LABEL && id == "nope"
    ));
}

#[test]
fn delete_reports_whether_there_was_anything_to_delete() {
    let mut g = graph();
    set(&mut g, &record("alpha")).unwrap();
    assert!(delete(&mut g, "alpha").unwrap());
    assert!(!delete(&mut g, "alpha").unwrap());
    assert!(get(&g, "alpha").is_err());
}

#[test]
fn list_is_sorted_by_name_and_carries_no_bodies() {
    let mut g = graph();
    for name in ["gamma", "alpha", "beta"] {
        set(&mut g, &record(name)).unwrap();
    }
    let listed = list(&g);
    assert_eq!(
        listed.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        ["alpha", "beta", "gamma"]
    );
    assert!(
        listed.iter().all(|r| r.body.is_empty()),
        "a 16 KiB body per skill is not what a catalogue is for"
    );
    assert!(!get(&g, "alpha").unwrap().body.is_empty());
}

#[test]
fn a_skill_write_is_refused_on_a_read_only_graph() {
    // `read_only` gates the Cypher path in the bindings, never in core — so
    // without this refusal a Rust embedder would mutate a graph the wheel
    // would have protected.
    let mut g = graph();
    set(&mut g, &record("alpha")).unwrap();
    g.read_only = true;
    let err = set(&mut g, &record("beta")).unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("read-only"),
        "got: {err}"
    );
    assert!(delete(&mut g, "alpha").is_err());
    assert_eq!(list(&g).len(), 1, "the refused writes changed nothing");
}

#[test]
fn a_schema_locked_graph_refuses_an_undeclared_skill_type() {
    // Documents what the Cypher path already enforces: the lock is what makes
    // an operator's curated schema stick, and skills are not exempt from it.
    let mut g = graph();
    g.schema_locked = true;
    assert!(set(&mut g, &record("alpha")).is_err());
    g.schema_locked = false;
    assert!(set(&mut g, &record("alpha")).is_ok());
}

#[test]
fn a_skill_node_stays_hidden_from_the_type_enumeration() {
    let mut g = graph();
    set(&mut g, &record("alpha")).unwrap();
    assert!(!g.get_node_types().contains(&SKILL_LABEL.to_string()));
    assert!(g.has_node_type(SKILL_LABEL));
}

// ── Markdown ───────────────────────────────────────────────────────────────

#[cfg(feature = "okf")]
#[test]
fn render_then_parse_round_trips_every_field() {
    for delivery in [Delivery::Eager, Delivery::Lazy] {
        let mut original = record("round_trip");
        original.delivery = delivery;
        original.description = "quotes \" and a colon: still survive".to_string();
        assert_eq!(
            parse_markdown(&render_markdown(&original)).unwrap(),
            original
        );
    }
}

#[cfg(feature = "okf")]
#[test]
fn a_bundled_skill_file_parses_into_its_declared_shape() {
    // The dialect is upstream's, not ours: if mcp-methods' own frontmatter
    // stops parsing here, export produces files it cannot read.
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../kglite-mcp-server/skills/cypher_query.md"
    );
    let text = std::fs::read_to_string(path).expect("bundled skill file");
    let parsed = parse_markdown(&text).expect("parse bundled skill");
    assert_eq!(parsed.name, "cypher_query");
    assert_eq!(parsed.references_tools, vec!["cypher_query".to_string()]);
    assert!(parsed.description.contains("Run Cypher"));
    assert!(parsed.body.starts_with("# `cypher_query` methodology"));
    assert_eq!(
        parsed.delivery,
        Delivery::Lazy,
        "a file with no delivery key takes the default"
    );
}

#[cfg(feature = "okf")]
#[test]
fn a_document_that_fails_validation_is_refused_at_parse() {
    assert!(parse_markdown("---\ndescription: \"d\"\n---\n\nbody").is_err());
    assert!(parse_markdown("---\nname: \"a/b\"\ndescription: \"d\"\n---\n").is_err());
    assert!(
        parse_markdown("---\nname: \"a\"\ndescription: \"d\"\ndelivery: \"soon\"\n---\n").is_err()
    );
}

#[cfg(feature = "okf")]
#[test]
fn import_of_a_directory_then_export_reproduces_the_same_set() {
    let src = tempfile::tempdir().expect("src dir");
    let out = tempfile::tempdir().expect("out dir");
    let mut expected = Vec::new();
    for name in ["beta", "alpha"] {
        let r = record(name);
        std::fs::write(src.path().join(format!("{name}.md")), render_markdown(&r)).unwrap();
        expected.push(r);
    }
    // A non-markdown sibling must be ignored, not parsed.
    std::fs::write(src.path().join("README.txt"), "not a skill").unwrap();
    expected.sort_by(|a, b| a.name.cmp(&b.name));

    let mut g = graph();
    let imported = import_path(&mut g, src.path()).expect("import");
    assert_eq!(imported, vec!["alpha".to_string(), "beta".to_string()]);

    let exported = export_dir(&g, out.path()).expect("export");
    assert_eq!(exported, imported);

    let mut reloaded = graph();
    import_path(&mut reloaded, out.path()).expect("re-import");
    let round_tripped: Vec<SkillRecord> = list(&reloaded)
        .iter()
        .map(|s| get(&reloaded, &s.name).unwrap())
        .collect();
    assert_eq!(round_tripped, expected);
}

#[cfg(feature = "okf")]
#[test]
fn import_of_a_single_file_upserts_just_that_skill() {
    let dir = tempfile::tempdir().expect("dir");
    let file = dir.path().join("solo.md");
    std::fs::write(&file, render_markdown(&record("solo"))).unwrap();
    let mut g = graph();
    assert_eq!(
        import_path(&mut g, &file).unwrap(),
        vec!["solo".to_string()]
    );
    assert_eq!(
        import_path(&mut g, &file).unwrap(),
        vec!["solo".to_string()]
    );
    assert_eq!(list(&g).len(), 1, "a second import upserts, not duplicates");
}

#[cfg(feature = "okf")]
#[test]
fn import_of_a_missing_path_names_the_path() {
    let mut g = graph();
    assert!(matches!(
        import_path(&mut g, Path::new("/nonexistent/skills")),
        Err(KgError::FileNotFound(_))
    ));
}

#[test]
fn export_creates_the_directory_and_names_each_file_after_its_skill() {
    let out = tempfile::tempdir().expect("out dir");
    let nested = out.path().join("a").join("b");
    let mut g = graph();
    set(&mut g, &record("alpha")).unwrap();
    assert_eq!(export_dir(&g, &nested).unwrap(), vec!["alpha".to_string()]);
    let written = std::fs::read_to_string(nested.join("alpha.md")).expect("alpha.md");
    assert!(
        written.starts_with("---\nname: \"alpha\""),
        "got: {written}"
    );
    assert!(written.ends_with("methodology"));
}
