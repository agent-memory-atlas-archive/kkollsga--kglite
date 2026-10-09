//! Online backup for the Bolt server: the path policy, the one-at-a-time gate,
//! and the blocking call that writes the file.
//!
//! [`BackupService::run_blocking`] is the single entry point. The
//! `CALL db.backup(...)` verb reaches it through [`BackupService::resolve`] and
//! `spawn_blocking`; any scheduler calls the same function, so both routes share
//! the path policy, the alias refusal and the single in-flight slot.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use kglite::api::session::{BackupOptions, BackupReport, Session};

/// Where a client-supplied backup name may point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BackupPolicy {
    /// `db.backup` is refused: no `--backup-dir` was configured.
    Disabled,
    /// Bare file names, written inside this canonical directory.
    Dir(PathBuf),
    /// `--backup-allow-any-path`: the client's path is used as given. A bare
    /// name resolves inside `dir` when `--backup-dir` is also set.
    AnyPath { dir: Option<PathBuf> },
}

impl BackupPolicy {
    /// Build the policy from the startup flags.
    ///
    /// `--backup-allow-any-path` hands remote callers a file-write primitive
    /// anywhere this process can write, so it is refused outright under
    /// `--auth none`, where anyone who can connect is a caller.
    pub(crate) fn from_flags(
        dir: Option<&Path>,
        allow_any_path: bool,
        auth_is_none: bool,
    ) -> Result<Self, String> {
        if allow_any_path && auth_is_none {
            return Err(
                "--backup-allow-any-path is refused with --auth none: it lets any \
                        client that can connect write backup files to any path this server \
                        can write. Start the server with --auth basic, or drop the flag and \
                        use --backup-dir"
                    .to_string(),
            );
        }
        let dir = dir
            .map(|dir| {
                std::fs::create_dir_all(dir)
                    .and_then(|()| dir.canonicalize())
                    .map_err(|e| format!("--backup-dir {}: {e}", dir.display()))
            })
            .transpose()?;
        Ok(match (allow_any_path, dir) {
            (true, dir) => Self::AnyPath { dir },
            (false, Some(dir)) => Self::Dir(dir),
            (false, None) => Self::Disabled,
        })
    }

    /// The directory backups are confined to, when one was configured.
    pub(crate) fn dir(&self) -> Option<&Path> {
        match self {
            Self::Disabled => None,
            Self::Dir(dir) => Some(dir),
            Self::AnyPath { dir } => dir.as_deref(),
        }
    }

    /// Resolve the client's `name` to the destination path, or say why not.
    pub(crate) fn resolve(&self, name: &str) -> Result<PathBuf, String> {
        match self {
            Self::Disabled => Err("db.backup() is disabled: start the server with \
                                   --backup-dir <DIR> to let clients write backups into DIR"
                .to_string()),
            Self::Dir(dir) => {
                validate_bare_name(name)?;
                Ok(dir.join(name))
            }
            Self::AnyPath { dir } => {
                if name.is_empty() {
                    return Err("backup name is empty".to_string());
                }
                if name.contains('\0') {
                    return Err("backup name contains a NUL byte".to_string());
                }
                let path = Path::new(name);
                // Rooted without a drive (`\x.kgl`, Windows only) resolves against the
                // current drive of whichever process runs, so it names no one place.
                if path.has_root() && !path.is_absolute() {
                    return Err(format!(
                        "invalid backup name {name:?}: a path with a root but no drive is \
                         ambiguous; give a full path such as 'C:\\backups\\x.kgl'"
                    ));
                }
                match dir {
                    Some(dir) if !path.is_absolute() => {
                        validate_bare_name(name)?;
                        Ok(dir.join(name))
                    }
                    _ => Ok(path.to_path_buf()),
                }
            }
        }
    }
}

