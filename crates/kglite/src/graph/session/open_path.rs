//! `open_path`: take write ownership of a graph path, open or create the graph
//! in the requested storage mode, and wrap it in a session logging at the
//! requested durability level.
//!
//! The order is the point. The writer lease is taken *before* the graph is
//! read, because acquiring it afterwards is a guard that lets the very race it
//! exists to stop happen first. And durability travels into both the open and
//! the session for one reason, recovery: the open is told a log is coming so
//! it does not refuse a sidecar holding commits the checkpoint lacks, and
//! [`Session::open_durable`] then replays exactly those frames.
//!
//! No async, no logging framework: what a server would log comes back as data
//! ([`OpenedSession::advisories`], [`OpenedSession::degraded_from`]).

use std::fmt;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use super::transaction::Session;
use crate::graph::advisories::{data_advisories, DataAdvisory};
use crate::graph::features::temporal::ValidTimeDefault;
use crate::graph::io::open::{
    open_or_create_graph_in_mode, GraphWriterLease, LeaseRefusal, OpenDisposition,
};
use crate::graph::storage::mode::{live_storage_mode, StorageMode};
use crate::graph::wal::DurabilityLevel;

/// What to open and how.
#[derive(Debug, Clone)]
pub struct OpenSpec {
    /// Create a missing graph in this mode, and convert an existing one to it.
    /// `None` lets the checkpoint decide. A request with no conversion (either
    /// disk direction) fails the open rather than serving another mode.
    pub storage: Option<StorageMode>,
    /// The write-ahead-log level the session logs at.
    pub durability: DurabilityLevel,
    /// Whether `durability` was asked for or inherited from a default. An
    /// *inherited* logging level on a disk-mode graph (which has no logical
    /// log) degrades to `off` instead of failing; an *explicit* one is the
    /// engine's error.
    pub durability_explicit: bool,
    /// How long to wait for another writer to release the path. `None` takes
    /// no lease at all, for a reader that publishes nothing and so must not
    /// exclude a writer. `Some(Duration::ZERO)` fails fast.
    pub lease_timeout: Option<Duration>,
    /// Runtime-only valid-time default applied before the session wraps the
    /// graph.
    pub valid_time_default: Option<ValidTimeDefault>,
}

impl OpenSpec {
    /// Writer creating a missing graph in memory, `off` durability, failing
    /// fast on a held lease. The shape tests and simple embedders start from.
    pub fn writer() -> Self {
        Self {
            storage: Some(StorageMode::Memory),
            durability: DurabilityLevel::Off,
            durability_explicit: true,
            lease_timeout: Some(Duration::ZERO),
            valid_time_default: None,
        }
    }
}

/// The ordered steps [`open_path_observed`] completes, reported to an injected
/// observer so a test can assert the lease is taken *before* the graph is read
/// instead of inferring it from a timing race between two processes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenStep {
    LeaseAcquired,
    GraphOpened,
    SessionOpened,
}

/// A session ready to serve, plus the write ownership it depends on.
pub struct OpenedSession {
    pub session: Session,
    /// `None` when [`OpenSpec::lease_timeout`] was `None`. Otherwise held for
    /// as long as the session serves; its `Drop` releases the lease.
    pub lease: Option<GraphWriterLease>,
    /// The level the session actually logs at. Equal to the requested level
    /// except where a default was degraded; serve and report *this* one.
    pub durability: DurabilityLevel,
    /// The requested level, when it was degraded to `off` (an inherited logging
    /// level on a disk-mode graph). `None` when nothing was degraded.
    pub degraded_from: Option<DurabilityLevel>,
    /// The mode the graph is actually in, read while the open still owns the
    /// graph alone (a snapshot held across a checkpoint would turn its
    /// copy-on-write into a deep clone of the whole graph).
    pub live_mode: StorageMode,
    /// The mode the graph was in before `storage` converted it; `None` when
    /// nothing was converted.
    pub converted_from: Option<StorageMode>,
    pub disposition: OpenDisposition,
    /// A quarantined log or a saved torn tail the open reported
    /// (`wal_quarantined` / `wal_tail_saved`); an operator should read them
    /// before any client connects.
    pub advisories: Vec<DataAdvisory>,
}

