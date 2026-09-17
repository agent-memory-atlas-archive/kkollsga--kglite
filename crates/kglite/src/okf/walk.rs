//! Bundle directory walk: enumerate concept `.md` files and per-directory
//! `index.md` files.
//!
//! Reserved filenames (`index.md`, `log.md`) are not concepts *while the
//! profile says so*: `index.md` is captured per directory (it describes the
//! directory — it enriches the `Folder` node in the builder) and `log.md` is
//! dropped. The vault profile reserves neither (VAULT.md §2.4). Hidden
//! directories (`.git`, `.obsidian`, …) are pruned, mirroring codingest's
//! `walk_filter`.

use crate::okf::model::BuildOptions;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// A discovered concept file, with its bundle-relative (forward-slashed) path.
#[derive(Debug, Clone)]
pub struct DiscoveredFile {
    /// Bundle-relative path, forward-slashed (e.g. `tables/users.md`).
    pub rel_path: String,
    /// Absolute path on disk (for reading the file).
    pub abs_path: PathBuf,
}

/// A non-`.md` file the walk saw, with the `stat` metadata VAULT.md §6.3 puts
/// on its node — and the only metadata it puts there: the bytes are never
/// read, so build cost is independent of image volume (§6.5).
///
/// `(rel_path, mtime, size)` is also the tuple P11 fingerprints a vault by, so
/// the walk hands it over whole rather than making the fingerprint pass stat
/// every file a second time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredAttachment {
    /// Vault-relative path, forward-slashed (`img/diagram.png`) — the node id.
    pub rel_path: String,
    /// Absolute path on disk. Never opened by the build; here for the
    /// exporter, which copies the file.
    pub abs_path: PathBuf,
    /// File size in bytes.
    pub size: u64,
    /// Modification time as whole seconds since the Unix epoch, UTC. Seconds
    /// rather than the platform's native precision because a fingerprint is
    /// compared across copies and filesystems — APFS keeps nanoseconds, a FAT
    /// or SMB copy keeps two — and a sub-second difference that no filesystem
    /// agrees on would report a change that never happened. `None` when the
    /// filesystem has no mtime for the file.
    pub mtime: Option<i64>,
}

/// Result of a bundle walk: concept files + each directory's `index.md`.
#[derive(Debug, Clone, Default)]
pub struct WalkResult {
    pub concepts: Vec<DiscoveredFile>,
    /// Bundle-relative directory path (`""` = root) → that directory's
    /// `index.md` absolute path.
    pub index_files: HashMap<String, PathBuf>,
    /// Every non-`.md`, non-hidden file under the root, sorted by `rel_path`.
    /// Empty unless [`crate::okf::model::Profile::attachments`] is set — an
    /// OKF sweep drops attachment references, so stat'ing the files would buy
    /// nothing.
    pub attachments: Vec<DiscoveredAttachment>,
}

fn is_ignored_dir(name: &str) -> bool {
    // Hidden dirs (.git, .obsidian, .venv, …) plus the usual build noise.
    name.starts_with('.')
        || matches!(
            name,
            "node_modules" | "target" | "__pycache__" | "venv" | "env" | "site-packages"
        )
}

/// True if a directory at bundle-relative path `rel` (basename `name`) matches a
/// caller `skip_dirs` entry: bare name → match at any depth; entry with `/` →
/// anchored relative-path prefix (the dir and its subtree).
fn matches_skip(rel: &str, name: &str, skip_dirs: &[&str]) -> bool {
    skip_dirs.iter().any(|raw| {
        let entry = raw.trim_matches('/');
        if entry.is_empty() {
            false
        } else if entry.contains('/') {
            rel == entry || rel.starts_with(&format!("{entry}/"))
        } else {
            name == entry
        }
    })
}

/// Walk `root`, returning concept `.md` files plus per-directory `index.md`
/// files. `opts.skip_dirs` and `opts.profile.skip_dirs` both prune matching
/// directories (and their subtrees) — the caller's list and the vault's own
/// declaration (VAULT.md §2.4) are unioned, never one overriding the other;
/// `opts.profile` also decides which filenames are reserved. Errors only on an
/// unreadable root.
/// The two conditions that make a root unwalkable rather than merely faulty.
///
/// Shared with [`crate::okf::validate`], which reserves its `Err` for exactly
/// these and turns every other build failure into a report finding: the
/// distinction is only honest while both sides ask the same question in the
/// same words.
pub(crate) fn check_root(root: &Path) -> Result<(), String> {
    if !root.exists() {
        return Err(format!(
            "OKF bundle path does not exist: {}",
            root.display()
        ));
    }
    if !root.is_dir() {
        return Err(format!(
            "OKF bundle path is not a directory: {}",
            root.display()
        ));
    }
    Ok(())
}