/// A bare file name: no separator of either kind, no `..`, not absolute, not
/// empty. Checked on the text rather than on parsed components so a
/// backslash is refused on every platform, not only where it separates.
fn validate_bare_name(name: &str) -> Result<(), String> {
    let refuse = |why: &str| {
        Err(format!(
            "invalid backup name {name:?}: {why}; db.backup() takes a bare file name such as \
             'nightly.kgl', written inside the server's --backup-dir"
        ))
    };
    if name.is_empty() {
        return refuse("the name is empty");
    }
    if name.contains('/') || name.contains('\\') {
        return refuse("path separators are not allowed");
    }
    if name.contains("..") {
        return refuse("'..' is not allowed");
    }
    if name.contains('\0') {
        return refuse("the name contains a NUL byte");
    }
    if name == "." || Path::new(name).is_absolute() || Path::new(name).has_root() {
        return refuse("the name must not be absolute");
    }
    Ok(())
}

/// Why a backup did not happen.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum BackupError {
    /// Another backup is writing; retry when it finishes.
    Busy,
    /// Declined before anything was written (alias, stray log, disk mode).
    Refused(String),
    /// The write itself failed.
    Failed(String),
}

/// Releases the in-flight slot on drop, including on panic.
struct InFlight<'a>(&'a AtomicBool);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// The served session, its checkpoint path and the policy, plus the gate that
/// allows one backup at a time. Cheap to clone: every clone shares the gate.
#[derive(Clone)]
pub(crate) struct BackupService {
    session: Arc<Session>,
    live_path: PathBuf,
    policy: BackupPolicy,
    busy: Arc<AtomicBool>,
}

