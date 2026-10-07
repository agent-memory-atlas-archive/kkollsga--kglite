//! Continuing a recovered WAL must never append behind a discarded frame.
use super::*;
use std::io::Cursor;

fn node_frame(lsn: u64) -> WalFrame {
    WalFrame {
        lsn,
        ops: vec![MutationOp::UpsertNode {
            node_type: "Item".into(),
            id: Value::Int64(lsn as i64),
            title: Value::String(format!("item-{lsn}")),
            properties: Vec::new(),
        }],
    }
}

fn prefix(version: u8) -> Vec<u8> {
    let mut bytes = WAL_MAGIC.to_vec();
    bytes.push(version);
    append_frame(&mut bytes, &node_frame(1)).unwrap();
    bytes
}

fn encoded_frame(lsn: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    append_frame(&mut bytes, &node_frame(lsn)).unwrap();
    bytes
}

fn assert_continuation(tail: &[u8], version: u8, sync: SyncMode) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.kgl-wal");
    let intact = prefix(version);
    let mut bytes = intact.clone();
    bytes.extend_from_slice(tail);
    std::fs::write(&path, &bytes).unwrap();
    assert_eq!(recover(&path).unwrap(), vec![node_frame(1)]);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        bytes,
        "recovery is read-only"
    );

    let mut wal = Wal::open(path.clone(), sync).unwrap();
    let repaired = std::fs::read(&path).unwrap();
    assert_eq!(
        repaired.len(),
        intact.len(),
        "truncate to the last verified frame boundary"
    );
    assert_eq!(&repaired[5..], &intact[5..], "keep every prefix byte");
    assert_eq!(repaired[4], WAL_FORMAT_VERSION);
    wal.append(&node_frame(2)).unwrap();
    drop(wal);
    assert_eq!(recover(&path).unwrap(), vec![node_frame(1), node_frame(2)]);
    let mut wal = Wal::open(path.clone(), sync).unwrap();
    wal.append(&node_frame(3)).unwrap();
    drop(wal);
    assert_eq!(
        recover(&path).unwrap(),
        vec![node_frame(1), node_frame(2), node_frame(3)]
    );
}

#[test]
fn resumed_appends_follow_partial_frame_repair_at_both_sync_modes() {
    let frame = encoded_frame(2);
    for sync in [SyncMode::PageCache, SyncMode::Barrier] {
        for cut in [1, 3, 4, 7, 8, frame.len() - 1] {
            assert_continuation(&frame[..cut], WAL_FORMAT_VERSION, sync);
        }
    }
}

#[test]
fn resumed_appends_follow_checksum_tail_repair_and_legacy_upgrade() {
    let mut frame = encoded_frame(2);
    frame[8] ^= 0xff;
    for version in [MIN_READABLE_WAL_FORMAT_VERSION, WAL_FORMAT_VERSION] {
        for sync in [SyncMode::PageCache, SyncMode::Barrier] {
            assert_continuation(&frame, version, sync);
        }
    }
}

#[test]
fn non_tail_damage_is_quarantined_and_the_valid_prefix_continues() {
    for later in [encoded_frame(3), vec![1, 2, 3]] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.kgl-wal");
        let intact = prefix(MIN_READABLE_WAL_FORMAT_VERSION);
        let mut bytes = intact.clone();
        let mut bad = encoded_frame(2);
        bad[8] ^= 0xff;
        bytes.extend_from_slice(&bad);
        bytes.extend_from_slice(&later);
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(recover(&path).unwrap(), vec![node_frame(1)]);
        let mut wal = Wal::open(path.clone(), SyncMode::PageCache).unwrap();
        let report = wal.quarantine().cloned().expect("damage is reported");
        assert_eq!(report.damage_offset, intact.len() as u64);
        assert_eq!(report.bytes_set_aside, (bad.len() + later.len()) as u64);
        assert_eq!(std::fs::read(&report.path).unwrap(), bytes, "kept whole");
        wal.append(&node_frame(2)).unwrap();
        drop(wal);
        assert_eq!(
            recover(&path).unwrap(),
            vec![node_frame(1), node_frame(2)],
            "the live log is the valid prefix plus new commits"
        );
        assert_eq!(std::fs::read(&report.path).unwrap(), bytes);
    }
}

