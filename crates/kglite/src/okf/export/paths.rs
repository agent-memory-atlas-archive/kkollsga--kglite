//! Where each note's file goes (VAULT.md §10.2).
//!
//! Two rules pull against each other. A note the vault already placed must not
//! move, or a round trip would shuffle a human's directories; and a note's
//! label must be recoverable from the layout, because §10.3 never writes
//! `type:`. Preserving a path whose top-level folder already *is* the label
//! satisfies both; anything else is filed under `<Label>/` so the next
//! import's folder rung answers with the label it had.

use super::Note;
use std::collections::{HashMap, HashSet};

/// Characters a vault path may not carry (VAULT.md §10.2) — the union of what
/// Windows, macOS and Linux refuse or reinterpret.
const FORBIDDEN: [char; 9] = ['/', '\\', ':', '*', '?', '"', '<', '>', '|'];

/// Give every note its vault-relative output path, in place.
///
/// Notes arrive sorted by `(label, id)` and are processed in that order, so
/// which of two colliding notes keeps the plain spelling does not depend on
/// the graph's node order.
pub(super) fn assign_paths(notes: &mut [Note]) {
    let preserved = preserved_paths(notes);
    // Lowercased path → the note that took it. Case-insensitive on **every**
    // host: a vault written on Linux must still open on macOS and Windows,
    // where two files differing only in case are one file.
    let mut taken: HashSet<String> = HashSet::with_capacity(notes.len());
    for (at, note) in notes.iter_mut().enumerate() {
        let mut path = match preserved.get(&at) {
            Some(path) => path.clone(),
            None => format!(
                "{}/{}.md",
                sanitize_segment(&note.label),
                sanitize_segment(note.display_name())
            ),
        };
        if taken.contains(&path.to_ascii_lowercase()) {
            let stem = path.strip_suffix(".md").unwrap_or(&path).to_string();
            path = format!("{stem}-{}.md", sanitize_segment(note.id()));
            // The id is unique, so this settles it for every vault-built
            // graph; the counter is for a graph whose ids are not (an
            // arbitrary graph can hold two nodes of one label and one id).
            let mut nth = 2;
            while taken.contains(&path.to_ascii_lowercase()) {
                path = format!("{stem}-{}-{nth}.md", sanitize_segment(note.id()));
                nth += 1;
            }
        }
        taken.insert(path.to_ascii_lowercase());
        note.out = path;
    }
}

/// The notes whose stored `file_path` survives: the ones whose top-level folder
/// is already their label.
///
/// A folder note (`X.md` beside `X/`) is one of these whenever its own top
/// folder matches, and then keeps that exact spelling — the file is not moved
/// into `X/`, which would dissolve the layout the folder note expresses.
fn preserved_paths(notes: &[Note]) -> HashMap<usize, String> {
    let mut out = HashMap::new();
    for (at, note) in notes.iter().enumerate() {
        let Some(path) = note.file_path.as_deref() else {
            continue;
        };
        if !path.ends_with(".md") {
            continue;
        }
        if top_folder(path) == Some(note.label.as_str()) {
            out.insert(at, path.to_string());
        }
    }
    out
}

/// The first path segment of a vault-relative path, or `None` for a file at
/// the root (which no label can match, so a root note is always re-filed).
fn top_folder(path: &str) -> Option<&str> {
    path.split_once('/').map(|(head, _)| head)
}

/// One path segment with everything a filesystem refuses replaced by `-`
/// (VAULT.md §10.2).
///
/// Also collapses the two names a directory entry can never be — `.` and `..` —
/// and trims the trailing dots and spaces Windows silently strips, which would
/// otherwise make the written filename differ from the one the manifest
/// recorded and the next export refuse its own file.
pub(super) fn sanitize_segment(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| {
            if FORBIDDEN.contains(&c) || c.is_control() {
                '-'
            } else {
                c
            }
        })
        .collect();
    while out.ends_with('.') || out.ends_with(' ') {
        out.pop();
    }
    let trimmed = out.trim_start();
    if trimmed.len() != out.len() {
        out = trimmed.to_string();
    }
    if out.is_empty() {
        out.push_str("untitled");
    }
    out
}

#[cfg(test)]
#[path = "paths_tests.rs"]
mod paths_tests;
