//! Power-cut crash images of the WAL.
//!
//! A process kill keeps the page cache, so it cannot show what an OS crash or
//! power cut leaves. These tests build the *file image* such a cut can leave
//! and run the real recovery and reopen paths over it.
//!
//! Model: the file is a run of 4 KiB pages. A page that was never synced
//! survives or not independently of every other page; one that did not
//! survive reads as zeros (an extended length whose data never landed) or is
//! cut off the end of the file. Pages already barriered survive. This is the
//! weakest guarantee a journalling filesystem gives for data it was not asked
//! to flush; it does not model bit rot or a lying drive cache.
use super::*;
use tempfile::TempDir;

const PAGE: usize = 4096;

fn big_frame(lsn: u64) -> WalFrame {
    WalFrame {
        lsn,
        ops: vec![MutationOp::UpsertNode {
            node_type: "Item".into(),
            id: Value::Int64(lsn as i64),
            title: Value::String("t".repeat(300)),
            properties: Vec::new(),
        }],
    }
}

/// A WAL image holding `n` frames and the byte offset where each frame ends.
fn image(n: u64) -> (Vec<u8>, Vec<usize>) {
    let mut bytes = WAL_MAGIC.to_vec();
    bytes.push(WAL_FORMAT_VERSION);
    let mut ends = Vec::new();
    for lsn in 1..=n {
        append_frame(&mut bytes, &big_frame(lsn)).unwrap();
        ends.push(bytes.len());
    }
    (bytes, ends)
}

/// The image after a cut in which exactly the pages in `persisted` (bitmask)
/// reached the disk. Page 0 always carries the barriered header.
fn cut_image(full: &[u8], persisted: u32, extended: bool) -> Vec<u8> {
    let pages = full.len().div_ceil(PAGE);
    let mut out = vec![0u8; full.len()];
    out[..WAL_MAGIC.len() + 1].copy_from_slice(&full[..WAL_MAGIC.len() + 1]);
    let mut last_kept = 0;
    for page in 0..pages {
        if persisted & (1 << page) != 0 {
            let (lo, hi) = (page * PAGE, ((page + 1) * PAGE).min(full.len()));
            out[lo..hi].copy_from_slice(&full[lo..hi]);
            last_kept = hi;
        }
    }
    if !extended {
        out.truncate(last_kept.max(WAL_MAGIC.len() + 1));
    }
    out
}

/// Whether `got` is a prefix of the frames in `full` (lsn 1..).
fn is_prefix(got: &[WalFrame], n: u64) -> bool {
    got.iter()
        .enumerate()
        .all(|(i, f)| *f == big_frame(i as u64 + 1))
        && got.len() as u64 <= n
}