pub fn discover(root: &Path, opts: &BuildOptions) -> Result<WalkResult, String> {
    let skip_dirs: Vec<&str> = opts
        .skip_dirs
        .iter()
        .chain(opts.profile.skip_dirs.iter())
        .map(String::as_str)
        .collect();
    let skip_dirs = skip_dirs.as_slice();
    check_root(root)?;

    let mut out = Vec::new();
    let mut attachments = Vec::new();
    let mut index_files: HashMap<String, PathBuf> = HashMap::new();
    let walker = WalkDir::new(root).into_iter().filter_entry(|e| {
        // Never prune the root itself (depth 0) — the bundle directory may
        // legitimately be hidden (e.g. a `.tmpXXXX` temp dir, or a path under
        // `.claude/`). Only prune *descendant* hidden / build / skip dirs.
        if e.depth() == 0 || !e.file_type().is_dir() {
            return true;
        }
        let Some(name) = e.file_name().to_str() else {
            return true;
        };
        if is_ignored_dir(name) {
            return false;
        }
        if !skip_dirs.is_empty() {
            let rel = e
                .path()
                .strip_prefix(root)
                .ok()
                .map(|r| {
                    r.components()
                        .filter_map(|c| c.as_os_str().to_str())
                        .collect::<Vec<_>>()
                        .join("/")
                })
                .unwrap_or_default();
            if matches_skip(&rel, name, skip_dirs) {
                return false;
            }
        }
        true
    });

    for entry in walker.filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        let name = match entry.file_name().to_str() {
            Some(n) => n,
            None => continue,
        };
        let rel = match entry.path().strip_prefix(root) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let rel_path = rel
            .components()
            .filter_map(|c| c.as_os_str().to_str())
            .collect::<Vec<_>>()
            .join("/");
        if !name.ends_with(".md") {
            // A non-`.md` file is a candidate attachment (VAULT.md §1.2): a
            // node only if some note references it, so this is an index, not
            // a node list. Hidden files (`.DS_Store`, editor swap files) are
            // not vault content and no note references them.
            if opts.profile.attachments && !name.starts_with('.') {
                let meta = entry.metadata().ok();
                attachments.push(DiscoveredAttachment {
                    rel_path,
                    abs_path: entry.path().to_path_buf(),
                    size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
                    mtime: meta.as_ref().and_then(mtime_secs),
                });
            }
            continue;
        }
        if name == "log.md" && opts.profile.skip_log_files {
            continue;
        }
        if name == "index.md" && opts.profile.index_as_folder_metadata {
            // Record per directory (bundle-relative dir path; "" = root).
            let dir = rel_path
                .rfind('/')
                .map(|i| rel_path[..i].to_string())
                .unwrap_or_default();
            index_files.insert(dir, entry.path().to_path_buf());
            continue;
        }
        out.push(DiscoveredFile {
            rel_path,
            abs_path: entry.path().to_path_buf(),
        });
    }
    // Deterministic order (parallelism happens at parse time, but a stable file
    // list keeps id-collision resolution and tests reproducible).
    out.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    attachments.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(WalkResult {
        concepts: out,
        index_files,
        attachments,
    })
}

/// A file's mtime as whole seconds since the Unix epoch, UTC. Pre-epoch times
/// stay signed rather than saturating at 0 — a 1969 mtime is odd, but claiming
/// it is 1970 is a lie the fingerprint would then compare.
fn mtime_secs(meta: &std::fs::Metadata) -> Option<i64> {
    let modified = meta.modified().ok()?;
    Some(match modified.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::model::Profile;
    use std::fs;
    use tempfile::tempdir;

    fn bundle() -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("notes")).unwrap();
        for rel in ["notes/a.md", "notes/index.md", "notes/log.md"] {
            fs::write(dir.path().join(rel), "body").unwrap();
        }
        dir
    }

    fn rel_paths(r: &WalkResult) -> Vec<&str> {
        r.concepts.iter().map(|f| f.rel_path.as_str()).collect()
    }

    #[test]
    fn default_profile_reserves_index_and_log() {
        let dir = bundle();
        let r = discover(dir.path(), &BuildOptions::default()).unwrap();
        assert_eq!(rel_paths(&r), vec!["notes/a.md"]);
        assert!(
            r.index_files.contains_key("notes"),
            "index.md → folder meta"
        );
    }

    #[test]
    fn only_the_attachment_profile_indexes_non_md_files() {
        let dir = bundle();
        fs::create_dir_all(dir.path().join("img")).unwrap();
        fs::write(dir.path().join("img/diagram.png"), b"\x89PNG\r\n").unwrap();
        fs::write(dir.path().join("notes/.DS_Store"), b"junk").unwrap();
        fs::create_dir_all(dir.path().join(".obsidian")).unwrap();
        fs::write(dir.path().join(".obsidian/workspace.json"), b"{}").unwrap();

        assert!(
            discover(dir.path(), &BuildOptions::default())
                .unwrap()
                .attachments
                .is_empty(),
            "an OKF sweep drops attachment references, so it stats nothing"
        );

        let opts = BuildOptions::for_dialect(crate::okf::model::Dialect::Obsidian);
        let r = discover(dir.path(), &opts).unwrap();
        let paths: Vec<&str> = r.attachments.iter().map(|a| a.rel_path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["img/diagram.png"],
            "hidden files and pruned dot-directories are not vault content"
        );
        assert_eq!(r.attachments[0].size, 6);
        assert!(
            r.attachments[0].mtime.is_some_and(|m| m > 1_600_000_000),
            "a freshly written file has an mtime well past 2020"
        );
        assert!(
            rel_paths(&r).contains(&"notes/log.md"),
            "the vault profile keeps `log.md` a note, not an attachment"
        );
    }

    #[test]
    fn profile_can_make_index_and_log_ordinary_concepts() {
        let dir = bundle();
        let opts = BuildOptions {
            profile: Profile {
                index_as_folder_metadata: false,
                skip_log_files: false,
                ..Profile::default()
            },
            ..BuildOptions::default()
        };
        let r = discover(dir.path(), &opts).unwrap();
        assert_eq!(
            rel_paths(&r),
            vec!["notes/a.md", "notes/index.md", "notes/log.md"]
        );
        assert!(r.index_files.is_empty(), "no folder metadata was diverted");
    }
}
