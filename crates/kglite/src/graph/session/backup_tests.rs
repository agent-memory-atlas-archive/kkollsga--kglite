//! `Session::backup`: a consistent single-file snapshot taken without stalling
//! committers, stamped with the log position it contains.
use super::backup::window_hook;
use super::execute::{execute_mut, execute_read, ExecuteOptions};
use super::{BackupOptions, CommitOutcome, Session};
use crate::graph::dir_graph::DirGraph;
use crate::graph::durable_io::trace;
use crate::graph::io::file::{checkpoint_lsn_from_file, load_file, SaveError};
use crate::graph::storage::mode::{new_dir_graph_in_mode, StorageMode};
use crate::graph::storage::GraphRead;
use crate::graph::wal::{wal_path, DurabilityLevel, SyncMode, Wal, WalFrame};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

const MODES: [StorageMode; 2] = [StorageMode::Memory, StorageMode::Mapped];

fn commit(session: &Session, query: &str) {
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    let mut tx = session.begin();
    execute_mut(tx.working_mut().unwrap(), query, &opts).unwrap();
    assert!(matches!(
        session.commit(tx, true),
        CommitOutcome::Committed { .. }
    ));
}

fn in_mode(mode: StorageMode) -> Session {
    Session::new(new_dir_graph_in_mode(mode, None).unwrap())
}

fn durable(path: &Path) -> Session {
    let p = path.to_string_lossy().into_owned();
    let graph = if path.exists() {
        load_file(&p).unwrap()
    } else {
        Arc::new(DirGraph::new())
    };
    Session::open_durable(graph, &p, DurabilityLevel::Full).unwrap()
}

fn populate(session: &Session) {
    commit(
        session,
        "CREATE (a:N {id: 1, name: 'a'}), (b:N {id: 2, name: 'b'}), (a)-[:R {w: 1}]->(b)",
    );
    commit(session, "CREATE (:N {id: 3, name: 'c'})");
}

/// What a reader of `graph` sees: every node and relationship, in order.
fn contents(graph: &DirGraph) -> String {
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    let nodes = execute_read(
        graph,
        "MATCH (n:N) RETURN n.id AS id, n.name AS name ORDER BY id",
        &opts,
    )
    .unwrap();
    let rels = execute_read(
        graph,
        "MATCH (a)-[r:R]->(b) RETURN a.id AS a, b.id AS b, r.w AS w ORDER BY a",
        &opts,
    )
    .unwrap();
    format!("{:?}\n{:?}", nodes.result.rows, rels.result.rows)
}

