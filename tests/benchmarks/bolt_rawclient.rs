// Minimal Bolt 5.4 raw reader: measures server-only RUN->SUCCESS and PULL(-1) stream time without
// decoding results (the Python raw client was itself the bottleneck above ~1 us/record).
// rustc -O rawclient.rs -o rawclient ; rawclient <port> <reps> <query>
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Instant;

fn ps(s: &str, out: &mut Vec<u8>) {
    let b = s.as_bytes();
    if b.len() < 16 { out.push(0x80 | b.len() as u8) } else if b.len() < 256 { out.push(0xD0); out.push(b.len() as u8) } else { out.push(0xD1); out.extend((b.len() as u16).to_be_bytes()) }
    out.extend(b);
}
fn frame(sig: u8, fields: &[Vec<u8>]) -> Vec<u8> {
    let mut body = vec![0xB0 | fields.len() as u8, sig];
    for f in fields { body.extend(f) }
    let mut o = (body.len() as u16).to_be_bytes().to_vec();
    o.extend(body); o.extend([0, 0]); o
}
struct R { s: TcpStream, buf: Vec<u8>, pos: usize, end: usize }
impl R {
    fn need(&mut self, n: usize) {
        while self.end - self.pos < n {
            if self.pos > (1 << 22) { self.buf.copy_within(self.pos..self.end, 0); self.end -= self.pos; self.pos = 0; }
            if self.buf.len() - self.end < (1 << 16) { let l = self.buf.len(); self.buf.resize(l * 2, 0); }
            let k = self.s.read(&mut self.buf[self.end..]).expect("read");
            assert!(k > 0, "eof");
            self.end += k;
        }
    }
    // returns (signature, wire bytes)
    fn msg(&mut self) -> (u8, usize) {
        let (mut sig, mut first, mut tot) = (0u8, true, 0usize);
        loop {
            self.need(2);
            let n = u16::from_be_bytes([self.buf[self.pos], self.buf[self.pos + 1]]) as usize;
            self.pos += 2;
            if n == 0 { if first { continue } return (sig, tot) }
            self.need(n);
            if first { sig = self.buf[self.pos + 1]; first = false }
            self.pos += n; tot += n;
        }
    }
}
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (port, reps, q) = (&a[1], a[2].parse::<usize>().unwrap(), &a[3]);
    let mut s = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
    s.set_nodelay(true).unwrap();
    s.write_all(&[0x60, 0x60, 0xB0, 0x17, 0, 0, 4, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).unwrap();
    let mut v = [0u8; 4]; s.read_exact(&mut v).unwrap();
    let mut r = R { s, buf: vec![0; 1 << 20], pos: 0, end: 0 };
    let mut d = vec![0xA1]; ps("user_agent", &mut d); ps("perf/1", &mut d);
    r.s.write_all(&frame(0x01, &[d])).unwrap(); assert_eq!(r.msg().0, 0x70);
    let mut d = vec![0xA3]; ps("scheme", &mut d); ps("basic", &mut d); ps("principal", &mut d); ps("neo4j", &mut d); ps("credentials", &mut d); ps("x", &mut d);
    r.s.write_all(&frame(0x6A, &[d])).unwrap(); assert_eq!(r.msg().0, 0x70);
    let mut qq = vec![]; ps(q, &mut qq);
    let run = frame(0x10, &[qq, vec![0xA0], vec![0xA0]]);
    let mut pd = vec![0xA1]; ps("n", &mut pd); pd.push(0xFF); // n = -1
    let pull = frame(0x3F, &[pd]);
    for _ in 0..reps {
        r.s.write_all(&run).unwrap();
        let t0 = Instant::now(); let (sg, _) = r.msg(); assert_eq!(sg, 0x70);
        let t_run = t0.elapsed();
        r.s.write_all(&pull).unwrap();
        let t1 = Instant::now(); let (mut recs, mut bytes) = (0u64, 0u64);
        loop { let (sg, n) = r.msg(); if sg == 0x71 { recs += 1; bytes += n as u64 } else { assert_eq!(sg, 0x70, "sig {sg:x}"); break } }
        println!("{{\"run_success_ms\":{:.3},\"stream_ms\":{:.3},\"records\":{},\"bytes\":{}}}", t_run.as_secs_f64() * 1e3, t1.elapsed().as_secs_f64() * 1e3, recs, bytes);
    }
}
