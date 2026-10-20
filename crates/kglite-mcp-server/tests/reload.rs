//! `ping(reload=true)` through real stdio sessions against copies of the
//! server binary.

mod support;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use kglite::api::session::{execute_mut, ExecuteOptions};
use kglite::api::storage::{new_dir_graph_in_mode, StorageMode};
use serde_json::{json, Value};
use support::*;

fn seed_graph(path: &Path, mode: StorageMode, label: &str, nodes: u64) {
    let disk = matches!(mode, StorageMode::Disk);
    let mut dir = new_dir_graph_in_mode(mode, disk.then_some(path)).expect("create graph");
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    for i in 0..nodes {
        execute_mut(&mut dir, &format!("CREATE (:{label} {{id:'{i}'}})"), &opts).expect("seed");
    }
    let mut graph = Arc::new(dir);
    kglite::api::io::save_graph(&mut graph, path.to_str().expect("utf-8 path")).expect("save");
}

struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    installed: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical");
    let installed = root.join("kglite-mcp-server");
    install_binary(server_binary().as_ref(), &installed);
    Fixture {
        _tmp: tmp,
        root,
        installed,
    }
}

/// The single value of a one-row `count(n) AS c` answer: the line after the
/// `c` header (digits elsewhere in the text, such as the status footer, must
/// not satisfy an assertion).
fn count(session: &mut Session, id: i64, label: &str) -> i64 {
    let text = result_text(&session.call(
        id,
        "cypher_query",
        json!({"query": format!("MATCH (n:{label}) RETURN count(n) AS c")}),
    ));
    let mut lines = text.lines().skip_while(|line| *line != "c");
    lines.next();
    lines
        .next()
        .and_then(|line| line.trim().parse().ok())
        .unwrap_or_else(|| panic!("no count in: {text}"))
}

fn ping_reload(session: &mut Session, id: i64) -> Value {
    session.call(id, "ping", json!({"reload": true}))
}

#[test]
fn pipelined_request_survives_a_reload() {
    let fx = fixture();
    let graph = fx.root.join("a.kgl");
    seed_graph(&graph, StorageMode::Memory, "A", 3);
    let mut session = Session::spawn(
        &fx.installed,
        &["--graph".as_ref(), graph.as_os_str()],
        &[],
        None,
    );
    session.initialize();
    assert_eq!(count(&mut session, 1, "A"), 3);

    replace_binary(server_binary().as_ref(), &fx.installed);
    let pid = session.child.id();

    // Both requests are written before either answer is read: the second one
    // arrives behind the reload and must be answered by the new image.
    session.request(
        10,
        "tools/call",
        json!({"name": "ping", "arguments": {"reload": true}}),
    );
    session.request(
        11,
        "tools/call",
        json!({"name": "cypher_query", "arguments": {"query": "MATCH (n:A) RETURN count(n) AS c"}}),
    );
    let reloaded = result_text(&session.recv_id(10, Duration::from_secs(60)));
    assert!(reloaded.contains("reloaded"), "{reloaded}");
    let after = result_text(&session.recv_id(11, Duration::from_secs(60)));
    assert!(after.lines().any(|line| line.trim() == "3"), "{after}");
    assert!(!after.contains("stale:"), "the new image is fresh: {after}");
    assert_eq!(session.child.id(), pid);
    assert!(session.child.try_wait().unwrap().is_none());
}

#[test]
fn unsaved_changes_refuse_a_reload() {
    let fx = fixture();
    let graph = fx.root.join("a.kgl");
    seed_graph(&graph, StorageMode::Memory, "A", 1);
    let mut session = Session::spawn(
        &fx.installed,
        &["--graph".as_ref(), graph.as_os_str(), "--writable".as_ref()],
        &[],
        None,
    );
    session.initialize();
    session.call(
        1,
        "cypher_query",
        json!({"query": "CREATE (:A {id: 'dirty'})"}),
    );
    replace_binary(server_binary().as_ref(), &fx.installed);

    let refused = result_text(&ping_reload(&mut session, 2));
    assert!(refused.contains("unsaved"), "{refused}");
    assert!(!refused.contains("reloaded"), "{refused}");
    // Still the same process, still holding the change.
    assert_eq!(count(&mut session, 3, "A"), 2);
}

