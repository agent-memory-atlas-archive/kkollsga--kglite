//! Helpers the disk-graph test modules share: run a statement, reopen a saved
//! graph, find the published generation, read its column metadata and copy a
//! directory tree.

use super::DirGraph;
use crate::datatypes::Value;
use crate::graph::io::columns_meta::{self, ColumnsMeta};
use crate::graph::io::file::load_file;
use crate::graph::session::execute::{execute_mut, ExecuteOptions};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Run `query` and return its rows; a failing statement panics with its text.
pub(super) fn run(graph: &mut DirGraph, query: &str) -> Vec<Vec<Value>> {
    let params = HashMap::new();
    execute_mut(graph, query, &ExecuteOptions::eager(&params))
        .unwrap_or_else(|e| panic!("{query}: {e}"))
        .result
        .rows
}

/// Load the saved graph at `path` as an owned, uniquely held graph.
pub(super) fn load_owned(path: &str) -> DirGraph {
    match Arc::try_unwrap(load_file(path).unwrap()) {
        Ok(graph) => graph,
        Err(_) => panic!("fresh load unexpectedly shared"),
    }
}

/// The generation directory the graph at `root` currently publishes.
pub(super) fn current_generation(root: impl AsRef<Path>) -> PathBuf {
    let root = root.as_ref();
    let current = std::fs::read_to_string(root.join("CURRENT")).unwrap();
    root.join("generations").join(current.trim())
}

/// The column metadata `generation` publishes, read the way a load reads it.
pub(super) fn column_meta(generation: &Path) -> ColumnsMeta {
    columns_meta::read(&columns_meta::locate(generation).expect("column metadata")).unwrap()
}

/// Copy the directory tree at `from` to `to`.
pub(super) fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}
