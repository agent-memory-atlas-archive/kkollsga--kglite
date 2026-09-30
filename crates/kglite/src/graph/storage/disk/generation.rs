//! Immutable disk-generation publication and writer ownership.

use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const CURRENT_FILE: &str = "CURRENT";
const GENERATIONS_DIR: &str = "generations";
const GENERATION_PREFIX: &str = "gen_";
const STAGE_PREFIX: &str = ".stage-";
static NEXT_WORKSPACE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

#[cfg(test)]
thread_local! {
    static PUBLISH_FAILPOINT: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
}

fn publish_failpoint(stage: &'static str) -> io::Result<()> {
    #[cfg(test)]
    if PUBLISH_FAILPOINT.with(|point| point.get() == Some(stage)) {
        return Err(io::Error::other(format!(
            "injected generation publish failure at {stage}"
        )));
    }
    let _ = stage;
    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedSnapshot {
    pub(crate) logical_root: PathBuf,
    pub(crate) snapshot_dir: PathBuf,
    pub(crate) generation: Option<u64>,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn generation_name(id: u64) -> String {
    format!("{GENERATION_PREFIX}{id:020}")
}

fn parse_generation_name(name: &str) -> io::Result<u64> {
    let digits = name
        .strip_prefix(GENERATION_PREFIX)
        .ok_or_else(|| invalid("CURRENT does not name a KGLite generation"))?;
    if digits.len() != 20 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid("CURRENT contains an invalid generation name"));
    }
    digits
        .parse()
        .map_err(|_| invalid("CURRENT generation number is out of range"))
}

/// Resolve the immutable snapshot selected by `CURRENT`. A missing pointer is
/// the legacy flat-directory format; a present but invalid pointer is always
/// an error and never falls back to possibly stale legacy files.
pub(crate) fn resolve_snapshot(root: &Path) -> io::Result<ResolvedSnapshot> {
    let current = root.join(CURRENT_FILE);
    if !current.exists() {
        return Ok(ResolvedSnapshot {
            logical_root: root.to_path_buf(),
            snapshot_dir: root.to_path_buf(),
            generation: None,
        });
    }
    let raw = fs::read_to_string(&current)?;
    let name = raw
        .strip_suffix('\n')
        .ok_or_else(|| invalid("CURRENT must end with one newline"))?;
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err(invalid("CURRENT contains a path component"));
    }
    let generation = parse_generation_name(name)?;
    let snapshot_dir = root.join(GENERATIONS_DIR).join(name);
    if !snapshot_dir.is_dir() || !snapshot_dir.join("metadata.json").is_file() {
        return Err(invalid(format!(
            "CURRENT selects incomplete or missing generation {name}"
        )));
    }
    Ok(ResolvedSnapshot {
        logical_root: root.to_path_buf(),
        snapshot_dir,
        generation: Some(generation),
    })
}

/// Owned advisory writer lease. Readers do not take this lock: they resolve
/// `CURRENT` once and keep their immutable mmap generation alive.
#[derive(Debug)]
pub(crate) struct GraphDirectoryLock {
    file: std::sync::Mutex<Option<File>>,
    active: std::sync::atomic::AtomicBool,
    pub(crate) root: PathBuf,
}

#[derive(Debug)]
pub(crate) struct MutationWorkspace {
    root: PathBuf,
    segment: PathBuf,
}

/// Cleanup owner for the private writer root of an explicit graph copy.
///
/// Merely constructing this value does not touch the filesystem. The root is
/// materialised only when [`GraphDirectoryLock`] or [`MutationWorkspace`]
/// prepares the copy's first disk write. Ancestors keep earlier private roots
/// alive when an already-detached graph is copied again.
#[derive(Debug)]
pub(crate) struct IndependentGraphRoot {
    root: PathBuf,
    _ancestors: Vec<std::sync::Arc<IndependentGraphRoot>>,
}

