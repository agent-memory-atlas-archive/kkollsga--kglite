use crate::datatypes::Value;
use crate::graph::mutation::wal_replay::edge_embedding_delta::digest_group_state;
use crate::graph::wal::{
    append_frame, EdgeEmbeddingStoreState, EdgeGroupEmbeddingPatchWal, EdgeGroupMemberPatchWal,
    EdgeGroupStoreWalState, EdgeVectorCellPatchWal, EdgeVectorWalState, MutationOp, SyncMode, Wal,
    WalFrame,
};
use std::collections::BTreeMap;
use std::hint::black_box;
use std::time::Instant;

const WARMUPS: usize = 20;
const ROUNDS: usize = 200;

struct MatrixFrames {
    control: WalFrame,
    property_full: WalFrame,
    property_delta: WalFrame,
    one_full: WalFrame,
    one_delta: WalFrame,
}

fn properties(members: usize, revision: i64) -> Vec<Vec<(String, Value)>> {
    (0..members)
        .map(|member| {
            vec![
                ("member".into(), Value::Int64(member as i64)),
                ("revision".into(), Value::Int64(revision)),
            ]
        })
        .collect()
}

fn cells(members: usize, dimension: usize, changed: bool) -> Vec<Option<EdgeVectorWalState>> {
    (0..members)
        .map(|member| {
            Some(EdgeVectorWalState {
                vector: vec![if changed && member == 0 { 0.5 } else { 0.25 }; dimension],
                text_hash: Some(member as u64),
            })
        })
        .collect()
}

fn matrix_frames(members: usize, dimension: usize) -> MatrixFrames {
    let base_properties = properties(members, 0);
    let final_properties = properties(members, 1);
    let base_cells = cells(members, dimension, false);
    let changed_cells = cells(members, dimension, true);
    let base_stores = BTreeMap::from([("text".to_string(), base_cells.clone())]);
    let property_stores = base_stores.clone();
    let changed_stores = BTreeMap::from([("text".to_string(), changed_cells.clone())]);
    let topology = MutationOp::ReplaceEdgeGroup {
        conn_type: "R".into(),
        src_type: "N".into(),
        src_id: Value::Int64(1),
        tgt_type: "N".into(),
        tgt_id: Value::Int64(2),
        edges: final_properties.clone(),
    };
    let metadata = MutationOp::SetEdgeEmbeddingStore {
        conn_type: "R".into(),
        text_column: "text".into(),
        state: EdgeEmbeddingStoreState::Present {
            dimension,
            metric: Some("cosine".into()),
            model_id: None,
        },
    };
    let full = |members_state| MutationOp::ReplaceEdgeGroupEmbeddings {
        conn_type: "R".into(),
        src_type: "N".into(),
        src_id: Value::Int64(1),
        tgt_type: "N".into(),
        tgt_id: Value::Int64(2),
        member_count: members,
        stores: vec![EdgeGroupStoreWalState {
            text_column: "text".into(),
            members: members_state,
        }],
    };
    let patch = |result_stores: &BTreeMap<String, Vec<Option<EdgeVectorWalState>>>, changed| {
        MutationOp::PatchEdgeGroupEmbeddings {
            conn_type: "R".into(),
            src_type: "N".into(),
            src_id: Value::Int64(1),
            tgt_type: "N".into(),
            tgt_id: Value::Int64(2),
            patch: EdgeGroupEmbeddingPatchWal {
                base_digest: digest_group_state(&base_properties, &base_stores).unwrap(),
                result_digest: digest_group_state(&final_properties, result_stores).unwrap(),
                base_stores: vec!["text".into()],
                stores: vec!["text".into()],
                members: (0..members)
                    .map(|member| EdgeGroupMemberPatchWal::Prior {
                        prior_ordinal: member as u32,
                        cells: vec![if changed && member == 0 {
                            EdgeVectorCellPatchWal::Replace(
                                result_stores["text"][member].clone().unwrap(),
                            )
                        } else {
                            EdgeVectorCellPatchWal::Keep
                        }],
                    })
                    .collect(),
            },
        }
    };
    let frame = |ops| WalFrame { lsn: 1, ops };
    MatrixFrames {
        control: frame(vec![topology.clone()]),
        property_full: frame(vec![topology.clone(), full(base_cells)]),
        property_delta: frame(vec![topology.clone(), patch(&property_stores, false)]),
        one_full: frame(vec![
            metadata.clone(),
            topology.clone(),
            full(changed_cells),
        ]),
        one_delta: frame(vec![metadata, topology, patch(&changed_stores, true)]),
    }
}

