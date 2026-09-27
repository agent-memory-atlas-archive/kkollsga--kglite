//! The valid slice's heap is bounded by what it keeps, in every storage
//! mode: it never copies a base type's column store (a mapped or Disk store
//! is file-backed, so a copy would put the whole type on the heap), and a
//! Disk walk holds nothing sized by the graph before its element cap refuses.

use std::collections::HashMap;
use std::sync::Arc;

use super::*;
use crate::datatypes::values::Value;
use crate::graph::core::graph_filter::{GraphFilter, ValidTimeSelector};
use crate::graph::features::temporal::declarations::{declare, TemporalTarget};
use crate::graph::features::temporal::eval::IntervalConvention;
use crate::graph::languages::cypher::valid_time::declared_template;
use crate::graph::session::execute::{execute_mut, ExecuteOptions};
use crate::graph::storage::mode::{new_dir_graph_in_mode, StorageMode};
use crate::test_alloc::peak_during;

const ROWS: i64 = 100_000;

/// `ROWS` declared `T` nodes with four integer columns in `mode`; the first
/// `valid` of them are valid at 2020-06-01, the rest closed in 2010.
fn wide(mode: StorageMode, dir: &tempfile::TempDir, valid: i64) -> DirGraph {
    let path = matches!(mode, StorageMode::Disk).then(|| dir.path());
    let mut g = new_dir_graph_in_mode(mode, path).expect("graph");
    let params = HashMap::from([
        ("n".to_string(), Value::Int64(ROWS)),
        ("v".to_string(), Value::Int64(valid)),
    ]);
    execute_mut(
        &mut g,
        "UNWIND range(1, $n) AS i CREATE (:T {id: i, a: i, b: 2 * i, c: 3 * i, d: 4 * i, \
         vf: date('2000-01-01'), \
         vt: CASE WHEN i <= $v THEN date('2090-01-01') ELSE date('2010-01-01') END})",
        &ExecuteOptions::eager(&params),
    )
    .expect("load");
    declare(
        &mut g,
        &TemporalTarget::Node("T".into()),
        "vf",
        "vt",
        IntervalConvention::Closed,
    )
    .unwrap();
    g
}

fn filter(g: &DirGraph) -> Option<ElementFilter> {
    let filter = GraphFilter {
        template: Arc::new(declared_template(g).unwrap()),
        selector: ValidTimeSelector::AsOf(Instant::Date(
            chrono::NaiveDate::from_ymd_opt(2020, 6, 1).unwrap(),
        )),
    };
    let resolved = filter.resolve(g);
    ElementFilter::new(&filter, resolved)
}

/// A fixed allowance for the fresh graph's own scaffolding (its interner and
/// schema copies, empty indexes), independent of the base's size.
const SCAFFOLD: usize = 256 << 10;

#[test]
fn a_slice_of_a_mapped_graph_holds_only_its_kept_rows() {
    let dir = tempfile::tempdir().unwrap();
    let g = wide(StorageMode::Mapped, &dir, 10);
    let t = InternedKey::from_str("T");
    let base_store = g.graph.column_store(t).expect("store");
    assert!(
        base_store.heap_bytes() < 2 * ROWS as usize,
        "premise: the base store is file-backed ({} heap bytes)",
        base_store.heap_bytes()
    );
    let filter = filter(&g);
    let caps = SliceCaps {
        bytes: usize::MAX,
        disk_elements: None,
    };
    let (slice, peak) = peak_during(|| slice_at(&g, filter.as_ref(), caps).unwrap());
    assert_eq!(slice.graph().graph.node_count(), 10);
    let store = slice.graph().graph.column_store(t).expect("slice store");
    assert!(
        !Arc::ptr_eq(store, base_store),
        "the slice shares the base store"
    );
    assert!(
        slice.bytes() >= store.heap_bytes(),
        "bytes() {} does not count the slice's store ({})",
        slice.bytes(),
        store.heap_bytes()
    );
    assert!(
        peak <= 4 * slice.bytes() + SCAFFOLD,
        "slicing 10 of {ROWS} rows grew the heap by {peak} bytes; the slice counts {}",
        slice.bytes()
    );
}

#[test]
fn bytes_is_what_the_slice_holds_in_every_mode() {
    for mode in [StorageMode::Memory, StorageMode::Mapped, StorageMode::Disk] {
        let dir = tempfile::tempdir().unwrap();
        let g = wide(mode, &dir, 50);
        let filter = filter(&g);
        let caps = SliceCaps::for_graph(&g);
        let slice = slice_at(&g, filter.as_ref(), caps).unwrap();
        let t = InternedKey::from_str("T");
        let stores = slice
            .graph()
            .graph
            .column_store(t)
            .map_or(0, |s| s.heap_bytes());
        assert_eq!(slice.graph().graph.node_count(), 50, "{mode:?}");
        assert!(slice.bytes() >= stores, "{mode:?}");
        assert!(
            slice.bytes() < 64 << 10,
            "{mode:?}: 50 kept rows estimated at {} bytes",
            slice.bytes()
        );
    }
}

#[test]
fn a_disk_walk_holds_nothing_sized_by_the_graph_before_its_cap_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let g = wide(StorageMode::Disk, &dir, ROWS);
    let filter = filter(&g);
    for cap in [1, 1_000] {
        let caps = SliceCaps {
            bytes: usize::MAX,
            disk_elements: Some(cap),
        };
        let (result, peak) = peak_during(|| slice_at(&g, filter.as_ref(), caps));
        let err = result.unwrap_err();
        assert!(err.contains(&format!("more than {cap}")), "{err}");
        assert!(
            peak <= 64 * cap + SCAFFOLD,
            "a cap of {cap} over {ROWS} nodes grew the heap by {peak} bytes"
        );
    }
}