impl IndependentGraphRoot {
    pub(crate) fn new(ancestors: Vec<std::sync::Arc<Self>>) -> Self {
        let nonce = NEXT_WORKSPACE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("kglite_copy_{}_{nonce:x}", std::process::id()));
        Self {
            root,
            _ancestors: ancestors,
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for IndependentGraphRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl MutationWorkspace {
    pub(crate) fn create(graph_root: &Path) -> io::Result<Self> {
        let nonce = NEXT_WORKSPACE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = graph_root.join(format!(".working-{}-{nonce:x}", std::process::id()));
        let segment = root.join("seg_000");
        fs::create_dir_all(&segment)?;
        Ok(Self { root, segment })
    }

    pub(crate) fn segment_dir(&self) -> &Path {
        &self.segment
    }
}

impl Drop for MutationWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl GraphDirectoryLock {
    pub(crate) fn is_active(&self) -> bool {
        self.active.load(std::sync::atomic::Ordering::Acquire)
    }

    /// A publish that already owns this permit finishes before authority ends.
    /// No caller may acquire a graph lock while holding this control lock.
    pub(crate) fn publication_permit(&self) -> io::Result<std::sync::MutexGuard<'_, Option<File>>> {
        let guard = self.file.lock().unwrap_or_else(|p| p.into_inner());
        if guard.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "disk writer ownership has ended",
            ));
        }
        Ok(guard)
    }

    pub(crate) fn end(&self) {
        let mut guard = self.file.lock().unwrap_or_else(|p| p.into_inner());
        self.active
            .store(false, std::sync::atomic::Ordering::Release);
        if let Some(file) = guard.take() {
            release_lock(&file);
        }
    }

    pub(crate) fn try_acquire(root: &Path) -> io::Result<Self> {
        fs::create_dir_all(root)?;
        let lock_path = root.join(".kglite.lock");
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        file.try_lock_exclusive().map_err(|error| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                format!(
                    "disk graph {} already has an active writer: {error}",
                    root.display()
                ),
            )
        })?;
        // Written for a human reading the file directly, and deliberately
        // never read back by this codebase. It cannot be: `fs2` locks via
        // `LockFileEx` on Windows, whose locks are mandatory, so any other
        // handle reading these bytes gets ERROR_LOCK_VIOLATION (33) rather
        // than the pid. Code that needs to *name* a lock holder must publish
        // the record to an unlocked sibling instead — see `GraphWriterLease`
        // in `graph/io/open.rs`, which had exactly this bug.
        //
        // Stamped through a closure so a failure here gives the lock back
        // through `release_lock` instead of by dropping the descriptor — the
        // release rule every other exit from a held lock follows.
        let stamped = (|| {
            file.set_len(0)?;
            writeln!(file, "pid={}", std::process::id())?;
            file.sync_all()
        })();
        if let Err(error) = stamped {
            release_lock(&file);
            return Err(error);
        }
        Ok(Self {
            file: std::sync::Mutex::new(Some(file)),
            active: std::sync::atomic::AtomicBool::new(true),
            root: root.to_path_buf(),
        })
    }
}

/// Give the OS lock back explicitly instead of leaving it to the descriptor's
/// close.
///
/// `flock` ownership belongs to the *open file description*, not to the
/// descriptor and not to the process, and a description outlives the last
/// `close` in this process whenever some other descriptor still refers to it.
/// `fork`/`posix_spawn` makes exactly that happen on every ordinary program:
/// the child receives a copy of the whole descriptor table, and `O_CLOEXEC`
/// closes those copies at **`exec`**, not at fork. A graph directory unlocked
/// by dropping its descriptor inside that window stays locked until the child
/// reaches `exec` — so a process that saves a disk graph while any thread
/// spawns a subprocess could have its own next mutation refused with "already
/// has an active writer" against a directory nothing else was writing.
/// `LOCK_UN` releases the description's lock outright and is not subject to
/// that race. `GraphWriterLease::drop` releases the `.kgl` lease the same way,
/// for the same reason.
fn release_lock(file: &File) {
    let _ = FileExt::unlock(file);
}

impl Drop for GraphDirectoryLock {
    fn drop(&mut self) {
        let mut guard = self.file.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(file) = guard.take() {
            release_lock(&file);
        }
    }
}

/// Generations some live value in this process still reads from.
static PINNED: std::sync::Mutex<std::collections::BTreeMap<PathBuf, usize>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

fn pinned_map() -> std::sync::MutexGuard<'static, std::collections::BTreeMap<PathBuf, usize>> {
    PINNED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The key a generation directory is pinned and looked up under: spelled the
/// same way however a handle reached it.
fn pin_key(generation_dir: &Path) -> PathBuf {
    fs::canonicalize(generation_dir).unwrap_or_else(|_| generation_dir.to_path_buf())
}

/// A claim that something in this process maps files of one generation.
///
/// Retention never deletes a pinned generation. Every value that can outlive
/// the save that replaced its generation carries one: the graph handle (and so
/// every transaction fork and independent copy cloned from it), the id index
/// and type index it serves from, and each column store mapped from the
/// generation's files. Dropping the last holder releases it, and the
/// generation is pruned by a later save.
#[derive(Debug)]
pub(crate) struct GenerationPin {
    key: PathBuf,
}

