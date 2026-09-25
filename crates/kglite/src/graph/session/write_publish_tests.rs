//! `Session::write` and `Session::transact` are commit boundaries: a change
//! they apply reaches the change-data-capture log without the caller draining
//! the capture buffer itself, and a change they discard never does.

use super::{execute_mut, ExecuteOptions, Session};
use crate::graph::cdc::{self, CdcEnrichment};
use crate::graph::dir_graph::DirGraph;
use std::collections::HashMap;

fn cdc_session() -> Session {
    let mut graph = DirGraph::new();
    cdc::enable(&mut graph, Some(64), CdcEnrichment::Off).unwrap();
    Session::new(graph)
}

fn published(session: &Session) -> usize {
    cdc::read(&session.snapshot(), 0, None, &[]).unwrap().len()
}

#[test]
fn a_write_guard_publishes_what_it_applied() {
    let session = cdc_session();
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    execute_mut(&mut session.write(), "CREATE (:N {id: 1})", &opts).unwrap();
    assert_eq!(published(&session), 1);
    execute_mut(&mut session.write(), "MATCH (n:N) SET n.x = 2", &opts).unwrap();
    assert_eq!(published(&session), 2);
}

#[test]
fn a_failed_statement_under_the_write_guard_publishes_nothing() {
    let session = cdc_session();
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    // The first row creates a node before the second row fails the statement.
    let failed = execute_mut(
        &mut session.write(),
        "UNWIND [1, 0] AS d CREATE (:N {v: 10 / d})",
        &opts,
    );
    assert!(failed.is_err());
    assert_eq!(published(&session), 0);
    execute_mut(&mut session.write(), "CREATE (:N {id: 1})", &opts).unwrap();
    assert_eq!(
        published(&session),
        1,
        "the rolled-back row never follows a later commit out"
    );
}

#[test]
fn transact_publishes_on_success_and_not_on_error() {
    let session = cdc_session();
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    session
        .transact(|working| {
            execute_mut(working, "CREATE (:N {id: 1}), (:N {id: 2})", &opts)
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
        .unwrap();
    assert_eq!(published(&session), 2);
    let failed: Result<(), &str> = session.transact(|working| {
        execute_mut(working, "CREATE (:N {id: 3})", &opts).unwrap();
        Err("abandon")
    });
    assert!(failed.is_err());
    assert_eq!(published(&session), 2);
}
