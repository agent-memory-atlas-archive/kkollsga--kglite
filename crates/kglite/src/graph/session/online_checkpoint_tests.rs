//! `Session::checkpoint_online` and the automatic-checkpoint policy.
use super::execute::{execute_mut, execute_read, ExecuteOptions};
use super::online_checkpoint::hooks::{self, Stage};
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

fn ids(graph: &DirGraph) -> Vec<i64> {
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    let mut out: Vec<i64> = execute_read(graph, "MATCH (n:N) RETURN n.id AS id", &opts)
        .unwrap()
        .result
        .rows
        .iter()
        .map(|r| match &r[0] {
            crate::datatypes::Value::Int64(i) => *i,
            other => panic!("unexpected id {other:?}"),
        })
        .collect();
    out.sort_unstable();
    out
}

fn open(path: &Path, level: DurabilityLevel) -> Session {
    let p = path.to_string_lossy().into_owned();
    let graph = if path.exists() {
        crate::graph::io::file::load_file(&p).unwrap()
    } else {
        Arc::new(DirGraph::new())
    };
    Session::open_durable(graph, &p, level).unwrap()
}

fn frames(path: &Path) -> usize {
    recover(&wal_path(path)).unwrap().len()
}

/// Copy the checkpoint and its log to a new directory: the disk a crash at
/// this instant would leave behind.
fn crash_image(path: &Path, into: &Path) -> std::path::PathBuf {
    let copy = into.join("g.kgl");
    if path.exists() {
        std::fs::copy(path, &copy).unwrap();
    }
    std::fs::copy(wal_path(path), wal_path(&copy)).unwrap();
    copy
}

#[test]
fn checkpoint_online_replaces_the_checkpoint_and_empties_the_log() {
    for level in [DurabilityLevel::Full, DurabilityLevel::Normal] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("g.kgl");
        let session = open(&path, level);
        for i in 0..20 {
            commit(&session, &format!("CREATE (:N {{id: {i}}})"));
        }
        let before = std::fs::metadata(wal_path(&path)).unwrap().len();
        let report = session.checkpoint_online().unwrap();
        let after = std::fs::metadata(wal_path(&path)).unwrap().len();
        assert!(after < before, "{level:?}: log {before} -> {after}");
        assert_eq!(frames(&path), 0);
        assert_eq!(report.lsn, 20);
        assert_eq!(report.wal_bytes_after, 0);
        assert!(report.wal_bytes_before > 0 && report.bytes > 0);
        // The append handle follows the new log file.
        commit(&session, "CREATE (:N {id: 100})");
        assert_eq!(frames(&path), 1);
        let live = ids(&session.snapshot());
        drop(session);
        assert_eq!(ids(&open(&path, level).snapshot()), live);
    }
}

#[test]
fn commits_during_the_write_survive_the_trim_and_the_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("g.kgl");
    let session = Arc::new(open(&path, DurabilityLevel::Full));
    for i in 0..5 {
        commit(&session, &format!("CREATE (:N {{id: {i}}})"));
    }
    let during = Arc::clone(&session);
    hooks::set(Stage::AfterSnapshot, move || {
        commit(&during, "CREATE (:N {id: 50})");
        commit(&during, "CREATE (:N {id: 51})");
    });
    let report = session.checkpoint_online().unwrap();
    assert_eq!(report.lsn, 5, "the snapshot predates the in-window commits");
    let kept = recover(&wal_path(&path)).unwrap();
    assert_eq!(
        kept.iter().map(|f| f.lsn).collect::<Vec<_>>(),
        vec![6, 7],
        "frames after the stamped LSN are preserved, those before are gone"
    );
    commit(&session, "CREATE (:N {id: 52})");
    let live = ids(&session.snapshot());
    assert_eq!(live, vec![0, 1, 2, 3, 4, 50, 51, 52]);
    drop(session);
    assert_eq!(ids(&open(&path, DurabilityLevel::Full).snapshot()), live);
}

