//! `DirGraph::graph_info` and the [`GraphInfo`] report it returns.

use super::DirGraph;
use crate::graph::storage::GraphRead;

impl DirGraph {
    /// Return diagnostic information about graph storage health. A
    /// `fragmentation_ratio` above 0.3 (the auto-vacuum default) is a good
    /// point to call [`Self::vacuum`].
    pub fn graph_info(&self) -> GraphInfo {
        let (columnar_total, columnar_live) = self.columnar_row_census();
        let (edges_mapped, edge_property_overlay_rows) = self.graph.edge_storage_info();
        let topology = self.graph.topology_memory();
        let node_count = self.graph.node_count();
        let node_bound = self.graph.node_bound();
        let edge_count = self.graph.edge_count();
        let edge_bound = self.graph.edge_bound();
        let node_tombstones = node_bound - node_count;
        let budget = self.write_budget_report();

        GraphInfo {
            node_count,
            node_capacity: node_bound,
            node_tombstones,
            edge_count,
            edge_capacity: edge_bound,
            edge_tombstones: edge_bound.saturating_sub(edge_count),
            fragmentation_ratio: if node_bound == 0 {
                0.0
            } else {
                node_tombstones as f64 / node_bound as f64
            },
            type_count: self.type_indices.len(),
            property_index_count: self.property_indices.len(),
            composite_index_count: self.composite_indices.len(),
            columnar_total_rows: columnar_total,
            columnar_live_rows: columnar_live,
            columnar_heap_bytes: self
                .graph
                .column_stores_iter()
                .map(|(_, s)| s.heap_bytes())
                .sum(),
            columnar_is_mapped: self.graph.column_stores_iter().any(|(_, s)| s.is_mapped()),
            edges_mapped,
            edge_property_overlay_rows,
            topology_mapped_bytes: topology.mapped_bytes,
            topology_heap_bytes: topology.heap_bytes,
            topology_overlay_entries: topology.overlay_entries,
            write_budget_bytes: budget.limit,
            write_budget_used_bytes: budget.used,
            write_budget_state: budget.state,
        }
    }
}

/// Diagnostic information about graph storage health.
#[derive(Debug, Clone)]
pub struct GraphInfo {
    pub node_count: usize,
    /// Upper bound of node indices (includes tombstones from deletions)
    pub node_capacity: usize,
    /// Number of tombstone slots (node_capacity - node_count)
    pub node_tombstones: usize,
    pub edge_count: usize,
    /// Upper bound of edge indices (includes slots freed by `DELETE r`).
    ///
    /// Always equal to [`Self::edge_count`] on the disk backend, whose edges
    /// are a frozen CSR generation with no free list.
    pub edge_capacity: usize,
    /// Number of free edge slots (`edge_capacity - edge_count`).
    ///
    /// The third garbage population, alongside node tombstones and dead
    /// columnar rows, and the only one a relationship-only delete workload
    /// produces. Reported separately rather than folded into
    /// [`Self::fragmentation_ratio`], which stays node-shaped so its documented
    /// meaning does not silently change; the auto-vacuum trigger takes the
    /// worst of all three.
    pub edge_tombstones: usize,
    /// Ratio of wasted node storage (0.0 = clean, approaching 1.0 = heavily
    /// fragmented). Node slots only — see [`Self::edge_tombstones`] and
    /// [`Self::columnar_total_rows`] for the other two populations.
    pub fragmentation_ratio: f64,
    pub type_count: usize,
    /// Number of single-property indexes
    pub property_index_count: usize,
    pub composite_index_count: usize,
    /// Total rows across all columnar stores (including orphaned from deletions)
    pub columnar_total_rows: usize,
    /// Rows backed by live nodes (columnar_total_rows - columnar_live_rows = orphaned)
    pub columnar_live_rows: usize,
    /// Heap bytes held by the column stores the backend owns. Exposed here so
    /// a binding can report columnar memory without reaching into storage.
    pub columnar_heap_bytes: usize,
    /// `true` when at least one column store has been spilled to mmap.
    ///
    /// Reports **property-column spilling** (`set_memory_limit`, or the
    /// `mapped` storage mode which pins that limit at 0) — not disk-mode
    /// health. A disk graph reports `false` here unless its *columns* also
    /// spilled; [`Self::edges_mapped`] is the disk-mode reading.
    pub columnar_is_mapped: bool,
    /// `true` when the edge CSR arrays are memory-mapped from files rather
    /// than heap-resident — the structure `enable_disk_mode()` materializes.
    /// Always `false` on the memory and mapped backends, which keep edges in
    /// the heap graph and have no CSR at all.
    pub edges_mapped: bool,
    /// Edges whose properties sit in the disk backend's heap mutation overlay
    /// instead of the mmap-backed columnar base. `0` on every non-disk
    /// backend.
    pub edge_property_overlay_rows: usize,
    /// Bytes of the topology served in place from a `.kgl`'s mapped sections
    /// (the frozen base of a mapped load). `0` when the topology is on the
    /// heap, was loaded with `topology="memory"`, or `KGLITE_MMAP=0` is set.
    /// These pages count toward RSS in proportion to the part queries touch,
    /// never toward private memory.
    pub topology_mapped_bytes: usize,
    /// Heap bytes of the topology. For a heap graph, the node and edge arrays
    /// (40 and 32 bytes per slot, excluding allocator slack); for a frozen
    /// base, only what a load copied or decoded (per-type templates, sparse
    /// edge properties, sections copied on Windows). `0` on the disk backend,
    /// whose CSR [`Self::edges_mapped`] reports. A write overlay's entries
    /// are heap too and are counted in [`Self::topology_overlay_entries`].
    pub topology_heap_bytes: usize,
    /// Operations, node weights and edge weights the copy-on-write overlay
    /// over a frozen base holds. `0` when no write has touched the topology.
    pub topology_overlay_entries: usize,
    /// The limit, in bytes, on unsaved writes held over the mapped file: the
    /// `write_budget` the graph was loaded with (default `max(512 MiB, half
    /// the file)`). `None` when no budget applies or the load asked for none;
    /// [`Self::write_budget_state`] says which.
    pub write_budget_bytes: Option<u64>,
    /// Estimated heap, in bytes, of the writes since the load or the last
    /// save that rebased the graph. `0` when no budget applies.
    pub write_budget_used_bytes: u64,
    /// Where the graph stands against the budget; see [`WriteBudgetState`].
    pub write_budget_state: super::WriteBudgetState,
}