/// Which step of [`open_path`] failed.
#[derive(Debug)]
pub enum OpenError {
    /// The writer lease could not be taken. On contention the refusal carries
    /// the holder structured (`pid`, `since`, `label`) so a binding can
    /// re-render it without parsing the message.
    Lease(LeaseRefusal),
    /// The graph could not be opened, created or converted.
    Open(io::Error),
    /// The session (and its write-ahead log) could not be opened.
    Session {
        durability: DurabilityLevel,
        message: String,
    },
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OpenError::Lease(refusal) => refusal.error.fmt(f),
            OpenError::Open(e) => e.fmt(f),
            OpenError::Session { message, .. } => f.write_str(message),
        }
    }
}

impl std::error::Error for OpenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            OpenError::Lease(refusal) => Some(&refusal.error),
            OpenError::Open(e) => Some(e),
            OpenError::Session { .. } => None,
        }
    }
}

/// Take write ownership of `path` (unless the spec asks for none), open or
/// create the graph in `spec.storage`, and wrap it in a session logging at
/// `spec.durability`.
///
/// A `storage` conversion is safe under a log because it is performed in
/// memory, before the session exists: the `.kgl` on disk, and therefore the
/// `checkpoint_lsn` replay is gated on, is untouched until the first
/// checkpoint, which writes the converted graph and truncates the log together.
pub fn open_path(path: &Path, spec: &OpenSpec) -> Result<OpenedSession, OpenError> {
    open_path_observed(path, spec, &mut |_| {})
}

/// [`open_path`], reporting each completed [`OpenStep`] to `observe`.
pub fn open_path_observed(
    path: &Path,
    spec: &OpenSpec,
    observe: &mut dyn FnMut(OpenStep),
) -> Result<OpenedSession, OpenError> {
    let lease = match spec.lease_timeout {
        None => None,
        Some(timeout) => {
            let lease = GraphWriterLease::acquire_ex(path, timeout).map_err(OpenError::Lease)?;
            observe(OpenStep::LeaseAcquired);
            Some(lease)
        }
    };
    let mut opened = open_or_create_graph_in_mode(path, spec.storage, spec.durability)
        .map_err(OpenError::Open)?;
    observe(OpenStep::GraphOpened);
    let live_mode = live_storage_mode(&opened.graph);
    // The one refusal that becomes a degrade: a disk graph has no logical WAL
    // at any level. Decided here because this is the first point where the
    // *live* mode is known: `storage` may have converted, and a graph opened
    // without it reports whatever it was saved in.
    let (durability, degraded_from) =
        if spec.durability.logs() && !spec.durability_explicit && live_mode == StorageMode::Disk {
            (DurabilityLevel::Off, Some(spec.durability))
        } else {
            (spec.durability, None)
        };
    if let Some(default) = spec.valid_time_default {
        // `opened.graph` is still the only reference, so this mutates in place.
        if let Some(graph) = Arc::get_mut(&mut opened.graph) {
            graph.valid_time_default = default;
        }
    }
    // `opened.graph` is the only reference, which `open_durable` requires: a
    // shared Arc would be deep-cloned and the other holder would keep mutating
    // an unlogged copy.
    let session = Session::open_durable(opened.graph, &path.to_string_lossy(), durability)
        .map_err(|message| OpenError::Session {
            durability,
            message,
        })?;
    observe(OpenStep::SessionOpened);
    let advisories = data_advisories(&session.snapshot())
        .into_iter()
        .filter(|a| a.code == "wal_quarantined" || a.code == "wal_tail_saved")
        .collect();
    Ok(OpenedSession {
        session,
        lease,
        durability,
        degraded_from,
        live_mode,
        converted_from: opened.converted_from,
        disposition: opened.disposition,
        advisories,
    })
}