/// Every page subset of an unsynced log must recover to a prefix of the
/// commits **and** let the server open the log for appending again: a device
/// that cannot restart after a power cut has lost everything, not a suffix.
#[test]
fn every_unsynced_page_subset_reopens_to_a_prefix() {
    let (full, _) = image(40);
    let pages = full.len().div_ceil(PAGE);
    assert!((3..=8).contains(&pages), "fixture spans {pages} pages");
    let mut failures = Vec::new();
    for persisted in 0..(1u32 << pages) {
        for extended in [true, false] {
            let dir = TempDir::new().unwrap();
            let path = dir.path().join("g.kgl-wal");
            std::fs::write(&path, cut_image(&full, persisted, extended)).unwrap();
            let recovered = match recover(&path) {
                Ok(frames) => frames,
                Err(e) => {
                    failures.push(format!("{persisted:#b}/{extended}: recover failed: {e}"));
                    continue;
                }
            };
            if !is_prefix(&recovered, 40) {
                failures.push(format!("{persisted:#b}/{extended}: not a prefix"));
                continue;
            }
            match Wal::open(path.clone(), SyncMode::PageCache) {
                Ok(mut wal) => {
                    let next = recovered.len() as u64 + 1;
                    wal.append(&big_frame(next)).unwrap();
                    drop(wal);
                    let again = recover(&path).unwrap();
                    assert!(is_prefix(&again, 41), "{persisted:#b}: append after repair");
                    assert_eq!(again.len(), recovered.len() + 1);
                }
                Err(e) => failures.push(format!("{persisted:#b}/{extended}: open refused: {e}")),
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} power-cut images do not reopen:\n{}",
        failures.len(),
        (1u32 << pages) * 2,
        failures[..failures.len().min(8)].join("\n")
    );
}

/// A frame that spans several pages and loses one in the middle is still a
/// torn tail, not damage.
#[test]
fn a_multi_page_frame_with_a_lost_middle_page_reads_as_a_torn_tail() {
    let mut full = WAL_MAGIC.to_vec();
    full.push(WAL_FORMAT_VERSION);
    append_frame(&mut full, &big_frame(1)).unwrap();
    let keep = full.len();
    let mut wide = big_frame(2);
    if let MutationOp::UpsertNode { title, .. } = &mut wide.ops[0] {
        *title = Value::String("w".repeat(3 * PAGE));
    }
    append_frame(&mut full, &wide).unwrap();
    append_frame(&mut full, &big_frame(3)).unwrap();
    // Lose the page-aligned page that lies wholly inside the wide frame.
    let lost = (keep + 8).div_ceil(PAGE) * PAGE;
    full[lost..lost + PAGE].fill(0);
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("g.kgl-wal");
    std::fs::write(&path, &full).unwrap();
    assert_eq!(recover(&path).unwrap(), vec![big_frame(1)]);
    drop(Wal::open(path.clone(), SyncMode::PageCache).unwrap());
    assert_eq!(std::fs::metadata(&path).unwrap().len(), keep as u64);
}

/// Damage that is not a lost page is still refused: a flipped byte with a
/// complete frame after it is committed work being discarded.
#[test]
fn a_flipped_byte_before_more_frames_still_refuses_to_open() {
    let (mut full, ends) = image(6);
    full[ends[2] + 20] ^= 0xff;
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("g.kgl-wal");
    std::fs::write(&path, &full).unwrap();
    let err = Wal::open(path, SyncMode::PageCache).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

fn seq_frame(lsn: u64) -> WalFrame {
    big_frame(lsn)
}

/// A commit whose frame fails to reach the log (full device, failing barrier)
/// is reported as failed. The frames acknowledged after it must still recover:
/// the failed frame's bytes may not stay in front of them.
#[test]
fn a_failed_append_leaves_no_bytes_in_front_of_later_commits() {
    for sync in [SyncMode::PageCache, SyncMode::Barrier] {
        for fault in [
            AppendFault::ShortWrite(10),
            AppendFault::ShortWrite(200),
            AppendFault::SyncError,
        ] {
            let dir = TempDir::new().unwrap();
            let path = dir.path().join("g.kgl-wal");
            let mut wal = Wal::open(path.clone(), sync).unwrap();
            wal.append(&seq_frame(1)).unwrap();
            wal.fault = Some(fault);
            wal.append(&seq_frame(2)).unwrap_err();
            wal.fault = None;
            // The session reuses the LSN of a frame that never committed.
            wal.append(&seq_frame(2)).unwrap();
            wal.append(&seq_frame(3)).unwrap();
            drop(wal);
            assert_eq!(
                recover(&path).unwrap(),
                vec![seq_frame(1), seq_frame(2), seq_frame(3)],
                "{sync:?} {fault:?}"
            );
            drop(Wal::open(path, sync).unwrap());
        }
    }
}

/// When the failed tail cannot be removed the log stops accepting commits
/// instead of appending behind bytes of unknown shape.
#[test]
fn an_uncuttable_failed_append_poisons_the_log() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("g.kgl-wal");
    let mut wal = Wal::open(path.clone(), SyncMode::Barrier).unwrap();
    wal.append(&seq_frame(1)).unwrap();
    wal.fault = Some(AppendFault::ShortWrite(10));
    std::fs::remove_file(&path).unwrap();
    wal.append(&seq_frame(2)).unwrap_err();
    wal.fault = None;
    let err = wal.append(&seq_frame(2)).unwrap_err();
    assert!(
        err.to_string().contains("refusing further appends"),
        "{err}"
    );
    assert!(wal.sync().is_err());
}
