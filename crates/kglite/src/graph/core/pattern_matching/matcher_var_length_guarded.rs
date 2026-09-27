//! Variable-length segment expansion under a resolved [`ElementFilter`] (a
//! statement under `FOR VALID_TIME AS OF`).
//!
//! The plain expansions in `matcher_var_length.rs` carry no filter test: a
//! test in their shared per-relationship accept cost the unfiltered deep
//! multi-hop cells 4–12%, so the filter is chosen once per segment call in
//! `expand_from_node` and the three loops are cloned here. Each clone differs
//! from its plain twin in one place: a relationship is followed only when it
//! and the node it reaches are admitted, tested before that node is marked
//! visited, queued or emitted. An invisible node therefore neither answers
//! nor carries a path, and a bound relationship list holds only admitted
//! relationships. The segment's source is a guarded anchor or an earlier
//! hop's admitted target, so a zero-length path needs no test.
//!
//! The clones keep their twins' semantics — the distance frontier only for
//! `min_hops <= 1` without a trail, the source's own role, the closed-trail
//! probe and its budget, the caps and ceilings. A change to either side
//! belongs in both: `test_the_guarded_var_length_expansion_matches_the_plain_one`
//! (`tests/test_cypher_valid_time.py`) runs every corpus variable-length entry
//! through both under a filter that admits everything the entry reaches.

use super::var_length::VarLengthSegment;
use super::*;
use crate::graph::core::iterators::GraphEdgeRef;
use fixedbitset::FixedBitSet;
use petgraph::graph::EdgeIndex;
use rustc_hash::FxHashSet;
use std::collections::VecDeque;

/// The plain expansion's closed-trail probe budget (see
/// `CLOSED_TRAIL_PROBE_BUDGET` there); exhausting it sends the segment to
/// the trail expansion.
const CLOSED_TRAIL_PROBE_BUDGET: usize = 20_000;

/// Marks one row may hold in its hash set before they move to a bit set.
const SPARSE_MARKS: usize = 128;

/// One row's "already reached" marks for the guarded distance frontier. A
/// capped row (an existence check) may stop after a handful of nodes, so it
/// starts in a hash set and moves to a bit set over the graph's node bound
/// past [`SPARSE_MARKS`]; an uncapped row sweeps its whole reach and starts
/// with the bit set. Per row: at most `node_bound / 8` bytes per source.
struct RowMarks {
    sparse: FxHashSet<usize>,
    dense: Option<FixedBitSet>,
}

impl RowMarks {
    fn new(node_bound: usize, may_stop_early: bool) -> Self {
        RowMarks {
            sparse: FxHashSet::default(),
            dense: (!may_stop_early).then(|| FixedBitSet::with_capacity(node_bound)),
        }
    }

    #[inline]
    fn is_marked(&self, index: usize) -> bool {
        match &self.dense {
            Some(bits) => bits.contains(index),
            None => self.sparse.contains(&index),
        }
    }

    #[inline]
    fn mark(&mut self, index: usize, node_bound: usize) {
        if let Some(bits) = &mut self.dense {
            bits.grow(index + 1);
            bits.insert(index);
            return;
        }
        self.sparse.insert(index);
        if self.sparse.len() >= SPARSE_MARKS {
            let mut bits = FixedBitSet::with_capacity(node_bound.max(index + 1));
            for &marked in &self.sparse {
                bits.grow(marked + 1);
                bits.insert(marked);
            }
            self.sparse.clear();
            self.dense = Some(bits);
        }
    }
}

enum Closed {
    Found(usize),
    Absent,
    Undecided,
}

/// A segment's relationship types, interned once per expansion.
struct SegmentTypes {
    any_of: Option<Vec<InternedKey>>,
    one: Option<InternedKey>,
}

impl SegmentTypes {
    fn new(edge_pattern: &EdgePattern) -> Self {
        let any_of: Option<Vec<InternedKey>> = edge_pattern
            .connection_types
            .as_ref()
            .map(|types| types.iter().map(|t| InternedKey::from_str(t)).collect());
        let one = if any_of.is_none() {
            edge_pattern
                .connection_type
                .as_ref()
                .map(|ct| InternedKey::from_str(ct))
        } else {
            None
        };
        SegmentTypes { any_of, one }
    }

