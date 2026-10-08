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
        assert_eq!(
            policy.resolve("/var/backups/x.kgl").unwrap(),
            PathBuf::from("/var/backups/x.kgl")
        );
        assert!(policy.resolve("../x.kgl").is_err());
        let bare = BackupPolicy::from_flags(None, true, false).unwrap();
        assert_eq!(
            bare.resolve("rel/x.kgl").unwrap(),
            PathBuf::from("rel/x.kgl")
        );
    }
}