impl GenerationPin {
    /// A pin on the generation `path` lies in (a generation directory, or any
    /// file or directory beneath one); `None` for a path outside `generations/`
    /// — a legacy flat directory, a workspace, a scratch root — which retention
    /// never touches.
    pub(crate) fn containing(path: &Path) -> Option<std::sync::Arc<Self>> {
        let generation = path.ancestors().find(|ancestor| {
            ancestor.parent().and_then(|parent| parent.file_name())
                == Some(std::ffi::OsStr::new(GENERATIONS_DIR))
                && ancestor
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| parse_generation_name(name).is_ok())
        })?;
        let key = pin_key(generation);
        *pinned_map().entry(key.clone()).or_insert(0) += 1;
        Some(std::sync::Arc::new(Self { key }))
    }
}

impl Drop for GenerationPin {
    fn drop(&mut self) {
        let mut pinned = pinned_map();
        if let Some(count) = pinned.get_mut(&self.key) {
            *count -= 1;
            if *count == 0 {
                pinned.remove(&self.key);
            }
        }
    }
}

pub(crate) fn is_pinned(generation_dir: &Path) -> bool {
    pinned_map().contains_key(&pin_key(generation_dir))
}

/// Previous generations kept besides the current one when `KGLITE_KEEP_GENERATIONS`
/// does not say otherwise.
const DEFAULT_KEEP_PREVIOUS: usize = 1;

/// How many generations older than the current one a save keeps; `None` keeps
/// every one. Read from `KGLITE_KEEP_GENERATIONS`: a count, or `all`. A value
/// that is neither takes the default rather than deleting more than asked.
pub(crate) fn keep_previous_generations() -> Option<usize> {
    parse_keep_previous(std::env::var("KGLITE_KEEP_GENERATIONS").ok().as_deref())
}

fn parse_keep_previous(value: Option<&str>) -> Option<usize> {
    match value.map(str::trim) {
        Some(text) if text.eq_ignore_ascii_case("all") => None,
        Some(text) => Some(text.parse().unwrap_or(DEFAULT_KEEP_PREVIOUS)),
        None => Some(DEFAULT_KEEP_PREVIOUS),
    }
}

/// What a retention pass did.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct PruneReport {
    pub(crate) removed: Vec<u64>,
    /// Older than the kept window but still mapped by a live value here.
    pub(crate) held: Vec<u64>,
    /// Older than the kept window but refused removal (a platform that will
    /// not delete a mapped file, or another process's map).
    pub(crate) undeletable: Vec<u64>,
}

/// Delete generations older than the `keep_previous` newest ones below the one
/// `CURRENT` selects. A generation that cannot go now — pinned here, or the
/// platform refuses — stays, and the next save tries again; this never fails a
/// save. Files shared with a newer generation by hard link survive the unlink.
///
/// Must run while the writer lease is held, after `CURRENT` names the
/// generation just published.
pub(crate) fn prune_generations(root: &Path, keep_previous: Option<usize>) -> PruneReport {
    let mut report = PruneReport::default();
    let Some(keep_previous) = keep_previous else {
        return report;
    };
    let Ok(current) = resolve_snapshot(root) else {
        return report;
    };
    let Some(current_id) = current.generation else {
        return report;
    };
    let generations = root.join(GENERATIONS_DIR);
    let Ok(entries) = fs::read_dir(&generations) else {
        return report;
    };
    let mut older: Vec<(u64, PathBuf)> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| {
            let id = parse_generation_name(entry.file_name().to_str()?).ok()?;
            (id < current_id).then(|| (id, entry.path()))
        })
        .collect();
    older.sort_by_key(|(id, _)| std::cmp::Reverse(*id));
    for (id, dir) in older.into_iter().skip(keep_previous) {
        if is_pinned(&dir) {
            report.held.push(id);
        } else if fs::remove_dir_all(&dir).is_ok() {
            report.removed.push(id);
        } else {
            report.undeletable.push(id);
        }
    }
    report
}

#[derive(Debug)]
pub(crate) struct GenerationTxn {
    root: PathBuf,
    generations: PathBuf,
    stage: PathBuf,
    final_dir: PathBuf,
    name: String,
    /// The generation `CURRENT` selects now, which this one replaces.
    previous: Option<PathBuf>,
}