    #[inline]
    fn accepts(&self, conn_type: InternedKey) -> bool {
        match (&self.any_of, self.one) {
            (Some(keys), _) => keys.contains(&conn_type),
            (None, Some(key)) => conn_type == key,
            (None, None) => true,
        }
    }
}

#[inline]
fn cap_reached(found: usize, max_results: Option<usize>) -> bool {
    max_results.is_some_and(|max| found >= max)
}

fn segment_directions(edge_pattern: &EdgePattern) -> &'static [Direction] {
    match edge_pattern.direction {
        EdgeDirection::Outgoing => &[Direction::Outgoing],
        EdgeDirection::Incoming => &[Direction::Incoming],
        EdgeDirection::Both => &[Direction::Outgoing, Direction::Incoming],
    }
}

impl PatternExecutor<'_> {
    /// `Some((type, far node))` when the segment may follow `edge` in
    /// `direction`: its type and properties match, and the filter admits the
    /// relationship (keyed on its own source) and the node it reaches.
    #[inline]
    fn guarded_step(
        &self,
        filter: &ElementFilter,
        edge: &GraphEdgeRef<'_>,
        direction: Direction,
        edge_pattern: &EdgePattern,
        types: &SegmentTypes,
    ) -> Option<(InternedKey, NodeIndex)> {
        let conn_type = edge.connection_type();
        if !types.accepts(conn_type) {
            return None;
        }
        if let Some(props) = edge_pattern.properties.as_ref() {
            let edge_data = edge.weight();
            let matches = props.iter().all(|(key, matcher)| {
                edge_data
                    .get_property(key)
                    .map(|v| self.value_matches(v, matcher))
                    .unwrap_or_else(|| matcher.accepts_absent())
            });
            if !matches {
                return None;
            }
        }
        let far = match direction {
            Direction::Outgoing => edge.target(),
            Direction::Incoming => edge.source(),
        };
        filter
            .admits_hop(self.graph, edge.id(), conn_type, edge.source(), far)
            .then_some((conn_type, far))
    }

    /// The target test of the plain expansion, `skip_target_type_check`
    /// included (the planner never sets it under a filter).
    #[inline]
    fn guarded_target_matches(
        &self,
        idx: NodeIndex,
        edge_pattern: &EdgePattern,
        node_pattern: &NodePattern,
    ) -> bool {
        if !edge_pattern.skip_target_type_check
            && !self.node_matches_pattern_labels(idx, node_pattern)
        {
            return false;
        }
        match node_pattern.properties.as_ref() {
            Some(props) => self.node_matches_properties(idx, props),
            None => true,
        }
    }

    /// The zero-hop arm's target test: labels are always checked.
    #[inline]
    fn guarded_source_matches(&self, idx: NodeIndex, node_pattern: &NodePattern) -> bool {
        self.node_matches_pattern_labels(idx, node_pattern)
            && match node_pattern.properties.as_ref() {
                Some(props) => self.node_matches_properties(idx, props),
                None => true,
            }
    }

    #[inline]
    fn guarded_path_binding(
        source: NodeIndex,
        target: NodeIndex,
        hops: usize,
        path: Vec<PathHop>,
    ) -> MatchBinding {
        MatchBinding::VariableLengthPath {
            source,
            target,
            hops,
            path,
        }
    }

    /// [`Self::expand_var_length`] under `filter`.
    #[cold]
    #[inline(never)]
    pub(super) fn expand_var_length_guarded(
        &self,
        filter: &ElementFilter,
        source: NodeIndex,
        segment: &VarLengthSegment<'_>,
        max_results: Option<usize>,
    ) -> Result<Vec<(NodeIndex, MatchBinding)>, String> {
        if !segment.edge.needs_path_info && segment.min_hops <= 1 {
            if let Some(fast) =
                self.expand_var_length_fast_guarded(filter, source, segment, max_results)?
            {
                return Ok(fast);
            }
        }
        self.expand_var_length_trails_guarded(filter, source, segment, max_results)
    }

    /// The trail expansion under `filter`: a relationship and the node it
    /// reaches are tested before the hop joins the frontier.
    fn expand_var_length_trails_guarded(
        &self,
        filter: &ElementFilter,
        source: NodeIndex,
        segment: &VarLengthSegment<'_>,
        max_results: Option<usize>,
    ) -> Result<Vec<(NodeIndex, MatchBinding)>, String> {
        let VarLengthSegment {
            edge: edge_pattern,
            node: node_pattern,
            min_hops,
            max_hops,
        } = *segment;
        let mut results = Vec::new();
        let directions = segment_directions(edge_pattern);
        let types = SegmentTypes::new(edge_pattern);
        let mut queue: VecDeque<(NodeIndex, usize, Vec<PathHop>)> = VecDeque::new();
        queue.push_back((source, 0, Vec::new()));

        if min_hops == 0 && self.guarded_source_matches(source, node_pattern) {
            results.push((
                source,
                Self::guarded_path_binding(source, source, 0, Vec::new()),
            ));
            if cap_reached(results.len(), max_results) {
                return Ok(results);
            }
        }
        if self.connection_types_absent(edge_pattern) {
            return Ok(results);
        }

        let mut popped: usize = 0;
        while let Some((current, depth, path)) = queue.pop_front() {
            popped += 1;
            if popped.is_multiple_of(512) {
                if let Some(msg) = self.interrupt_reason() {
                    return Err(msg);
                }
                self.check_match_ceiling(queue.len())?;
            }
            if depth >= max_hops {
                continue;
            }
            let mut valid_targets: Vec<PathHop> = Vec::new();
            for &direction in directions {
                for edge in self
                    .graph
                    .graph
                    .edges_directed_filtered(current, direction, types.one)
                {
                    let Some((conn_type, target)) =
                        self.guarded_step(filter, &edge, direction, edge_pattern, &types)
                    else {
                        continue;
                    };
                    let edge_index = edge.id();
                    // Relationship-unique trails; a self-loop appears in both
                    // directional iterators and is one candidate.
                    if path.iter().any(|hop| hop.edge == edge_index)
                        || valid_targets.iter().any(|hop| hop.edge == edge_index)
                    {
                        continue;
                    }
                    valid_targets.push(PathHop {
                        node: target,
                        edge: edge_index,
                        connection_type: conn_type,
                    });
                }
            }

            let new_depth = depth + 1;
            for hop in valid_targets {
                let target = hop.node;
                let needs_queue = new_depth < max_hops;
                let mut new_path = path.clone();
                new_path.push(hop);
                if new_depth >= min_hops
                    && self.guarded_target_matches(target, edge_pattern, node_pattern)
                {
                    let path_for_binding = if needs_queue {
                        new_path.clone()
                    } else {
                        std::mem::take(&mut new_path)
                    };
                    results.push((
                        target,
                        Self::guarded_path_binding(source, target, new_depth, path_for_binding),
                    ));
                    if cap_reached(results.len(), max_results) {
                        return Ok(results);
                    }
                    self.check_match_ceiling(results.len())?;
                }
                if needs_queue {
                    queue.push_back((target, new_depth, new_path));
                }
            }
        }
        Ok(results)
    }

    /// The distance frontier under `filter` (`min_hops <= 1`, no trail).
    /// `Ok(None)` when the closed-trail probe ran out of budget.
    fn expand_var_length_fast_guarded(
        &self,
        filter: &ElementFilter,
        source: NodeIndex,
        segment: &VarLengthSegment<'_>,
        max_results: Option<usize>,
    ) -> Result<Option<Vec<(NodeIndex, MatchBinding)>>, String> {
        let VarLengthSegment {
            edge: edge_pattern,
            node: node_pattern,
            min_hops,
            max_hops,
        } = *segment;
        debug_assert!(min_hops <= 1, "the distance frontier needs min_hops <= 1");
        let directions = segment_directions(edge_pattern);
        let types = SegmentTypes::new(edge_pattern);
        let mut results: Vec<(NodeIndex, MatchBinding)> = Vec::new();
        // The source's role in its own segment, as the plain frontier decides
        // it: the zero-length row, rediscovery over a directed cycle (left
        // unvisited), or the undirected closed-trail probe.
        let mut leave_source_unvisited = false;
        let mut probe_pending = false;
        if min_hops == 0 {
            if self.guarded_source_matches(source, node_pattern) {
                results.push((
                    source,
                    Self::guarded_path_binding(source, source, 0, Vec::new()),
                ));
            }
        } else if max_hops > 0 && self.guarded_target_matches(source, edge_pattern, node_pattern) {
            if matches!(edge_pattern.direction, EdgeDirection::Both) {
                probe_pending = true;
            } else {
                leave_source_unvisited = true;
            }
        }
        if cap_reached(results.len(), max_results) || self.connection_types_absent(edge_pattern) {
            return Ok(Some(results));
        }

        let node_bound = self.graph.graph.node_bound();
        let mut marks = RowMarks::new(node_bound, max_results.is_some());
        if !leave_source_unvisited {
            marks.mark(source.index(), node_bound);
        }
        let mut queue: VecDeque<(NodeIndex, usize)> = VecDeque::new();
        queue.push_back((source, 0));
        let mut popped: usize = 0;
        while let Some((current, depth)) = queue.pop_front() {
            popped += 1;
            if popped & 511 == 0 {
                if let Some(msg) = self.interrupt_reason() {
                    return Err(msg);
                }
            }
            if depth >= max_hops {
                continue;
            }
            for &direction in directions {
                let mut inner: usize = 0;
                for edge in self
                    .graph
                    .graph
                    .edges_directed_filtered(current, direction, types.one)
                {
                    inner += 1;
                    if inner.is_multiple_of(1 << 20) {
                        if let Some(msg) = self.interrupt_reason() {
                            return Err(msg);
                        }
                    }
                    let far = match direction {
                        Direction::Outgoing => edge.target(),
                        Direction::Incoming => edge.source(),
                    };
                    // A reached node is never re-tested; an unreached one is
                    // marked only once a valid relationship reaches it.
                    if marks.is_marked(far.index())
                        || self
                            .guarded_step(filter, &edge, direction, edge_pattern, &types)
                            .is_none()
                    {
                        continue;
                    }
                    marks.mark(far.index(), node_bound);
                    let new_depth = depth + 1;
                    if new_depth >= min_hops
                        && self.guarded_target_matches(far, edge_pattern, node_pattern)
                    {
                        results.push((
                            far,
                            Self::guarded_path_binding(source, far, new_depth, Vec::new()),
                        ));
                        if cap_reached(results.len(), max_results) {
                            return Ok(Some(results));
                        }
                    }
                    if new_depth < max_hops && far != source {
                        queue.push_back((far, new_depth));
                    }
                }
            }
        }

        if probe_pending {
            match self.closed_trail_guarded(filter, source, edge_pattern, max_hops, &types)? {
                Closed::Found(hops) => {
                    results.insert(
                        0,
                        (
                            source,
                            Self::guarded_path_binding(source, source, hops, Vec::new()),
                        ),
                    );
                }
                Closed::Absent => {}
                Closed::Undecided => return Ok(None),
            }
        }
        Ok(Some(results))
    }

    /// The undirected closed-trail probe under `filter`: the shortest trail
    /// of `1..=max_hops` valid relationships through valid nodes that leaves
    /// and returns to `source`.
    fn closed_trail_guarded(
        &self,
        filter: &ElementFilter,
        source: NodeIndex,
        edge_pattern: &EdgePattern,
        max_hops: usize,
        types: &SegmentTypes,
    ) -> Result<Closed, String> {
        let mut queue: VecDeque<(NodeIndex, usize, Vec<EdgeIndex>)> = VecDeque::new();
        queue.push_back((source, 0, Vec::new()));
        let mut budget = CLOSED_TRAIL_PROBE_BUDGET;
        let mut popped: usize = 0;
        while let Some((current, depth, trail)) = queue.pop_front() {
            popped += 1;
            if popped.is_multiple_of(256) {
                if let Some(msg) = self.interrupt_reason() {
                    return Err(msg);
                }
            }
            for &direction in segment_directions(edge_pattern) {
                for edge in self
                    .graph
                    .graph
                    .edges_directed_filtered(current, direction, types.one)
                {
                    if budget == 0 {
                        return Ok(Closed::Undecided);
                    }
                    budget -= 1;
                    let Some((_, target)) =
                        self.guarded_step(filter, &edge, direction, edge_pattern, types)
                    else {
                        continue;
                    };
                    let edge_index = edge.id();
                    if trail.contains(&edge_index) {
                        continue;
                    }
                    if target == source {
                        return Ok(Closed::Found(depth + 1));
                    }
                    if depth + 1 < max_hops {
                        let mut next = trail.clone();
                        next.push(edge_index);
                        queue.push_back((target, depth + 1, next));
                    }
                }
            }
        }
        Ok(Closed::Absent)
    }
}
