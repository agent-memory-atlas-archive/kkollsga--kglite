//! `okf::validate` (VAULT.md §9): the same report the build makes, the
//! `Err` reserved for an unreadable root, and the two path-safety errors P5
//! deferred here.

use super::validate;
use crate::okf::model::{BuildOptions, Dialect};
use std::fs;
use std::path::Path;
use tempfile::tempdir;

fn write(dir: &Path, rel: &str, content: &str) {
    let p = dir.join(rel);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(p, content).unwrap();
}

fn vault() -> BuildOptions {
    BuildOptions::for_dialect(Dialect::Obsidian)
}

fn golden_vault() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/okf/golden/vault")
}

#[test]
fn validate_returns_the_report_the_build_made() {
    let root = golden_vault();
    let opts = vault();
    let built = crate::okf::build(&root, &opts).unwrap().report;
    let checked = validate(&root, &opts).unwrap();
    assert_eq!(
        checked, built,
        "the check must be the build, not a second opinion about it"
    );
}

#[test]
fn a_broken_vault_yaml_is_one_error_not_an_err() {
    let dir = tempdir().unwrap();
    write(dir.path(), "note.md", "prose");
    write(
        dir.path(),
        ".kglite/vault.yaml",
        "kglite_vault: 1\nnonsense: 3\n",
    );

    // The build refuses outright (VAULT.md §7) — the validator's job is to
    // say so in the shape every other finding arrives in.
    assert!(crate::okf::build(dir.path(), &vault()).is_err());

    let report = validate(dir.path(), &vault()).expect("a broken config is a finding, not an Err");
    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    assert!(
        report.errors[0].contains("vault.yaml") && report.errors[0].contains("nonsense"),
        "{}",
        report.errors[0]
    );
    assert!(!report.is_ok(false));
}

#[test]
fn a_missing_or_non_directory_root_is_an_err() {
    let dir = tempdir().unwrap();
    let absent = dir.path().join("nope");
    let message = validate(&absent, &vault()).unwrap_err();
    assert!(message.contains("does not exist"), "{message}");

    write(dir.path(), "note.md", "prose");
    let message = validate(&dir.path().join("note.md"), &vault()).unwrap_err();
    assert!(message.contains("not a directory"), "{message}");
}

#[test]
fn an_absolute_target_is_an_error() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "notes/a.md",
        "See [the plan](C:/secrets/plan.md) and ![chart](~/Pictures/chart.png).\n\
         Also [home](file:///etc/hosts.md).\n\
         And [the plan again](C:/secrets/plan.md).\n",
    );
    let report = validate(dir.path(), &vault()).unwrap();
    // Three, not four: one note referencing one bad path twice has one
    // problem to fix, not two.
    assert_eq!(report.errors.len(), 3, "{:?}", report.errors);
    for (error, target) in report.errors.iter().zip([
        "`C:/secrets/plan.md`",
        "`~/Pictures/chart.png`",
        "`file:///etc/hosts.md`",
    ]) {
        assert!(
            error.starts_with("notes/a.md: ")
                && error.contains(target)
                && error.contains("absolute filesystem path"),
            "{error}"
        );
    }
}

#[test]
fn a_target_escaping_the_vault_root_is_an_error() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "notes/deep/a.md",
        "[out](../../../elsewhere/x.md) and ![img](../../../etc/logo.png)\n\
         and [[../../../../outside/note]] and [fine](../b.md).\n",
    );
    write(dir.path(), "notes/b.md", "still inside");
    let report = validate(dir.path(), &vault()).unwrap();
    assert_eq!(report.errors.len(), 3, "{:?}", report.errors);
    assert!(report
        .errors
        .iter()
        .all(|e| e.contains("escapes the vault root") && e.starts_with("notes/deep/a.md: ")));
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.contains("`../../../etc/logo.png`")),
        "the attachment reference escapes too: {:?}",
        report.errors
    );
}

