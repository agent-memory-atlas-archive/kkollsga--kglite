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

/// The `.quarantine-` siblings of the log in `dir`.
fn quarantines(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().contains(".quarantine-"))
        .collect();
    found.sort();
    found
}

/// The `.torn-` siblings of the log in `dir`, with the log offset each began at
/// (the `-at-<offset>` suffix of its name).
fn torn_files(dir: &std::path::Path) -> Vec<(u64, std::path::PathBuf)> {
    let mut found: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter_map(|p| {
            let name = p.file_name()?.to_string_lossy().into_owned();
            let (_, rest) = name.split_once(".torn-")?;
            let (_, at) = rest.split_once("-at-")?;
            Some((at.split('-').next()?.parse().ok()?, p))
        })
        .collect();
    found.sort();
    found
}

/// Whether an image that recovers `kept` frames must be quarantined, worked
/// out from how the fixture was laid out rather than from the scanner: the
/// first lost frame is followed by non-zero bytes beyond its own span, or its
/// length field is zeroed with non-zero bytes after it.
fn expects_quarantine(image: &[u8], kept: usize, ends: &[usize]) -> bool {
    if kept >= ends.len() {
        return false;
    }
    let stop = if kept == 0 {
        WAL_MAGIC.len() + 1
    } else {
        ends[kept - 1]
    };
    let nonzero = |from: usize| image.get(from..).is_some_and(|b| b.iter().any(|&x| x != 0));
    let zero_len = image.get(stop..stop + 4).is_some_and(|b| b == [0; 4]);
    nonzero(ends[kept]) || (zero_len && nonzero(stop + 4))
}

