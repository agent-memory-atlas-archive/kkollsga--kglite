//! What a bundle looked like when it was built, and whether it still does
//! (VAULT.md §12).
//!
//! [`fingerprint`] is a 64-bit summary of every file a build of that directory
//! would read: each note, each attachment, and — for a vault — everything under
//! `.kglite/`, as `(rel_path, size, mtime)`. [`crate::okf::build`] stamps it
//! onto the graph beside the root it was built from and the dialect it read
//! them with, `save_graph` persists all three, and [`rebuild_if_changed`]
//! compares the stamp with the directory as it is now.
//!
//! **Why the dialect is part of the stamp.** The summary covers "what a build
//! would read", and that depends on the dialect: `.kglite/` is an input for
//! `obsidian` alone, and the dialect decides which files are notes. Two
//! dialects therefore give two numbers for one untouched directory, so a
//! caller who asked "is my graph current?" without naming the dialect its
//! build used was told "changed" every time — and got back a graph of a
//! different shape, which loads and looks valid.
//!
//! **Why `(path, size, mtime)` and not the bytes.** The fingerprint has to be
//! cheap enough to ask on demand — a vault is thousands of small files, and
//! hashing their contents is the build. `stat` is what every incremental build
//! system in use has settled on, and the failure it admits is narrow: a file
//! rewritten within the same second, to exactly the same length, with a
//! filesystem that does not move the mtime. A caller that cannot tolerate that
//! calls `build` unconditionally.
//!
//! **Why whole seconds.** See [`crate::okf::walk::DiscoveredAttachment::mtime`]:
//! the value is compared across copies and filesystems, and sub-second
//! precision that no two filesystems agree on would report changes that never
//! happened.

use std::path::Path;

use crate::graph::dir_graph::DirGraph;
use crate::graph::embedder::Embedder;
use crate::okf::build::{build, effective_options, BuildOutput};
use crate::okf::model::{BuildOptions, Dialect, RebuildOptions};
use crate::okf::walk;