#[test]
fn a_percent_encoded_target_resolves_against_the_file_it_names() {
    let dir = tempdir().unwrap();
    write(dir.path(), "img/a b.png", "PNG");
    write(dir.path(), "notes/sales report.md", "the quarterly numbers");
    write(
        dir.path(),
        "notes/a.md",
        "![chart](../img/a%20b.png) and [report](sales%20report.md).\n",
    );
    let report = validate(dir.path(), &vault()).unwrap();

    assert_eq!(report.missing_attachments, 0, "{:?}", report.warnings);
    assert_eq!(report.nodes_by_label.get("Image"), Some(&1));
    assert_eq!(report.dangling, 0, "{:?}", report.warnings);
    assert_eq!(report.edges_by_type.get("LINKS_TO"), Some(&1));
    assert!(report.errors.is_empty(), "{:?}", report.errors);
}

#[test]
fn a_percent_escape_that_decodes_to_a_climb_is_still_an_error() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "notes/a.md",
        "[out](%2e%2e/../elsewhere/x.md)\n",
    );
    let report = validate(dir.path(), &vault()).unwrap();
    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    assert!(
        report.errors[0].contains("escapes the vault root"),
        "{}",
        report.errors[0]
    );
}

#[test]
fn a_wikilink_is_never_percent_decoded() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "notes/a%20b.md",
        "a note spelled with an escape",
    );
    write(dir.path(), "notes/link.md", "[[a%20b]]\n");
    let report = validate(dir.path(), &vault()).unwrap();
    assert_eq!(report.dangling, 0, "{:?}", report.warnings);
    assert_eq!(report.edges_by_type.get("LINKS_TO"), Some(&1));
}

/// §4.3's typed-edge rule resolves its targets the way §5.2 resolves a body
/// link, so a target naming a place the vault does not own is the same §9
/// error there — a reader that follows the edge does not care which half of
/// the file spelled it.
#[test]
fn a_frontmatter_wikilink_is_path_checked_like_a_body_one() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "notes/deep/a.md",
        "---\n\
         depends_on: \"[[../../../outside/note]]\"\n\
         parent: \"[[C:/secrets/plan]]\"\n\
         see_also: [\"[[../b]]\", \"[[../../../../far/away]]\"]\n\
         ---\n\
         prose\n",
    );
    write(dir.path(), "notes/b.md", "still inside");
    let report = validate(dir.path(), &vault()).unwrap();
    let errors = report.errors.join("\n");
    assert_eq!(report.errors.len(), 3, "{errors}");
    assert!(
        report
            .errors
            .iter()
            .all(|e| e.starts_with("notes/deep/a.md: ")),
        "{errors}"
    );
    for (target, why) in [
        ("`../../../outside/note`", "escapes the vault root"),
        ("`C:/secrets/plan`", "absolute filesystem path"),
        ("`../../../../far/away`", "escapes the vault root"),
    ] {
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.contains(target) && e.contains(why)),
            "{target} was not reported: {errors}"
        );
    }
}

/// A wikilink is a *name*: only one spelled like a path is checked, or every
/// note called `C: the sequel` would be an error. The Windows and UNC
/// spellings carry no `/` at all, so the name check is the absolute-path one
/// as well as the `/` one.
#[test]
fn a_wikilink_naming_a_windows_path_is_an_error_without_a_slash() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "a.md",
        "---\ndepends_on: \"[[~]]\"\n---\n[[D:\\\\vault\\\\x]] and [[an ordinary name]]\n",
    );
    let report = validate(dir.path(), &vault()).unwrap();
    let errors = report.errors.join("\n");
    assert_eq!(report.errors.len(), 2, "{errors}");
    assert!(
        report
            .errors
            .iter()
            .all(|e| e.contains("absolute filesystem path")),
        "{errors}"
    );
}

#[test]
fn okf_and_loose_bundles_are_not_path_checked() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "notes/a.md",
        "---\ntype: Concept\n---\n[out](../../../elsewhere/x.md)\n",
    );
    for dialect in [Dialect::Okf, Dialect::Loose] {
        let report = validate(dir.path(), &BuildOptions::for_dialect(dialect)).unwrap();
        assert!(
            report.errors.is_empty(),
            "a bundle sweep links across roots by design: {:?}",
            report.errors
        );
    }
}