/// A trim that cannot publish its staged log (the rename is what Windows
/// refuses while another handle pins the target) is an error from the
/// checkpoint, never a success over a log that stayed long.
#[test]
fn a_failing_trim_surfaces_as_a_checkpoint_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("g.kgl");
    let session = Arc::new(open(&path, DurabilityLevel::Full));
    commit(&session, "CREATE (:N {id: 1})");
    // A directory squatting on the staging name fails the trim's temp create.
    let mut staged = wal_path(&path).into_os_string();
    staged.push(".trim");
    std::fs::create_dir(&staged).unwrap();
    let during = Arc::clone(&session);
    hooks::set(Stage::AfterSnapshot, move || {
        commit(&during, "CREATE (:N {id: 2})");
    });
    assert!(session.checkpoint_online().is_err());
    assert_eq!(frames(&path), 2, "a failed trim leaves the log whole");
    std::fs::remove_dir(&staged).unwrap();
    commit(&session, "CREATE (:N {id: 3})");
    drop(session);
    assert_eq!(
        ids(&open(&path, DurabilityLevel::Full).snapshot()),
        vec![1, 2, 3]
    );
}

/// A crash between the checkpoint's rename and the log trim: the new file is
/// stamped, the log still holds every frame, and replay must apply only the
/// frames past the stamp.
#[test]
fn a_crash_between_the_rename_and_the_trim_recovers_every_commit_once() {
    let dir = tempfile::tempdir().unwrap();
    let crash = tempfile::tempdir().unwrap();
    let path = dir.path().join("g.kgl");
    let session = Arc::new(open(&path, DurabilityLevel::Normal));
    for i in 0..5 {
        commit(&session, &format!("CREATE (:N {{id: {i}}})"));
    }
    let during = Arc::clone(&session);
    hooks::set(Stage::AfterSnapshot, move || {
        commit(&during, "CREATE (:N {id: 50})");
    });
    let image = crash.path().to_path_buf();
    let live_path = path.clone();
    let crashed = std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot = crashed.clone();
    hooks::set(Stage::BeforeTrim, move || {
        *slot.borrow_mut() = Some(crash_image(&live_path, &image));
    });
    session.checkpoint_online().unwrap();
    let crashed = crashed.borrow().clone().unwrap();
    assert_eq!(frames(&crashed), 6, "the untrimmed log holds every frame");
    let recovered = open(&crashed, DurabilityLevel::Normal);
    assert_eq!(ids(&recovered.snapshot()), vec![0, 1, 2, 3, 4, 50]);
    // And it keeps working: the next commit and a reopen lose nothing.
    commit(&recovered, "CREATE (:N {id: 60})");
    drop(recovered);
    assert_eq!(
        ids(&open(&crashed, DurabilityLevel::Normal).snapshot()),
        vec![0, 1, 2, 3, 4, 50, 60]
    );
}

/// A crash while the trimmed log is staged: the old log is whole and the stray
/// `.trim` file is ignored.
#[test]
fn a_stray_trim_temp_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("g.kgl");
    let session = open(&path, DurabilityLevel::Full);
    commit(&session, "CREATE (:N {id: 1})");
    session.checkpoint_online().unwrap();
    commit(&session, "CREATE (:N {id: 2})");
    drop(session);
    let mut stray = wal_path(&path).into_os_string();
    stray.push(".trim");
    std::fs::write(&stray, b"KWAL\x00half a log").unwrap();
    assert_eq!(
        ids(&open(&path, DurabilityLevel::Full).snapshot()),
        vec![1, 2]
    );
}