#[test]
fn a_build_that_fails_the_preflight_is_not_loaded() {
    let fx = fixture();
    let graph = fx.root.join("a.kgl");
    seed_graph(&graph, StorageMode::Memory, "A", 3);
    let manifest = fx.root.join("m.yaml");
    std::fs::write(&manifest, "name: preflight\n").unwrap();
    let mut session = Session::spawn(
        &fx.installed,
        &[
            "--graph".as_ref(),
            graph.as_os_str(),
            "--mcp-config".as_ref(),
            manifest.as_os_str(),
        ],
        &[],
        None,
    );
    session.initialize();
    replace_binary(server_binary().as_ref(), &fx.installed);
    // What the new build would choke on at boot: the manifest it is launched with.
    std::fs::write(&manifest, "name: [unterminated\n").unwrap();

    let refused = result_text(&ping_reload(&mut session, 2));
    assert!(refused.contains("--reload-preflight"), "{refused}");
    assert!(!refused.contains("reloaded"), "{refused}");
    assert_eq!(count(&mut session, 3, "A"), 3, "old image keeps serving");
}

#[test]
fn a_rebound_graph_is_reopened_after_the_reload() {
    let fx = fixture();
    let a = fx.root.join("a.kgl");
    let b = fx.root.join("b.kgl");
    seed_graph(&a, StorageMode::Memory, "A", 3);
    seed_graph(&b, StorageMode::Memory, "B", 5);
    let mut session = Session::spawn(
        &fx.installed,
        &["--graph".as_ref(), a.as_os_str(), "--writable".as_ref()],
        &[],
        None,
    );
    session.initialize();
    let loaded = result_text(&session.call(1, "load_graph", json!({"path": b})));
    assert!(loaded.contains("Loaded"), "{loaded}");

    replace_binary(server_binary().as_ref(), &fx.installed);
    let reloaded = result_text(&ping_reload(&mut session, 2));
    assert!(reloaded.contains("reloaded"), "{reloaded}");
    assert_eq!(
        count(&mut session, 3, "B"),
        5,
        "the rebound graph, not the boot graph, is served"
    );
}

#[cfg(unix)]
#[test]
fn a_writable_disk_graph_leaves_no_working_directories_behind() {
    let fx = fixture();
    let graph = fx.root.join("disk");
    seed_graph(&graph, StorageMode::Disk, "A", 3);
    let mut session = Session::spawn(
        &fx.installed,
        &["--graph".as_ref(), graph.as_os_str(), "--writable".as_ref()],
        &[],
        None,
    );
    session.initialize();
    session.call(1, "cypher_query", json!({"query": "CREATE (:A {id: 'w'})"}));
    let pid = session.child.id();
    let working = || -> Vec<String> {
        std::fs::read_dir(&graph)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(&format!(".working-{pid}-")))
            .collect()
    };
    assert!(
        !working().is_empty(),
        "non-vacuous: the write created a working directory"
    );

    // Save so the reload is not refused as dirty, then reload.
    let saved = result_text(&session.call(2, "save_graph", json!({})));
    assert!(!saved.to_lowercase().contains("error"), "{saved}");
    replace_binary(server_binary().as_ref(), &fx.installed);
    let reloaded = result_text(&ping_reload(&mut session, 3));
    assert!(reloaded.contains("reloaded"), "{reloaded}");

    // Saving already removes the workspace and a dirty graph refuses the reload,
    // so this pins the property rather than the drop in `before_exec` alone.
    assert_eq!(
        working(),
        Vec::<String>::new(),
        "no orphaned working directory"
    );
    let prefixes = [
        format!("kglite_copy_{pid}_"),
        format!("kglite_spill_{pid}_"),
        format!("kglite_portable_{pid}_"),
    ];
    let leaked: Vec<String> = std::fs::read_dir(std::env::temp_dir())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| prefixes.iter().any(|p| n.starts_with(p)))
        .collect();
    assert!(leaked.is_empty(), "leaked scratch directories: {leaked:?}");
    assert_eq!(count(&mut session, 4, "A"), 4, "the saved write survived");
}
