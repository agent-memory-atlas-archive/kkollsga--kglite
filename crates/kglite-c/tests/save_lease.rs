//! The C save entries refuse a path another process holds open for writing,
//! with the `WriterLeaseHeld` status and the holder in the structured detail;
//! a lease this process holds, or none, is not foreign.

use kglite::api::io::{save_graph, GraphWriterLease};
use kglite::api::DirGraph;
use kglite_c::*;
use std::ffi::{CStr, CString};
use std::sync::Arc;

struct TestDirectory(std::path::PathBuf);

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch(name: &str) -> (TestDirectory, std::path::PathBuf) {
    let dir =
        std::env::temp_dir().join(format!("kglite-c-save-lease-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let target = dir.join("target.kgl");
    (TestDirectory(dir), target)
}

/// Hold `target`'s lease and record another process as its owner.
fn foreign_holder(target: &std::path::Path) -> GraphWriterLease {
    let lease = GraphWriterLease::acquire(target, std::time::Duration::ZERO).unwrap();
    let mut owner = target.as_os_str().to_owned();
    owner.push(".lock-owner");
    std::fs::write(owner, "pid=1\nsince=2026-01-01T00:00:00Z\n").unwrap();
    lease
}

/// Run one save route against `target`; returns the status and error text.
fn save_through(route: &str, target: &std::path::Path) -> (KgliteStatusCode, String) {
    let target_c = CString::new(target.to_str().unwrap()).unwrap();
    let mut graph = Arc::new(DirGraph::new());
    let source = target.with_file_name("source.kgl");
    save_graph(&mut graph, source.to_str().unwrap()).unwrap();
    let source_c = CString::new(source.to_str().unwrap()).unwrap();
    let mut handle = std::ptr::null_mut();
    let mut error = std::ptr::null();
    // All handles and strings remain live across these synchronous C calls.
    unsafe {
        assert_eq!(
            kglite_load_file(source_c.as_ptr(), &mut handle, &mut error),
            KgliteStatusCode::Ok
        );
        let status = match route {
            "graph" => kglite_save_graph(handle, target_c.as_ptr(), &mut error),
            "durable_graph" => kglite_save_graph_durable(handle, target_c.as_ptr(), 1, &mut error),
            _ => {
                let mut session = std::ptr::null_mut();
                assert_eq!(
                    kglite_session_new(handle, &mut session),
                    KgliteStatusCode::Ok
                );
                let status = kglite_session_save(session, target_c.as_ptr(), 1, &mut error);
                kglite_session_free(session);
                status
            }
        };
        let message = if error.is_null() {
            String::new()
        } else {
            let text = CStr::from_ptr(error).to_string_lossy().into_owned();
            kglite_free_string(error);
            text
        };
        if route != "session" {
            kglite_graph_free(handle);
        }
        (status, message)
    }
}

#[test]
fn a_foreign_writer_refuses_every_save_route() {
    for route in ["graph", "durable_graph", "session"] {
        let (_cleanup, target) = scratch(route);
        let lease = foreign_holder(&target);
        let (status, message) = save_through(route, &target);
        assert_eq!(
            status,
            KgliteStatusCode::WriterLeaseHeld,
            "{route}: {message}"
        );
        assert!(message.contains("pid 1"), "{route}: {message}");
        assert!(!target.exists(), "{route}: a refused save writes nothing");
        let detail = unsafe {
            let raw = kglite_last_error_details_json();
            assert!(!raw.is_null(), "{route}: the holder rides the detail");
            let text = CStr::from_ptr(raw).to_string_lossy().into_owned();
            kglite_free_string(raw);
            text
        };
        assert!(detail.contains("\"WriterLeaseHeld\""), "{route}: {detail}");
        drop(lease);
    }
}

#[test]
fn this_process_lease_and_no_lease_do_not_refuse() {
    for route in ["graph", "durable_graph", "session"] {
        let (_cleanup, target) = scratch(&format!("own-{route}"));
        let (status, message) = save_through(route, &target);
        assert_eq!(
            status,
            KgliteStatusCode::Ok,
            "{route} without a lease: {message}"
        );
        std::fs::remove_file(&target).unwrap();
        let _lease = GraphWriterLease::acquire(&target, std::time::Duration::ZERO).unwrap();
        let (status, message) = save_through(route, &target);
        assert_eq!(
            status,
            KgliteStatusCode::Ok,
            "{route} with its own lease: {message}"
        );
    }
}
