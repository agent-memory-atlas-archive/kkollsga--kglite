//! What a power cut can leave at each step of a checkpoint.
//!
//! The order of the syncs is the guarantee: the new `.kgl` and its directory
//! entry are on disk before the log that describes the same commits is cut.
use super::execute::{execute_mut, execute_read, ExecuteOptions};
use super::{CommitOutcome, Session};
use crate::graph::dir_graph::DirGraph;
use crate::graph::durable_io::trace;
use crate::graph::wal::{recover, wal_path, DurabilityLevel};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

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

fn nodes(graph: &DirGraph) -> usize {
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    execute_read(graph, "MATCH (n:N) RETURN n.id AS id", &opts)
        .unwrap()
        .result
        .rows
        .len()
}

fn open(path: &Path, level: DurabilityLevel) -> Result<Session, String> {
    let p = path.to_string_lossy().into_owned();
    let graph = if path.exists() {
        crate::graph::io::file::load_file(&p).unwrap()
    } else {
        Arc::new(DirGraph::new())
    };
    Session::open_durable(graph, &p, level)
}

fn position(events: &[String], needle: &str) -> usize {
    events
        .iter()
        .position(|e| e == needle)
        .unwrap_or_else(|| panic!("no `{needle}` in {events:?}"))
}

#[test]
fn a_checkpoint_syncs_the_file_then_its_directory_entry_before_cutting_the_log() {
    for level in [DurabilityLevel::Full, DurabilityLevel::Normal] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("g.kgl");
        let session = open(&path, level).unwrap();
        commit(&session, "CREATE (:N {id: 1})");
        commit(&session, "CREATE (:N {id: 2})");
        let (saved, events) = trace::capture(|| session.save(&path.to_string_lossy(), true));
        saved.unwrap();
        let order = [
            position(&events, "kgl temp sync_all"),
            position(&events, "kgl rename"),
            position(&events, "kgl sync_dir"),
            position(&events, "wal truncate+sync_all"),
        ];
        assert!(
            order.windows(2).all(|w| w[0] < w[1]),
            "{level:?}: {events:?}"
        );
        if level == DurabilityLevel::Normal {
            assert!(
                position(&events, "wal sync_data") < order[0],
                "frames still in the page cache must be barriered before the checkpoint \
                 can cut them: {events:?}"
            );
        }
    }
}

/// A checkpoint whose rename cannot be made durable must not cut the log.
#[cfg(unix)]
#[test]
fn a_checkpoint_that_cannot_sync_its_directory_keeps_the_log() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("g.kgl");
    let session = open(&path, DurabilityLevel::Full).unwrap();
    commit(&session, "CREATE (:N {id: 1})");
    let wal = wal_path(&path);
    let before = std::fs::read(&wal).unwrap();
    // Writable and searchable but not readable: files can be created and
    // renamed, the directory cannot be opened to sync.
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o300)).unwrap();
    let saved = session.save(&path.to_string_lossy(), true);
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(saved.is_err(), "the checkpoint must report the failed sync");
    assert_eq!(std::fs::read(&wal).unwrap(), before, "log must be intact");
}

/// Cut after the new `.kgl` is durable and before the log is truncated: both
/// files are on disk, and replay must not apply the folded frames twice.
#[test]
fn a_cut_between_the_new_file_and_the_log_truncation_recovers_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("g.kgl");
    let session = open(&path, DurabilityLevel::Full).unwrap();
    commit(&session, "CREATE (:N {id: 1})");
    commit(&session, "CREATE (:N {id: 2})");
    let wal = wal_path(&path);
    let before_cut = std::fs::read(&wal).unwrap();
    session.save(&path.to_string_lossy(), true).unwrap();
    drop(session);
    std::fs::write(&wal, &before_cut).unwrap(); // the truncation never landed
    assert_eq!(recover(&wal).unwrap().len(), 2);
    let recovered = open(&path, DurabilityLevel::Full).unwrap();
    assert_eq!(nodes(&recovered.snapshot()), 2);
    commit(&recovered, "CREATE (:N {id: 3})");
    drop(recovered);
    let again = open(&path, DurabilityLevel::Full).unwrap();
    assert_eq!(nodes(&again.snapshot()), 3, "no commit lost or doubled");
}

/// Cut during the temp write: the old `.kgl` (none yet) and the whole log are
/// intact beside a stray temp, which is ignored and reaped.
#[test]
fn a_cut_while_the_checkpoint_temp_is_being_written_recovers_from_the_log() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("g.kgl");
    let session = open(&path, DurabilityLevel::Full).unwrap();
    commit(&session, "CREATE (:N {id: 1})");
    drop(session);
    std::fs::write(
        dir.path().join("g.kgl.tmp.999999999.0"),
        b"half a checkpoint",
    )
    .unwrap();
    let recovered = open(&path, DurabilityLevel::Full).unwrap();
    assert_eq!(nodes(&recovered.snapshot()), 1);
}