#[test]
fn unsupported_version_refuses_before_tail_repair() {
    for version in [1, WAL_FORMAT_VERSION + 1] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.kgl-wal");
        let mut bytes = prefix(version);
        bytes.extend_from_slice(&[1, 2]);
        std::fs::write(&path, &bytes).unwrap();
        assert!(Wal::open(path.clone(), SyncMode::Barrier).is_err());
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
}

#[test]
fn intact_log_is_not_rewritten_on_append_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.kgl-wal");
    let bytes = prefix(WAL_FORMAT_VERSION);
    std::fs::write(&path, &bytes).unwrap();
    let wal = Wal::open(path.clone(), SyncMode::Barrier).unwrap();
    assert_eq!(std::fs::read(path).unwrap(), bytes);
    drop(wal);
}

#[test]
fn replaced_same_length_file_refuses_recovered_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.kgl-wal");
    let mut bytes = prefix(WAL_FORMAT_VERSION);
    bytes.extend_from_slice(&[1, 2]);
    std::fs::write(&path, &bytes).unwrap();
    let recovered = recover_for_append(&path, drop).unwrap();
    let old = dir.path().join("retained-old-wal");
    std::fs::rename(&path, &old).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    let error = Wal::open_recovered(path.clone(), SyncMode::Barrier, recovered).unwrap_err();
    assert!(error.to_string().contains("identity"), "{error}");
    assert_eq!(std::fs::read(path).unwrap(), bytes);
    assert_eq!(std::fs::read(old).unwrap(), bytes);
}

#[test]
fn tail_truncation_error_is_not_reported_as_repaired() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.kgl-wal");
    let intact = prefix(WAL_FORMAT_VERSION);
    let mut bytes = intact.clone();
    bytes.extend_from_slice(&[1, 2]);
    std::fs::write(&path, &bytes).unwrap();
    let read_only = File::open(&path).unwrap();
    let point = ResumePoint {
        version: WAL_FORMAT_VERSION,
        stream_len: bytes.len() as u64,
        valid_bytes: intact.len() as u64,
        non_tail: None,
    };
    assert!(repair_tail(&read_only, point).is_err());
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[test]
fn shared_durable_open_repairs_before_its_next_frame() {
    use crate::graph::dir_graph::DirGraph;
    use crate::graph::durability::open_log;
    use crate::graph::storage::GraphRead;
    use std::sync::Arc;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.kgl");
    let sidecar = wal_path(&path);
    let mut bytes = prefix(WAL_FORMAT_VERSION);
    bytes.extend_from_slice(&[1, 2]);
    std::fs::write(&sidecar, bytes).unwrap();
    let mut graph = Arc::new(DirGraph::new());
    let (mut wal, next) = open_log(&mut graph, &path, DurabilityLevel::Normal)
        .unwrap()
        .unwrap();
    assert_eq!(next, 2);
    assert_eq!(graph.graph.node_count(), 1);
    assert!(graph.graph.is_wal_owner());
    wal.append(&node_frame(next)).unwrap();
    drop(wal);
    assert_eq!(
        recover(&sidecar).unwrap(),
        vec![node_frame(1), node_frame(2)]
    );
}

#[test]
fn newly_appeared_file_refuses_empty_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.kgl-wal");
    let recovered = recover_for_append(&path, drop).unwrap();
    let bytes = prefix(WAL_FORMAT_VERSION);
    std::fs::write(&path, &bytes).unwrap();
    assert!(Wal::open_recovered(path.clone(), SyncMode::Barrier, recovered).is_err());
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

