//! Reads a connection's input independently of `boltr`, so a RESET or a dropped
//! connection is seen while a query is still running.
//!
//! `boltr` reads one message, handles it to completion, then reads the next.
//! During a long RUN nothing polls the socket, so neither a RESET queued behind
//! the RUN nor a peer that vanished is noticed until the query ends. A pump task
//! owns the real reader instead: it forwards every byte to `boltr` unchanged
//! (through a bounded channel) and watches the framing for two signals:
//!
//! - a RESET message: Bolt's "interrupt the current work";
//! - end of input or a read error that was not preceded by GOODBYE: the peer
//!   dropped, so nobody will read the answer.
//!
//! Either calls the `on_interrupt` callback. The bytes still reach `boltr` in
//! order, so it answers the interrupted RUN with FAILURE and then runs the
//! RESET as usual. An orderly GOODBYE does not interrupt: a client that
//! pipelines `RUN, PULL, GOODBYE` and half-closes still gets its statement run.
//!
//! The channel is bounded, so a client that floods input while a query runs
//! stalls the pump once the buffer is full; signals behind that buffer are seen
//! when `boltr` drains it.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};
use tokio::sync::mpsc;
use tokio::task::AbortHandle;
use tokio::time::Instant;

const SIG_RESET: u8 = 0x0F;
const SIG_GOODBYE: u8 = 0x02;
const SIG_LOGON: u8 = 0x6A;
const SIG_LOGOFF: u8 = 0x6B;
/// Bytes per read, and the channel depth: at most `READ_SIZE * DEPTH` input
/// bytes are buffered ahead of `boltr`.
const READ_SIZE: usize = 8192;
const DEPTH: usize = 32;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Signal {
    Reset,
    Goodbye,
    /// A LOGON message (it has an auth map, so more than the two lead bytes).
    Logon,
    Logoff,
}

/// How long a connection may stay unauthenticated.
#[derive(Clone, Copy)]
pub struct AuthWindow {
    /// When the first LOGON must have arrived.
    pub first: Instant,
    /// How long after a LOGOFF the next LOGON may take.
    pub repeat: Duration,
}

/// Follows chunk framing and reports the control messages that matter here.
#[derive(Default)]
pub(crate) struct FrameWatch {
    header: [u8; 2],
    header_len: usize,
    chunk_remaining: usize,
    message_len: usize,
    lead: [u8; 2],
}

impl FrameWatch {
    pub(crate) fn feed(&mut self, mut bytes: &[u8], out: &mut Vec<Signal>) {
        while !bytes.is_empty() {
            if self.chunk_remaining == 0 {
                self.header[self.header_len] = bytes[0];
                self.header_len += 1;
                bytes = &bytes[1..];
                if self.header_len == 2 {
                    self.header_len = 0;
                    let len = u16::from_be_bytes(self.header) as usize;
                    if len == 0 {
                        self.end_message(out);
                    } else {
                        self.chunk_remaining = len;
                    }
                }
                continue;
            }
            let take = self.chunk_remaining.min(bytes.len());
            for (i, byte) in bytes[..take].iter().take(2).enumerate() {
                if self.message_len + i < 2 {
                    self.lead[self.message_len + i] = *byte;
                }
            }
            self.message_len += take;
            self.chunk_remaining -= take;
            bytes = &bytes[take..];
        }
    }

    fn end_message(&mut self, out: &mut Vec<Signal>) {
        // RESET, GOODBYE and LOGOFF are fieldless structures: `B0 <signature>`.
        if self.message_len == 2 && self.lead[0] == 0xB0 {
            match self.lead[1] {
                SIG_RESET => out.push(Signal::Reset),
                SIG_GOODBYE => out.push(Signal::Goodbye),
                SIG_LOGOFF => out.push(Signal::Logoff),
                _ => {}
            }
        } else if self.message_len > 2 && self.lead[0] == 0xB1 && self.lead[1] == SIG_LOGON {
            out.push(Signal::Logon);
        }
        self.message_len = 0;
    }
}

/// The reader `boltr` sees: bytes forwarded by the pump task.
pub struct PumpedReader {
    rx: mpsc::Receiver<io::Result<Vec<u8>>>,
    buf: Vec<u8>,
    pos: usize,
    task: AbortHandle,
}

impl PumpedReader {
    /// Spawn the pump over `inner`. `on_interrupt` runs on the pump task for
    /// every RESET and for a non-orderly end of input; it must not block.
    ///
    /// With an [`AuthWindow`], input ends if no LOGON has arrived by its first
    /// deadline, and again if none follows within `repeat` after a LOGOFF.
    /// `boltr` then sees the end of input and closes the connection through its
    /// normal cleanup.
    pub fn spawn<R>(
        mut inner: R,
        on_interrupt: impl Fn() + Send + 'static,
        auth_window: Option<AuthWindow>,
    ) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
    {
        let (tx, rx) = mpsc::channel(DEPTH);
        let task = tokio::spawn(async move {
            let mut watch = FrameWatch::default();
            let mut signals = Vec::new();
            let mut said_goodbye = false;
            let mut chunk = vec![0u8; READ_SIZE];
            let mut deadline = auth_window.map(|window| window.first);
            loop {
                let read = match deadline {
                    Some(at) => match tokio::time::timeout_at(at, inner.read(&mut chunk)).await {
                        Ok(read) => read,
                        Err(_) => break,
                    },
                    None => inner.read(&mut chunk).await,
                };
                match read {
                    Ok(0) => {
                        if !said_goodbye {
                            on_interrupt();
                        }
                        break;
                    }
                    Ok(n) => {
                        signals.clear();
                        watch.feed(&chunk[..n], &mut signals);
                        for signal in &signals {
                            match signal {
                                Signal::Reset => on_interrupt(),
                                Signal::Goodbye => said_goodbye = true,
                                Signal::Logon => deadline = None,
                                Signal::Logoff => {
                                    deadline =
                                        auth_window.map(|window| Instant::now() + window.repeat);
                                }
                            }
                        }
                        if tx.send(Ok(chunk[..n].to_vec())).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        on_interrupt();
                        let _ = tx.send(Err(e)).await;
                        break;
                    }
                }
            }
        })
        .abort_handle();
        Self {
            rx,
            buf: Vec::new(),
            pos: 0,
            task,
        }
    }
}