#[test]
fn the_file_is_durable_before_the_log_is_trimmed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("g.kgl");
    let session = open(&path, DurabilityLevel::Normal);
    commit(&session, "CREATE (:N {id: 1})");
    commit(&session, "CREATE (:N {id: 2})");
    let (done, events) = trace::capture(|| session.checkpoint_online());
    done.unwrap();
    let at = |needle: &str| {
        events
            .iter()
            .position(|e| e == needle)
            .unwrap_or_else(|| panic!("no `{needle}` in {events:?}"))
    };
    let order = [
        at("wal sync_data"),
        at("kgl temp sync_all"),
        at("kgl rename"),
        at("kgl sync_dir"),
    ];
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{events:?}");
    // A trim with nothing appended since is a reset; with a tail it is a rename.
    assert!(
        at("kgl sync_dir") < at("wal truncate+sync_all"),
        "{events:?}"
    );
}

#[test]
fn a_trim_with_a_tail_renames_after_the_file_is_durable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("g.kgl");
    let session = Arc::new(open(&path, DurabilityLevel::Full));
    commit(&session, "CREATE (:N {id: 1})");
    let during = Arc::clone(&session);
    hooks::set(Stage::AfterSnapshot, move || {
        commit(&during, "CREATE (:N {id: 2})")
    });
    let (done, events) = trace::capture(|| session.checkpoint_online());
    done.unwrap();
    let at = |needle: &str| events.iter().position(|e| e == needle).unwrap();
    assert!(
        at("kgl sync_dir") < at("wal trim temp sync_all"),
        "{events:?}"
    );
    assert!(
        at("wal trim temp sync_all") < at("wal trim rename"),
        "{events:?}"
    );
}

#[test]
fn the_policy_fires_at_the_threshold_and_not_before() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("g.kgl");
    let session = open(&path, DurabilityLevel::Full);
    assert!(!session.needs_checkpoint(), "an empty log needs nothing");
    commit(&session, "CREATE (:N {id: 1})");
    let one_frame = std::fs::metadata(wal_path(&path)).unwrap().len() - 5;
    session.set_auto_checkpoint_wal_bytes(Some(one_frame * 3));
    commit(&session, "CREATE (:N {id: 2})");
    assert!(
        !session.needs_checkpoint(),
        "two frames are under the bound"
    );
    assert!(session.maybe_checkpoint_online().unwrap().is_none());
    commit(&session, "CREATE (:N {id: 3})");
    assert!(session.needs_checkpoint());
    let report = session.maybe_checkpoint_online().unwrap().expect("ran");
    assert_eq!(report.lsn, 3);
    assert!(!session.needs_checkpoint(), "the trim reset the count");
}

#[test]
fn a_disabled_policy_never_asks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("g.kgl");
    let session = open(&path, DurabilityLevel::Full);
    session.set_auto_checkpoint_wal_bytes(None);
    for i in 0..10 {
        commit(&session, &format!("CREATE (:N {{id: {i}}})"));
    }
    assert!(!session.needs_checkpoint());
    assert!(session.maybe_checkpoint_online().unwrap().is_none());
    assert_eq!(frames(&path), 10);
    // An explicit checkpoint still works.
    session.checkpoint_online().unwrap();
    assert_eq!(frames(&path), 0);
}

#[test]
fn a_log_smaller_than_its_checkpoint_does_not_trigger_a_rewrite() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("g.kgl");
    let session = open(&path, DurabilityLevel::Full);
    for i in 0..200 {
        commit(
            &session,
            &format!("CREATE (:N {{id: {i}, pad: 'xxxxxxxxxxxxxxxx'}})"),
        );
    }
    session.checkpoint_online().unwrap();
    let size = std::fs::metadata(&path).unwrap().len();
    session.set_auto_checkpoint_wal_bytes(Some(1));
    commit(&session, "CREATE (:N {id: 1000})");
    assert!(size > 100);
    assert!(
        !session.needs_checkpoint(),
        "one frame is far below the {size}-byte file"
    );
}