fn names(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

fn refused(result: Result<super::BackupReport, SaveError>) -> String {
    match result {
        Err(SaveError::Refused(message)) => message,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn a_backup_equals_a_save_of_the_same_graph_in_every_mode() {
    for mode in MODES {
        let session = in_mode(mode);
        populate(&session);
        let dir = tempfile::tempdir().unwrap();
        let backup = dir.path().join("backup.kgl");
        let saved = dir.path().join("saved.kgl");
        let live_before = session.snapshot();
        let report = session.backup(&backup, &BackupOptions::default()).unwrap();
        // The backup read the published Arc as it stood: same allocation, same version.
        assert!(Arc::ptr_eq(&live_before, &session.snapshot()), "{mode:?}");
        session.save(saved.to_str().unwrap(), true).unwrap();
        assert_eq!(
            std::fs::read(&backup).unwrap(),
            std::fs::read(&saved).unwrap(),
            "{mode:?}: backup must be byte-identical to save() of the same graph"
        );
        assert_eq!(report.bytes, std::fs::metadata(&backup).unwrap().len());
        assert_eq!((report.nodes, report.relationships), (3, 1), "{mode:?}");
        assert_eq!(report.lsn, None);
        assert_eq!(report.graph_version, live_before.version());
        assert_eq!(names(dir.path()), ["backup.kgl", "saved.kgl"]);
    }
}

#[test]
fn a_snapshot_needing_preparation_is_prepared_on_a_private_copy() {
    for mode in MODES {
        let session = in_mode(mode);
        populate(&session);
        // Deleting a node strands its column row; the next create reuses the
        // freed slot out of row order, which is what a save must repair.
        commit(&session, "MATCH (n:N {id: 1}) DETACH DELETE n");
        commit(&session, "CREATE (:N {id: 4, name: 'd'})");
        let dir = tempfile::tempdir().unwrap();
        let backup = dir.path().join("backup.kgl");
        let saved = dir.path().join("saved.kgl");
        let live_before = session.snapshot();
        assert!(live_before.columnar_rebuild_needed(), "{mode:?}: fixture");
        let report = session.backup(&backup, &BackupOptions::default()).unwrap();
        assert!(report.prepared_copy, "{mode:?}");
        assert!(
            live_before.columnar_rebuild_needed() && Arc::ptr_eq(&live_before, &session.snapshot()),
            "{mode:?}: the published graph must not have been prepared in place"
        );
        session.save(saved.to_str().unwrap(), true).unwrap();
        assert_eq!(
            std::fs::read(&backup).unwrap(),
            std::fs::read(&saved).unwrap(),
            "{mode:?}"
        );
        assert_eq!(
            contents(&load_file(backup.to_str().unwrap()).unwrap()),
            contents(&session.snapshot()),
            "{mode:?}"
        );
    }
}

#[test]
fn a_durable_backup_is_stamped_with_the_last_committed_lsn_and_reopens_clean() {
    let live_dir = tempfile::tempdir().unwrap();
    let live = live_dir.path().join("g.kgl");
    let session = durable(&live);
    let out = tempfile::tempdir().unwrap();
    let backup = out.path().join("backup.kgl");

    let empty = session.backup(&backup, &BackupOptions::default()).unwrap();
    assert_eq!(empty.lsn, Some(0), "a durable session that logged nothing");

    populate(&session); // two commits: LSN 1 and 2
    commit(&session, "CREATE (:N {id: 4, name: 'd'})");
    let report = session.backup(&backup, &BackupOptions::default()).unwrap();
    assert_eq!(report.lsn, Some(3));
    assert_ne!(
        Some(report.graph_version),
        report.lsn,
        "version and LSN are different counters"
    );
    assert_eq!(checkpoint_lsn_from_file(&backup).unwrap(), 3);

    // The same bytes a checkpoint of the same state writes.
    session.save(live.to_str().unwrap(), true).unwrap();
    assert_eq!(
        std::fs::read(&backup).unwrap(),
        std::fs::read(&live).unwrap()
    );

    assert_eq!(names(out.path()), ["backup.kgl"], "no sidecar of any kind");
    let reopened = load_file(backup.to_str().unwrap()).unwrap();
    assert_eq!(contents(&reopened), contents(&session.snapshot()));
    assert_eq!(
        names(out.path()),
        ["backup.kgl"],
        "reading it adds none either"
    );
}

#[test]
fn a_commit_during_the_serialize_window_is_neither_blocked_nor_in_the_file() {
    for mode in MODES {
        let session = Arc::new(in_mode(mode));
        populate(&session);
        let dir = tempfile::tempdir().unwrap();
        let backup = dir.path().join("backup.kgl");
        let (done, finished) = mpsc::channel();
        {
            let session = Arc::clone(&session);
            window_hook::set(move || {
                let writer = std::thread::spawn(move || {
                    commit(&session, "CREATE (:N {id: 99, name: 'late'})");
                    done.send(()).unwrap();
                });
                // Held locks would park the writer forever; fail instead of hanging.
                finished
                    .recv_timeout(Duration::from_secs(20))
                    .expect("a committer must not wait for the backup's serialize");
                writer.join().unwrap();
            });
        }
        let report = session.backup(&backup, &BackupOptions::default()).unwrap();
        assert_eq!(
            report.nodes, 3,
            "{mode:?}: the point in time precedes the commit"
        );
        assert_eq!(session.snapshot().graph.node_count(), 4);
        let reopened = load_file(backup.to_str().unwrap()).unwrap();
        assert!(!contents(&reopened).contains("late"), "{mode:?}");
        eprintln!("{mode:?} lock_hold = {:?}", report.lock_hold);
        assert!(report.lock_hold < Duration::from_millis(250), "{mode:?}");
    }
}

#[test]
fn a_durable_commit_during_the_window_leaves_the_stamp_at_the_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("g.kgl");
    let session = Arc::new(durable(&live));
    populate(&session);
    let out = tempfile::tempdir().unwrap();
    let backup = out.path().join("backup.kgl");
    let (done, finished) = mpsc::channel();
    {
        let session = Arc::clone(&session);
        window_hook::set(move || {
            let writer = std::thread::spawn(move || {
                commit(&session, "CREATE (:N {id: 99, name: 'late'})");
                done.send(()).unwrap();
            });
            finished.recv_timeout(Duration::from_secs(20)).unwrap();
            writer.join().unwrap();
        });
    }
    let report = session.backup(&backup, &BackupOptions::default()).unwrap();
    assert_eq!(report.lsn, Some(2));
    assert_eq!(checkpoint_lsn_from_file(&backup).unwrap(), 2);
    assert!(!contents(&load_file(backup.to_str().unwrap()).unwrap()).contains("late"));
}

#[test]
fn nothing_is_written_to_the_destination_before_the_final_rename() {
    let session = in_mode(StorageMode::Memory);
    populate(&session);
    let dir = tempfile::tempdir().unwrap();
    let backup = dir.path().join("backup.kgl");
    session.backup(&backup, &BackupOptions::default()).unwrap();
    let previous = std::fs::read(&backup).unwrap();
    commit(&session, "CREATE (:N {id: 7, name: 'g'})");

    let seen = Arc::new(std::sync::Mutex::new(None));
    {
        let (seen, backup, dir) = (Arc::clone(&seen), backup.clone(), dir.path().to_path_buf());
        window_hook::set(move || {
            *seen.lock().unwrap() = Some((std::fs::read(&backup).unwrap(), names(&dir)));
        });
    }
    let (report, events) =
        trace::capture(|| session.backup(&backup, &BackupOptions::default()).unwrap());
    let (during, _) = seen.lock().unwrap().take().unwrap();
    assert_eq!(
        during, previous,
        "the old backup stays whole until the rename"
    );
    assert_ne!(std::fs::read(&backup).unwrap(), previous);
    assert_eq!(
        events,
        ["kgl temp sync_all", "kgl rename", "kgl sync_dir"],
        "temp is synced, renamed, then the directory entry is synced; no log is touched"
    );
    assert_eq!(report.bytes, std::fs::metadata(&backup).unwrap().len());
    assert_eq!(names(dir.path()), ["backup.kgl"], "no temp litter");
}

#[test]
fn a_failed_backup_leaves_the_previous_one_untouched() {
    let session = in_mode(StorageMode::Memory);
    populate(&session);
    let dir = tempfile::tempdir().unwrap();
    let backup = dir.path().join("backup.kgl");
    session.backup(&backup, &BackupOptions::default()).unwrap();
    let previous = std::fs::read(&backup).unwrap();
    let missing = dir.path().join("absent").join("backup.kgl");
    assert!(session.backup(&missing, &BackupOptions::default()).is_err());
    assert_eq!(std::fs::read(&backup).unwrap(), previous);
    assert_eq!(names(dir.path()), ["backup.kgl"]);
}

fn alias_destinations(live: &Path) -> Vec<(&'static str, PathBuf)> {
    let dir = live.parent().unwrap();
    let name = live.file_name().unwrap();
    let mut out = vec![
        ("same path", live.to_path_buf()),
        ("dot alias", dir.join(".").join(name)),
        ("parent hop", dir.join("sub").join("..").join(name)),
    ];
    #[cfg(unix)]
    {
        let link = dir.join("link");
        std::os::unix::fs::symlink(dir, &link).unwrap();
        out.push(("symlinked directory", link.join(name)));
    }
    out
}

#[test]
fn a_destination_aliasing_the_live_graph_is_refused_before_anything_is_written() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    let live = dir.path().join("g.kgl");
    let session = durable(&live);
    populate(&session);
    session.save(live.to_str().unwrap(), true).unwrap();
    let wal_before = std::fs::read(wal_path(&live)).unwrap();
    let tree_before = {
        let mut tree = names(dir.path());
        if cfg!(unix) {
            tree.push("link".to_string());
        }
        tree.sort();
        tree
    };
    let file_before = std::fs::read(&live).unwrap();
    for (what, dest) in alias_destinations(&live) {
        // Durable: the live path is derived from the log; a caller-supplied one agrees.
        for opts in [
            BackupOptions::default(),
            BackupOptions {
                live_path: Some(live.clone()),
            },
        ] {
            let message = refused(session.backup(&dest, &opts));
            assert!(message.contains("live graph"), "{what}: {message}");
        }
        assert_eq!(std::fs::read(&live).unwrap(), file_before, "{what}");
        assert_eq!(
            std::fs::read(wal_path(&live)).unwrap(),
            wal_before,
            "{what}"
        );
    }
    assert_eq!(names(dir.path()), tree_before, "nothing was created");
}

#[test]
fn a_caller_supplied_live_path_guards_a_session_that_logs_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("g.kgl");
    let session = in_mode(StorageMode::Memory);
    populate(&session);
    session.save(live.to_str().unwrap(), true).unwrap();
    let opts = BackupOptions {
        live_path: Some(live.clone()),
    };
    refused(session.backup(&live, &opts));
    refused(session.backup(&dir.path().join(".").join("g.kgl"), &opts));
    session
        .backup(&dir.path().join("other.kgl"), &opts)
        .unwrap();
}

