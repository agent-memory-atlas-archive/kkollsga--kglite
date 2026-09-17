//! In-flight save temps: the name they take, and reaping the ones a crash
//! left behind.
//!
//! Split out of `file.rs` (module-directory pattern, §"god files"): the
//! naming half is one function, the reaping half is its exact inverse plus
//! the liveness probe, and nothing else in the save path reads either.

use std::path::Path;
use std::time::{Duration, SystemTime};

/// The `<name>.kgl.tmp.` prefix every in-flight save writes under, for `dest`.
///
/// Single source of truth for the shape: [`write_kgl_with`] appends
/// `<pid>.<nonce>` to it, and [`reap_stale_save_temps`] parses the same two
/// fields back out. A drift between the two would turn the reaper into a
/// silent no-op — the failure mode it exists to fix.
pub(crate) fn save_temp_prefix(dest: &Path) -> String {
    format!(
        "{}.tmp.",
        dest.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "graph.kgl".to_string()),
    )
}

/// How long a temp whose owner cannot be identified is kept before it is
/// treated as abandoned.
///
/// Only reached where process liveness is unavailable (non-Unix, or a `kill`
/// that answers neither "alive" nor "gone"). Long enough that no plausible
/// save is still running, short enough that the litter does not accumulate
/// across a machine's lifetime.
const UNIDENTIFIED_TEMP_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Delete `<name>.tmp.<pid>.<nonce>` siblings of `path` whose writing process
/// is gone, and report how many were removed.
///
/// [`write_kgl_with`] removes its own temp on every error path it can see, but
/// it cannot see the one that matters: a `SIGKILL`, an OOM-kill or a power cut
/// part-way through a save leaves a **full-size** copy of the graph beside the
/// real file, and nothing else deletes it — a crash-looping writer over a
/// multi-GB graph fills the volume with copies of itself.
///
/// **Nothing a live process could still be writing is touched.** A temp is
/// removed only when its embedded pid is provably not a running process (or,
/// where that cannot be established, when the file has not been touched for
/// [`UNIDENTIFIED_TEMP_MAX_AGE`]); this process's own pid is always skipped,
/// since a concurrent save on another thread is exactly the in-flight case.
/// Pid reuse can only make the reaper *keep* a file it could have deleted,
/// which leaks a temp rather than destroying a live save.
///
/// Best-effort: nothing depends on the reap having happened, so an unreadable
/// directory or a failed unlink is not reported.
pub fn reap_stale_save_temps(path: &Path) -> usize {
    let prefix = save_temp_prefix(path);
    let dir = match path.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(d) => d.to_path_buf(),
        None => Path::new(".").to_path_buf(),
    };
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(_) => return 0,
    };
    let mut reaped = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(pid) = temp_owner_pid(name, &prefix) else {
            continue;
        };
        if pid == std::process::id() {
            continue;
        }
        let abandoned = match process_is_alive(pid) {
            Some(alive) => !alive,
            None => temp_is_older_than(&entry, UNIDENTIFIED_TEMP_MAX_AGE),
        };
        if abandoned && std::fs::remove_file(entry.path()).is_ok() {
            reaped += 1;
        }
    }
    reaped
}

/// The pid embedded in `name`, when `name` is a save temp of `prefix`'s graph.
///
/// Both trailing fields must parse: a file that merely *starts* with the
/// prefix (`app.kgl.tmp.notes`) is somebody else's, and deleting it because it
/// shared a prefix would be the reaper causing the data loss it prevents.
fn temp_owner_pid(name: &str, prefix: &str) -> Option<u32> {
    let rest = name.strip_prefix(prefix)?;
    let (pid, nonce) = rest.split_once('.')?;
    nonce.parse::<u64>().ok()?;
    let pid: u32 = pid.parse().ok()?;
    (pid > 0).then_some(pid)
}

/// Whether `entry` was last modified longer than `max_age` ago. An
/// unreadable timestamp answers "no" — the reaper's default is always to keep.
fn temp_is_older_than(entry: &std::fs::DirEntry, max_age: Duration) -> bool {
    entry
        .metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age > max_age)
}

/// Whether process `pid` exists, or `None` when this platform cannot say.
///
/// `kill(pid, 0)` sends no signal — it performs only the existence and
/// permission checks — so it is the standard liveness probe. `EPERM` means the
/// process exists and belongs to another user, which is still "alive"; `ESRCH`
/// is the only answer that licenses a delete.
#[cfg(unix)]
fn process_is_alive(pid: u32) -> Option<bool> {
    // SAFETY: `kill` with signal 0 performs no action beyond the existence and
    // permission check, and `pid > 0` (checked by `temp_owner_pid`) keeps it
    // from addressing a process *group*.
    if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
        return Some(true);
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::ESRCH) => Some(false),
        Some(libc::EPERM) => Some(true),
        _ => None,
    }
}

/// Non-Unix platforms have no cheap equivalent, so the age fallback owns the
/// decision there.
#[cfg(not(unix))]
fn process_is_alive(_pid: u32) -> Option<bool> {
    None
}
