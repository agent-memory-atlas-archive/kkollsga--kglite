//! The directory hierarchy: `Folder` nodes, the folder notes that replace
//! them, and the containment edges either one carries (VAULT.md §2.2, §2.3).

use super::{count_nodes, doc_path, EdgeGroups};
use crate::datatypes::values::{DataFrame, Value};
use crate::graph::mutation::maintain;
use crate::graph::DirGraph;
use crate::okf::model::{
    BuildOptions, BuildReport, ConceptDoc, FolderNoteDirection, Profile, CONTAINS_CONN_TYPE,
    FOLDER_LABEL,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// What holds a note or a subfolder: the `Folder` node standing for its
/// directory, or the folder note that replaced it (VAULT.md §2.3).
enum Container<'a> {
    Folder(String),
    Note(&'a ConceptDoc),
}

/// Which directories a folder note replaced. Empty unless
/// [`Profile::folder_notes`] is set, which is what keeps `okf` and `loose`
/// on the pure-`Folder` hierarchy they have always had.
#[derive(Default)]
struct FolderLayout<'a> {
    /// Directory path → the note standing in for its `Folder` node.
    note_of_dir: BTreeMap<String, &'a ConceptDoc>,
    /// A folder note's `doc_path` → the directory it owns. The note is not
    /// held by that directory but by the directory's *parent*: it took the
    /// folder's place, so it hangs where the folder hung.
    dir_of_note: BTreeMap<&'a str, String>,
}

impl<'a> FolderLayout<'a> {
    /// What holds the children of `dir` — `None` at the vault root, which has
    /// neither a `Folder` node nor a folder note.
    fn owner(&self, dir: &str) -> Option<Container<'a>> {
        if dir.is_empty() {
            None
        } else if let Some(note) = self.note_of_dir.get(dir) {
            Some(Container::Note(note))
        } else {
            Some(Container::Folder(dir.to_string()))
        }
    }
}

/// Match each directory with its folder note: `X.md` beside `X/`, or `X/X.md`
/// (VAULT.md §2.3). Declaring both is an error — one directory cannot have two
/// notes standing for it — and `X.md` wins so the build still produces a graph.
fn folder_layout<'a>(
    by_path: &BTreeMap<&'a str, &'a ConceptDoc>,
    dirs: &BTreeSet<String>,
    report: &mut BuildReport,
) -> FolderLayout<'a> {
    let mut layout = FolderLayout::default();
    for dir in dirs {
        let base = dir.rsplit('/').next().unwrap_or(dir);
        let beside = by_path.get(dir.as_str()).copied();
        let inside = by_path.get(format!("{dir}/{base}").as_str()).copied();
        if beside.is_some() && inside.is_some() {
            report.errors.push(format!(
                "folder note declared twice for `{dir}/`: `{dir}.md` and `{dir}/{base}.md`; `{dir}.md` is used"
            ));
        }
        if let Some(note) = beside.or(inside) {
            layout.note_of_dir.insert(dir.clone(), note);
            layout.dir_of_note.insert(doc_path(note), dir.clone());
        }
    }
    layout
}

/// One containment row: the folder-note edge when a note holds a note, and
/// `CONTAINS` whenever a `Folder` node is either endpoint — a folder note
/// standing in for a directory still *contains* the plain subfolders under it
/// (VAULT.md §2.2, §2.3).
fn push_containment(
    groups: &mut EdgeGroups,
    container: &Container<'_>,
    child_label: &str,
    child_id: &str,
    child_is_note: bool,
    profile: &Profile,
) {
    let (conn, src_label, src_id, tgt_label, tgt_id) = match container {
        Container::Note(parent) if child_is_note => {
            let down = profile.folder_note_direction == FolderNoteDirection::ParentToChild;
            let (s, si, t, ti) = if down {
                (
                    parent.label.as_str(),
                    parent.concept_id.as_str(),
                    child_label,
                    child_id,
                )
            } else {
                (
                    child_label,
                    child_id,
                    parent.label.as_str(),
                    parent.concept_id.as_str(),
                )
            };
            (profile.folder_note_edge.as_str(), s, si, t, ti)
        }
        Container::Note(parent) => (
            CONTAINS_CONN_TYPE,
            parent.label.as_str(),
            parent.concept_id.as_str(),
            child_label,
            child_id,
        ),
        Container::Folder(dir) => (
            CONTAINS_CONN_TYPE,
            FOLDER_LABEL,
            dir.as_str(),
            child_label,
            child_id,
        ),
    };
    groups
        .entry((
            conn.to_string(),
            src_label.to_string(),
            tgt_label.to_string(),
        ))
        .or_default()
        .push((src_id.to_string(), tgt_id.to_string(), Vec::new()));
}

