use kglite::api::io::save_graph;
use kglite::api::DirGraph;
use kglite_c::*;
use std::ffi::{c_char, CStr, CString};
use std::sync::Arc;

struct TestDirectory(std::path::PathBuf);

impl TestDirectory {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "kglite-c-session-backup-{}-{tag}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn c(path: &std::path::Path) -> CString {
    CString::new(path.to_str().unwrap()).unwrap()
}

fn session_from_file(path: &std::path::Path) -> *mut KgliteSession {
    let path_c = c(path);
    let mut graph = std::ptr::null_mut();
    let mut error: *const c_char = std::ptr::null();
    let mut session = std::ptr::null_mut();
    // Pointers are live locals for these synchronous calls.
    unsafe {
        assert_eq!(
            kglite_load_file(path_c.as_ptr(), &mut graph, &mut error),
            KgliteStatusCode::Ok
        );
        assert_eq!(
            kglite_session_new(graph, &mut session),
            KgliteStatusCode::Ok
        );
    }
    session
}

fn mutate(session: *mut KgliteSession, query: &str) {
    let q = CString::new(query).unwrap();
    let mut result = std::ptr::null_mut();
    let mut error: *const c_char = std::ptr::null();
    unsafe {
        assert_eq!(
            kglite_session_execute_mut(
                session,
                q.as_ptr(),
                std::ptr::null(),
                &mut result,
                &mut error
            ),
            KgliteStatusCode::Ok
        );
        kglite_cypher_result_free(result);
    }
}

fn backup(
    session: *mut KgliteSession,
    dest: &std::path::Path,
    live: Option<&std::path::Path>,
) -> (KgliteStatusCode, Option<String>, Option<String>) {
    let dest_c = c(dest);
    let live_c = live.map(c);
    let mut report: *const c_char = std::ptr::NonNull::<c_char>::dangling().as_ptr();
    let mut error: *const c_char = std::ptr::null();
    let status = unsafe {
        kglite_session_backup(
            session,
            dest_c.as_ptr(),
            live_c.as_ref().map_or(std::ptr::null(), |l| l.as_ptr()),
            &mut report,
            &mut error,
        )
    };
    let take = |p: *const c_char| {
        (!p.is_null()).then(|| {
            let s = unsafe { CStr::from_ptr(p) }.to_str().unwrap().to_owned();
            unsafe { kglite_free_string(p) };
            s
        })
    };
    (status, take(report), take(error))
}

fn seed_file(path: &std::path::Path) {
    let mut graph = Arc::new(DirGraph::new());
    save_graph(&mut graph, path.to_str().unwrap()).unwrap();
}

#[test]
fn backup_round_trips_with_report_shape() {
    let dir = TestDirectory::new("roundtrip");
    let live = dir.0.join("live.kgl");
    seed_file(&live);
    let session = session_from_file(&live);
    mutate(session, "CREATE (:T {id: 1})-[:R]->(:T {id: 2})");
    let dest = dir.0.join("copy.kgl");

    let (status, report, error) = backup(session, &dest, Some(&live));
    assert_eq!(status, KgliteStatusCode::Ok, "{error:?}");
    let json: serde_json::Value = serde_json::from_str(&report.unwrap()).unwrap();
    for key in [
        "path",
        "bytes",
        "nodes",
        "relationships",
        "graph_version",
        "lsn",
        "lock_hold_ms",
        "elapsed_ms",
        "prepared_copy",
    ] {
        assert!(json.get(key).is_some(), "missing key {key}: {json}");
    }
    assert!(json["prepared_copy"].is_boolean(), "{json}");
    assert_eq!(json["nodes"], 2);
    assert_eq!(json["relationships"], 1);
    assert!(json["lsn"].is_null(), "no write-ahead log, so no lsn");
    assert_eq!(json["bytes"], std::fs::metadata(&dest).unwrap().len());

    let reopened = session_from_file(&dest);
    let q = CString::new("MATCH (n:T) RETURN count(n) AS c").unwrap();
    let mut result = std::ptr::null_mut();
    let mut err: *const c_char = std::ptr::null();
    unsafe {
        assert_eq!(
            kglite_session_execute_read(
                reopened,
                q.as_ptr(),
                std::ptr::null(),
                &mut result,
                &mut err
            ),
            KgliteStatusCode::Ok
        );
        let rows = kglite_cypher_result_rows_json(result);
        let text = CStr::from_ptr(rows).to_str().unwrap().to_owned();
        assert!(text.contains('2'), "reopened backup has both nodes: {text}");
        kglite_free_string(rows);
        kglite_cypher_result_free(result);
        kglite_session_free(reopened);
        kglite_session_free(session);
    }
}

#[test]
fn backup_over_the_live_file_is_refused_and_leaves_it_intact() {
    let dir = TestDirectory::new("alias");
    let live = dir.0.join("live.kgl");
    seed_file(&live);
    let before = std::fs::read(&live).unwrap();
    let session = session_from_file(&live);
    mutate(session, "CREATE (:T {id: 1})");

    let (status, report, error) = backup(session, &live, Some(&live));
    assert_eq!(status, KgliteStatusCode::FileIo);
    assert!(report.is_none());
    assert!(error.unwrap().contains("live graph's checkpoint"));
    assert_eq!(std::fs::read(&live).unwrap(), before);
    unsafe { kglite_session_free(session) };
}

#[test]
fn disk_graph_backup_is_refused() {
    let dir = TestDirectory::new("disk");
    let mut handle = std::ptr::null_mut();
    let mut err: *const c_char = std::ptr::null();
    let mut converted: *const c_char = std::ptr::null();
    let mode = CString::new("disk").unwrap();
    let path = c(&dir.0.join("disk2"));
    let mut session = std::ptr::null_mut();
    unsafe {
        assert_eq!(
            kglite_open_or_create_graph_in_mode(
                path.as_ptr(),
                mode.as_ptr(),
                &mut handle,
                &mut converted,
                &mut err
            ),
            KgliteStatusCode::Ok
        );
        assert_eq!(
            kglite_session_new(handle, &mut session),
            KgliteStatusCode::Ok
        );
    }
    let (status, report, error) = backup(session, &dir.0.join("copy.kgl"), None);
    assert_eq!(status, KgliteStatusCode::FileIo);
    assert!(report.is_none());
    assert!(error.unwrap().contains("disk-mode"));
    unsafe { kglite_session_free(session) };
}

#[test]
fn backup_rejects_null_arguments() {
    let dir = TestDirectory::new("null");
    let dest = c(&dir.0.join("x.kgl"));
    let mut report: *const c_char = std::ptr::NonNull::<c_char>::dangling().as_ptr();
    let mut error: *const c_char = std::ptr::NonNull::<c_char>::dangling().as_ptr();
    unsafe {
        let status = kglite_session_backup(
            std::ptr::null(),
            dest.as_ptr(),
            std::ptr::null(),
            &mut report,
            &mut error,
        );
        assert_eq!(status, KgliteStatusCode::NullPointer);
        assert!(report.is_null() && error.is_null(), "out slots reset first");
        let status = kglite_session_backup(
            std::ptr::NonNull::dangling().as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            &mut report,
            &mut error,
        );
        assert_eq!(status, KgliteStatusCode::NullPointer);
    }
}