#[test]
fn a_stray_log_beside_the_destination_holding_foreign_commits_is_refused() {
    let session = in_mode(StorageMode::Memory);
    populate(&session);
    let dir = tempfile::tempdir().unwrap();
    let backup = dir.path().join("backup.kgl");
    Wal::open(wal_path(&backup), SyncMode::Barrier)
        .unwrap()
        .append(&WalFrame {
            lsn: 5,
            ops: vec![],
        })
        .unwrap();
    let message = refused(session.backup(&backup, &BackupOptions::default()));
    assert!(message.contains("write-ahead log"), "{message}");
    assert!(!backup.exists(), "a refused backup writes nothing");
}

#[test]
fn a_stray_log_already_contained_in_the_old_destination_is_cleared_not_replayed() {
    let session = in_mode(StorageMode::Memory);
    populate(&session);
    let dir = tempfile::tempdir().unwrap();
    let backup = dir.path().join("backup.kgl");
    let out = Session::open_durable(
        Arc::new(DirGraph::new()),
        backup.to_str().unwrap(),
        DurabilityLevel::Full,
    )
    .unwrap();
    commit(&out, "CREATE (:N {id: 50, name: 'old'})");
    out.save(backup.to_str().unwrap(), true).unwrap();
    drop(out);
    Wal::open(wal_path(&backup), SyncMode::Barrier)
        .unwrap()
        .append(&WalFrame {
            lsn: 1,
            ops: vec![],
        })
        .unwrap();
    session.backup(&backup, &BackupOptions::default()).unwrap();
    assert!(crate::graph::wal::recover(&wal_path(&backup))
        .unwrap()
        .is_empty());
    assert_eq!(
        contents(&load_file(backup.to_str().unwrap()).unwrap()),
        contents(&session.snapshot())
    );
}

