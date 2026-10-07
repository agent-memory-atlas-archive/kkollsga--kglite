//! Write coalescing for the Bolt connection's outgoing half.
//!
//! `boltr` writes every message as three small `write_all`s (length, chunk,
//! terminator) and then `flush()`es, including after each RECORD. With Nagle
//! on, the kernel merged those writes; with `TCP_NODELAY` (see `accept.rs`)
//! each becomes its own packet and streaming a result gets 2-3x slower.
//!
//! [`CoalescingWriter`] sits between `boltr` and the socket. It buffers
//! writes, follows the Bolt chunk framing of what passes through, and
//! forwards a `flush()` to the socket only when the buffer ends on a message
//! that is not a RECORD. A RECORD stream always ends in SUCCESS or FAILURE,
//! which flushes, so nothing is held waiting for input; the buffer is also
//! drained once it reaches [`FLUSH_CAP`], so large results stream.

use std::io;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use tokio::io::AsyncWrite;

/// Buffered bytes at which the writer stops holding RECORDs back.
const FLUSH_CAP: usize = 64 * 1024;
/// PackStream signature byte of a RECORD message (`0xB1 0x71 ...`).
const RECORD_SIGNATURE: u8 = 0x71;

/// Tracks Bolt chunk framing across arbitrary write boundaries: a message is
/// `(u16 length, data)*` followed by a zero length; data byte 1 of the message
/// is its signature.
#[derive(Default)]
struct Framing {
    header: [u8; 2],
    header_len: usize,
    chunk_remaining: usize,
    /// Data bytes of the current message seen so far (saturating at 2).
    message_pos: u8,
    signature: u8,
    /// No message is partially written.
    at_boundary: bool,
    /// The last completed message was a RECORD.
    last_was_record: bool,
}

impl Framing {
    fn feed(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            self.at_boundary = false;
            if self.chunk_remaining > 0 {
                let n = self.chunk_remaining.min(bytes.len());
                if self.message_pos == 0 && n >= 2 {
                    self.signature = bytes[1];
                } else if self.message_pos == 1 {
                    self.signature = bytes[0];
                }
                self.message_pos = (self.message_pos as usize + n).min(2) as u8;
                self.chunk_remaining -= n;
                bytes = &bytes[n..];
                continue;
            }
            self.header[self.header_len] = bytes[0];
            self.header_len += 1;
            bytes = &bytes[1..];
            if self.header_len == 2 {
                self.header_len = 0;
                let len = u16::from_be_bytes(self.header) as usize;
                if len == 0 {
                    self.last_was_record =
                        self.message_pos == 2 && self.signature == RECORD_SIGNATURE;
                    self.message_pos = 0;
                    self.at_boundary = true;
                } else {
                    self.chunk_remaining = len;
                }
            }
        }
    }

    /// True when the bytes written so far end exactly after a RECORD.
    fn ends_on_record(&self) -> bool {
        self.at_boundary && self.last_was_record
    }
}

pub struct CoalescingWriter<W> {
    inner: W,
    buf: Vec<u8>,
    sent: usize,
    framing: Framing,
}

impl<W: AsyncWrite + Unpin> CoalescingWriter<W> {
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            buf: Vec::with_capacity(FLUSH_CAP),
            sent: 0,
            framing: Framing::default(),
        }
    }

    fn pending(&self) -> usize {
        self.buf.len() - self.sent
    }

    /// Write the buffered bytes to the inner writer (without flushing it).
    fn poll_drain(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        while self.sent < self.buf.len() {
            let n = ready!(Pin::new(&mut self.inner).poll_write(cx, &self.buf[self.sent..]))?;
            if n == 0 {
                return Poll::Ready(Err(io::ErrorKind::WriteZero.into()));
            }
            self.sent += n;
        }
        self.buf.clear();
        self.sent = 0;
        Poll::Ready(Ok(()))
    }
}

