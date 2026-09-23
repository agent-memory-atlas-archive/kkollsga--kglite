use std::collections::{BTreeMap, BTreeSet};

use sha2::{Digest, Sha256};

use crate::graph::wal::{
    EdgeEmbeddingGroupDigest, EdgeGroupEmbeddingPatchWal, EdgeGroupMemberPatchWal,
    EdgeVectorCellPatchWal, EdgeVectorWalState,
};

use super::edge_embeddings::{
    LogicalEdgeEmbeddingState, LogicalGroupKey, LogicalGroupState, OrderedEdgeEmbeddingEvent,
};

/// Applies full fallback and relative records while retaining the group state
/// immediately before each ordered topology replacement.
pub(super) struct DeltaInterpreter {
    state: LogicalEdgeEmbeddingState,
    pending_bases: BTreeMap<LogicalGroupKey, LogicalGroupState>,
}

impl DeltaInterpreter {
    pub(super) fn from_state(state: LogicalEdgeEmbeddingState) -> Self {
        Self {
            state,
            pending_bases: BTreeMap::new(),
        }
    }

    pub(super) fn interpret(
        mut self,
        events: impl IntoIterator<Item = OrderedEdgeEmbeddingEvent>,
    ) -> Result<LogicalEdgeEmbeddingState, String> {
        for event in events {
            match event {
                OrderedEdgeEmbeddingEvent::PatchEmbeddings { key, patch } => {
                    self.apply_patch(key, patch)?;
                }
                event => self.apply_full(event)?,
            }
        }
        if !self.pending_bases.is_empty() {
            return Err("topology event is missing its relationship embedding record".into());
        }
        self.state.validate_complete()?;
        Ok(self.state)
    }

    fn apply_full(&mut self, event: OrderedEdgeEmbeddingEvent) -> Result<(), String> {
        match &event {
            OrderedEdgeEmbeddingEvent::ReplaceTopology { key, .. } => {
                let before = self
                    .state
                    .groups
                    .get(key)
                    .cloned()
                    .unwrap_or_else(empty_group);
                if self.pending_bases.insert(key.clone(), before).is_some() {
                    return Err(format!(
                        "relationship group '{}' has consecutive topology records without embedding state",
                        key.conn_type
                    ));
                }
            }
            OrderedEdgeEmbeddingEvent::ReplaceEmbeddings { key, .. } => {
                if self.pending_bases.remove(key).is_none() {
                    return Err(format!(
                        "relationship group '{}' full state has no preceding topology record",
                        key.conn_type
                    ));
                }
            }
            OrderedEdgeEmbeddingEvent::SetStore { key, .. } => {
                if !self.pending_bases.is_empty() {
                    return Err(format!(
                        "relationship embedding store '{}.{}' changes between topology and embedding state",
                        key.conn_type, key.text_column
                    ));
                }
            }
            OrderedEdgeEmbeddingEvent::PatchEmbeddings { .. } => unreachable!(),
        }
        self.state.apply_event(event)
    }