#[test]
fn a_disk_graph_is_refused_with_the_reason() {
    let dir = tempfile::tempdir().unwrap();
    let graph = new_dir_graph_in_mode(StorageMode::Disk, Some(&dir.path().join("g"))).unwrap();
    let session = Session::new(graph);
    let out = tempfile::tempdir().unwrap();
    let backup = out.path().join("backup.kgl");
    let message = refused(session.backup(&backup, &BackupOptions::default()));
    assert!(message.contains("single .kgl"), "{message}");
    assert!(!backup.exists());
}

#[cfg(unix)]
#[test]
fn a_symlink_or_hardlink_to_the_live_graph_is_refused_but_other_links_are_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("g.kgl");
    let session = in_mode(StorageMode::Memory);
    populate(&session);
    session.save(live.to_str().unwrap(), true).unwrap();
    let opts = BackupOptions {
        live_path: Some(live.clone()),
    };
    let before = std::fs::read(&live).unwrap();

    let sym = dir.path().join("sym.kgl");
    std::os::unix::fs::symlink(&live, &sym).unwrap();
    let message = refused(session.backup(&sym, &opts));
    assert!(message.contains("symlink"), "{message}");

    let hard = dir.path().join("hard.kgl");
    std::fs::hard_link(&live, &hard).unwrap();
    let message = refused(session.backup(&hard, &opts));
    assert!(message.contains("hardlink"), "{message}");
    assert_eq!(std::fs::read(&live).unwrap(), before);

    // A symlink to an unrelated file is still replaced by the atomic publish.
    let other = dir.path().join("other.kgl");
    std::fs::write(&other, b"unrelated").unwrap();
    let ok = dir.path().join("ok.kgl");
    std::os::unix::fs::symlink(&other, &ok).unwrap();
    session.backup(&ok, &opts).unwrap();
    assert!(!std::fs::symlink_metadata(&ok)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(std::fs::read(&other).unwrap(), b"unrelated");
}

#[cfg(unix)]
#[test]
fn a_durable_session_refuses_a_symlink_to_its_checkpoint_without_a_live_path() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("g.kgl");
    let session = durable(&live);
    populate(&session);
    session.save(live.to_str().unwrap(), true).unwrap();
    let sym = dir.path().join("sym.kgl");
    std::os::unix::fs::symlink(&live, &sym).unwrap();
    let message = refused(session.backup(&sym, &BackupOptions::default()));
    assert!(message.contains("symlink"), "{message}");
}

#[cfg(unix)]
#[test]
fn stale_destination_temps_are_reaped_and_live_ones_kept() {
    let session = in_mode(StorageMode::Memory);
    populate(&session);
    let dir = tempfile::tempdir().unwrap();
    let backup = dir.path().join("backup.kgl");
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let dead_pid = child.id();
    child.wait().unwrap();
    let stale = dir.path().join(format!("backup.kgl.tmp.{dead_pid}.7"));
    let mine = dir
        .path()
        .join(format!("backup.kgl.tmp.{}.8", std::process::id()));
    let foreign = dir.path().join("backup.kgl.tmp.notes");
    for p in [&stale, &mine, &foreign] {
        std::fs::write(p, b"partial").unwrap();
    }
    session.backup(&backup, &BackupOptions::default()).unwrap();
    assert!(!stale.exists(), "dead writer's temp is reaped");
    assert!(mine.exists(), "a live process's temp is kept");
    assert!(foreign.exists(), "a non-temp name is never touched");
}