impl<W: AsyncWrite + Unpin> AsyncWrite for CoalescingWriter<W> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.pending() >= FLUSH_CAP {
            ready!(self.poll_drain(cx))?;
        }
        self.buf.extend_from_slice(data);
        self.framing.feed(data);
        Poll::Ready(Ok(data.len()))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.framing.ends_on_record() && self.pending() < FLUSH_CAP {
            return Poll::Ready(Ok(()));
        }
        ready!(self.poll_drain(cx))?;
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        ready!(self.poll_drain(cx))?;
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boltr::chunk::ChunkWriter;
    use std::sync::{Arc, Mutex};

    /// What reached the "socket": bytes, write calls, flush calls.
    #[derive(Default)]
    struct Seen {
        bytes: Vec<u8>,
        writes: usize,
        flushes: usize,
    }

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Seen>>);

    impl AsyncWrite for Sink {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            data: &[u8],
        ) -> Poll<io::Result<usize>> {
            let mut seen = self.0.lock().unwrap();
            seen.bytes.extend_from_slice(data);
            seen.writes += 1;
            Poll::Ready(Ok(data.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            self.0.lock().unwrap().flushes += 1;
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    fn writer() -> (ChunkWriter<CoalescingWriter<Sink>>, Sink) {
        let sink = Sink::default();
        (ChunkWriter::new(CoalescingWriter::new(sink.clone())), sink)
    }

    /// A message as boltr encodes it: struct marker, signature, payload.
    fn message(signature: u8, payload_len: usize) -> Vec<u8> {
        let mut m = vec![0xB1, signature];
        m.resize(2 + payload_len, 0x2A);
        m
    }

    /// One boltr `send_message`: the chunked write, then a flush.
    async fn send(w: &mut ChunkWriter<CoalescingWriter<Sink>>, msg: &[u8]) {
        w.write_message(msg).await.unwrap();
        w.flush().await.unwrap();
    }

    fn framed(msg: &[u8]) -> Vec<u8> {
        let mut out = (msg.len() as u16).to_be_bytes().to_vec();
        out.extend_from_slice(msg);
        out.extend([0, 0]);
        out
    }

    #[tokio::test]
    async fn a_record_stream_reaches_the_socket_in_one_write_and_one_flush() {
        let (mut w, sink) = writer();
        let mut expected = Vec::new();
        for _ in 0..1000 {
            let record = message(RECORD_SIGNATURE, 3);
            send(&mut w, &record).await;
            expected.extend(framed(&record));
        }
        let success = message(0x70, 4);
        send(&mut w, &success).await;
        expected.extend(framed(&success));

        let seen = sink.0.lock().unwrap();
        assert_eq!(seen.bytes, expected, "bytes pass through unchanged");
        assert_eq!(seen.writes, 1, "1000 RECORDs + SUCCESS: one socket write");
        assert_eq!(seen.flushes, 1, "flushed once, at the SUCCESS");
    }

    #[tokio::test]
    async fn every_non_record_message_is_flushed_immediately() {
        for signature in [0x70u8, 0x7F, 0x7E] {
            let (mut w, sink) = writer();
            send(&mut w, &message(signature, 2)).await;
            let seen = sink.0.lock().unwrap();
            assert_eq!(seen.flushes, 1, "signature {signature:#x}");
            assert_eq!(seen.bytes, framed(&message(signature, 2)));
        }
    }

    #[tokio::test]
    async fn a_held_record_is_not_visible_until_the_closing_message() {
        let (mut w, sink) = writer();
        send(&mut w, &message(RECORD_SIGNATURE, 3)).await;
        assert_eq!(sink.0.lock().unwrap().writes, 0, "RECORD alone is held");
        send(&mut w, &message(0x7F, 1)).await;
        assert!(
            !sink.0.lock().unwrap().bytes.is_empty(),
            "FAILURE releases it"
        );
    }

    #[tokio::test]
    async fn records_past_the_cap_stream_without_a_closing_message() {
        let (mut w, sink) = writer();
        // 1000 records of ~1 KiB: ~1 MiB, no SUCCESS yet.
        for _ in 0..1000 {
            send(&mut w, &message(RECORD_SIGNATURE, 1000)).await;
        }
        let seen = sink.0.lock().unwrap();
        assert!(
            seen.bytes.len() >= 1_000_000 - 2 * FLUSH_CAP,
            "streamed {}",
            seen.bytes.len()
        );
        assert!(
            seen.writes > 1 && seen.writes < 100,
            "writes {}",
            seen.writes
        );
    }

    #[tokio::test]
    async fn a_message_split_across_chunks_keeps_its_signature() {
        let (mut w, sink) = writer();
        // 70 000 bytes: boltr splits it into a 65 535-byte chunk plus a rest.
        let big = message(RECORD_SIGNATURE, 70_000 - 2);
        send(&mut w, &big).await;
        send(&mut w, &message(0x70, 1)).await;
        let seen = sink.0.lock().unwrap();
        let mut expected = Vec::new();
        expected.extend(65_535u16.to_be_bytes());
        expected.extend(&big[..65_535]);
        expected.extend(((big.len() - 65_535) as u16).to_be_bytes());
        expected.extend(&big[65_535..]);
        expected.extend([0, 0]);
        expected.extend(framed(&message(0x70, 1)));
        assert_eq!(seen.bytes, expected);
        assert_eq!(seen.flushes, 1, "the big RECORD was held, SUCCESS flushed");
    }
}