    fn apply_patch(
        &mut self,
        key: LogicalGroupKey,
        patch: EdgeGroupEmbeddingPatchWal,
    ) -> Result<(), String> {
        let base = self.pending_bases.remove(&key).ok_or_else(|| {
            format!(
                "relationship embedding patch for '{}' has no preceding topology record",
                key.conn_type
            )
        })?;
        let current = self
            .state
            .groups
            .get(&key)
            .cloned()
            .ok_or_else(|| format!("relationship group '{}' is absent", key.conn_type))?;

        if group_digest(&current)? == patch.result_digest {
            return Ok(());
        }
        if group_digest(&base)? != patch.base_digest {
            return Err(format!(
                "relationship embedding patch base digest mismatch for '{}'",
                key.conn_type
            ));
        }
        if current.properties.len() != patch.members.len() {
            return Err(format!(
                "relationship embedding patch has {} members but final topology has {}",
                patch.members.len(),
                current.properties.len()
            ));
        }

        let expected_stores: Vec<_> = self
            .state
            .stores
            .keys()
            .filter(|store| store.conn_type == key.conn_type)
            .map(|store| store.text_column.clone())
            .collect();
        if patch.stores != expected_stores {
            return Err(format!(
                "relationship embedding patch for '{}' does not name the complete sorted store set",
                key.conn_type
            ));
        }

        let mut used_prior = BTreeSet::new();
        let mut result_stores: BTreeMap<String, Vec<Option<EdgeVectorWalState>>> = patch
            .stores
            .iter()
            .map(|name| (name.clone(), Vec::with_capacity(patch.members.len())))
            .collect();
        for member in patch.members {
            match member {
                EdgeGroupMemberPatchWal::Prior {
                    prior_ordinal,
                    cells,
                } => {
                    require_cell_count(patch.stores.len(), cells.len())?;
                    if !used_prior.insert(prior_ordinal) {
                        return Err(format!("prior ordinal {prior_ordinal} is reused"));
                    }
                    let ordinal = prior_ordinal as usize;
                    if ordinal >= base.properties.len() {
                        return Err(format!("prior ordinal {prior_ordinal} is out of range"));
                    }
                    for ((name, cells_out), action) in result_stores.iter_mut().zip(cells) {
                        let prior = base
                            .stores
                            .get(name)
                            .and_then(|cells| cells.get(ordinal))
                            .cloned()
                            .flatten();
                        cells_out.push(match action {
                            EdgeVectorCellPatchWal::Keep => prior,
                            EdgeVectorCellPatchWal::Replace(state) => Some(state),
                            EdgeVectorCellPatchWal::Clear => None,
                        });
                    }
                }
                EdgeGroupMemberPatchWal::New { cells } => {
                    require_cell_count(patch.stores.len(), cells.len())?;
                    for ((_, cells_out), state) in result_stores.iter_mut().zip(cells) {
                        cells_out.push(state);
                    }
                }
            }
        }

        let result = LogicalGroupState {
            properties: current.properties,
            stores: result_stores,
        };
        if group_digest(&result)? != patch.result_digest {
            return Err(format!(
                "relationship embedding patch result digest mismatch for '{}'",
                key.conn_type
            ));
        }
        self.state.groups.insert(key, result);
        Ok(())
    }
}

fn require_cell_count(expected: usize, actual: usize) -> Result<(), String> {
    if actual != expected {
        return Err(format!(
            "relationship embedding patch member has {actual} cells, expected {expected}"
        ));
    }
    Ok(())
}

fn empty_group() -> LogicalGroupState {
    LogicalGroupState {
        properties: Vec::new(),
        stores: BTreeMap::new(),
    }
}

pub(crate) fn group_digest(group: &LogicalGroupState) -> Result<EdgeEmbeddingGroupDigest, String> {
    digest_group_state(&group.properties, &group.stores)
}

#[derive(Clone, Copy)]
pub(crate) struct BorrowedEdgeVectorCell<'a> {
    pub vector: &'a [f32],
    pub text_hash: Option<u64>,
}

/// One canonical, name-sorted store view used only while computing a digest.
/// The member vector owns `Option`/slice descriptors, never embedding data.
pub(crate) struct BorrowedEdgeGroupStore<'a> {
    pub text_column: &'a str,
    pub members: Vec<Option<BorrowedEdgeVectorCell<'a>>>,
}