#[test]
fn a_failed_checkpoint_backs_the_policy_off() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("g.kgl");
    let session = open(&path, DurabilityLevel::Full);
    session.set_auto_checkpoint_wal_bytes(Some(1));
    commit(&session, "CREATE (:N {id: 1})");
    assert!(session.needs_checkpoint());
    // A directory squatting on the checkpoint path makes the rename fail.
    std::fs::create_dir(&path).unwrap();
    assert!(session.maybe_checkpoint_online().is_err());
    assert!(!session.needs_checkpoint(), "no retry on every commit");
    assert_eq!(frames(&path), 1, "a failed checkpoint leaves the log whole");
    std::fs::remove_dir(&path).unwrap();
    for i in 2..8 {
        commit(&session, &format!("CREATE (:N {{id: {i}}})"));
    }
    assert!(session.needs_checkpoint());
    session.maybe_checkpoint_online().unwrap().unwrap();
    assert_eq!(frames(&path), 0);
}

#[test]
fn a_session_without_a_log_refuses() {
    let session = Session::from_arc(Arc::new(DirGraph::new()));
    assert!(session.checkpoint_online().is_err());
    assert!(!session.needs_checkpoint());
}

/// Writers commit flat out on several threads while checkpoints run; nothing
/// acknowledged is lost, the log stays bounded, and a reopen equals the live
/// state.
#[test]
fn concurrent_writers_lose_nothing_across_repeated_checkpoints() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("g.kgl");
    let session = Arc::new(open(&path, DurabilityLevel::Normal));
    session.set_auto_checkpoint_wal_bytes(Some(4096));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writers: Vec<_> = (0..3)
        .map(|w| {
            let session = Arc::clone(&session);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut acked = Vec::new();
                let mut i = 0;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) && i < 400 {
                    let id = w * 10_000 + i;
                    let params = HashMap::new();
                    let opts = ExecuteOptions::eager(&params);
                    let mut tx = session.begin();
                    execute_mut(
                        tx.working_mut().unwrap(),
                        &format!("CREATE (:N {{id: {id}, pad: '{}'}})", "x".repeat(200)),
                        &opts,
                    )
                    .unwrap();
                    if matches!(session.commit(tx, true), CommitOutcome::Committed { .. }) {
                        acked.push(id as i64);
                    }
                    i += 1;
                }
                acked
            })
        })
        .collect();
    let mut runs = 0;
    while writers.iter().any(|w| !w.is_finished()) {
        if session.maybe_checkpoint_online().unwrap().is_some() {
            runs += 1;
        }
        std::thread::yield_now();
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let mut acked: Vec<i64> = writers
        .into_iter()
        .flat_map(|w| w.join().unwrap())
        .collect();
    acked.sort_unstable();
    assert!(
        runs >= 2,
        "the policy should have fired repeatedly, ran {runs}"
    );
    let live = ids(&session.snapshot());
    assert_eq!(
        live, acked,
        "every acknowledged commit is in the live graph"
    );
    // The policy fires only when the log reaches max(threshold, the checkpoint
    // it extends) and only when this thread gets scheduled, so a slow runner
    // can leave the writers' final commits unswept. One drain pass makes the
    // bound deterministic: afterwards the frames are below that trigger.
    let _ = session.maybe_checkpoint_online().unwrap();
    let log = std::fs::metadata(wal_path(&path)).unwrap().len();
    let checkpoint = std::fs::metadata(&path).unwrap().len();
    let bound = 4096.max(checkpoint) + 64;
    assert!(
        log < bound,
        "log stayed bounded, is {log} bytes (checkpoint {checkpoint})"
    );
    drop(session);
    assert_eq!(ids(&open(&path, DurabilityLevel::Normal).snapshot()), acked);
}

#[test]
fn save_and_online_checkpoint_interleave_safely() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("g.kgl");
    let session = Arc::new(open(&path, DurabilityLevel::Full));
    commit(&session, "CREATE (:N {id: 1})");
    session.checkpoint_online().unwrap();
    commit(&session, "CREATE (:N {id: 2})");
    session.save(&path.to_string_lossy(), true).unwrap();
    commit(&session, "CREATE (:N {id: 3})");
    session.checkpoint_online().unwrap();
    drop(session);
    assert_eq!(
        ids(&open(&path, DurabilityLevel::Full).snapshot()),
        vec![1, 2, 3]
    );
}