/// FNV-1a offset basis and prime (64-bit).
///
/// Written out rather than taken from a hasher crate because this value is
/// *persisted*: it goes into a `.kgl` and is compared by a later process,
/// possibly a later build of kglite. `DefaultHasher` and `FxHasher` are both
/// explicitly unspecified across versions, so either would silently start
/// reporting "changed" for every graph saved by an older binary.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a over `bytes`, folded into `state`.
fn fold(state: u64, bytes: &[u8]) -> u64 {
    let mut hash = state;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// One file's contribution: its path, size and mtime, each length-delimited so
/// no two different file lists can fold to the same bytes (`a` + `bc` and `ab`
/// + `c` are one string otherwise).
fn fold_entry(state: u64, rel_path: &str, size: u64, mtime: Option<i64>) -> u64 {
    let mut hash = fold(state, &(rel_path.len() as u64).to_le_bytes());
    hash = fold(hash, rel_path.as_bytes());
    hash = fold(hash, &size.to_le_bytes());
    // `None` is its own value, not a zero mtime — a filesystem that reports no
    // mtime must not read as one that reports the epoch.
    fold(hash, &mtime.unwrap_or(i64::MIN).to_le_bytes())
}

/// A stable 64-bit summary of what a build of `root` with `opts` would read.
///
/// Same directory, same options, same value — across processes, machines and
/// runs. A touched, resized, renamed, added or removed file changes it; the
/// order the filesystem hands the entries back does not, because both lists the
/// walk returns are sorted by path and the `.kglite/` scan sorts its own.
///
/// The `.kglite/` directory is included only for the dialect that reads it
/// (`obsidian`), because the fingerprint's contract is "what a build would
/// read": under `okf`/`loose` that directory is not an input, and hashing it
/// would report a change no rebuild could act on.
///
/// Errors only where [`walk::discover`] does — a root that does not exist or is
/// not a directory — and where a `.kglite/vault.yaml` will not parse, since a
/// build of that vault would fail on the same file.
pub fn fingerprint(root: &Path, opts: &BuildOptions) -> Result<u64, String> {
    let mut warnings = Vec::new();
    let (effective, _config) = effective_options(root, opts, &mut warnings)?;
    let walked = walk::discover(root, &effective)?;
    Ok(fingerprint_of(root, &walked, &effective))
}

/// The fingerprint of a walk already done — what [`build`] stamps, so a build
/// pays for no second walk and cannot disagree with [`fingerprint`] about the
/// file set it just read.
pub(crate) fn fingerprint_of(root: &Path, walked: &walk::WalkResult, opts: &BuildOptions) -> u64 {
    let mut hash = FNV_OFFSET;
    for file in walked.concepts.iter().chain(walked.diverted.iter()) {
        hash = fold_entry(hash, &file.rel_path, file.size, file.mtime);
    }
    for attachment in &walked.attachments {
        hash = fold_entry(
            hash,
            &attachment.rel_path,
            attachment.size,
            attachment.mtime,
        );
    }
    if opts.dialect == crate::okf::Dialect::Obsidian {
        for (rel_path, size, mtime) in kglite_dir_entries(root) {
            hash = fold_entry(hash, &rel_path, size, mtime);
        }
    }
    hash
}

/// Every file under `root/.kglite/`, sorted by relative path.
///
/// The walk prunes dot-directories, so `vault.yaml`, `skills/` and `recipes/`
/// are invisible to it — and they are build inputs (VAULT.md §7, §8), so a
/// fingerprint that ignored them would call a vault unchanged after its
/// declarations were rewritten. Unreadable entries are skipped rather than
/// erroring: this is a summary, and a file the build cannot read is a build
/// problem to report, not a fingerprint problem.
fn kglite_dir_entries(root: &Path) -> Vec<(String, u64, Option<i64>)> {
    let dir = crate::okf::vault_config::config_dir(root);
    let mut entries: Vec<(String, u64, Option<i64>)> = walkdir::WalkDir::new(&dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| {
            let rel = entry.path().strip_prefix(root).ok()?;
            let rel_path = rel
                .components()
                .filter_map(|c| c.as_os_str().to_str())
                .collect::<Vec<_>>()
                .join("/");
            let meta = entry.metadata().ok();
            Some((
                rel_path,
                meta.as_ref().map(|m| m.len()).unwrap_or(0),
                meta.as_ref().and_then(walk::mtime_secs),
            ))
        })
        .collect();
    entries.sort();
    entries
}

/// The dialect `graph`'s build read its directory with, if it recorded one.
///
/// `Ok(None)` is a graph that was not built from a directory, or one saved by
/// 0.17.8–0.17.10, which stamped the root and the fingerprint and not this.
/// A name this build does not know is an error rather than a fallback to
/// `okf`: it means the `.kgl` came from a build that knows a dialect this one
/// does not, and silently reading a vault as a bundle is the failure the
/// stamp exists to prevent.
pub fn stamped_dialect(graph: &DirGraph) -> Result<Option<Dialect>, String> {
    let Some(name) = graph.source_dialect.as_deref() else {
        return Ok(None);
    };
    Dialect::from_name(name).map(Some).ok_or_else(|| {
        format!(
            "this graph was built with dialect `{name}`, which this build of kglite does not \
             know — it was written by a newer one. Upgrade kglite, or rebuild the directory \
             with a dialect this build has."
        )
    })
}

/// Rebuild a graph from the directory it was built from, but only if that
/// directory has changed.
///
/// `Ok(None)` means the vault's fingerprint still matches the one stamped on
/// `old`: nothing was read beyond the `stat` pass, and the caller keeps the
/// graph it has. Otherwise the vault is rebuilt, `old`'s vectors are carried
/// across (same label, same id — a note that changed *label*, by moving
/// between folders, re-embeds; VAULT.md §12), and, when an embedder is given,
/// every `embed:` target the rebuilt `.kglite/vault.yaml` declares runs a
/// changed-mode embedding pass over the carried hashes. A target that fails
/// leaves a warning in the returned report rather than discarding a graph that
/// is otherwise correct.
///
/// The dialect comes from the stamp when `opts` names none — that is what
/// makes `rebuild_if_changed(graph, &RebuildOptions::default(), None)` a
/// question about *this* graph rather than about a bundle that happens to
/// live at the same path. Naming one that contradicts the stamp is refused
/// rather than obeyed: the two answers ("rebuild it as it was built" and
/// "build something else here") are far enough apart that guessing between
/// them is how the wrong graph gets written. The remaining options are the
/// caller's own, and a rebuild with different ones is a different build that
/// says so by rebuilding.
///
/// A graph saved before the dialect was stamped (0.17.8–0.17.10) keeps the
/// old behaviour — the caller's dialect, `okf` when they named none — and the
/// returned report says the stamp was missing, since that is the case where
/// an unchanged vault can still read as changed.
pub fn rebuild_if_changed(
    old: &DirGraph,
    opts: &RebuildOptions,
    embedder: Option<&dyn Embedder>,
) -> Result<Option<BuildOutput>, String> {
    let Some(root) = old.source_root.clone() else {
        return Err(
            "this graph carries no source_root: it was not built by okf::build, or it was \
             saved by a version that did not record one. Build the directory with okf::build \
             instead."
                .to_string(),
        );
    };
    let stamped = stamped_dialect(old)?;
    let dialect = match (opts.dialect, stamped) {
        (Some(asked), Some(stamped)) if asked != stamped => {
            return Err(format!(
                "this graph was built with dialect `{}`, and the rebuild asks for `{}`. \
                 Reading the same directory the other way is a different build, not a \
                 refresh of this one: drop the dialect to rebuild it as it was built, or \
                 call okf::build for the other reading.",
                stamped.name(),
                asked.name(),
            ));
        }
        (asked, stamped) => asked.or(stamped).unwrap_or(Dialect::Okf),
    };
    let opts = &opts.resolve(dialect);
    let root = Path::new(&root);
    if old.source_fingerprint == Some(fingerprint(root, opts)?) {
        return Ok(None);
    }
    let mut out = build(root, opts)?;
    if stamped.is_none() {
        out.report.warnings.push(format!(
            "this graph carries no dialect stamp (it was built before kglite recorded one), \
             so it was rebuilt as `{}` — check that is what it was built as, because the \
             fingerprint of the other dialects will not match it",
            dialect.name(),
        ));
    }
    let graph = crate::graph::handle::make_dir_graph_mut(&mut out.graph);
    let (stores, _vectors, _skipped) = graph.copy_embeddings_from(old);
    if let Some(model) = embedder {
        let hooks = crate::graph::embeddings::EmbedHooks::default();
        for (label, property) in out.report.embed_targets.clone() {
            if let Err(error) = crate::graph::embeddings::embed_property(
                &mut out.graph,
                &label,
                &property,
                crate::graph::embeddings::EmbedMode::Changed,
                model,
                &hooks,
            ) {
                out.report
                    .warnings
                    .push(format!("embed target `{label}.{property}` failed: {error}"));
            }
        }
    } else if !out.report.embed_targets.is_empty() {
        out.report.warnings.push(format!(
            "`.kglite/vault.yaml` declares {} embed target(s) but no embedder was given — \
             no vectors were computed",
            out.report.embed_targets.len()
        ));
    }
    let _ = stores;
    Ok(Some(out))
}

#[cfg(test)]
#[path = "fingerprint_tests.rs"]
mod tests;
