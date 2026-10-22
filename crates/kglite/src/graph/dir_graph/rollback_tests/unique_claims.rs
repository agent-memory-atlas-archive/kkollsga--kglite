//! Unique-constraint claims across a rollback
//!
//! A constraint owns an entry in `property_indices` / `composite_indices`, so a
//! journal rollback reverses its claims with the bucket entries every user
//! index gets. These tests pin both directions of getting that wrong:
//!
//! - a **phantom claim** — a value the failed statement claimed stays occupied,
//!   so a later legitimate insert is rejected forever;
//! - a **lost claim** — a value the failed statement released stays free, so a
//!   real duplicate is admitted on the next write.
//!
//! The declared tuple is `Item.name`, an explicit non-`id` property, on purpose:
//! a primary key on `id` is held by the id index, so a constraint over `id`
//! would leave the owned index empty and make these tests vacuous.

use super::*;

/// One constraint's claimed values as `(value, holding slot)`, sorted.
type UniqueClaims = Vec<(String, usize)>;

/// `(node_type, constraint properties, claims)` per declared constraint.
pub(super) type UniqueFingerprint = Vec<(String, Vec<String>, UniqueClaims)>;

/// The whole occupancy, per declared constraint: constraint tuple → the claimed
/// values and the slot holding each. Slot-level so a claim that comes back
/// pointing at the wrong node is a failure, not just a missing one.
pub(super) fn unique_fingerprint(graph: &DirGraph) -> UniqueFingerprint {
    let mut out: Vec<(String, Vec<String>, UniqueClaims)> = graph
        .unique_constraint_keys
        .iter()
        .map(|(node_type, properties)| (node_type.clone(), properties.clone(), Vec::new()))
        .collect();
    for ((node_type, properties), value, idx) in graph.unique_occupancy() {
        if let Some(entry) = out
            .iter_mut()
            .find(|(nt, props, _)| *nt == node_type && *props == properties)
        {
            entry.2.push((value, idx));
        }
    }
    for entry in &mut out {
        entry.2.sort();
    }
    out.sort();
    out
}

/// `seeded()` plus a declared UNIQUE constraint over `Item.name`.
///
/// Asserts the graph still takes the journal path: the owned index is the only
/// index on the graph, and the backend still supports the undo journal. If that
/// ever changes, these tests would silently start exercising the clone path and
/// prove nothing — so the precondition is checked, not assumed.
pub(super) fn seeded_with_unique_name() -> DirGraph {
    let mut graph = seeded();
    run(
        &mut graph,
        "CREATE CONSTRAINT FOR (i:Item) REQUIRE i.name IS UNIQUE",
    );
    assert_eq!(
        graph.unique_constraint_keys.len(),
        1,
        "the constraint must be declared and enforcing"
    );
    assert!(
        graph.property_indices.len() == 1
            && graph.composite_indices.is_empty()
            && graph.range_indices.is_empty()
            && graph.graph.supports_undo_journal(),
        "the constraint's owned index must be the only index and the journal \
         must cover the graph, or these tests would exercise the clone \
         checkpoint instead of the journal"
    );
    graph
}

/// `seeded()` with a UNIQUE constraint over `Item.qty`, then saved.
///
/// `qty` rather than `name` because the columnar fast path deliberately skips
/// `name`/`title` (they fall through to the inline node setter), so a
/// constraint over `name` would exercise the ordinary journalled write and
/// prove nothing about the master side channel. The seeded `qty` values are
/// distinct, so the constraint is satisfiable.
fn seeded_columnar_with_unique_qty() -> DirGraph {
    let mut graph = seeded();
    run(
        &mut graph,
        "CREATE CONSTRAINT FOR (i:Item) REQUIRE i.qty IS UNIQUE",
    );
    graph.enable_columnar();
    assert_eq!(
        graph.unique_constraint_keys.len(),
        1,
        "the constraint must be declared and enforcing"
    );
    assert!(
        graph.column_store_count() > 0,
        "the graph must be saved, or this is the plain unique fixture again"
    );
    graph
}