impl BackupService {
    pub(crate) fn new(session: Arc<Session>, live_path: PathBuf, policy: BackupPolicy) -> Self {
        Self {
            session,
            live_path,
            policy,
            busy: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn with_policy(mut self, policy: BackupPolicy) -> Self {
        self.policy = policy;
        self
    }

    #[cfg(test)]
    pub(crate) fn hold_for_test(&self) {
        self.busy.store(true, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn release_for_test(&self) {
        self.busy.store(false, Ordering::Release);
    }

    /// The served graph's current version, for the scheduler's unchanged check.
    pub(crate) fn graph_version(&self) -> u64 {
        self.session.version()
    }

    /// Resolve a client-supplied name under the policy.
    pub(crate) fn resolve(&self, name: &str) -> Result<PathBuf, String> {
        self.policy.resolve(name)
    }

    /// Write a backup of the served graph to `dest`, blocking the calling
    /// thread for the serialize. Fails with [`BackupError::Busy`] instead of
    /// queueing when another backup holds the gate.
    pub(crate) fn run_blocking(&self, dest: &Path) -> Result<BackupReport, BackupError> {
        if self
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(BackupError::Busy);
        }
        let _slot = InFlight(&self.busy);
        let opts = BackupOptions {
            live_path: Some(self.live_path.clone()),
        };
        self.session.backup(dest, &opts).map_err(|e| match e {
            kglite::api::io::SaveError::Refused(m) => BackupError::Refused(m),
            kglite::api::io::SaveError::Io(m) => BackupError::Failed(m),
        })
    }
}

/// Validate `--backup-interval` / `--backup-keep` against `--backup-dir`.
pub(crate) fn validate_schedule(
    interval: Option<Duration>,
    keep: Option<usize>,
    dir: Option<&Path>,
) -> Result<(), String> {
    if dir.is_none() {
        let flag = match (interval, keep) {
            (Some(_), _) => "--backup-interval",
            (None, Some(_)) => "--backup-keep",
            (None, None) => return Ok(()),
        };
        return Err(format!(
            "{flag} requires --backup-dir: scheduled backups are written into that directory"
        ));
    }
    if keep.is_some() && interval.is_none() {
        return Err(
            "--backup-keep requires --backup-interval: retention only prunes the \
                    files the scheduler writes"
                .to_string(),
        );
    }
    Ok(())
}

/// Parse `--backup-interval`: a whole number of seconds, at least 1.
pub(crate) fn parse_backup_interval(raw: &str) -> Result<Duration, String> {
    let trimmed = raw.trim();
    match trimmed.parse::<u64>() {
        Ok(0) => Err(
            "invalid backup interval 0: the interval must be at least 1 second; \
                      omit --backup-interval to disable scheduled backups"
                .to_string(),
        ),
        Ok(secs) => Ok(Duration::from_secs(secs)),
        Err(_) => Err(format!(
            "invalid backup interval {trimmed:?}: expected a whole number of seconds \
             (for example 3600)"
        )),
    }
}

/// Parse `--backup-keep`: a whole number, at least 1 (0 would delete every backup).
pub(crate) fn parse_backup_keep(raw: &str) -> Result<usize, String> {
    let trimmed = raw.trim();
    match trimmed.parse::<usize>() {
        Ok(0) => Err(
            "invalid --backup-keep 0: at least 1 backup must be kept; omit the \
                      flag to keep every backup"
                .to_string(),
        ),
        Ok(n) => Ok(n),
        Err(_) => Err(format!(
            "invalid --backup-keep {trimmed:?}: expected a whole number (for example 7)"
        )),
    }
}

const STAMP_LEN: usize = 16; // YYYYMMDDTHHMMSSZ

/// UTC `YYYYMMDDTHHMMSSZ` for `secs` since the Unix epoch (Hinnant's
/// civil-from-days; the crate's chrono has no clock or formatting features).
pub(crate) fn utc_stamp(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

/// `<stem>-YYYYMMDDTHHMMSSZ.kgl`: lexicographic order is chronological order.
pub(crate) fn scheduled_name(stem: &str, secs: u64) -> String {
    format!("{stem}-{}.kgl", utc_stamp(secs))
}

/// Whether `name` is exactly a scheduler-written file for `stem`.
pub(crate) fn is_scheduled_name(stem: &str, name: &str) -> bool {
    let Some(rest) = name
        .strip_prefix(stem)
        .and_then(|r| r.strip_prefix('-'))
        .and_then(|r| r.strip_suffix(".kgl"))
    else {
        return false;
    };
    let b = rest.as_bytes();
    b.len() == STAMP_LEN
        && b[8] == b'T'
        && b[15] == b'Z'
        && b[..8].iter().chain(&b[9..15]).all(u8::is_ascii_digit)
}

/// The scheduler-written files in `names` that fall outside the newest `keep`.
/// Anything not matching the scheduler's own pattern is never returned.
pub(crate) fn files_to_prune(stem: &str, names: &[String], keep: usize) -> Vec<String> {
    let mut mine: Vec<&String> = names
        .iter()
        .filter(|n| is_scheduled_name(stem, n))
        .collect();
    mine.sort();
    let excess = mine.len().saturating_sub(keep);
    mine.into_iter().take(excess).cloned().collect()
}

fn prune_dir(dir: &Path, stem: &str, keep: usize) {
    let names: Vec<String> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .collect(),
        Err(e) => {
            tracing::error!(dir = %dir.display(), error = %e, "backup-interval: cannot list the backup dir to prune");
            return;
        }
    };
    for name in files_to_prune(stem, &names, keep) {
        match std::fs::remove_file(dir.join(&name)) {
            Ok(()) => tracing::info!(file = %name, keep, "backup-interval: pruned old backup"),
            Err(e) => tracing::error!(file = %name, error = %e, "backup-interval: prune FAILED"),
        }
    }
}

/// Spawn the `--backup-interval` task. The first backup lands one interval in;
/// a tick is skipped when the graph version equals that of the last successful
/// scheduled backup, or when a manual backup holds the gate. Failures are
/// logged and the loop continues.
pub(crate) fn spawn_scheduler(
    service: BackupService,
    dir: PathBuf,
    stem: String,
    interval: Duration,
    keep: Option<usize>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await;
        let mut last_version: Option<u64> = None;
        loop {
            ticker.tick().await;
            let version = service.graph_version();
            if last_version == Some(version) {
                tracing::debug!(
                    graph_version = version,
                    "backup-interval: skipped (graph unchanged since the last scheduled backup)"
                );
                continue;
            }
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let dest = dir.join(scheduled_name(&stem, secs));
            let svc = service.clone();
            let started = std::time::Instant::now();
            let result = tokio::task::spawn_blocking(move || svc.run_blocking(&dest)).await;
            match result {
                Ok(Ok(report)) => {
                    tracing::info!(
                        path = %report.path.display(),
                        graph_version = report.graph_version,
                        lsn = ?report.lsn,
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        "backup-interval: backup written"
                    );
                    last_version = Some(report.graph_version);
                    if let Some(keep) = keep {
                        let (dir, stem) = (dir.clone(), stem.clone());
                        let _ =
                            tokio::task::spawn_blocking(move || prune_dir(&dir, &stem, keep)).await;
                    }
                }
                Ok(Err(BackupError::Busy)) => {
                    tracing::info!("backup-interval: skipped (another backup is in progress)")
                }
                Ok(Err(e)) => tracing::error!(
                    error = ?e,
                    "backup-interval: backup FAILED — the server keeps serving"
                ),
                Err(e) => tracing::error!(error = %e, "backup-interval: backup task failed"),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir_policy() -> (tempfile_dir::Dir, BackupPolicy) {
        let dir = tempfile_dir::Dir::new("policy");
        let policy = BackupPolicy::from_flags(Some(dir.path()), false, false).unwrap();
        (dir, policy)
    }

    /// Minimal self-cleaning temp dir (the crate has no tempfile dependency).
    mod tempfile_dir {
        use std::path::{Path, PathBuf};
        pub struct Dir(PathBuf);
        impl Dir {
            pub fn new(tag: &str) -> Self {
                let nonce = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                let path = std::env::temp_dir().join(format!(
                    "kglite-bolt-backup-{tag}-{}-{nonce}",
                    std::process::id()
                ));
                std::fs::create_dir_all(&path).unwrap();
                Dir(path)
            }
            pub fn path(&self) -> &Path {
                &self.0
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    #[test]
    fn backup_name_inside_dir_resolves_under_the_canonical_dir() {
        let (dir, policy) = dir_policy();
        let resolved = policy.resolve("nightly.kgl").unwrap();
        assert_eq!(
            resolved,
            dir.path().canonicalize().unwrap().join("nightly.kgl")
        );
    }

    #[test]
    fn backup_name_refuses_traversal_separators_and_absolute_paths() {
        let (_dir, policy) = dir_policy();
        for bad in [
            "",
            ".",
            "..",
            "../x.kgl",
            "a/../x.kgl",
            "sub/x.kgl",
            "sub\\x.kgl",
            "/etc/passwd",
            "\\x.kgl",
            "..x.kgl",
            "x\0.kgl",
        ] {
            assert!(policy.resolve(bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn backup_without_backup_dir_is_refused_with_the_enabling_flag() {
        let policy = BackupPolicy::from_flags(None, false, false).unwrap();
        let err = policy.resolve("x.kgl").unwrap_err();
        assert!(err.contains("--backup-dir"), "{err}");
    }

    #[test]
    fn backup_allow_any_path_is_refused_at_startup_under_auth_none() {
        let err = BackupPolicy::from_flags(None, true, true).unwrap_err();
        assert!(err.contains("--backup-allow-any-path"), "{err}");
        assert!(err.contains("--auth none"), "{err}");
        assert!(BackupPolicy::from_flags(None, true, false).is_ok());
    }

    #[test]
    fn backup_allow_any_path_takes_absolute_paths_but_dir_names_stay_bare() {
        let (dir, _) = dir_policy();
        let policy = BackupPolicy::from_flags(Some(dir.path()), true, false).unwrap();
        let absolute = std::env::temp_dir().join("x.kgl");
        assert!(absolute.is_absolute());
        assert_eq!(
            policy.resolve(absolute.to_str().unwrap()).unwrap(),
            absolute
        );
        assert!(policy.resolve("../x.kgl").is_err());
        let bare = BackupPolicy::from_flags(None, true, false).unwrap();
        assert_eq!(
            bare.resolve("rel/x.kgl").unwrap(),
            PathBuf::from("rel/x.kgl")
        );
    }

    #[cfg(windows)]
    #[test]
    fn backup_allow_any_path_refuses_a_rooted_path_without_a_drive() {
        let (dir, _) = dir_policy();
        for policy in [
            BackupPolicy::from_flags(Some(dir.path()), true, false).unwrap(),
            BackupPolicy::from_flags(None, true, false).unwrap(),
        ] {
            let err = policy.resolve("\\foo\\x.kgl").unwrap_err();
            assert!(err.contains("no drive"), "{err}");
        }
    }

    #[test]
    fn utc_stamp_formats_known_instants() {
        assert_eq!(utc_stamp(0), "19700101T000000Z");
        assert_eq!(utc_stamp(1_760_000_000), "20251009T085320Z");
        assert_eq!(utc_stamp(951_782_400 + 86_399), "20000229T235959Z");
    }

    #[test]
    fn scheduled_names_round_trip_and_reject_lookalikes() {
        let name = scheduled_name("graph", 1_760_000_000);
        assert_eq!(name, "graph-20251009T085320Z.kgl");
        assert!(is_scheduled_name("graph", &name));
        for bad in [
            "graph.kgl",
            "nightly.kgl",
            "graph-20251009T085320Z.kgl.tmp",
            "graph-20251009T080000.kgl",
            "graph-2025100XT080000Z.kgl",
            "other-20251009T085320Z.kgl",
            "graph-20251009T085320Z-extra.kgl",
            ".graph-20251009T085320Z.kgl",
        ] {
            assert!(!is_scheduled_name("graph", bad), "{bad}");
        }
    }

    #[test]
    fn retention_keeps_newest_n_and_ignores_foreign_files() {
        let names: Vec<String> = [
            "graph-20250101T000000Z.kgl",
            "graph-20250103T000000Z.kgl",
            "graph-20250102T000000Z.kgl",
            "graph-20250104T000000Z.kgl",
            "manual.kgl",
            "notes.txt",
            "other-20240101T000000Z.kgl",
        ]
        .map(String::from)
        .to_vec();
        let mut pruned = files_to_prune("graph", &names, 2);
        pruned.sort();
        assert_eq!(
            pruned,
            ["graph-20250101T000000Z.kgl", "graph-20250102T000000Z.kgl"]
        );
        assert!(files_to_prune("graph", &names, 4).is_empty());
    }

    #[test]
    fn schedule_flags_require_backup_dir() {
        let secs = Some(Duration::from_secs(5));
        assert!(validate_schedule(secs, None, None)
            .unwrap_err()
            .contains("--backup-dir"));
        assert!(validate_schedule(None, Some(3), None)
            .unwrap_err()
            .contains("--backup-dir"));
        let dir = Path::new("/x");
        assert!(validate_schedule(None, Some(3), Some(dir))
            .unwrap_err()
            .contains("--backup-interval"));
        assert!(validate_schedule(secs, Some(3), Some(dir)).is_ok());
        assert!(validate_schedule(None, None, None).is_ok());
    }

    #[test]
    fn interval_and_keep_parsers_refuse_zero_and_junk() {
        assert_eq!(parse_backup_interval(" 30 "), Ok(Duration::from_secs(30)));
        assert!(parse_backup_interval("0").is_err());
        assert!(parse_backup_interval("1h").is_err());
        assert_eq!(parse_backup_keep("3"), Ok(3));
        assert!(parse_backup_keep("0").is_err());
        assert!(parse_backup_keep("-1").is_err());
    }
}