impl GenerationTxn {
    pub(crate) fn begin(root: &Path) -> io::Result<Self> {
        fs::create_dir_all(root)?;
        let generations = root.join(GENERATIONS_DIR);
        fs::create_dir_all(&generations)?;
        for entry in fs::read_dir(&generations)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(STAGE_PREFIX) && entry.file_type()?.is_dir() {
                fs::remove_dir_all(entry.path())?;
            }
        }
        let current = resolve_snapshot(root)?;
        let mut max_id = current.generation.unwrap_or(0);
        for entry in fs::read_dir(&generations)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            if let Some(name) = entry.file_name().to_str() {
                if let Ok(id) = parse_generation_name(name) {
                    max_id = max_id.max(id);
                }
            }
        }
        let id = max_id
            .checked_add(1)
            .ok_or_else(|| invalid("generation counter exhausted"))?;
        let name = generation_name(id);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let stage = generations.join(format!("{STAGE_PREFIX}{}-{nonce:x}", std::process::id()));
        fs::create_dir(&stage)?;
        let final_dir = generations.join(&name);
        Ok(Self {
            root: root.to_path_buf(),
            generations,
            stage,
            final_dir,
            name,
            previous: current.generation.map(|_| current.snapshot_dir),
        })
    }

    pub(crate) fn stage_dir(&self) -> &Path {
        &self.stage
    }

    /// The published generation this one replaces; `None` for a directory with
    /// no `CURRENT` yet. Its files are immutable, so a save may link them into
    /// the stage.
    pub(crate) fn previous_snapshot(&self) -> Option<&Path> {
        self.previous.as_deref()
    }

    pub(crate) fn publish(self) -> io::Result<PathBuf> {
        if !self.stage.join("metadata.json").is_file()
            || !self.stage.join("disk_graph_meta.json").is_file()
        {
            return Err(invalid("generation stage is missing completion metadata"));
        }
        sync_tree(&self.stage)?;
        publish_failpoint("before_generation_rename")?;
        fs::rename(&self.stage, &self.final_dir)?;
        sync_directory(&self.generations)?;
        publish_failpoint("after_generation_rename")?;

        let mut pointer = tempfile::NamedTempFile::new_in(&self.root)?;
        writeln!(pointer, "{}", self.name)?;
        pointer.flush()?;
        pointer.as_file().sync_all()?;
        publish_failpoint("before_current_replace")?;
        pointer
            .persist(self.root.join(CURRENT_FILE))
            .map_err(|e| e.error)?;
        publish_failpoint("after_current_replace")?;
        sync_directory(&self.root)?;
        Ok(self.final_dir)
    }
}

fn sync_tree(root: &Path) -> io::Result<()> {
    let mut dirs = Vec::new();
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = entry.map_err(io::Error::other)?;
        if entry.file_type().is_file() {
            // `sync_all` is `FlushFileBuffers` on Windows, which MSDN
            // documents as requiring `GENERIC_WRITE` on the handle: fsyncing
            // a staged file through a read-only handle fails there with
            // `ERROR_ACCESS_DENIED`, aborting `publish` before it ever
            // reaches the rename. Open the file for writing so the durability
            // fsync is portable — the stage directory is owned exclusively by
            // this writer, so write access is always grantable.
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(entry.path())?
                .sync_all()?;
        } else if entry.file_type().is_dir() {
            dirs.push(entry.path().to_path_buf());
        }
    }
    for dir in dirs.into_iter().rev() {
        sync_directory(&dir)?;
    }
    Ok(())
}