/// A unique claim moved by a *columnar* `SET` must come back.
///
/// This is the shape with no `NodeWeight` entry behind it: the value goes into
/// the master column store, so the node's weight never changes and the journal
/// sees only `UndoEntry::ColumnarCell` entries. The owned index's bucket
/// entries, journalled by the index maintenance that follows the write, are what
/// move the claim back. The test measures the claim rather than the mechanism,
/// so it survives a rewrite of either: without the bucket undo a failed
/// columnar `SET` leaves the claim it took behind and the claim it released
/// free.
#[test]
fn rollback_restores_claims_moved_by_a_columnar_property_overwrite() {
    let mut graph = seeded_columnar_with_unique_qty();
    let before = unique_fingerprint(&graph);
    assert!(
        !before.is_empty() && !before[0].2.is_empty(),
        "the constraint must hold claims, or this test is vacuous"
    );

    let error = expect_failure(
        &mut graph,
        "MATCH (i:Item {id: 1}) SET i.qty = 999 \
         WITH i MATCH (j:Item {id: 2}) SET j.bad = duration({months: 2147483648})",
        None,
    );

    assert_eq!(
        unique_fingerprint(&graph),
        before,
        "a claim moved through the master column store must move back.\
         \nerror: {error}"
    );
    // The observable half: 10 is claimed again (no lost claim) and 999 is free
    // (no phantom occupant).
    expect_failure(&mut graph, "CREATE (:Item {id: 40, qty: 10})", None);
    run(&mut graph, "CREATE (:Item {id: 41, qty: 999})");
}

/// A statement that claims a new value and *then* fails must not leave the
/// claim behind. Without the rebuild, `'zeta'` stays occupied by a node that
/// no longer exists and every later insert of it is rejected forever.
#[test]
fn rollback_releases_a_claim_the_failed_statement_added() {
    let mut graph = seeded_with_unique_name();
    let before = unique_fingerprint(&graph);

    // First `CREATE` claims 'zeta'; the second collides with 'b' (held by the
    // Item seeded with id 2), so the statement fails after its first write.
    let error = expect_failure(
        &mut graph,
        "CREATE (:Item {id: 10, name: 'zeta'}), (:Item {id: 11, name: 'b'})",
        None,
    );

    assert_eq!(
        unique_fingerprint(&graph),
        before,
        "the rolled-back claim must be gone.\nerror: {error}"
    );
    // The observable half: 'zeta' is insertable, which it would not be if the
    // phantom claim survived.
    run(&mut graph, "CREATE (:Item {id: 13, name: 'zeta'})");
}

/// A statement that *releases* a claim by deleting its holder and then fails
/// must put the claim back. Without the rebuild, `'a'` stays free and a real
/// duplicate is admitted on the next write.
#[test]
fn rollback_restores_a_claim_the_failed_statement_released() {
    let mut graph = seeded_with_unique_name();
    let before = unique_fingerprint(&graph);

    let error = expect_failure(
        &mut graph,
        "MATCH (i:Item {id: 1}) DETACH DELETE i CREATE (:Item {id: 20, name: 'b'})",
        None,
    );

    assert_eq!(
        unique_fingerprint(&graph),
        before,
        "the released claim must be restored, pointing at the restored slot.\
         \nerror: {error}"
    );
    // The observable half: 'a' is claimed again, so a duplicate is refused.
    expect_failure(&mut graph, "CREATE (:Item {id: 21, name: 'a'})", None);
}

/// The `NodeWeight`-only shape: a property overwrite moves a claim from the old
/// value to the new one without touching identity, so only the owned index's
/// bucket entries can move it back.
#[test]
fn rollback_restores_claims_moved_by_a_property_overwrite() {
    let mut graph = seeded_with_unique_name();
    let before = unique_fingerprint(&graph);

    let error = expect_failure(
        &mut graph,
        "MATCH (i:Item {id: 1}) SET i.name = 'renamed' \
         WITH i MATCH (j:Item {id: 2}) SET j.bad = duration({months: 2147483648})",
        None,
    );

    assert_eq!(
        unique_fingerprint(&graph),
        before,
        "an overwritten claim must move back.\nerror: {error}"
    );
    // 'a' is claimed again (no lost claim) and 'renamed' is free (no phantom).
    expect_failure(&mut graph, "CREATE (:Item {id: 30, name: 'a'})", None);
    run(&mut graph, "CREATE (:Item {id: 31, name: 'renamed'})");
}
