use serde::{Deserialize, Serialize};

/// Complete store-wide state carried by [`super::MutationOp::SetEdgeEmbeddingStore`].
///
/// **Variant order is on-disk format.** Append only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EdgeEmbeddingStoreState {
    Absent,
    Present {
        dimension: usize,
        metric: Option<String>,
        model_id: Option<String>,
    },
}

/// Complete vector and source-text provenance for one relationship.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EdgeVectorWalState {
    pub vector: Vec<f32>,
    /// `None` explicitly clears generated-text provenance for this member.
    pub text_hash: Option<u64>,
}

/// One complete store column over a parallel group's final ordered members.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EdgeGroupStoreWalState {
    /// Source text column, not the derived `_emb` store spelling.
    pub text_column: String,
    /// Exactly one cell per final group member.
    pub members: Vec<Option<EdgeVectorWalState>>,
}

pub type EdgeEmbeddingGroupDigest = [u8; 32];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EdgeVectorCellPatchWal {
    Keep,
    Replace(EdgeVectorWalState),
    Clear,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EdgeGroupMemberPatchWal {
    Prior {
        prior_ordinal: u32,
        cells: Vec<EdgeVectorCellPatchWal>,
    },
    New {
        cells: Vec<Option<EdgeVectorWalState>>,
    },
}

/// Relative vector state paired with the immediately preceding topology event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EdgeGroupEmbeddingPatchWal {
    pub base_digest: EdgeEmbeddingGroupDigest,
    pub result_digest: EdgeEmbeddingGroupDigest,
    /// Complete, sorted source-column order for every member's `cells`.
    pub stores: Vec<String>,
    /// Final member order; omitted prior ordinals are deletions.
    pub members: Vec<EdgeGroupMemberPatchWal>,
}