impl Drop for PumpedReader {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl AsyncRead for PumpedReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        if this.pos == this.buf.len() {
            match this.rx.poll_recv(cx) {
                Poll::Pending => return Poll::Pending,
                // The pump ended: end of input.
                Poll::Ready(None) => return Poll::Ready(Ok(())),
                Poll::Ready(Some(Err(e))) => return Poll::Ready(Err(e)),
                Poll::Ready(Some(Ok(bytes))) => {
                    this.buf = bytes;
                    this.pos = 0;
                }
            }
        }
        let n = (this.buf.len() - this.pos).min(out.remaining());
        out.put_slice(&this.buf[this.pos..this.pos + n]);
        this.pos += n;
        Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    const RESET: [u8; 6] = [0x00, 0x02, 0xB0, 0x0F, 0x00, 0x00];
    const GOODBYE: [u8; 6] = [0x00, 0x02, 0xB0, 0x02, 0x00, 0x00];

    fn signals(wire: &[u8], split: usize) -> Vec<Signal> {
        let mut w = FrameWatch::default();
        let mut out = Vec::new();
        let (a, b) = wire.split_at(split);
        w.feed(a, &mut out);
        w.feed(b, &mut out);
        out
    }

    #[test]
    fn reset_and_goodbye_are_found_across_every_split() {
        // RUN-like message with a payload byte pair that looks like RESET
        // inside a longer message must not count.
        let mut wire = vec![0x00, 0x04, 0xB1, 0x10, 0xB0, 0x0F, 0x00, 0x00];
        wire.extend(RESET);
        wire.extend(GOODBYE);
        for split in 0..=wire.len() {
            assert_eq!(
                signals(&wire, split),
                vec![Signal::Reset, Signal::Goodbye],
                "split at {split}"
            );
        }
    }

    #[tokio::test]
    async fn bytes_pass_through_unchanged_and_reset_interrupts() {
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        let mut wire = vec![0x00, 0x03, 0xB1, 0x10, 0xA0, 0x00, 0x00];
        wire.extend(RESET);
        let mut reader = PumpedReader::spawn(
            io::Cursor::new(wire.clone()),
            move || {
                h.fetch_add(1, Ordering::SeqCst);
            },
            None,
        );
        let mut out = Vec::new();
        reader.read_to_end(&mut out).await.unwrap();
        assert_eq!(out, wire);
        // RESET interrupts; the EOF that follows (no GOODBYE) interrupts too.
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn eof_after_goodbye_is_orderly() {
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        let mut reader = PumpedReader::spawn(
            io::Cursor::new(GOODBYE.to_vec()),
            move || {
                h.fetch_add(1, Ordering::SeqCst);
            },
            None,
        );
        let mut out = Vec::new();
        reader.read_to_end(&mut out).await.unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 0);
    }

    fn logon() -> Vec<u8> {
        vec![0x00, 0x03, 0xB1, 0x6A, 0xA0, 0x00, 0x00]
    }

    #[test]
    fn logon_and_logoff_are_found_and_a_lookalike_field_is_not() {
        let mut wire = vec![0x00, 0x04, 0xB1, 0x10, 0xB1, 0x6A, 0x00, 0x00];
        wire.extend(logon());
        wire.extend([0x00, 0x02, 0xB0, 0x6B, 0x00, 0x00]);
        assert_eq!(signals(&wire, 5), vec![Signal::Logon, Signal::Logoff]);
    }

    fn window(first_ms: u64) -> Option<AuthWindow> {
        Some(AuthWindow {
            first: Instant::now() + Duration::from_millis(first_ms),
            repeat: Duration::from_millis(first_ms),
        })
    }

    #[tokio::test]
    async fn input_ends_when_no_logon_arrives_by_the_deadline() {
        let (client, server) = tokio::io::duplex(64);
        let mut reader = PumpedReader::spawn(server, || {}, window(50));
        let mut out = Vec::new();
        let ended =
            tokio::time::timeout(Duration::from_secs(5), reader.read_to_end(&mut out)).await;
        assert!(ended.expect("the deadline ends input").is_ok());
        drop(client);
    }

    #[tokio::test]
    async fn a_logon_lifts_the_deadline() {
        let (mut client, server) = tokio::io::duplex(64);
        let mut reader = PumpedReader::spawn(server, || {}, window(50));
        tokio::io::AsyncWriteExt::write_all(&mut client, &logon())
            .await
            .unwrap();
        let mut out = vec![0u8; logon().len()];
        reader.read_exact(&mut out).await.unwrap();
        let more = tokio::time::timeout(
            Duration::from_millis(300),
            reader.read_to_end(&mut Vec::new()),
        )
        .await;
        assert!(more.is_err(), "still open after the first deadline passed");
    }
}