#[test]
fn is_ok_reads_errors_always_and_warnings_only_under_strict() {
    let dir = tempdir().unwrap();
    write(dir.path(), "a.md", "[[Nowhere]]\n");
    let report = validate(dir.path(), &vault()).unwrap();
    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(report.errors.is_empty());
    assert!(report.is_ok(false), "a dangling link is a warning (§9)");
    assert!(!report.is_ok(true), "--strict promotes it");

    let clean = tempdir().unwrap();
    write(clean.path(), "a.md", "prose only\n");
    let report = validate(clean.path(), &vault()).unwrap();
    assert!(report.is_ok(false) && report.is_ok(true), "{:?}", report);
}

#[test]
fn render_is_the_stable_text_both_front_ends_print() {
    let dir = tempdir().unwrap();
    write(dir.path(), "a.md", "---\ntags: [x]\n---\n[[Nowhere]]\n");
    let report = validate(dir.path(), &vault()).unwrap();
    assert_eq!(
        report.render(),
        "files scanned: 1\n\
         concepts: 1\n\
         nodes: Concept 1, Note 1, Tag 1\n\
         edges: LINKS_TO 1, TAGGED 1\n\
         dangling links: 1\n\
         folder notes: 0\n\
         missing attachments: 0 (0 ambiguous)\n\
         indexes declared: 0\n\
         text indexes built: 0\n\
         skills imported: 0\n\
         recipes imported: 0\n\
         forced chunk splits: 0\n\
         embed targets: none\n\
         errors: none\n\
         warnings (1):\n  \
         - dangling link: `Nowhere`\n"
    );
}

/// One fixture per §9 **error** class, so the spec's list is provable rather
/// than aspirational. A class that stops being producible fails here, not in a
/// converter's test suite six months later.
#[test]
fn every_error_class_the_spec_names_is_producible() {
    let dir = tempdir().unwrap();
    // 1. unparseable frontmatter (§4)
    write(dir.path(), "broken.md", "---\na: [1, 2\n---\nprose");
    // 2. a reserved key of the wrong shape (§4.1)
    write(dir.path(), "reserved.md", "---\ntags: seismic\n---\nprose");
    // 3. an id collision (§3)
    write(dir.path(), "one/dup.md", "prose");
    write(dir.path(), "two/dup.md", "prose");
    // 4. two folder notes for one directory (§2.3)
    write(dir.path(), "wing.md", "the folder note beside");
    write(dir.path(), "wing/wing.md", "and the one inside");
    write(dir.path(), "wing/leaf.md", "a note under it");
    // 5. a reference naming a place the vault does not own (§6.2, §9)
    write(dir.path(), "escape.md", "[out](../../elsewhere/x.md)\n");

    let errors = validate(dir.path(), &vault()).unwrap().errors;
    let classes = [
        "invalid YAML in frontmatter",
        "reserved key `tags:` must be a list",
        "id collision",
        "folder note declared twice",
        "escapes the vault root",
    ];
    for class in classes {
        assert!(
            errors.iter().any(|e| e.contains(class)),
            "no `{class}` in {errors:?}"
        );
    }
    // Six, not five: `wing.md` and `wing/wing.md` share a stem, so declaring
    // the folder note twice also collides as an id.
    assert_eq!(errors.len(), classes.len() + 1, "{errors:?}");

    // 6. a `.kglite/vault.yaml` the schema refuses — the build's `Err`, which
    // the validator reports in the same shape (§7).
    let broken = tempdir().unwrap();
    write(broken.path(), "a.md", "prose");
    write(broken.path(), ".kglite/vault.yaml", "kglite_vault: 2\n");
    let errors = validate(broken.path(), &vault()).unwrap().errors;
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("kglite_vault"), "{}", errors[0]);

    // 7. an ontology the declaration API refuses (§7).
    let ontology = tempdir().unwrap();
    write(ontology.path(), "a.md", "prose");
    write(
        ontology.path(),
        ".kglite/vault.yaml",
        "kglite_vault: 1\nontology:\n  relationships:\n    - type: LINKS_TO\n",
    );
    let errors = validate(ontology.path(), &vault()).unwrap().errors;
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("ontology"), "{}", errors[0]);
}
