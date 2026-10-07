//! The two filesystem steps the durable path shares: syncing a directory so a
//! create or rename survives a power cut, and a test-only record of the order
//! in which the durable path syncs and renames.

use std::io;
use std::path::Path;

/// `fsync` a directory, making the entries created or renamed in it durable.
///
/// A failure is returned: a checkpoint whose rename may not be durable must not
/// go on to truncate the log, and a log whose directory entry may not be
/// durable must not acknowledge commits. The one tolerated answer is "this
/// filesystem cannot sync a directory" (`EINVAL`/`ENOTSUP`), which no retry
/// fixes. Windows has no directory handles to sync.
pub(crate) fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        let _ = dir;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let dir = if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        };
        match std::fs::File::open(dir)?.sync_all() {
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::InvalidInput | io::ErrorKind::Unsupported
                ) =>
            {
                Ok(())
            }
            other => other,
        }
    }
}

/// Order of the durable path's syncs and renames, recorded per thread.
#[cfg(test)]
pub(crate) mod trace {
    use std::cell::RefCell;

    thread_local! {
        static EVENTS: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
    }

    pub(crate) fn record(event: impl FnOnce() -> String) {
        EVENTS.with(|events| {
            if let Some(events) = events.borrow_mut().as_mut() {
                events.push(event());
            }
        });
    }

    /// Run `f` and return what the durable path recorded while it ran.
    pub(crate) fn capture<R>(f: impl FnOnce() -> R) -> (R, Vec<String>) {
        EVENTS.with(|events| *events.borrow_mut() = Some(Vec::new()));
        let result = f();
        let events = EVENTS.with(|events| events.borrow_mut().take().unwrap_or_default());
        (result, events)
    }
}

/// Production builds record nothing.
#[cfg(not(test))]
pub(crate) mod trace {
    #[inline(always)]
    pub(crate) fn record(_event: impl FnOnce() -> String) {}
}