fn encoded(frame: &WalFrame) -> Vec<u8> {
    let mut bytes = Vec::new();
    append_frame(&mut bytes, frame).unwrap();
    bytes
}

fn encode_min_ns(frame: &WalFrame) -> u128 {
    for _ in 0..WARMUPS {
        black_box(encoded(black_box(frame)));
    }
    (0..ROUNDS)
        .map(|_| {
            let start = Instant::now();
            black_box(encoded(black_box(frame)));
            start.elapsed().as_nanos()
        })
        .min()
        .unwrap()
}

fn append_mean_ns(frame: &WalFrame, mode: SyncMode) -> u128 {
    let dir = tempfile::tempdir().unwrap();
    let mut wal = Wal::open(dir.path().join("measure.wal"), mode).unwrap();
    let mut next = frame.clone();
    for lsn in 1..=WARMUPS as u64 {
        next.lsn = lsn;
        wal.append(&next).unwrap();
    }
    let start = Instant::now();
    for round in 0..ROUNDS {
        next.lsn = (WARMUPS + round + 1) as u64;
        wal.append(&next).unwrap();
    }
    start.elapsed().as_nanos() / ROUNDS as u128
}

#[test]
#[ignore = "release-only edge WAL codec and append measurement"]
fn measure_edge_wal_delta_matrix() {
    println!("kind,members,dimension,variant,bytes,statistic,rounds,warmups,value_ns,durability");
    for members in [1usize, 10, 100] {
        for dimension in [384usize, 1536] {
            let frames = matrix_frames(members, dimension);
            // The delta codec's whole claim, independent of profile: a patch
            // that keeps every cell is far smaller than the full state, a
            // one-cell patch shrinks below the full state once the group has
            // members to keep, and over a single member the patch's fixed
            // overhead (two digests, store names) stays bounded.
            let one_full = encoded(&frames.one_full).len();
            let one_delta = encoded(&frames.one_delta).len();
            let property_full = encoded(&frames.property_full).len();
            let property_delta = encoded(&frames.property_delta).len();
            assert!(
                property_delta * 4 < property_full,
                "members={members} dimension={dimension}: a keep-everything patch must be a small \
                 fraction of the full state ({property_delta}/{property_full} B)"
            );
            if members >= 10 {
                assert!(
                    one_delta * 4 < one_full,
                    "members={members} dimension={dimension}: a one-cell patch over {members} \
                     members should be a small fraction of the full state ({one_delta}/{one_full} B)"
                );
            } else {
                assert!(
                    one_delta < one_full + 256,
                    "members={members} dimension={dimension}: patch overhead over one member is \
                     unbounded ({one_delta} vs {one_full} B)"
                );
            }
            for (variant, frame) in [
                ("control", &frames.control),
                ("property_full", &frames.property_full),
                ("property_delta", &frames.property_delta),
                ("one_full", &frames.one_full),
                ("one_delta", &frames.one_delta),
            ] {
                println!(
                    "encode,{members},{dimension},{variant},{},min,{ROUNDS},{WARMUPS},{},none",
                    encoded(frame).len(),
                    encode_min_ns(frame)
                );
            }
            if matches!(members, 1 | 100) {
                for (variant, frame) in [
                    ("property_full", &frames.property_full),
                    ("property_delta", &frames.property_delta),
                    ("one_full", &frames.one_full),
                    ("one_delta", &frames.one_delta),
                ] {
                    for (durability, mode) in [
                        ("normal_page_cache", SyncMode::PageCache),
                        ("full_barrier", SyncMode::Barrier),
                    ] {
                        println!(
                            "append,{members},{dimension},{variant},{},mean,{ROUNDS},{WARMUPS},{},{durability}",
                            encoded(frame).len(),
                            append_mean_ns(frame, mode)
                        );
                    }
                }
            }
        }
    }
}