#[cfg(unix)]
pub(super) fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
pub(super) fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inject(stage: &'static str, action: impl FnOnce()) {
        PUBLISH_FAILPOINT.with(|point| point.set(Some(stage)));
        action();
        PUBLISH_FAILPOINT.with(|point| point.set(None));
    }

    fn complete_stage(txn: &GenerationTxn) {
        fs::write(txn.stage_dir().join("metadata.json"), b"{}").unwrap();
        fs::write(txn.stage_dir().join("disk_graph_meta.json"), b"{}").unwrap();
    }

    #[test]
    fn a_pin_holds_a_generation_until_every_holder_is_gone() {
        let root = tempfile::tempdir().unwrap();
        let generation = root.path().join(GENERATIONS_DIR).join(generation_name(3));
        let segment = generation.join("seg_000");
        fs::create_dir_all(&segment).unwrap();
        assert!(!is_pinned(&generation));

        // A path anywhere beneath the generation pins the generation itself.
        let first = GenerationPin::containing(&segment.join("x.bin")).unwrap();
        let second = GenerationPin::containing(&generation).unwrap();
        assert!(is_pinned(&generation));
        drop(first);
        assert!(is_pinned(&generation), "one holder remains");
        drop(second);
        assert!(!is_pinned(&generation));

        // Outside `generations/gen_<id>` there is nothing to pin.
        assert!(GenerationPin::containing(root.path()).is_none());
        assert!(GenerationPin::containing(&root.path().join("seg_000")).is_none());
        let stage = root.path().join(GENERATIONS_DIR).join(".stage-1");
        assert!(GenerationPin::containing(&stage).is_none());
    }

    #[test]
    fn the_retention_setting_reads_a_count_or_all_and_defaults_otherwise() {
        assert_eq!(parse_keep_previous(None), Some(1));
        assert_eq!(parse_keep_previous(Some("0")), Some(0));
        assert_eq!(parse_keep_previous(Some(" 4 ")), Some(4));
        assert_eq!(parse_keep_previous(Some("ALL")), None);
        assert_eq!(parse_keep_previous(Some("-1")), Some(1));
        assert_eq!(parse_keep_previous(Some("lots")), Some(1));
        assert_eq!(parse_keep_previous(Some("")), Some(1));
    }

    #[test]
    fn legacy_and_current_resolution_are_strict() {
        let root = tempfile::tempdir().unwrap();
        let legacy = resolve_snapshot(root.path()).unwrap();
        assert_eq!(legacy.snapshot_dir, root.path());
        assert_eq!(legacy.generation, None);

        fs::write(root.path().join(CURRENT_FILE), "../escape\n").unwrap();
        assert_eq!(
            resolve_snapshot(root.path()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn publish_selects_only_complete_generation_and_cleans_stages() {
        let root = tempfile::tempdir().unwrap();
        let lock = GraphDirectoryLock::try_acquire(root.path()).unwrap();
        let abandoned = root.path().join(GENERATIONS_DIR).join(".stage-old");
        fs::create_dir_all(&abandoned).unwrap();
        fs::write(abandoned.join("junk"), b"x").unwrap();

        let txn = GenerationTxn::begin(root.path()).unwrap();
        assert!(!abandoned.exists());
        complete_stage(&txn);
        let published = txn.publish().unwrap();
        let resolved = resolve_snapshot(root.path()).unwrap();
        assert_eq!(resolved.snapshot_dir, published);
        assert_eq!(resolved.generation, Some(1));
        drop(lock);
    }

    #[test]
    fn second_writer_is_rejected_until_first_drops() {
        let root = tempfile::tempdir().unwrap();
        let first = GraphDirectoryLock::try_acquire(root.path()).unwrap();
        assert_eq!(
            GraphDirectoryLock::try_acquire(root.path())
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        drop(first);
        GraphDirectoryLock::try_acquire(root.path()).unwrap();
    }

    /// The eager publish that disk-mode *creation* runs is the same
    /// all-or-nothing transaction as any other. A failure part-way through it
    /// must leave the directory in exactly the state creation used to leave —
    /// no `CURRENT`, no generation — and never a `CURRENT` selecting a
    /// half-written one.
    #[test]
    fn a_failed_initial_publish_leaves_no_pointer_and_no_generation() {
        use crate::graph::storage::mode::{new_dir_graph_in_mode, StorageMode};

        for stage in [
            "before_generation_rename",
            "after_generation_rename",
            "before_current_replace",
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().join("graph");
            inject(stage, || {
                assert!(
                    new_dir_graph_in_mode(StorageMode::Disk, Some(&root)).is_err(),
                    "{stage}: a failed initial publish must fail the create"
                );
            });
            assert!(
                !root.join(CURRENT_FILE).exists(),
                "{stage}: no pointer may be published"
            );
            assert_eq!(
                resolve_snapshot(&root).unwrap().generation,
                None,
                "{stage}: nothing is selectable"
            );
        }
    }

    #[test]
    fn failures_before_pointer_keep_old_and_after_pointer_select_new() {
        let root = tempfile::tempdir().unwrap();
        let _lock = GraphDirectoryLock::try_acquire(root.path()).unwrap();
        let first = GenerationTxn::begin(root.path()).unwrap();
        complete_stage(&first);
        first.publish().unwrap();

        for stage in [
            "before_generation_rename",
            "after_generation_rename",
            "before_current_replace",
        ] {
            let before = resolve_snapshot(root.path()).unwrap().generation;
            let txn = GenerationTxn::begin(root.path()).unwrap();
            complete_stage(&txn);
            inject(stage, || assert!(txn.publish().is_err()));
            assert_eq!(resolve_snapshot(root.path()).unwrap().generation, before);
        }

        let before = resolve_snapshot(root.path()).unwrap().generation.unwrap();
        let txn = GenerationTxn::begin(root.path()).unwrap();
        complete_stage(&txn);
        inject("after_current_replace", || assert!(txn.publish().is_err()));
        assert!(resolve_snapshot(root.path()).unwrap().generation.unwrap() > before);
    }
}
