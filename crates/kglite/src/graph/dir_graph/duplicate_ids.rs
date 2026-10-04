//! The duplicate-id warning: raised where an id index collapses two nodes of
//! one type onto one id, and carried by the statement that caused it.

/// Report that a type's id index collapses duplicate ids — `MATCH (n {id: …})`
/// then returns only one node per id. Detected where the index already probes
/// the id (a build, a bulk fold, a `CREATE` into a cached index) rather than by
/// a per-mutation scan, so bulk `UNWIND … CREATE` and `add_nodes` stay O(n), not
/// O(n²).
///
/// Inside a Cypher statement (see [`collect_id_warnings`]) the message joins
/// that statement's warnings, so it reaches `result.warnings` and every
/// binding's diagnostics, echoed like any other query warning. Outside one —
/// a bulk loader, an index rebuild on load, a parallel worker thread — there is
/// no statement to carry it, and it goes to stderr, rate-limited.
pub(crate) fn warn_on_duplicate_ids(node_type: &str, entry_count: usize, unique_count: usize) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static WARN_COUNT: AtomicUsize = AtomicUsize::new(0);
    if unique_count >= entry_count {
        return;
    }
    let dups = entry_count - unique_count;
    let message = format!(
        "{dups} duplicate id(s) on type '{node_type}' — `MATCH (n {{id: …}})` returns \
         only one node per id. ids must be unique: MERGE on the id alone, or dedupe the \
         input."
    );
    let collected = STATEMENT_ID_WARNINGS.with(|slot| match slot.borrow_mut().as_mut() {
        Some(warnings) => {
            if !warnings.iter().any(|(_, seen)| *seen == message) {
                warnings.push((node_type.to_string(), message.clone()));
            }
            true
        }
        None => false,
    });
    if collected {
        return;
    }
    let seen = WARN_COUNT.fetch_add(1, Ordering::Relaxed);
    if seen < 5 {
        eprintln!("warning: {message}");
    } else if seen == 5 {
        eprintln!("warning: further duplicate-id warnings suppressed.");
    }
}

thread_local! {
    /// The duplicate-id warnings of the statement running on this thread, or
    /// `None` outside [`collect_id_warnings`].
    static STATEMENT_ID_WARNINGS: std::cell::RefCell<Option<Vec<(String, String)>>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `f` as one statement, returning the duplicate-id warnings raised on
/// this thread while it ran (see [`warn_on_duplicate_ids`]). Nests: an inner
/// statement's warnings are its own, and the outer collection resumes after it.
pub(crate) fn collect_id_warnings<R>(f: impl FnOnce() -> R) -> (R, Vec<String>) {
    let (result, warnings) = collect_id_warnings_by_type(f);
    (result, warnings.into_iter().map(|(_, m)| m).collect())
}

/// [`collect_id_warnings`] with the node type each warning names, for a caller
/// that drops the warnings of types it later learns are declared versioned (a
/// blueprint build declares its temporal labels after the rows load).
pub(crate) fn collect_id_warnings_by_type<R>(f: impl FnOnce() -> R) -> (R, Vec<(String, String)>) {
    /// Restores the outer collection even when `f` unwinds, so a panicking
    /// statement cannot leave this thread collecting into a dropped list.
    struct Restore(Option<Vec<(String, String)>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let outer = self.0.take();
            STATEMENT_ID_WARNINGS.with(|slot| *slot.borrow_mut() = outer);
        }
    }
    let restore = Restore(STATEMENT_ID_WARNINGS.with(|slot| slot.borrow_mut().replace(Vec::new())));
    let result = f();
    let collected = STATEMENT_ID_WARNINGS.with(|slot| slot.borrow_mut().take().unwrap_or_default());
    drop(restore);
    (result, collected)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outside_any_statement() -> bool {
        STATEMENT_ID_WARNINGS.with(|slot| slot.borrow().is_none())
    }

    /// A statement's duplicate-id warnings are its own: a nested statement
    /// keeps its warnings, the outer one resumes after it, and a repeat
    /// message is reported once.
    #[test]
    fn duplicate_id_warnings_belong_to_the_innermost_statement() {
        let ((), outer) = collect_id_warnings(|| {
            warn_on_duplicate_ids("A", 2, 1);
            let ((), inner) = collect_id_warnings(|| warn_on_duplicate_ids("B", 3, 1));
            assert_eq!(inner.len(), 1, "{inner:?}");
            assert!(
                inner[0].starts_with("2 duplicate id(s) on type 'B'"),
                "{inner:?}"
            );
            warn_on_duplicate_ids("A", 2, 1);
            warn_on_duplicate_ids("A", 5, 5);
        });
        assert_eq!(outer.len(), 1, "{outer:?}");
        assert!(
            outer[0].contains("MERGE on the id alone, or dedupe the input"),
            "{outer:?}"
        );
        assert!(outside_any_statement());
    }

    /// The typed collection names the node type of each warning, once per
    /// message, and is not capped by the stderr rate limit.
    #[test]
    fn typed_collection_names_each_warnings_type_without_a_cap() {
        let ((), warnings) = collect_id_warnings_by_type(|| {
            for _ in 0..10 {
                warn_on_duplicate_ids("A", 2, 1);
            }
            warn_on_duplicate_ids("B", 3, 1);
        });
        let types: Vec<&str> = warnings.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(types, ["A", "B"], "{warnings:?}");
        let ((), again) = collect_id_warnings_by_type(|| warn_on_duplicate_ids("A", 2, 1));
        assert_eq!(again.len(), 1, "a later collection warns afresh");
    }

    /// A statement that unwinds does not leave the thread collecting into a
    /// list nobody reads.
    #[test]
    fn a_panicking_statement_restores_the_outer_collection() {
        let unwound = std::panic::catch_unwind(|| {
            collect_id_warnings(|| {
                warn_on_duplicate_ids("A", 2, 1);
                panic!("statement failed");
            })
        });
        assert!(unwound.is_err());
        assert!(outside_any_statement());
    }
}