/// A frame whose payload is dominated by vectors. The torn-tail rule has to
/// hold for the edge-embedding ops exactly as for node ops: a truncation that
/// lands inside a vector drops the frame whole rather than surfacing a short
/// vector or a store without its members.
fn edge_embedding_ops() -> Vec<MutationOp> {
    vec![
        MutationOp::SetEdgeEmbeddingStore {
            conn_type: "R".into(),
            text_column: "text".into(),
            state: EdgeEmbeddingStoreState::Present {
                dimension: 4,
                metric: Some("cosine".into()),
                model_id: Some("m".into()),
            },
        },
        MutationOp::ReplaceEdgeGroup {
            conn_type: "R".into(),
            src_type: "N".into(),
            src_id: Value::Int64(1),
            tgt_type: "N".into(),
            tgt_id: Value::Int64(2),
            edges: vec![
                vec![("k".into(), Value::Int64(0))],
                vec![("k".into(), Value::Int64(1))],
            ],
        },
        MutationOp::ReplaceEdgeGroupEmbeddings {
            conn_type: "R".into(),
            src_type: "N".into(),
            src_id: Value::Int64(1),
            tgt_type: "N".into(),
            tgt_id: Value::Int64(2),
            member_count: 2,
            stores: vec![EdgeGroupStoreWalState {
                text_column: "text".into(),
                members: vec![
                    Some(EdgeVectorWalState {
                        vector: vec![0.1, 0.2, 0.3, 0.4],
                        text_hash: Some(7),
                    }),
                    None,
                ],
            }],
        },
    ]
}

#[test]
fn edge_embedding_frames_round_trip_and_a_torn_one_is_discarded_whole() {
    let frames = vec![
        node_frame(1),
        WalFrame {
            lsn: 2,
            ops: edge_embedding_ops(),
        },
    ];
    let mut bytes = WAL_MAGIC.to_vec();
    bytes.push(WAL_FORMAT_VERSION);
    let intact_prefix = {
        let mut prefix = bytes.clone();
        append_frame(&mut prefix, &frames[0]).unwrap();
        prefix.len()
    };
    for frame in &frames {
        append_frame(&mut bytes, frame).unwrap();
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.kgl-wal");
    std::fs::write(&path, &bytes).unwrap();
    assert_eq!(recover(&path).unwrap(), frames);

    // Every cut inside the vector frame — mid-payload, and just short of its
    // end — recovers exactly the intact prefix, never a partial store.
    for cut in [
        intact_prefix + (bytes.len() - intact_prefix) / 2,
        bytes.len() - 3,
    ] {
        std::fs::write(&path, &bytes[..cut]).unwrap();
        assert_eq!(
            recover(&path).unwrap(),
            vec![frames[0].clone()],
            "cut at {cut} of {}",
            bytes.len()
        );
    }
}

/// Recovery hands each frame to its sink before reading the next and keeps
/// none: a decoded frame is about ten times its on-disk size, so holding the
/// log whole is what made a 6 MB WAL cost 90 MiB at restart.
#[test]
fn scan_hands_frames_over_one_at_a_time_and_retains_none() {
    struct Counting<'a> {
        inner: Cursor<Vec<u8>>,
        consumed: &'a std::cell::Cell<u64>,
    }
    impl Read for Counting<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = self.inner.read(buf)?;
            self.consumed.set(self.consumed.get() + n as u64);
            Ok(n)
        }
    }

    let mut bytes = WAL_MAGIC.to_vec();
    bytes.push(WAL_FORMAT_VERSION);
    for lsn in 1..=50 {
        append_frame(&mut bytes, &node_frame(lsn)).unwrap();
    }
    let total = bytes.len() as u64;
    let consumed = std::cell::Cell::new(0);
    let reader = Counting {
        inner: Cursor::new(bytes),
        consumed: &consumed,
    };
    let mut seen_at = Vec::new();
    let read = scan_frames(reader, total, |frame| {
        seen_at.push((frame.lsn, consumed.get()));
    })
    .unwrap();

    assert_eq!(seen_at.len(), 50);
    assert_eq!(read.resume.valid_bytes, total);
    assert!(
        seen_at.windows(2).all(|pair| pair[0].1 < pair[1].1),
        "each frame must reach the sink before the next one is read"
    );
    assert!(
        seen_at[0].1 < total / 10,
        "the first frame was delivered after {} of {total} bytes",
        seen_at[0].1
    );
}

#[test]
fn recover_with_yields_what_recover_returns() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.kgl-wal");
    let mut wal = Wal::open(path.clone(), SyncMode::PageCache).unwrap();
    for lsn in 1..=5 {
        wal.append(&node_frame(lsn)).unwrap();
    }
    let mut streamed = Vec::new();
    recover_with(&path, |frame| streamed.push(frame)).unwrap();
    assert_eq!(streamed, recover(&path).unwrap());
    recover_with(&dir.path().join("absent-wal"), |_| {
        panic!("no log, no frames")
    })
    .unwrap();
}