/// Materialize the directory hierarchy as `Folder` nodes:
/// `(:Folder)-[:CONTAINS]->(:Concept)` and `(:Folder)-[:CONTAINS]->(:Folder)`.
/// A directory's `index.md` enriches its Folder node's title/description (so the
/// reserved file is recovered as structure rather than discarded). Co-located
/// concepts gain a 2-hop hub, capturing the taxonomic meaning of the layout.
///
/// Under [`Profile::folder_notes`] a directory with a folder note gets no
/// `Folder` node at all: the note takes its place in the hierarchy, and the
/// notes inside it are joined by the profile's folder-note edge instead
/// (VAULT.md §2.3). The containment rows are returned rather than emitted —
/// see the note in [`super::build`].
pub(super) fn build_folders<'a>(
    graph: &mut DirGraph,
    docs: &'a [ConceptDoc],
    index_files: &HashMap<String, PathBuf>,
    opts: &BuildOptions,
    report: &mut BuildReport,
) -> Result<EdgeGroups, String> {
    // Every directory holding a concept, plus all ancestor directories.
    let mut dirs: BTreeSet<String> = BTreeSet::new();
    for d in docs {
        let mut p = crate::okf::parent_dir(doc_path(d)).to_string();
        while !p.is_empty() {
            let parent = crate::okf::parent_dir(&p).to_string();
            dirs.insert(p);
            p = parent;
        }
    }
    if dirs.is_empty() {
        return Ok(BTreeMap::new());
    }
    let profile = &opts.profile;
    let by_path: BTreeMap<&'a str, &'a ConceptDoc> =
        docs.iter().map(|d| (doc_path(d), d)).collect();
    let layout = if profile.folder_notes {
        folder_layout(&by_path, &dirs, report)
    } else {
        FolderLayout::default()
    };
    report.folder_notes = layout.note_of_dir.len();
    let folder_dirs: Vec<&str> = dirs
        .iter()
        .map(String::as_str)
        .filter(|d| !layout.note_of_dir.contains_key(*d))
        .collect();
    count_nodes(report, FOLDER_LABEL, folder_dirs.len());

    // Folder nodes (id = dir path; title/description from index.md if present).
    if !folder_dirs.is_empty() {
        let mut rows: Vec<Vec<Value>> = Vec::with_capacity(folder_dirs.len());
        for dir in &folder_dirs {
            let (title, desc) = index_files
                .get(*dir)
                .map(|p| folder_meta(p))
                .unwrap_or((None, None));
            let title = title.unwrap_or_else(|| dir.rsplit('/').next().unwrap_or(dir).to_string());
            rows.push(vec![
                Value::String((*dir).to_string()),
                Value::String(title),
                desc.map(Value::String).unwrap_or(Value::Null),
            ]);
        }
        let df = DataFrame::from_cypher_rows(
            vec![
                "id".to_string(),
                "title".to_string(),
                "description".to_string(),
            ],
            rows,
        )?;
        maintain::add_nodes(
            graph,
            df,
            FOLDER_LABEL.to_string(),
            "id".to_string(),
            Some("title".to_string()),
            Some("update".to_string()),
        )?;
    }

    // Containment: every note and every surviving `Folder` hangs off whatever
    // holds its directory.
    let mut groups: EdgeGroups = BTreeMap::new();
    for d in docs {
        let dir = match layout.dir_of_note.get(doc_path(d)) {
            Some(owned) => crate::okf::parent_dir(owned),
            None => crate::okf::parent_dir(doc_path(d)),
        };
        if let Some(c) = layout.owner(dir) {
            push_containment(&mut groups, &c, &d.label, &d.concept_id, true, profile);
        }
    }
    for dir in &folder_dirs {
        if let Some(c) = layout.owner(crate::okf::parent_dir(dir)) {
            push_containment(&mut groups, &c, FOLDER_LABEL, dir, false, profile);
        }
    }
    Ok(groups)
}

/// Extract a `(title, description)` for a Folder node from its `index.md`:
/// the first heading is the title, the first prose line the description.
fn folder_meta(path: &Path) -> (Option<String>, Option<String>) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return (None, None);
    };
    let mut title = None;
    let mut desc = None;
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if let Some(h) = heading_line(t) {
            if title.is_none() {
                title = Some(h.to_string());
            }
        } else if desc.is_none() {
            desc = Some(t.to_string());
        }
        if title.is_some() && desc.is_some() {
            break;
        }
    }
    (title.filter(|s| !s.is_empty()), desc)
}

/// The text of an ATX heading line (already left-trimmed), or `None` when the
/// line is not a heading. A heading is one to six `#` followed by a space, a
/// tab, or the end of the line; `#tag see [[Alice]]` is a tag line, not a
/// heading.
///
/// A line scan, not `okf::structure`'s block tree, because [`folder_meta`]
/// reads an `index.md` **whole** — frontmatter included. CommonMark reads the
/// `title: x` above a closing `---` as a setext heading, so a parser would
/// title every folder after its own frontmatter's last key.
fn heading_line(trimmed: &str) -> Option<&str> {
    let hashes = trimmed.len() - trimmed.trim_start_matches('#').len();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &trimmed[hashes..];
    if rest.is_empty() || rest.starts_with([' ', '\t']) {
        Some(rest.trim())
    } else {
        None
    }
}

#[cfg(test)]
#[path = "folders_tests.rs"]
mod folders_tests;