/// Hash a logical group directly over borrowed store vectors.
///
/// `stores` must be strictly name-sorted, matching `BTreeMap` iteration in
/// [`digest_group_state`]. This lets WAL capture hash unchanged cells without
/// cloning their vector payloads into the transient capture buffer.
pub(crate) fn digest_borrowed_group_state(
    properties: &[Vec<(String, crate::datatypes::Value)>],
    stores: &[BorrowedEdgeGroupStore<'_>],
) -> Result<EdgeEmbeddingGroupDigest, String> {
    for pair in stores.windows(2) {
        if pair[0].text_column >= pair[1].text_column {
            return Err("borrowed relationship embedding stores are not canonical".into());
        }
    }
    let mut digest = digest_properties(properties)?;
    put_len(&mut digest, stores.len());
    for store in stores {
        put_bytes(&mut digest, store.text_column.as_bytes());
        put_len(&mut digest, store.members.len());
        for member in &store.members {
            digest_cell(
                &mut digest,
                member.map(|cell| (cell.vector, cell.text_hash)),
            );
        }
    }
    Ok(digest.finalize().into())
}

pub(crate) fn digest_group_state(
    properties: &[Vec<(String, crate::datatypes::Value)>],
    stores: &BTreeMap<String, Vec<Option<EdgeVectorWalState>>>,
) -> Result<EdgeEmbeddingGroupDigest, String> {
    let mut digest = digest_properties(properties)?;
    put_len(&mut digest, stores.len());
    for (name, members) in stores {
        put_bytes(&mut digest, name.as_bytes());
        put_len(&mut digest, members.len());
        for member in members {
            digest_cell(
                &mut digest,
                member
                    .as_ref()
                    .map(|state| (state.vector.as_slice(), state.text_hash)),
            );
        }
    }
    Ok(digest.finalize().into())
}

fn digest_properties(
    properties: &[Vec<(String, crate::datatypes::Value)>],
) -> Result<Sha256, String> {
    let mut digest = Sha256::new();
    digest.update(b"kglite-edge-embedding-group-v1\0");
    put_len(&mut digest, properties.len());
    for member_properties in properties {
        put_len(&mut digest, member_properties.len());
        let mut ordered: Vec<_> = member_properties.iter().collect();
        ordered.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
        for (name, value) in ordered {
            put_bytes(&mut digest, name.as_bytes());
            let encoded = crate::serde_codec::encode_versioned(
                crate::serde_codec::CURRENT_CODEC,
                value,
                u32::MAX as u64,
            )
            .map_err(|error| format!("cannot digest relationship property '{name}': {error}"))?;
            put_bytes(&mut digest, &encoded);
        }
    }
    Ok(digest)
}

fn digest_cell(digest: &mut Sha256, cell: Option<(&[f32], Option<u64>)>) {
    match cell {
        None => digest.update([0]),
        Some((vector, text_hash)) => {
            digest.update([1]);
            put_len(digest, vector.len());
            for value in vector {
                digest.update(value.to_bits().to_le_bytes());
            }
            match text_hash {
                None => digest.update([0]),
                Some(hash) => {
                    digest.update([1]);
                    digest.update(hash.to_le_bytes());
                }
            }
        }
    }
}

fn put_len(digest: &mut Sha256, value: usize) {
    digest.update((value as u64).to_le_bytes());
}

fn put_bytes(digest: &mut Sha256, bytes: &[u8]) {
    put_len(digest, bytes.len());
    digest.update(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datatypes::Value;
    use crate::graph::mutation::wal_replay::edge_embeddings::{
        LogicalStoreKey, LogicalStoreMetadata,
    };
    use crate::graph::wal::{EdgeEmbeddingStoreState, EdgeGroupStoreWalState};

    fn key(target: i64) -> LogicalGroupKey {
        LogicalGroupKey {
            conn_type: "ASSERTS".into(),
            src_type: "Doc".into(),
            src_id: Value::Int64(1),
            tgt_type: "Doc".into(),
            tgt_id: Value::Int64(target),
        }
    }
    fn store_key() -> LogicalStoreKey {
        LogicalStoreKey {
            conn_type: "ASSERTS".into(),
            text_column: "description".into(),
        }
    }
    fn props(revision: i64) -> Vec<Vec<(String, Value)>> {
        vec![
            vec![
                ("same".into(), Value::Boolean(true)),
                ("rev".into(), Value::Int64(revision)),
            ],
            vec![
                ("rev".into(), Value::Int64(revision)),
                ("same".into(), Value::Boolean(true)),
            ],
        ]
    }
    fn cell(values: &[f32]) -> Option<EdgeVectorWalState> {
        Some(EdgeVectorWalState {
            vector: values.to_vec(),
            text_hash: None,
        })
    }
    fn state(
        dimension: usize,
        groups: impl IntoIterator<Item = (LogicalGroupKey, LogicalGroupState)>,
    ) -> LogicalEdgeEmbeddingState {
        LogicalEdgeEmbeddingState {
            stores: BTreeMap::from([(
                store_key(),
                LogicalStoreMetadata {
                    dimension,
                    metric: Some("cosine".into()),
                    model_id: None,
                },
            )]),
            groups: groups.into_iter().collect(),
        }
    }
    fn group(rev: i64, a: &[f32], b: &[f32]) -> LogicalGroupState {
        LogicalGroupState {
            properties: props(rev),
            stores: BTreeMap::from([("description".into(), vec![cell(a), cell(b)])]),
        }
    }
    fn patch(
        base: &LogicalGroupState,
        result: &LogicalGroupState,
        members: Vec<EdgeGroupMemberPatchWal>,
    ) -> EdgeGroupEmbeddingPatchWal {
        EdgeGroupEmbeddingPatchWal {
            base_digest: group_digest(base).unwrap(),
            result_digest: group_digest(result).unwrap(),
            stores: vec!["description".into()],
            members,
        }
    }
    fn prior(ordinal: u32, action: EdgeVectorCellPatchWal) -> EdgeGroupMemberPatchWal {
        EdgeGroupMemberPatchWal::Prior {
            prior_ordinal: ordinal,
            cells: vec![action],
        }
    }

    #[test]
    fn borrowed_digest_matches_owned_oracle_without_vector_copies() {
        let properties = props(7);
        let first = vec![0.1, 0.9];
        let second = vec![0.9, 0.1];
        let owned = LogicalGroupState {
            properties: properties.clone(),
            stores: BTreeMap::from([
                (
                    "description".into(),
                    vec![
                        Some(EdgeVectorWalState {
                            vector: first.clone(),
                            text_hash: Some(11),
                        }),
                        None,
                    ],
                ),
                (
                    "title".into(),
                    vec![
                        None,
                        Some(EdgeVectorWalState {
                            vector: second.clone(),
                            text_hash: Some(22),
                        }),
                    ],
                ),
            ]),
        };
        let borrowed = [
            BorrowedEdgeGroupStore {
                text_column: "description",
                members: vec![
                    Some(BorrowedEdgeVectorCell {
                        vector: &first,
                        text_hash: Some(11),
                    }),
                    None,
                ],
            },
            BorrowedEdgeGroupStore {
                text_column: "title",
                members: vec![
                    None,
                    Some(BorrowedEdgeVectorCell {
                        vector: &second,
                        text_hash: Some(22),
                    }),
                ],
            },
        ];
        assert_eq!(
            digest_borrowed_group_state(&properties, &borrowed).unwrap(),
            group_digest(&owned).unwrap()
        );
        let noncanonical = [
            BorrowedEdgeGroupStore {
                text_column: "title",
                members: Vec::new(),
            },
            BorrowedEdgeGroupStore {
                text_column: "description",
                members: Vec::new(),
            },
        ];
        assert!(digest_borrowed_group_state(&properties, &noncanonical).is_err());
    }

    #[test]
    fn pre_topology_base_and_final_property_digest_match_full_oracle() {
        let k = key(2);
        let before = group(1, &[0.1, 0.9], &[0.9, 0.1]);
        let after = group(2, &[0.1, 0.9], &[0.4, 0.6]);
        let initial = state(2, [(k.clone(), before.clone())]);
        let delta = DeltaInterpreter::from_state(initial.clone())
            .interpret([
                OrderedEdgeEmbeddingEvent::ReplaceTopology {
                    key: k.clone(),
                    edges: props(2),
                },
                OrderedEdgeEmbeddingEvent::PatchEmbeddings {
                    key: k.clone(),
                    patch: patch(
                        &before,
                        &after,
                        vec![
                            prior(0, EdgeVectorCellPatchWal::Keep),
                            prior(
                                1,
                                EdgeVectorCellPatchWal::Replace(cell(&[0.4, 0.6]).unwrap()),
                            ),
                        ],
                    ),
                },
            ])
            .unwrap();
        let oracle = initial
            .interpret([
                OrderedEdgeEmbeddingEvent::ReplaceTopology {
                    key: k.clone(),
                    edges: props(2),
                },
                OrderedEdgeEmbeddingEvent::ReplaceEmbeddings {
                    key: k.clone(),
                    member_count: 2,
                    stores: vec![EdgeGroupStoreWalState {
                        text_column: "description".into(),
                        members: after.stores["description"].clone(),
                    }],
                },
            ])
            .unwrap();
        assert_eq!(delta, oracle);
    }

    #[test]
    fn dimension_transition_validates_only_after_all_groups_reach_final_width() {
        let k1 = key(2);
        let k2 = key(3);
        let b1 = group(1, &[0.1, 0.2, 0.3], &[0.3, 0.2, 0.1]);
        let b2 = group(1, &[0.4, 0.5, 0.6], &[0.6, 0.5, 0.4]);
        let r1 = group(2, &[1.0, 0.0, 0.0, 0.0], &[0.0, 1.0, 0.0, 0.0]);
        let r2 = group(2, &[0.0, 0.0, 1.0, 0.0], &[0.0, 0.0, 0.0, 1.0]);
        let initial = state(3, [(k1.clone(), b1.clone()), (k2.clone(), b2.clone())]);
        let events = vec![
            OrderedEdgeEmbeddingEvent::SetStore {
                key: store_key(),
                state: EdgeEmbeddingStoreState::Present {
                    dimension: 4,
                    metric: Some("cosine".into()),
                    model_id: Some("B".into()),
                },
            },
            OrderedEdgeEmbeddingEvent::ReplaceTopology {
                key: k1.clone(),
                edges: props(2),
            },
            OrderedEdgeEmbeddingEvent::PatchEmbeddings {
                key: k1.clone(),
                patch: patch(
                    &b1,
                    &r1,
                    vec![
                        prior(
                            0,
                            EdgeVectorCellPatchWal::Replace(cell(&[1.0, 0.0, 0.0, 0.0]).unwrap()),
                        ),
                        prior(
                            1,
                            EdgeVectorCellPatchWal::Replace(cell(&[0.0, 1.0, 0.0, 0.0]).unwrap()),
                        ),
                    ],
                ),
            },
            OrderedEdgeEmbeddingEvent::ReplaceTopology {
                key: k2.clone(),
                edges: props(2),
            },
            OrderedEdgeEmbeddingEvent::PatchEmbeddings {
                key: k2.clone(),
                patch: patch(
                    &b2,
                    &r2,
                    vec![
                        prior(
                            0,
                            EdgeVectorCellPatchWal::Replace(cell(&[0.0, 0.0, 1.0, 0.0]).unwrap()),
                        ),
                        prior(
                            1,
                            EdgeVectorCellPatchWal::Replace(cell(&[0.0, 0.0, 0.0, 1.0]).unwrap()),
                        ),
                    ],
                ),
            },
        ];
        let result = DeltaInterpreter::from_state(initial)
            .interpret(events)
            .unwrap();
        assert_eq!(result.stores[&store_key()].dimension, 4);
        assert_eq!(result.groups[&k1], r1);
        assert_eq!(result.groups[&k2], r2);
    }

    #[test]
    fn mismatch_refuses_and_result_digest_is_idempotent() {
        let k = key(2);
        let before = group(1, &[0.1, 0.9], &[0.9, 0.1]);
        let after = group(2, &[0.1, 0.9], &[0.4, 0.6]);
        let good = patch(
            &before,
            &after,
            vec![
                prior(0, EdgeVectorCellPatchWal::Keep),
                prior(
                    1,
                    EdgeVectorCellPatchWal::Replace(cell(&[0.4, 0.6]).unwrap()),
                ),
            ],
        );
        let mut bad = good.clone();
        bad.base_digest[0] ^= 0xff;
        let initial = state(2, [(k.clone(), before)]);
        let error = DeltaInterpreter::from_state(initial)
            .interpret([
                OrderedEdgeEmbeddingEvent::ReplaceTopology {
                    key: k.clone(),
                    edges: props(2),
                },
                OrderedEdgeEmbeddingEvent::PatchEmbeddings {
                    key: k.clone(),
                    patch: bad,
                },
            ])
            .unwrap_err();
        assert!(error.contains("base digest mismatch"));
        let already = state(2, [(k.clone(), after.clone())]);
        let replayed = DeltaInterpreter::from_state(already.clone())
            .interpret([
                OrderedEdgeEmbeddingEvent::ReplaceTopology {
                    key: k.clone(),
                    edges: props(2),
                },
                OrderedEdgeEmbeddingEvent::PatchEmbeddings {
                    key: k,
                    patch: good,
                },
            ])
            .unwrap();
        assert_eq!(replayed, already);
    }

    #[test]
    fn identical_parallel_properties_keep_prior_and_new_members_distinct() {
        let k = key(2);
        let before = group(1, &[0.1, 0.9], &[0.9, 0.1]);
        let after = LogicalGroupState {
            properties: props(1),
            stores: BTreeMap::from([("description".into(), vec![cell(&[0.9, 0.1]), None])]),
        };
        let delta = DeltaInterpreter::from_state(state(2, [(k.clone(), before.clone())]))
            .interpret([
                OrderedEdgeEmbeddingEvent::ReplaceTopology {
                    key: k.clone(),
                    edges: props(1),
                },
                OrderedEdgeEmbeddingEvent::PatchEmbeddings {
                    key: k.clone(),
                    patch: patch(
                        &before,
                        &after,
                        vec![
                            prior(1, EdgeVectorCellPatchWal::Keep),
                            EdgeGroupMemberPatchWal::New { cells: vec![None] },
                        ],
                    ),
                },
            ])
            .unwrap();
        assert_eq!(delta.groups[&k], after);
    }

    #[test]
    fn malformed_order_and_noncanonical_store_list_are_refused() {
        let k = key(2);
        let before = group(1, &[0.1, 0.9], &[0.9, 0.1]);
        let after = group(2, &[0.1, 0.9], &[0.4, 0.6]);
        let initial = state(2, [(k.clone(), before.clone())]);
        let topology = OrderedEdgeEmbeddingEvent::ReplaceTopology {
            key: k.clone(),
            edges: props(2),
        };
        let error = DeltaInterpreter::from_state(initial.clone())
            .interpret([
                topology.clone(),
                OrderedEdgeEmbeddingEvent::SetStore {
                    key: store_key(),
                    state: EdgeEmbeddingStoreState::Present {
                        dimension: 2,
                        metric: Some("cosine".into()),
                        model_id: None,
                    },
                },
            ])
            .unwrap_err();
        assert!(error.contains("between topology and embedding state"));

        let mut noncanonical = patch(
            &before,
            &after,
            vec![
                prior(0, EdgeVectorCellPatchWal::Keep),
                prior(
                    1,
                    EdgeVectorCellPatchWal::Replace(cell(&[0.4, 0.6]).unwrap()),
                ),
            ],
        );
        noncanonical.stores.push("description".into());
        let error = DeltaInterpreter::from_state(initial)
            .interpret([
                topology,
                OrderedEdgeEmbeddingEvent::PatchEmbeddings {
                    key: k,
                    patch: noncanonical,
                },
            ])
            .unwrap_err();
        assert!(error.contains("complete sorted store set"));
    }
}
