//! Per-connection guards around `boltr` 0.2.0's connection handler.
//!
//! `boltr` 0.2.0 has three defects a client can reach before it is
//! authenticated, and two that cost resources afterwards. The accept loop closes them from the
//! outside, because `Connection::run` is the only public entry point:
//!
//! - **Authentication bypass.** A failed LOGON leaves the connection in
//!   `Failed`, and RESET moves `Failed` to `Ready` without authenticating.
//!   [`GuardedBackend`] refuses every query, transaction and routing call
//!   until `set_session_auth` has run (which `boltr` calls only after the
//!   validator accepted the credentials), and [`GuardedValidator`] marks the
//!   connection for closing on the first rejected LOGON.
//! - **Unbounded PackStream nesting** overflows the decoder's stack, which
//!   aborts the whole process. [`GuardedReader`] follows the message bytes
//!   and ends the connection before a message nested deeper than
//!   [`MAX_NESTING_DEPTH`] reaches the decoder.
//! - **Memory use before authentication.** A decoded value costs far more
//!   than its wire size, so a client that has not authenticated may send at
//!   most [`MAX_UNAUTHENTICATED_MESSAGE`] bytes per message.
//! - **Idle reaping of active sessions.** Only RUN refreshes a session's idle
//!   timer, so a client paging through a result with PULL is reaped
//!   mid-stream. [`GuardedReader`] refreshes it on every message.
//! - **Session leak.** When a write fails mid-response `Connection::run`
//!   returns without closing the backend session, which then holds a
//!   `--max-sessions` slot and any open transaction (and the writer slot)
//!   for good. [`serve_connection`] closes every session the connection
//!   created, however `run` ended.
//!
//! A violation is answered with a FAILURE written after `run` returns, then
//! the write side is shut down and input is drained so the client reads the
//! FAILURE instead of a connection reset.

use std::collections::HashMap;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use crate::discard::{DiscardTracker, SIG_DISCARD, SIG_PULL};
use boltr::chunk::ChunkWriter;
use boltr::error::BoltError;
use boltr::message::encode::encode_server_message;
use boltr::message::response::ServerMessage;
use boltr::server::connection::Connection;
use boltr::server::{
    AuthCredentials, AuthInfo, AuthValidator, BoltBackend, RoutingTable, SessionConfig,
    SessionHandle, SessionManager, SessionProperty, TransactionHandle,
};
use boltr::types::{BoltDict, BoltValue};
use bytes::BytesMut;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

/// Most non-empty containers a message may have open at once, the message
/// structure itself included. Matches the limit `boltr` 0.2.1 enforces.
pub const MAX_NESTING_DEPTH: usize = 128;

/// Largest message accepted from a client that has not authenticated.
pub const MAX_UNAUTHENTICATED_MESSAGE: usize = 64 * 1024;

/// How long to keep reading a client's input after a refusal, so the
/// refusal is not lost to a connection reset.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
/// Bytes read and discarded while draining.
const DRAIN_LIMIT: usize = 1024 * 1024;

const SIG_LOGON: u8 = 0x6A;

/// Why a connection is being closed by the guards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    TooDeep,
    TooLargeBeforeAuth { size: usize },
}

impl Violation {
    fn message(&self) -> String {
        match self {
            Self::TooDeep => {
                format!("message nesting exceeds {MAX_NESTING_DEPTH} levels")
            }
            Self::TooLargeBeforeAuth { .. } => {
                format!("message exceeds {MAX_UNAUTHENTICATED_MESSAGE} bytes before authentication")
            }
        }
    }
}

/// State shared by one connection's reader, validator and backend wrapper.
pub struct ConnGuard {
    require_auth: bool,
    authenticated: AtomicBool,
    /// A LOGON was rejected or an unauthenticated session tried to use the
    /// backend: end the connection at the next read.
    closing: AtomicBool,
    violation: Mutex<Option<Violation>>,
    sessions: Mutex<Vec<SessionHandle>>,
    /// Set when an idle timeout is configured: every message then refreshes
    /// the session's idle timer, not only RUN as in `boltr` 0.2.0.
    activity: Option<Arc<SessionManager>>,
}

