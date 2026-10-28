//! `--selftest` against the shipped binary: what the report says about skills
//! the server skipped and about a graph with declared valid time.

use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::Arc;

use kglite::api::session::{execute_mut, ExecuteOptions};
use kglite::api::storage::{new_dir_graph_in_mode, StorageMode};

const VALID_SKILL: &str = "---\nname: house_rules\ndescription: Project house rules for this graph.\n---\n\n# House rules\n\nKeep queries small.\n";

fn selftest(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kglite-mcp-server"))
        .arg("--selftest")
        .args(args)
        .output()
        .expect("run selftest")
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// A bare-mode manifest with a `<manifest>.skills/` project directory holding
/// one valid skill and one past mcp-methods' 16384-byte hard limit.
fn skills_fixture(dir: &Path) -> std::path::PathBuf {
    let manifest = dir.join("mcp.yaml");
    std::fs::write(&manifest, "name: Selftest skills\nskills: true\n").expect("manifest");
    let skills = dir.join("mcp.skills");
    std::fs::create_dir_all(&skills).expect("skills dir");
    std::fs::write(skills.join("house_rules.md"), VALID_SKILL).expect("valid skill");
    let oversize = format!(
        "---\nname: too_big\ndescription: A skill that grew past the limit.\n---\n\n{}\n",
        "x".repeat(20_000)
    );
    std::fs::write(skills.join("too_big.md"), oversize).expect("oversize skill");
    manifest
}

#[test]
fn an_oversize_project_skill_fails_the_selftest_and_names_the_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manifest = skills_fixture(tmp.path());
    let output = selftest(&["--mcp-config", manifest.to_str().expect("utf-8")]);
    let report = text(&output);
    assert!(!output.status.success(), "selftest must fail:\n{report}");
    assert!(report.contains("Selftest FAILED"), "{report}");
    let line = report
        .lines()
        .find(|l| l.contains("✗ skills skipped"))
        .unwrap_or_else(|| panic!("no failing skills line:\n{report}"));
    assert!(line.contains("too_big.md"), "{line}");
    assert!(line.contains("hard limit"), "{line}");
    // The valid skill still serves, with the layer it won from.
    assert!(report.contains("house_rules[project]"), "{report}");
    assert!(report.contains("cypher_query[bundled]"), "{report}");
}

#[test]
fn a_clean_project_skill_layer_passes_and_names_each_layer() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manifest = skills_fixture(tmp.path());
    std::fs::remove_file(tmp.path().join("mcp.skills/too_big.md")).expect("remove oversize");
    let output = selftest(&["--mcp-config", manifest.to_str().expect("utf-8")]);
    let report = text(&output);
    assert!(output.status.success(), "{report}");
    assert!(!report.contains("skills skipped"), "{report}");
    assert!(report.contains("house_rules[project]"), "{report}");
    // The stateless (2026-07-28) pass reports each step on its own line.
    for label in [
        "stateless discover",
        "stateless tools/list",
        "stateless tools/call",
        "stateless list-changed stream",
    ] {
        let line = report
            .lines()
            .find(|l| l.contains(label))
            .unwrap_or_else(|| panic!("no `{label}` line:\n{report}"));
        assert!(line.contains('✓'), "{line}");
    }
}

#[test]
fn hydration_counts_nodes_that_are_not_valid_today() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("vt.kgl");
    let mut dir = new_dir_graph_in_mode(StorageMode::Memory, None).expect("graph");
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    for statement in [
        "CREATE (:Rule {id:'live', vf:'2000-01-01', vt:'2999-12-31'})",
        "CREATE (:Rule {id:'ended', vf:'2000-01-01', vt:'2001-01-01'})",
        "CREATE (:Rule {id:'future', vf:'2998-01-01', vt:'2999-12-31'})",
        "CALL db.temporal.declare({node: 'Rule', from: 'vf', to: 'vt', convention: 'closed'})",
    ] {
        execute_mut(&mut dir, statement, &opts).expect(statement);
    }
    let mut graph = Arc::new(dir);
    kglite::api::io::save_graph(&mut graph, path.to_str().expect("utf-8")).expect("save");

    let output = selftest(&["--graph", path.to_str().expect("utf-8")]);
    let report = text(&output);
    assert!(output.status.success(), "{report}");
    let line = report
        .lines()
        .find(|l| l.contains("graph hydrates"))
        .unwrap_or_else(|| panic!("no hydration line:\n{report}"));
    assert!(line.contains("→ 3 node(s)"), "{line}");
}

#[test]
fn an_invalid_graph_carried_skill_fails_the_selftest_and_names_the_record() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("g.kgl");
    let mut dir = new_dir_graph_in_mode(StorageMode::Memory, None).expect("graph");
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    for statement in [
        "CREATE (:KgliteSkill {name:'good_skill', description:'Fine.', body:'Body.'})",
        "CREATE (:KgliteSkill {name:'bad_skill', description:'', body:'Body.'})",
    ] {
        execute_mut(&mut dir, statement, &opts).expect(statement);
    }
    let mut graph = Arc::new(dir);
    kglite::api::io::save_graph(&mut graph, path.to_str().expect("utf-8")).expect("save");
    let manifest = tmp.path().join("mcp.yaml");
    std::fs::write(&manifest, "name: Graph skills\nskills: true\n").expect("manifest");

    let output = selftest(&[
        "--graph",
        path.to_str().expect("utf-8"),
        "--mcp-config",
        manifest.to_str().expect("utf-8"),
    ]);
    let report = text(&output);
    assert!(!output.status.success(), "selftest must fail:\n{report}");
    let line = report
        .lines()
        .find(|l| l.contains("✗ skills skipped"))
        .unwrap_or_else(|| panic!("no failing skills line:\n{report}"));
    assert!(line.contains("bad_skill"), "{line}");
    assert!(line.contains("description"), "{line}");
    assert!(report.contains("good_skill[graph]"), "{report}");
}
