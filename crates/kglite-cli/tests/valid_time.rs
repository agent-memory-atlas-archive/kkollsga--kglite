//! Valid-time statements through the CLI: the `FOR VALID_TIME ALL` prefix as
//! text, and `--valid-time-default` on `query`, `write` and `session`.

use std::path::Path;
use std::process::{Command, Output, Stdio};

use serde_json::{json, Value};

fn invoke(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kglite"))
        .args(args)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone()).unwrap()
}

/// Well 1 (2000-2010, closed) and well 2 (from 2005), declared on `vf`/`vt`.
fn wells(dir: &Path) -> String {
    let path = dir.join("wells.kgl").to_str().unwrap().to_string();
    for statement in [
        "CREATE (:Well {id: 1, vf: date('2000-01-01'), vt: date('2010-01-01')}), \
         (:Well {id: 2, vf: date('2005-01-01')})",
        "CALL db.temporal.declare({node: 'Well', from: 'vf', to: 'vt', convention: 'closed'}) \
         YIELD declared RETURN declared",
    ] {
        stdout(&invoke(&["write", &path, statement, "--save"]));
    }
    path
}

fn ids(args: &[&str]) -> Vec<i64> {
    let text = stdout(&invoke(args));
    let rows: Value = serde_json::from_str(&text).unwrap();
    rows.as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_i64().unwrap())
        .collect()
}

const LIST: &str = "MATCH (w:Well) RETURN w.id AS id ORDER BY id";

#[test]
fn the_all_prefix_and_the_default_flag_govern_query() {
    let temp = tempfile::tempdir().unwrap();
    let graph = wells(temp.path());
    let query = |text: &str, extra: &[&str]| {
        let mut args = vec!["query", graph.as_str(), text, "--format", "json"];
        args.extend_from_slice(extra);
        ids(&args)
    };
    assert_eq!(query(LIST, &[]), [2], "the default is valid today");
    let all = format!("FOR VALID_TIME ALL {LIST}");
    assert_eq!(query(&all, &[]), [1, 2], "the text prefix opts out");
    assert_eq!(query(LIST, &["--valid-time-default", "all"]), [1, 2]);
    assert_eq!(query(LIST, &["--valid-time-default", "2003-01-01"]), [1]);
    assert_eq!(query(LIST, &["--valid-time-default", "2008-01-01"]), [1, 2]);
    assert_eq!(query(LIST, &["--valid-time-default", "today"]), [2]);
    let prefixed = format!("FOR VALID_TIME AS OF date('2003-01-01') {LIST}");
    assert_eq!(
        query(&prefixed, &["--valid-time-default", "all"]),
        [1],
        "a statement's own prefix wins over the flag"
    );
    let refused = invoke(&["query", &graph, LIST, "--valid-time-default", "yesterday"]);
    assert!(!refused.status.success());
}

#[test]
fn the_default_flag_governs_write_reads_and_the_session() {
    let temp = tempfile::tempdir().unwrap();
    let graph = wells(temp.path());
    let write = ids(&[
        "write",
        &graph,
        LIST,
        "--format",
        "json",
        "--valid-time-default",
        "all",
    ]);
    assert_eq!(write, [1, 2]);

    let mut session = Command::new(env!("CARGO_BIN_EXE_kglite"))
        .args(["session", &graph, "--valid-time-default", "2003-01-01"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        let stdin = session.stdin.as_mut().unwrap();
        writeln!(stdin, "{}", json!({"op": "query", "query": LIST})).unwrap();
        writeln!(stdin, "{}", json!({"op": "exit"})).unwrap();
    }
    let output = session.wait_with_output().unwrap();
    let first = String::from_utf8(output.stdout).unwrap();
    let reply: Value = serde_json::from_str(first.lines().next().unwrap()).unwrap();
    let rows = reply["rows"].as_array().expect(&first);
    assert_eq!(rows.len(), 1, "only well 1 is valid on 2003-01-01: {first}");
}

/// The wells graph again, with `all` stored as the graph's own default.
fn wells_storing_all(dir: &Path) -> String {
    let path = wells(dir);
    let mut graph = kglite::api::io::load_file(&path).unwrap();
    kglite::api::make_dir_graph_mut(&mut graph)
        .set_valid_time_default(kglite::api::temporal::ValidTimeDefault::All, true);
    kglite::api::io::save_graph(&mut graph, &path).unwrap();
    path
}

/// A stored default governs an unprefixed read; the flag overrides it for the
/// run, and an explicit prefix beats both.
#[test]
fn the_flag_overrides_a_stored_default() {
    let temp = tempfile::tempdir().unwrap();
    let graph = wells_storing_all(temp.path());
    let query = |text: &str, extra: &[&str]| {
        let mut args = vec!["query", graph.as_str(), text, "--format", "json"];
        args.extend_from_slice(extra);
        ids(&args)
    };
    assert_eq!(query(LIST, &[]), [1, 2], "the stored default is all");
    assert_eq!(query(LIST, &["--valid-time-default", "today"]), [2]);
    assert_eq!(query(LIST, &["--valid-time-default", "2003-01-01"]), [1]);
    let prefixed = format!("FOR VALID_TIME AS OF date('2003-01-01') {LIST}");
    assert_eq!(
        query(&prefixed, &[]),
        [1],
        "a prefix beats the stored default"
    );
}