impl ConnGuard {
    pub fn new(require_auth: bool, activity: Option<Arc<SessionManager>>) -> Arc<Self> {
        Arc::new(Self {
            activity,
            require_auth,
            authenticated: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            violation: Mutex::new(None),
            sessions: Mutex::new(Vec::new()),
        })
    }

    fn touch_session(&self) {
        let Some(sessions) = &self.activity else {
            return;
        };
        let last = self
            .sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .last()
            .cloned();
        if let Some(session) = last {
            sessions.touch(&session.0);
        }
    }

    fn is_authenticated(&self) -> bool {
        self.authenticated.load(Ordering::Acquire)
    }

    fn record_violation(&self, v: Violation) {
        let mut slot = self.violation.lock().unwrap_or_else(|p| p.into_inner());
        slot.get_or_insert(v);
        self.closing.store(true, Ordering::Release);
    }

    fn take_violation(&self) -> Option<Violation> {
        self.violation
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
    }

    /// Refuse a backend call from a session that has not authenticated.
    fn require_authenticated(&self) -> Result<(), BoltError> {
        if self.require_auth && !self.is_authenticated() {
            self.closing.store(true, Ordering::Release);
            return Err(BoltError::Authentication(
                "authentication required: send LOGON with valid credentials".into(),
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// PackStream scanner
// ---------------------------------------------------------------------------

/// What the next byte of a PackStream value is.
#[derive(Clone, Copy)]
enum Expect {
    Marker,
    /// Payload bytes of a scalar or string/bytes body still to skip.
    Skip(u64),
    /// Big-endian length bytes of a string/bytes/list/map header.
    Length {
        left: u8,
        acc: u64,
        kind: LenKind,
    },
    /// The signature byte of a structure with `fields` fields.
    Signature {
        fields: u64,
    },
    /// A marker `boltr` will reject; nothing more to scan in this message.
    Opaque,
}

#[derive(Clone, Copy)]
enum LenKind {
    Body,
    List,
    Map,
}

/// Follows PackStream containers across arbitrary read boundaries without
/// decoding values, tracking only how deep they nest.
struct NestingScanner {
    expect: Expect,
    /// Items still to come in each open container, innermost last.
    open: Vec<u64>,
}

impl NestingScanner {
    fn new() -> Self {
        Self {
            expect: Expect::Marker,
            open: Vec::new(),
        }
    }

    fn reset(&mut self) {
        self.expect = Expect::Marker;
        self.open.clear();
    }

    /// Scan message bytes; `false` when the nesting limit is exceeded.
    fn feed(&mut self, mut bytes: &[u8]) -> bool {
        while !bytes.is_empty() {
            match self.expect {
                Expect::Opaque => return true,
                Expect::Skip(n) => {
                    let take = n.min(bytes.len() as u64);
                    bytes = &bytes[take as usize..];
                    if take == n {
                        self.expect = Expect::Marker;
                        self.finish_value();
                    } else {
                        self.expect = Expect::Skip(n - take);
                    }
                }
                Expect::Length { left, acc, kind } => {
                    let acc = (acc << 8) | u64::from(bytes[0]);
                    bytes = &bytes[1..];
                    if left > 1 {
                        self.expect = Expect::Length {
                            left: left - 1,
                            acc,
                            kind,
                        };
                    } else {
                        self.expect = Expect::Marker;
                        if !self.sized(kind, acc) {
                            return false;
                        }
                    }
                }
                Expect::Signature { fields } => {
                    bytes = &bytes[1..];
                    self.expect = Expect::Marker;
                    if !self.open_container(fields) {
                        return false;
                    }
                }
                Expect::Marker => {
                    let marker = bytes[0];
                    bytes = &bytes[1..];
                    if !self.marker(marker) {
                        return false;
                    }
                }
            }
        }
        true
    }

    /// Handle a body/list/map whose size is now known.
    fn sized(&mut self, kind: LenKind, n: u64) -> bool {
        match kind {
            LenKind::Body => {
                if n == 0 {
                    self.finish_value();
                } else {
                    self.expect = Expect::Skip(n);
                }
                true
            }
            LenKind::List => self.open_container(n),
            LenKind::Map => self.open_container(n.saturating_mul(2)),
        }
    }

    fn length(&mut self, bytes: u8, kind: LenKind) {
        self.expect = Expect::Length {
            left: bytes,
            acc: 0,
            kind,
        };
    }

    fn marker(&mut self, marker: u8) -> bool {
        match marker {
            0x00..=0x7F | 0xF0..=0xFF | 0xC0 | 0xC2 | 0xC3 => self.finish_value(),
            0xC1 | 0xCB => self.expect = Expect::Skip(8),
            0xC8 => self.expect = Expect::Skip(1),
            0xC9 => self.expect = Expect::Skip(2),
            0xCA => self.expect = Expect::Skip(4),
            0x80..=0x8F => return self.sized(LenKind::Body, u64::from(marker & 0x0F)),
            0xD0 | 0xCC => self.length(1, LenKind::Body),
            0xD1 | 0xCD => self.length(2, LenKind::Body),
            0xD2 | 0xCE => self.length(4, LenKind::Body),
            0x90..=0x9F => return self.sized(LenKind::List, u64::from(marker & 0x0F)),
            0xD4 => self.length(1, LenKind::List),
            0xD5 => self.length(2, LenKind::List),
            0xD6 => self.length(4, LenKind::List),
            0xA0..=0xAF => return self.sized(LenKind::Map, u64::from(marker & 0x0F)),
            0xD8 => self.length(1, LenKind::Map),
            0xD9 => self.length(2, LenKind::Map),
            0xDA => self.length(4, LenKind::Map),
            0xB0..=0xBF => {
                self.expect = Expect::Signature {
                    fields: u64::from(marker & 0x0F),
                }
            }
            _ => self.expect = Expect::Opaque,
        }
        true
    }

    fn open_container(&mut self, items: u64) -> bool {
        if items == 0 {
            self.finish_value();
            return true;
        }
        if self.open.len() >= MAX_NESTING_DEPTH {
            return false;
        }
        self.open.push(items);
        true
    }

    /// A complete value ends: count it against its parent, closing parents
    /// that are now full.
    fn finish_value(&mut self) {
        while let Some(left) = self.open.last_mut() {
            *left -= 1;
            if *left > 0 {
                return;
            }
            self.open.pop();
        }
    }
}

// ---------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------

/// Follows Bolt chunk framing on the way in. Hands bytes to `boltr` until a
/// message breaks a limit, then reports end of input.
pub struct GuardedReader<R> {
    inner: R,
    guard: Arc<ConnGuard>,
    ended: bool,
    header: [u8; 2],
    header_len: usize,
    chunk_remaining: usize,
    message_len: usize,
    /// First two data bytes of the message: struct marker, signature.
    lead: [u8; 2],
    scanner: NestingScanner,
    discards: Arc<DiscardTracker>,
    /// The message being read is a DISCARD, rewritten to PULL.
    is_discard: bool,
}

impl<R: AsyncRead + Unpin> GuardedReader<R> {
    pub fn new(inner: R, guard: Arc<ConnGuard>, discards: Arc<DiscardTracker>) -> Self {
        Self {
            inner,
            guard,
            ended: false,
            header: [0; 2],
            header_len: 0,
            chunk_remaining: 0,
            message_len: 0,
            lead: [0; 2],
            scanner: NestingScanner::new(),
            discards,
            is_discard: false,
        }
    }

    /// Scan freshly read bytes; the violation, if any. A DISCARD's signature
    /// is rewritten to PULL in place (see `discard.rs`).
    fn inspect(&mut self, mut bytes: &mut [u8]) -> Option<Violation> {
        while !bytes.is_empty() {
            if self.chunk_remaining == 0 {
                self.header[self.header_len] = bytes[0];
                self.header_len += 1;
                bytes = &mut bytes[1..];
                if self.header_len == 2 {
                    self.header_len = 0;
                    let len = u16::from_be_bytes(self.header) as usize;
                    if len == 0 {
                        self.end_message();
                    } else {
                        self.chunk_remaining = len;
                    }
                }
                continue;
            }
            let take = self.chunk_remaining.min(bytes.len());
            let (data, rest) = bytes.split_at_mut(take);
            bytes = rest;
            self.chunk_remaining -= take;
            for (i, byte) in data.iter_mut().take(2).enumerate() {
                if self.message_len + i == 1 && self.lead[0] == 0xB1 && *byte == SIG_DISCARD {
                    *byte = SIG_PULL;
                    self.is_discard = true;
                }
                if self.message_len + i < 2 {
                    self.lead[self.message_len + i] = *byte;
                }
            }
            self.message_len += take;
            if !self.guard.is_authenticated() && self.message_len > MAX_UNAUTHENTICATED_MESSAGE {
                return Some(Violation::TooLargeBeforeAuth {
                    size: self.message_len,
                });
            }
            if !self.scanner.feed(data) {
                return Some(Violation::TooDeep);
            }
        }
        None
    }

    fn end_message(&mut self) {
        // Without a validator `boltr` accepts any LOGON; with one, only the
        // backend's `set_session_auth` lifts the pre-authentication limit.
        if !self.guard.require_auth && self.message_len >= 2 && self.lead[1] == SIG_LOGON {
            self.guard.authenticated.store(true, Ordering::Release);
        }
        self.guard.touch_session();
        if self.message_len >= 2 {
            self.discards.request_arrived(self.lead[1], self.is_discard);
        }
        self.is_discard = false;
        self.message_len = 0;
        self.scanner.reset();
    }

    /// Read and discard input for a short while (see [`DRAIN_TIMEOUT`]).
    async fn drain(&mut self) {
        let mut sink = [0u8; 8192];
        let mut total = 0usize;
        let read_all = async {
            while total < DRAIN_LIMIT {
                match self.inner.read(&mut sink).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => total += n,
                }
            }
        };
        let _ = tokio::time::timeout(DRAIN_TIMEOUT, read_all).await;
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for GuardedReader<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        if this.ended || this.guard.closing.load(Ordering::Acquire) {
            this.ended = true;
            return Poll::Ready(Ok(()));
        }
        let before = buf.filled().len();
        match Pin::new(&mut this.inner).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                if let Some(violation) = this.inspect(&mut buf.filled_mut()[before..]) {
                    buf.set_filled(before);
                    this.ended = true;
                    this.guard.record_violation(violation);
                }
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

// ---------------------------------------------------------------------------
// Validator and backend wrappers
// ---------------------------------------------------------------------------

/// Marks the connection for closing when LOGON credentials are rejected.
pub struct GuardedValidator {
    inner: Arc<dyn AuthValidator>,
    guard: Arc<ConnGuard>,
}

impl GuardedValidator {
    pub fn new(inner: Arc<dyn AuthValidator>, guard: Arc<ConnGuard>) -> Self {
        Self { inner, guard }
    }
}

#[async_trait::async_trait]
impl AuthValidator for GuardedValidator {
    async fn validate(&self, credentials: &AuthCredentials) -> Result<AuthInfo, BoltError> {
        let result = self.inner.validate(credentials).await;
        if result.is_err() {
            self.guard.closing.store(true, Ordering::Release);
        }
        result
    }
}

/// The server's backend as seen by one connection.
pub struct GuardedBackend<B> {
    inner: Arc<B>,
    guard: Arc<ConnGuard>,
}

impl<B> GuardedBackend<B> {
    pub fn new(inner: Arc<B>, guard: Arc<ConnGuard>) -> Self {
        Self { inner, guard }
    }
}

#[async_trait::async_trait]
impl<B: BoltBackend> BoltBackend for GuardedBackend<B> {
    async fn create_session(&self, config: &SessionConfig) -> Result<SessionHandle, BoltError> {
        let handle = self.inner.create_session(config).await?;
        self.guard
            .sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(handle.clone());
        Ok(handle)
    }

    async fn set_session_auth(
        &self,
        session: &SessionHandle,
        auth_info: AuthInfo,
    ) -> Result<(), BoltError> {
        self.inner.set_session_auth(session, auth_info).await?;
        self.guard.authenticated.store(true, Ordering::Release);
        Ok(())
    }

    async fn close_session(&self, session: &SessionHandle) -> Result<(), BoltError> {
        self.inner.close_session(session).await
    }

    async fn configure_session(
        &self,
        session: &SessionHandle,
        property: SessionProperty,
    ) -> Result<(), BoltError> {
        self.guard.require_authenticated()?;
        self.inner.configure_session(session, property).await
    }

    async fn reset_session(&self, session: &SessionHandle) -> Result<(), BoltError> {
        self.inner.reset_session(session).await
    }

    async fn execute(
        &self,
        session: &SessionHandle,
        query: &str,
        parameters: &HashMap<String, BoltValue>,
        extra: &BoltDict,
        transaction: Option<&TransactionHandle>,
    ) -> Result<boltr::server::ResultStream, BoltError> {
        self.guard.require_authenticated()?;
        self.inner
            .execute(session, query, parameters, extra, transaction)
            .await
    }

    async fn begin_transaction(
        &self,
        session: &SessionHandle,
        extra: &BoltDict,
    ) -> Result<TransactionHandle, BoltError> {
        self.guard.require_authenticated()?;
        self.inner.begin_transaction(session, extra).await
    }

    async fn commit(
        &self,
        session: &SessionHandle,
        transaction: &TransactionHandle,
    ) -> Result<BoltDict, BoltError> {
        self.guard.require_authenticated()?;
        self.inner.commit(session, transaction).await
    }

    async fn rollback(
        &self,
        session: &SessionHandle,
        transaction: &TransactionHandle,
    ) -> Result<(), BoltError> {
        self.inner.rollback(session, transaction).await
    }

    async fn get_server_info(&self) -> Result<BoltDict, BoltError> {
        self.inner.get_server_info().await
    }

    async fn route(
        &self,
        routing_context: &BoltDict,
        bookmarks: &[String],
        db: Option<&str>,
    ) -> Result<RoutingTable, BoltError> {
        self.guard.require_authenticated()?;
        self.inner.route(routing_context, bookmarks, db).await
    }
}

// ---------------------------------------------------------------------------
// Connection driver
// ---------------------------------------------------------------------------

/// What every connection of a listener shares.
pub struct ConnectionContext<B> {
    pub backend: Arc<B>,
    pub sessions: Arc<SessionManager>,
    pub auth: Option<Arc<dyn AuthValidator>>,
    pub max_message_size: usize,
    /// An idle timeout is configured, so messages must refresh the timer.
    pub idle_timeout: bool,
}

/// Run one handshaken connection under the guards.
///
/// The writer is `boltr`'s output half; the reader is the raw input half.
/// `discards` is shared with the writer (see `discard.rs`).
pub async fn serve_connection<R, W, B>(
    reader: R,
    mut writer: W,
    discards: Arc<DiscardTracker>,
    ctx: &ConnectionContext<B>,
    peer_addr: std::net::SocketAddr,
) where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    B: BoltBackend,
{
    let ConnectionContext {
        backend,
        sessions,
        auth,
        max_message_size,
        idle_timeout,
    } = ctx;
    let guard = ConnGuard::new(auth.is_some(), idle_timeout.then(|| sessions.clone()));
    let guarded_backend = Arc::new(GuardedBackend::new(backend.clone(), guard.clone()));
    let guarded_auth = auth.clone().map(|inner| {
        Arc::new(GuardedValidator::new(inner, guard.clone())) as Arc<dyn AuthValidator>
    });
    let mut reader = GuardedReader::new(reader, guard.clone(), discards);

    {
        let mut conn = Connection::new(
            &mut reader,
            &mut writer,
            guarded_backend,
            sessions.clone(),
            guarded_auth,
            peer_addr,
            Some(*max_message_size),
        );
        if let Err(e) = conn.run().await {
            tracing::debug!(%peer_addr, error = %e, "Bolt connection closed");
        }
    }

    // `Connection::run` skips its own cleanup when a write fails.
    let created: Vec<SessionHandle> =
        std::mem::take(&mut *guard.sessions.lock().unwrap_or_else(|p| p.into_inner()));
    for session in created {
        sessions.remove(&session.0);
        let _ = backend.close_session(&session).await;
    }

    if let Some(violation) = guard.take_violation() {
        tracing::debug!(%peer_addr, ?violation, "refusing Bolt message");
        let mut frame = BytesMut::new();
        encode_server_message(
            &mut frame,
            &ServerMessage::Failure {
                metadata: failure_metadata(&violation),
            },
        );
        let _ = ChunkWriter::new(&mut writer).write_message(&frame).await;
        let _ = writer.flush().await;
    }
    if guard.closing.load(Ordering::Acquire) {
        let _ = writer.shutdown().await;
        reader.drain().await;
    }
}

fn failure_metadata(violation: &Violation) -> HashMap<String, BoltValue> {
    HashMap::from([
        (
            "code".to_string(),
            BoltValue::String("Neo.ClientError.Request.Invalid".into()),
        ),
        (
            "message".to_string(),
            BoltValue::String(violation.message()),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(bytes: &[u8]) -> bool {
        NestingScanner::new().feed(bytes)
    }

    fn nested_lists(depth: usize) -> Vec<u8> {
        let mut v = vec![0x91; depth - 1];
        v.push(0x90);
        v
    }

    #[test]
    fn nesting_at_the_limit_passes_and_one_deeper_fails() {
        // The message structure is level 1; the innermost empty list is a
        // leaf and opens no level.
        let mut ok = vec![0xB1, 0x01];
        ok.extend(nested_lists(MAX_NESTING_DEPTH));
        assert!(scan(&ok));
        let mut deep = vec![0xB1, 0x01];
        deep.extend(nested_lists(MAX_NESTING_DEPTH + 1));
        assert!(!scan(&deep));
    }

    #[test]
    fn scanner_agrees_across_every_split_point() {
        let mut deep = vec![0xB1, 0x01];
        deep.extend(nested_lists(MAX_NESTING_DEPTH + 5));
        for split in 1..deep.len() {
            let mut s = NestingScanner::new();
            let (a, b) = deep.split_at(split);
            assert!(!(s.feed(a) && s.feed(b)), "split at {split}");
        }
    }

    #[test]
    fn string_bodies_that_look_like_markers_are_skipped() {
        // A 20-byte string of 0x91 bytes is data, not 20 nested lists.
        let mut msg = vec![0xB1, 0x01, 0xD0, 20];
        msg.extend(vec![0x91; 20]);
        msg.extend([0x91; 4]);
        msg.push(0x90);
        assert!(scan(&msg));
    }

    #[test]
    fn sibling_containers_do_not_accumulate_depth() {
        // 1000 empty lists in a list: depth 2.
        let mut msg = vec![0xD5, 0x03, 0xE8];
        msg.extend(vec![0x90; 1000]);
        assert!(scan(&msg));
    }

    #[test]
    fn maps_count_keys_and_values() {
        // {"a": [[]]} then a sibling int: map items = 2.
        let msg = [0xA1, 0x81, b'a', 0x91, 0x90];
        let mut s = NestingScanner::new();
        assert!(s.feed(&msg));
        assert!(s.open.is_empty());
    }

    /// A DISCARD is read as a PULL, and the writer is told it was a DISCARD.
    #[tokio::test]
    async fn discard_is_rewritten_to_pull_and_recorded() {
        let guard = ConnGuard::new(false, None);
        let tracker = DiscardTracker::new();
        // DISCARD {n: -1}, PULL {n: 1}, GOODBYE, each as one chunk.
        let wire: Vec<u8> = [
            vec![0x00, 0x05, 0xB1, 0x2F, 0xA1, 0x81, b'n', 0x00, 0x00],
            vec![0x00, 0x03, 0xB1, 0x3F, 0xA0, 0x00, 0x00],
            vec![0x00, 0x02, 0xB0, 0x02, 0x00, 0x00],
        ]
        .concat();
        // The first message above is malformed on purpose in its length only
        // for brevity; only the leading bytes matter to the adapter.
        let mut reader = GuardedReader::new(&wire[..], guard, tracker.clone());
        let mut out = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut out)
            .await
            .unwrap();
        assert_eq!(out[3], SIG_PULL, "DISCARD reads as PULL");
        assert!(tracker.answering_discard());
        tracker.response_closed();
        assert!(!tracker.answering_discard(), "the real PULL stays a PULL");
        tracker.response_closed();
        assert!(!tracker.answering_discard(), "GOODBYE queued nothing");
    }
}