/// Assert no non-zero byte of `original` was lost by an open that has just
/// finished: the live log is a prefix of it, and the rest is either all zero or
/// saved exactly (in a torn file at its offset, or inside a quarantined copy of
/// the whole log).
fn assert_no_byte_lost(dir: &std::path::Path, path: &std::path::Path, original: &[u8], tag: &str) {
    let live = std::fs::read(path).unwrap();
    assert!(
        original.starts_with(&live),
        "{tag}: live log is not a prefix"
    );
    let rest = &original[live.len()..];
    let quarantined = quarantines(dir);
    let torn = torn_files(dir);
    if !quarantined.is_empty() {
        assert_eq!(quarantined.len(), 1, "{tag}");
        assert!(torn.is_empty(), "{tag}: both kinds");
        assert_eq!(std::fs::read(&quarantined[0]).unwrap(), original, "{tag}");
    } else if rest.iter().any(|&b| b != 0) {
        assert_eq!(torn.len(), 1, "{tag}: non-zero tail not saved");
        assert_eq!(torn[0].0, live.len() as u64, "{tag}: offset");
        assert_eq!(std::fs::read(&torn[0].1).unwrap(), rest, "{tag}");
    } else {
        assert!(torn.is_empty(), "{tag}: all-zero tail saved");
    }
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
    let (full, ends) = image(40);
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
            let before = std::fs::read(&path).unwrap();
            match Wal::open(path.clone(), SyncMode::PageCache) {
                Ok(mut wal) => {
                    assert_no_byte_lost(
                        dir.path(),
                        &path,
                        &before,
                        &format!("{persisted:#b}/{extended}"),
                    );
                    let kept = quarantines(dir.path());
                    let want = expects_quarantine(&before, recovered.len(), &ends);
                    if kept.len() != usize::from(want) {
                        failures.push(format!(
                            "{persisted:#b}/{extended}: expected quarantine={want}, found {}",
                            kept.len()
                        ));
                        continue;
                    }
                    if want {
                        let report = wal.quarantine().expect("damage is reported");
                        assert_eq!(report.path, kept[0], "{persisted:#b}");
                        assert_eq!(std::fs::read(&kept[0]).unwrap(), before, "{persisted:#b}");
                    } else {
                        assert!(wal.quarantine().is_none(), "{persisted:#b}");
                    }
                    // No non-zero byte of the original may exist nowhere: it is in
                    // the live log, in the quarantine copy, or in a saved tail.
                    let live = std::fs::read(&path).unwrap();
                    if live != before[..live.len()] {
                        failures.push(format!(
                            "{persisted:#b}/{extended}: live log is not a prefix"
                        ));
                        continue;
                    }
                    let cut = &before[live.len()..];
                    let torn = torn_files(dir.path());
                    let covered = if cut.iter().all(|&b| b == 0) {
                        torn.is_empty()
                    } else if want {
                        true // the whole original is in the quarantine, asserted above
                    } else {
                        torn.len() == 1
                            && torn[0].0 == live.len() as u64
                            && std::fs::read(&torn[0].1).unwrap() == cut
                            && wal.saved_tail().is_some_and(|t| t.path == torn[0].1)
                    };
                    if !covered {
                        failures.push(format!(
                            "{persisted:#b}/{extended}: {} cut byte(s) kept nowhere",
                            cut.len()
                        ));
                        continue;
                    }
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

/// A frame that spans several pages and loses one in the middle, with a later
/// frame on disk, is damage: the original is kept whole and the log continues
/// on the frames before it.
#[test]
fn a_multi_page_frame_with_a_lost_middle_page_is_quarantined() {
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
    let wal = Wal::open(path.clone(), SyncMode::PageCache).unwrap();
    let report = wal.quarantine().expect("reported");
    assert_eq!(report.damage_offset, keep as u64);
    assert_eq!(report.bytes_set_aside, (full.len() - keep) as u64);
    assert_eq!(
        report.frames_set_aside, 1,
        "frame 3 decodes after the damage"
    );
    assert_eq!(std::fs::read(&report.path).unwrap(), full);
    assert_eq!(std::fs::metadata(&path).unwrap().len(), keep as u64);
    assert_eq!(quarantines(dir.path()).len(), 1);
    assert!(
        torn_files(dir.path()).is_empty(),
        "quarantine covers the tail"
    );
}

/// Bit damage with a complete frame after it is quarantined, never dropped.
#[test]
fn a_flipped_byte_before_more_frames_is_quarantined() {
    let (mut full, ends) = image(6);
    full[ends[2] + 20] ^= 0xff;
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("g.kgl-wal");
    std::fs::write(&path, &full).unwrap();
    let wal = Wal::open(path.clone(), SyncMode::PageCache).unwrap();
    let report = wal.quarantine().expect("reported");
    assert_eq!(report.damage_offset, ends[2] as u64);
    assert_eq!(report.frames_set_aside, 2);
    assert_eq!(std::fs::read(&report.path).unwrap(), full);
    drop(wal);
    assert_eq!(recover(&path).unwrap().len(), 3);
}

/// A torn tail is cut off with no quarantine. Its bytes are saved to a `.torn-`
/// sibling unless they are all zero.
#[test]
fn a_torn_tail_is_cut_off_and_its_nonzero_bytes_are_saved() {
    let (full, ends) = image(6);
    let mut tails: Vec<(&str, Vec<u8>, bool)> = Vec::new();
    tails.push(("short frame", full[..ends[3] + 50].to_vec(), true));
    tails.push(("short header", full[..ends[3] + 3].to_vec(), true));
    let mut zeros = full[..ends[3]].to_vec();
    zeros.extend(std::iter::repeat_n(0u8, 3 * PAGE));
    tails.push(("zeros to eof", zeros, false));
    let mut flipped = full[..ends[4]].to_vec();
    flipped[ends[3] + 20] ^= 0xff;
    tails.push(("bad last frame", flipped, true));
    for (name, bytes, saved) in tails {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("g.kgl-wal");
        std::fs::write(&path, &bytes).unwrap();
        let wal = Wal::open(path.clone(), SyncMode::PageCache).unwrap();
        assert!(wal.quarantine().is_none(), "{name}");
        assert_eq!(wal.saved_tail().is_some(), saved, "{name}");
        assert!(quarantines(dir.path()).is_empty(), "{name}");
        assert_eq!(torn_files(dir.path()).len(), usize::from(saved), "{name}");
        assert_no_byte_lost(dir.path(), &path, &bytes, name);
        assert_eq!(recover(&path).unwrap().len(), 4, "{name}");
    }
}

/// An undamaged log opens without a byte changing or a file appearing.
#[test]
fn an_undamaged_log_is_byte_for_byte_unchanged() {
    let (full, _) = image(12);
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("g.kgl-wal");
    std::fs::write(&path, &full).unwrap();
    let wal = Wal::open(path.clone(), SyncMode::Barrier).unwrap();
    assert!(wal.quarantine().is_none());
    drop(wal);
    assert_eq!(std::fs::read(&path).unwrap(), full);
    assert!(quarantines(dir.path()).is_empty());
    assert!(torn_files(dir.path()).is_empty());
}

/// Two quarantines in the same second never overwrite each other.
#[test]
fn quarantine_names_do_not_collide() {
    let (mut full, ends) = image(6);
    full[ends[2] + 20] ^= 0xff;
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("g.kgl-wal");
    for round in 0..3 {
        std::fs::write(&path, &full).unwrap();
        drop(Wal::open(path.clone(), SyncMode::PageCache).unwrap());
        assert_eq!(quarantines(dir.path()).len(), round + 1);
    }
    for q in quarantines(dir.path()) {
        assert_eq!(std::fs::read(q).unwrap(), full);
    }
}

/// When the original cannot be set aside the open is refused and the log is
/// left exactly as found: dropping it is the one outcome that is not allowed.
#[cfg(unix)]
#[test]
fn a_quarantine_that_cannot_be_written_refuses_to_open() {
    use std::os::unix::fs::PermissionsExt;
    let (mut full, ends) = image(6);
    full[ends[2] + 20] ^= 0xff;
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("g.kgl-wal");
    std::fs::write(&path, &full).unwrap();
    let set_mode =
        |mode| std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(mode)).unwrap();
    set_mode(0o555);
    let blocked = std::fs::File::create(dir.path().join("probe")).is_err();
    let opened = Wal::open(path.clone(), SyncMode::PageCache);
    set_mode(0o755);
    if !blocked {
        return; // running as a user the mode bits do not bind
    }
    let error = opened.unwrap_err();
    assert!(error.to_string().contains("refusing to open"), "{error}");
    assert_eq!(std::fs::read(&path).unwrap(), full, "log untouched");
    assert!(quarantines(dir.path()).is_empty());
}

/// A durable open surfaces the quarantine on the graph, where `graph_info()`
/// and `describe()` read advisories.
#[test]
fn a_durable_open_reports_the_quarantine_as_an_advisory() {
    use crate::graph::dir_graph::DirGraph;
    use crate::graph::durability::open_log;
    use std::sync::Arc;
    let (mut full, ends) = image(6);
    full[ends[2] + 20] ^= 0xff;
    let dir = TempDir::new().unwrap();
    let checkpoint = dir.path().join("g.kgl");
    std::fs::write(wal_path(&checkpoint), &full).unwrap();
    let mut graph = Arc::new(DirGraph::new());
    let (wal, _) = open_log(&mut graph, &checkpoint, DurabilityLevel::Normal)
        .unwrap()
        .unwrap();
    drop(wal);
    let found = quarantines(dir.path());
    assert_eq!(found.len(), 1);
    let advisory = graph
        .advisories
        .iter()
        .find(|a| a.code == "wal_quarantined")
        .expect("advisory raised");
    assert_eq!(advisory.affected, vec![found[0].display().to_string()]);
    assert!(
        advisory.message.contains("byte offset"),
        "{}",
        advisory.message
    );
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

/// A tail the repair cuts is saved first whenever it holds a non-zero byte, so
/// the open drops nothing without a trace.
mod saved_tail {
    use super::*;

    fn open_with(bytes: &[u8]) -> (TempDir, std::path::PathBuf, Wal) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("g.kgl-wal");
        std::fs::write(&path, bytes).unwrap();
        let wal = Wal::open(path.clone(), SyncMode::PageCache).unwrap();
        (dir, path, wal)
    }

    /// The length field of a frame header overwritten by a larger value reads as
    /// a torn frame running past EOF: everything after it, later frames
    /// included, is cut. The cut bytes must be kept.
    #[test]
    fn a_header_that_swallows_later_frames_is_saved() {
        let (mut full, ends) = image(6);
        full[ends[2] + 2] = 0x7f;
        full[ends[2] + 3] = 0x00;
        let (dir, path, wal) = open_with(&full);
        assert!(wal.quarantine().is_none());
        let saved = wal.saved_tail().expect("tail reported");
        let cut = &full[ends[2]..];
        assert_eq!(saved.offset, ends[2] as u64);
        assert_eq!(saved.bytes, cut.len() as u64);
        assert_eq!(std::fs::read(&saved.path).unwrap(), cut);
        assert_eq!(
            torn_files(dir.path()),
            vec![(ends[2] as u64, saved.path.clone())]
        );
        assert_eq!(std::fs::metadata(&path).unwrap().len(), ends[2] as u64);
    }

    /// A crash mid-append leaves a partial frame; it is saved, not just cut.
    #[test]
    fn a_partial_frame_from_a_crash_mid_append_is_saved() {
        let (full, ends) = image(5);
        for extra in [3usize, 8, 50, ends[4] - ends[3] - 1] {
            let bytes = &full[..ends[3] + extra.min(full.len() - ends[3])];
            let (dir, _path, wal) = open_with(bytes);
            let saved = wal.saved_tail().expect("partial frame reported");
            assert_eq!(
                std::fs::read(&saved.path).unwrap(),
                &bytes[ends[3]..],
                "{extra}"
            );
            assert_eq!(saved.offset, ends[3] as u64);
            assert_eq!(torn_files(dir.path()).len(), 1);
            assert!(quarantines(dir.path()).is_empty());
        }
    }

    /// Zero-filled or pre-allocated space is nothing to keep.
    #[test]
    fn an_all_zero_tail_saves_nothing() {
        let (full, ends) = image(5);
        let mut bytes = full[..ends[3]].to_vec();
        bytes.extend(std::iter::repeat_n(0u8, 2 * PAGE + 17));
        let (dir, path, wal) = open_with(&bytes);
        assert!(wal.saved_tail().is_none());
        assert!(torn_files(dir.path()).is_empty());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), ends[3] as u64);
    }

    /// A header-sized stub that is neither zero nor a header prefix is data.
    #[test]
    fn a_foreign_stub_in_place_of_the_header_is_saved() {
        let (dir, _path, wal) = open_with(&[1, 2, 3, 4, 5]);
        let saved = wal.saved_tail().expect("reported");
        assert_eq!(std::fs::read(&saved.path).unwrap(), [1, 2, 3, 4, 5]);
        assert_eq!(saved.offset, 0);
        drop(dir);
        let mut prefix = WAL_MAGIC.to_vec();
        prefix.truncate(3);
        let (dir, _path, wal) = open_with(&prefix);
        assert!(
            wal.saved_tail().is_none(),
            "a torn header write is not data"
        );
        assert!(torn_files(dir.path()).is_empty());
    }

    /// An undamaged log opens with no file created and no byte changed.
    #[test]
    fn an_undamaged_log_creates_no_file() {
        let (full, _) = image(8);
        let (dir, path, wal) = open_with(&full);
        assert!(wal.saved_tail().is_none() && wal.quarantine().is_none());
        drop(wal);
        assert_eq!(std::fs::read(&path).unwrap(), full);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    /// A tail that cannot be saved refuses the open and leaves the log as found.
    #[cfg(unix)]
    #[test]
    fn a_tail_that_cannot_be_saved_refuses_to_open() {
        use std::os::unix::fs::PermissionsExt;
        let (full, ends) = image(5);
        let bytes = &full[..ends[3] + 50];
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("g.kgl-wal");
        std::fs::write(&path, bytes).unwrap();
        let set_mode = |mode| {
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(mode)).unwrap()
        };
        set_mode(0o555);
        let blocked = std::fs::File::create(dir.path().join("probe")).is_err();
        let opened = Wal::open(path.clone(), SyncMode::PageCache);
        set_mode(0o755);
        if !blocked {
            return; // running as a user the mode bits do not bind
        }
        let error = opened.expect_err("refused");
        assert!(error.to_string().contains("refusing to open"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), bytes, "log untouched");
        assert!(torn_files(dir.path()).is_empty());
    }

    /// Two saves in the same second never overwrite each other.
    #[test]
    fn saved_tail_names_do_not_collide() {
        let (full, ends) = image(5);
        let bytes = &full[..ends[3] + 50];
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("g.kgl-wal");
        for round in 0..3 {
            std::fs::write(&path, bytes).unwrap();
            drop(Wal::open(path.clone(), SyncMode::PageCache).unwrap());
            assert_eq!(torn_files(dir.path()).len(), round + 1);
        }
        for (_, saved) in torn_files(dir.path()) {
            assert_eq!(std::fs::read(saved).unwrap(), &bytes[ends[3]..]);
        }
    }

    /// A durable open surfaces the saved tail where `graph_info()` and
    /// `describe()` read advisories.
    #[test]
    fn a_durable_open_reports_the_saved_tail_as_an_advisory() {
        use crate::graph::dir_graph::DirGraph;
        use crate::graph::durability::open_log;
        use std::sync::Arc;
        let (full, ends) = image(5);
        let dir = TempDir::new().unwrap();
        let checkpoint = dir.path().join("g.kgl");
        std::fs::write(wal_path(&checkpoint), &full[..ends[3] + 50]).unwrap();
        let mut graph = Arc::new(DirGraph::new());
        let (wal, _) = open_log(&mut graph, &checkpoint, DurabilityLevel::Normal)
            .unwrap()
            .unwrap();
        drop(wal);
        let found = torn_files(dir.path());
        assert_eq!(found.len(), 1);
        let advisory = graph
            .advisories
            .iter()
            .find(|a| a.code == "wal_tail_saved")
            .expect("advisory raised");
        assert_eq!(advisory.affected, vec![found[0].1.display().to_string()]);
        assert!(advisory.message.contains("offset"), "{}", advisory.message);
    }
}
