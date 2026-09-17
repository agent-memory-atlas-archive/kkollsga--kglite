//! Overwrite safety (VAULT.md §10.7).
//!
//! The export owns exactly the files its last manifest recorded, and it proves
//! ownership by hash rather than by mtime or by a marker in the file: a human
//! who edits an exported note leaves the bytes different from the recorded
//! digest, and that difference is the whole signal. Everything follows from it:
//!
//! - a file the manifest does not name was written by somebody else → refuse;
//! - a named file whose bytes moved was edited by a human → refuse;
//! - a named file whose bytes still match and whose node is gone → delete;
//! - a named file whose bytes already equal what we would write → leave alone,
//!   so an unchanged export does not touch a single modification time.
//!
//! `force` overrides the two refusals, and only those. Nothing here ever
//! removes a file the manifest did not name, with or without it.

use super::ExportReport;
use crate::okf::vault_config::CONFIG_DIR;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The manifest's filename inside `.kglite/`.
pub const MANIFEST_FILE: &str = "export-manifest.json";
/// The only `kglite_vault:` value this manifest format understands.
pub const MANIFEST_VERSION: i64 = 1;

/// Writes every file of one export, refusing the ones it does not own.
pub(super) struct Writer<'a> {
    dir: &'a Path,
    force: bool,
    /// What the previous export wrote: vault-relative path → sha256 hex.
    previous: BTreeMap<String, String>,
    /// What this export owns when it finishes.
    current: BTreeMap<String, String>,
    pub(super) report: ExportReport,
}

impl<'a> Writer<'a> {
    /// Read the manifest `dir` already carries, if any.
    ///
    /// A manifest that will not parse is an `Err`, not an empty one: treating
    /// it as "nothing is owned" would refuse every file the export is supposed
    /// to update, and treating it as "everything is owned" would overwrite a
    /// human's work. Neither guess is safe, so the caller is told.
    pub(super) fn open(dir: &'a Path, force: bool) -> Result<Self, String> {
        let path = manifest_path(dir);
        let previous = if path.is_file() {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("reading {}: {e}", path.display()))?;
            parse(&text).map_err(|e| format!("{}: {e}", path.display()))?
        } else {
            BTreeMap::new()
        };
        Ok(Writer {
            dir,
            force,
            previous,
            current: BTreeMap::new(),
            report: ExportReport::default(),
        })
    }

    /// Write one file, or account for why it was not written.
    pub(super) fn put(&mut self, rel: &str, bytes: &[u8]) -> Result<(), String> {
        self.put_at(rel, bytes, None)
    }

    /// Write one file and give it `modified` as its modification time.
    ///
    /// A copied attachment carries the source file's, because the loader reads
    /// `mtime` back off `stat` into a node property (VAULT.md §6.3) and §10.9
    /// does not list it among the round trip's losses: a destination stamped
    /// with the time of the copy makes the fixed point depend on which second
    /// the export ran in. Files the export *generates* pass `None` — their
    /// content is the graph's, and no earlier time belongs to them.
    pub(super) fn put_at(
        &mut self,
        rel: &str,
        bytes: &[u8],
        modified: Option<SystemTime>,
    ) -> Result<(), String> {
        let digest = hex_digest(bytes);
        let path = self.dir.join(rel);
        let on_disk = match std::fs::read(&path) {
            Ok(found) => Some(hex_digest(&found)),
            Err(_) => None,
        };
        match (on_disk, self.previous.get(rel)) {
            // Nothing there: ours to create.
            (None, _) => self.write(rel, &path, bytes, digest, modified),
            // There, and ours, and already exactly right.
            (Some(found), Some(recorded)) if &found == recorded && found == digest => {
                self.report.files_unchanged += 1;
                self.current.insert(rel.to_string(), digest);
                Ok(())
            }
            // There, ours, and stale: replace it.
            (Some(found), Some(recorded)) if &found == recorded => {
                self.write(rel, &path, bytes, digest, modified)
            }
            // There and edited, or there and never ours.
            (Some(_), owned) => {
                if self.force {
                    return self.write(rel, &path, bytes, digest, modified);
                }
                self.report.files_refused += 1;
                self.report.refusals.push(match owned {
                    Some(_) => {
                        format!("{rel}: edited since the last export (use force to replace)")
                    }
                    None => format!("{rel}: not written by an export (use force to replace)"),
                });
                // An owned-but-edited file stays owned, so the deletion pass
                // below does not read it as a file whose node disappeared and
                // remove the very edit that was just protected.
                if let Some(recorded) = owned.cloned() {
                    self.current.insert(rel.to_string(), recorded);
                }
                Ok(())
            }
        }
    }

    fn write(
        &mut self,
        rel: &str,
        path: &Path,
        bytes: &[u8],
        digest: String,
        modified: Option<SystemTime>,
    ) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("creating {}: {e}", parent.display()))?;
        }
        std::fs::write(path, bytes).map_err(|e| format!("writing {}: {e}", path.display()))?;
        if let Some(when) = modified {
            stamp(path, when)?;
        }
        self.report.files_written += 1;
        self.current.insert(rel.to_string(), digest);
        Ok(())
    }

    /// Remove the files this export no longer produces, then record what it
    /// owns.
    pub(super) fn finish(mut self) -> Result<ExportReport, String> {
        let stale: Vec<(String, String)> = self
            .previous
            .iter()
            .filter(|(rel, _)| !self.current.contains_key(*rel))
            .map(|(rel, digest)| (rel.clone(), digest.clone()))
            .collect();
        for (rel, recorded) in stale {
            let path = self.dir.join(&rel);
            let Ok(found) = std::fs::read(&path) else {
                // Already gone: the manifest simply stops naming it.
                continue;
            };
            if hex_digest(&found) == recorded || self.force {
                std::fs::remove_file(&path)
                    .map_err(|e| format!("removing {}: {e}", path.display()))?;
                self.report.files_deleted += 1;
            } else {
                self.report.files_refused += 1;
                self.report
                    .refusals
                    .push(format!("{rel}: edited since the last export, not deleted"));
                self.current.insert(rel, recorded);
            }
        }
        self.report.refusals.sort();
        let path = manifest_path(self.dir);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("creating {}: {e}", parent.display()))?;
        }
        std::fs::write(&path, render(&self.current))
            .map_err(|e| format!("writing {}: {e}", path.display()))?;
        Ok(self.report)
    }
}

