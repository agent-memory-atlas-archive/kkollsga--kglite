//! WAL durability policy types.

/// What a committed mutation is guaranteed to survive — the durability
/// vocabulary a binding exposes to its users. Deliberately mirrors SQLite's
/// `synchronous` levels (`FULL` / `NORMAL` / `OFF`), because the audience for
/// an embedded database already knows that vocabulary and the guarantees line
/// up.
///
/// The levels are stated in terms of *what survives*, not in terms of which
/// syscall runs, because the syscall differs by platform while the guarantee
/// does not. That is also why there is no separate "plain `fsync`" level: on
/// Linux `fsync` is the power-loss barrier, while on macOS it is not (only
/// `F_FULLFSYNC` flushes the drive cache), so such a level could not be given
/// one honest description.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DurabilityLevel {
    /// No write-ahead log. Nothing survives beyond the caller's most recent
    /// `save()` checkpoint.
    Off,
    /// Log every commit, but do not barrier. An acknowledged mutation
    /// survives the **process** dying — `SIGKILL`, an unhandled panic, an
    /// OOM-kill — because the frame is already in the kernel's page cache.
    /// An OS crash or power loss loses commits made since the last `save()`.
    Normal,
    /// Log every commit and barrier before returning. An acknowledged
    /// mutation survives **power loss**. The default, and the strongest
    /// guarantee the platform offers.
    #[default]
    Full,
}

impl DurabilityLevel {
    /// Whether this level writes a WAL at all.
    #[inline]
    pub fn logs(self) -> bool {
        !matches!(self, Self::Off)
    }

    /// How the WAL should make each frame durable, or `None` when this level
    /// keeps no log. Total by construction, so a new level cannot be added
    /// without deciding its sync behaviour.
    #[inline]
    pub fn sync_mode(self) -> Option<SyncMode> {
        match self {
            Self::Off => None,
            Self::Normal => Some(SyncMode::PageCache),
            Self::Full => Some(SyncMode::Barrier),
        }
    }

    /// The level named by a binding-facing string (`"full"` / `"normal"` /
    /// `"off"`), or `None` if unrecognised. Shared by every binding so the
    /// vocabulary cannot drift between them; the caller owns the error type
    /// and message.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "full" => Some(Self::Full),
            "normal" => Some(Self::Normal),
            "off" => Some(Self::Off),
            _ => None,
        }
    }

    /// The canonical name of this level, the inverse of [`Self::from_name`].
    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Normal => "normal",
            Self::Full => "full",
        }
    }

    /// Every accepted level name, for error messages that need to list them.
    pub const NAMES: [&'static str; 3] = ["full", "normal", "off"];
}

/// How [`Wal::append`] makes a frame durable. Derived from a
/// [`DurabilityLevel`] via [`DurabilityLevel::sync_mode`]; separate from it so
/// that "no log at all" is unrepresentable on an open WAL file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncMode {
    /// Barrier after every frame — `append` returns only once the bytes are
    /// on stable storage. On Apple targets this is `fcntl(F_FULLFSYNC)`;
    /// elsewhere it is `fdatasync`/`fsync`.
    Barrier,
    /// Hand the frame to the OS and return. Bytes are in the kernel page
    /// cache, which outlives the process but not the kernel.
    PageCache,
}
