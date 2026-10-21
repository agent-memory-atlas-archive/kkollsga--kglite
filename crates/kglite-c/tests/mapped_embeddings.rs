//! Mapped embeddings through the C ABI: `kglite_load_file_with_options`,
//! `kglite_save_graph_with_layout` and `kglite_graph_verify_integrity`.

use std::ffi::{c_char, CStr, CString};
use std::path::PathBuf;

use kglite_c::{
    kglite_cypher_result_free, kglite_free_string, kglite_graph_free, kglite_graph_new,
    kglite_graph_verify_integrity, kglite_load_file_with_options, kglite_save_graph_with_layout,
    kglite_session_execute_mut, kglite_session_free, kglite_session_new, kglite_session_save,
    kglite_session_set_embeddings, KgliteCypherResult, KgliteGraph, KgliteLoadOptions,
    KgliteSession, KgliteStatusCode,
};

const DIM: usize = 1024;
const DOCS: usize = 1100; // 4.4 MiB of f32, over the writer floor

fn c(text: &str) -> CString {
    CString::new(text).unwrap()
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kglite_c_mapped_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn take(json: *const c_char) -> serde_json::Value {
    assert!(!json.is_null());
    let value = serde_json::from_str(unsafe { CStr::from_ptr(json) }.to_str().unwrap());
    unsafe { kglite_free_string(json) };
    value.unwrap()
}

/// A `.kgl` holding one node store of `DOCS` vectors, saved through a session.
fn big_file(dir: &std::path::Path) -> PathBuf {
    let graph = kglite_graph_new();
    let mut session: *mut KgliteSession = std::ptr::null_mut();
    assert_eq!(
        unsafe { kglite_session_new(graph, &mut session) },
        KgliteStatusCode::Ok
    );
    let query = c(&format!(
        "UNWIND range(1, {DOCS}) AS i CREATE (:Doc {{id: i, title: 'd', text: 't'}})"
    ));
    let mut result: *mut KgliteCypherResult = std::ptr::null_mut();
    assert_eq!(
        unsafe {
            kglite_session_execute_mut(
                session,
                query.as_ptr(),
                std::ptr::null(),
                &mut result,
                std::ptr::null_mut(),
            )
        },
        KgliteStatusCode::Ok
    );
    unsafe { kglite_cypher_result_free(result) };
    let ids = c(&serde_json::to_string(&(1..=DOCS).collect::<Vec<_>>()).unwrap());
    let vectors: Vec<f32> = (0..DOCS * DIM)
        .map(|n| ((n * 31 % 101) as f32) / 50.0 - 1.0)
        .collect();
    let mut report: *const c_char = std::ptr::null();
    assert_eq!(
        unsafe {
            kglite_session_set_embeddings(
                session,
                c("Doc").as_ptr(),
                c("text").as_ptr(),
                ids.as_ptr(),
                vectors.as_ptr(),
                DIM,
                DOCS,
                std::ptr::null(),
                &mut report,
                std::ptr::null_mut(),
            )
        },
        KgliteStatusCode::Ok
    );
    unsafe { kglite_free_string(report) };
    let path = dir.join("docs.kgl");
    let path_c = c(path.to_str().unwrap());
    assert_eq!(
        unsafe { kglite_session_save(session, path_c.as_ptr(), 0, std::ptr::null_mut()) },
        KgliteStatusCode::Ok
    );
    unsafe { kglite_session_free(session) };
    path
}

fn load(
    path: &std::path::Path,
    options: Option<&KgliteLoadOptions>,
) -> (KgliteStatusCode, *mut KgliteGraph, String) {
    let path_c = c(path.to_str().unwrap());
    let mut graph: *mut KgliteGraph = std::ptr::null_mut();
    let mut error: *const c_char = std::ptr::null();
    let rc = unsafe {
        kglite_load_file_with_options(
            path_c.as_ptr(),
            options.map_or(std::ptr::null(), |o| o as *const _),
            &mut graph,
            &mut error,
        )
    };
    let message = if error.is_null() {
        String::new()
    } else {
        let text = unsafe { CStr::from_ptr(error) }
            .to_str()
            .unwrap()
            .to_string();
        unsafe { kglite_free_string(error) };
        text
    };
    (rc, graph, message)
}

fn options(embeddings: u32, verify: u32) -> KgliteLoadOptions {
    KgliteLoadOptions {
        struct_size: std::mem::size_of::<KgliteLoadOptions>(),
        embeddings,
        verify,
        columns: 0,
    }
}

fn report(graph: *mut KgliteGraph) -> serde_json::Value {
    let mut json: *const c_char = std::ptr::null();
    assert_eq!(
        unsafe { kglite_graph_verify_integrity(graph, &mut json, std::ptr::null_mut()) },
        KgliteStatusCode::Ok
    );
    take(json)
}

fn magic(path: &std::path::Path) -> Vec<u8> {
    std::fs::read(path).unwrap()[..4].to_vec()
}

#[test]
fn load_options_choose_where_the_store_lives() {
    let dir = scratch("load");
    let file = big_file(&dir);
    assert_eq!(magic(&file), b"RGF\x09");

    // 2 = mapped: the store is served from the file and verifies clean.
    let (rc, mapped, _) = load(&file, Some(&options(2, 0)));
    assert_eq!(rc, KgliteStatusCode::Ok);
    let mapped_report = report(mapped);
    assert_eq!(mapped_report["ok"], true);
    assert_eq!(
        mapped_report["sections"][0]["name"],
        "embeddings:Doc.text_emb"
    );
    assert_eq!(mapped_report["sections"][0]["mapped"], true);
    unsafe { kglite_graph_free(mapped) };

    // 3 = memory, null options and 0 (default Auto, 4.4 MB < 64 MB): heap.
    for opts in [Some(options(3, 0)), None, Some(options(0, 0))] {
        let (rc, graph, _) = load(&file, opts.as_ref());
        assert_eq!(rc, KgliteStatusCode::Ok);
        assert_eq!(report(graph)["sections"].as_array().unwrap().len(), 0);
        unsafe { kglite_graph_free(graph) };
    }

    // 2 = eager verify loads an intact file.
    let (rc, graph, _) = load(&file, Some(&options(2, 2)));
    assert_eq!(rc, KgliteStatusCode::Ok);
    unsafe { kglite_graph_free(graph) };
}

#[test]
fn bad_option_values_are_invalid_arguments_and_a_short_struct_means_defaults() {
    let dir = scratch("bad");
    let file = big_file(&dir);
    for bad in [options(9, 0), options(0, 9)] {
        let (rc, graph, message) = load(&file, Some(&bad));
        assert_eq!(rc, KgliteStatusCode::InvalidArgument, "{message}");
        assert!(graph.is_null());
    }
    // struct_size covers only the first field: the rest read as zero.
    let mut short = options(2, 2);
    short.struct_size = std::mem::size_of::<usize>();
    let (rc, graph, _) = load(&file, Some(&short));
    assert_eq!(rc, KgliteStatusCode::Ok);
    assert_eq!(report(graph)["sections"].as_array().unwrap().len(), 0);
    unsafe { kglite_graph_free(graph) };
}

#[test]
fn save_layout_compact_writes_v8_and_other_values_are_refused() {
    let dir = scratch("layout");
    let file = big_file(&dir);
    let (rc, graph, _) = load(&file, Some(&options(2, 0)));
    assert_eq!(rc, KgliteStatusCode::Ok);

    let compact = dir.join("compact.kgl");
    let auto = dir.join("auto.kgl");
    for (path, layout, expected) in [(&compact, 1u32, b"RGF\x08"), (&auto, 0u32, b"RGF\x09")] {
        let path_c = c(path.to_str().unwrap());
        assert_eq!(
            unsafe {
                kglite_save_graph_with_layout(
                    graph,
                    path_c.as_ptr(),
                    0,
                    layout,
                    std::ptr::null_mut(),
                )
            },
            KgliteStatusCode::Ok
        );
        assert_eq!(magic(path), expected);
    }
    let mapped = dir.join("mapped.kgl");
    let mapped_c = c(mapped.to_str().unwrap());
    assert_eq!(
        unsafe {
            kglite_save_graph_with_layout(graph, mapped_c.as_ptr(), 0, 2, std::ptr::null_mut())
        },
        KgliteStatusCode::Ok
    );
    assert_eq!(magic(&mapped), b"RGF\x09");
    let refused = c(dir.join("x.kgl").to_str().unwrap());
    assert_eq!(
        unsafe {
            kglite_save_graph_with_layout(graph, refused.as_ptr(), 0, 7, std::ptr::null_mut())
        },
        KgliteStatusCode::InvalidArgument
    );
    unsafe { kglite_graph_free(graph) };
}

/// A `.kgl` whose `Doc.score` / `Doc.ratio` columns are over the 256 KB floor
/// and do not delta-compress, so a mapped-layout save writes them to the data region.
fn column_file(dir: &std::path::Path) -> PathBuf {
    let graph = kglite_graph_new();
    let mut session: *mut KgliteSession = std::ptr::null_mut();
    assert_eq!(
        unsafe { kglite_session_new(graph, &mut session) },
        KgliteStatusCode::Ok
    );
    let query = c(
        "UNWIND range(1, 40000) AS i CREATE (:Doc {id: i, score: (i * 7919) % 40009 * 104729, \
         ratio: i / 7.0})",
    );
    let mut result: *mut KgliteCypherResult = std::ptr::null_mut();
    assert_eq!(
        unsafe {
            kglite_session_execute_mut(
                session,
                query.as_ptr(),
                std::ptr::null(),
                &mut result,
                std::ptr::null_mut(),
            )
        },
        KgliteStatusCode::Ok
    );
    unsafe { kglite_cypher_result_free(result) };
    // A session save writes the default layout, which keeps columns
    // compressed; layout 2 (mapped) re-saves them raw.
    let base = dir.join("columns-base.kgl");
    let base_c = c(base.to_str().unwrap());
    assert_eq!(
        unsafe { kglite_session_save(session, base_c.as_ptr(), 0, std::ptr::null_mut()) },
        KgliteStatusCode::Ok
    );
    unsafe { kglite_session_free(session) };
    let (rc, loaded, _) = load(&base, None);
    assert_eq!(rc, KgliteStatusCode::Ok);
    let path = dir.join("columns.kgl");
    let path_c = c(path.to_str().unwrap());
    assert_eq!(
        unsafe {
            kglite_save_graph_with_layout(loaded, path_c.as_ptr(), 0, 2, std::ptr::null_mut())
        },
        KgliteStatusCode::Ok
    );
    unsafe { kglite_graph_free(loaded) };
    path
}

fn column_sections(graph: *mut KgliteGraph) -> usize {
    report(graph)["sections"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["name"].as_str().unwrap().starts_with("col:"))
        .count()
}

#[test]
fn the_columns_option_chooses_where_property_columns_live() {
    let dir = scratch("columns");
    let path = column_file(&dir);
    assert_eq!(magic(&path), b"RGF\x09");

    // Unrequested, an in-memory graph decodes its columns.
    let (rc, graph, _) = load(&path, None);
    assert_eq!(rc, KgliteStatusCode::Ok);
    assert_eq!(column_sections(graph), 0);
    unsafe { kglite_graph_free(graph) };

    for (columns, mapped) in [(1u32, cfg!(unix)), (2, true), (3, false)] {
        let mut opts = options(0, 0);
        opts.columns = columns;
        let (rc, graph, _) = load(&path, Some(&opts));
        assert_eq!(rc, KgliteStatusCode::Ok);
        assert_eq!(column_sections(graph) > 0, mapped, "columns={columns}");
        unsafe { kglite_graph_free(graph) };
    }

    let mut bad = options(0, 0);
    bad.columns = 9;
    let (rc, graph, message) = load(&path, Some(&bad));
    assert_eq!(rc, KgliteStatusCode::InvalidArgument);
    assert!(graph.is_null());
    assert!(message.contains("columns"), "{message}");

    // A caller built before `columns` existed declares the shorter struct.
    let mut old = options(3, 0);
    old.columns = 2;
    old.struct_size = 8 + 2 * std::mem::size_of::<u32>();
    let (rc, graph, _) = load(&path, Some(&old));
    assert_eq!(rc, KgliteStatusCode::Ok);
    assert_eq!(column_sections(graph), 0, "columns reads as default");
    unsafe { kglite_graph_free(graph) };
}