/// `<dir>/.kglite/export-manifest.json`.
pub(super) fn manifest_path(dir: &Path) -> PathBuf {
    dir.join(CONFIG_DIR).join(MANIFEST_FILE)
}

/// Give a file the modification time it is supposed to carry.
///
/// A failure is an `Err` rather than a shrug: the whole point of the stamp is
/// that the value on disk is the one the graph will read back, and a vault
/// whose pictures quietly carry the wrong time round-trips differently every
/// run for no visible reason.
fn stamp(path: &Path, when: SystemTime) -> Result<(), String> {
    std::fs::File::options()
        .write(true)
        .open(path)
        .and_then(|file| file.set_modified(when))
        .map_err(|e| format!("setting the modification time of {}: {e}", path.display()))
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The manifest document. A `BTreeMap` so the file's bytes are the same for
/// the same set of files — §10.8's determinism reaches the manifest too.
fn render(files: &BTreeMap<String, String>) -> String {
    let document = serde_json::json!({
        "kglite_vault": MANIFEST_VERSION,
        "files": files,
    });
    format!(
        "{}\n",
        serde_json::to_string_pretty(&document).unwrap_or_default()
    )
}

/// Read a manifest, refusing a version this build does not understand.
fn parse(text: &str) -> Result<BTreeMap<String, String>, String> {
    let document: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
    match document.get("kglite_vault").and_then(|v| v.as_i64()) {
        Some(MANIFEST_VERSION) => {}
        Some(other) => {
            return Err(format!(
                "`kglite_vault: {other}` is not a manifest version this build writes \
                 ({MANIFEST_VERSION})"
            ))
        }
        None => return Err("no `kglite_vault` version".to_string()),
    }
    let Some(files) = document.get("files").and_then(|v| v.as_object()) else {
        return Err("no `files` object".to_string());
    };
    files
        .iter()
        .map(|(path, digest)| match digest.as_str() {
            Some(text) => Ok((path.clone(), text.to_string())),
            None => Err(format!("`files.{path}` is not a hash string")),
        })
        .collect()
}

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod manifest_tests;
