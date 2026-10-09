//! `open_path`: the lease-before-open order, the degrade rule, conversion
//! reporting and recovery advisories.

use super::open_path::{open_path, open_path_observed, OpenError, OpenSpec, OpenStep};
use crate::graph::io::open::OpenDisposition;
use crate::graph::storage::mode::StorageMode;
use crate::graph::wal::DurabilityLevel;
use std::time::Duration;

#[test]
fn the_lease_is_taken_before_the_graph_is_opened() {
    let dir = tempfile::tempdir().unwrap();
    let mut steps = Vec::new();
    let opened = open_path_observed(&dir.path().join("g.kgl"), &OpenSpec::writer(), &mut |s| {
        steps.push(s)
    })
    .unwrap();
    assert_eq!(
        steps,
        [
            OpenStep::LeaseAcquired,
            OpenStep::GraphOpened,
            OpenStep::SessionOpened
        ]
    );
    assert!(opened.lease.is_some());
    assert_eq!(opened.disposition, OpenDisposition::Created);
    assert_eq!(opened.live_mode, StorageMode::Memory);
    assert!(opened.advisories.is_empty());
}

#[test]
fn a_second_writer_is_refused_at_the_lease_and_reads_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("g.kgl");
    let _first = open_path(&path, &OpenSpec::writer()).unwrap();
    let mut steps = Vec::new();
    let err = open_path_observed(&path, &OpenSpec::writer(), &mut |s| steps.push(s))
        .err()
        .expect("lease held");
    assert!(matches!(err, OpenError::Lease(_)), "{err}");
    assert!(steps.is_empty(), "the graph must not be read first");
}

#[test]
fn no_lease_timeout_takes_no_lease() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("g.kgl");
    let writer = open_path(&path, &OpenSpec::writer()).unwrap();
    writer.session.save(&path.to_string_lossy(), true).unwrap();
    let spec = OpenSpec {
        lease_timeout: None,
        storage: None,
        ..OpenSpec::writer()
    };
    let reader = open_path(&path, &spec).unwrap();
    assert!(reader.lease.is_none());
}

#[test]
fn a_logging_level_opens_a_durable_session() {
    let dir = tempfile::tempdir().unwrap();
    let spec = OpenSpec {
        durability: DurabilityLevel::Full,
        ..OpenSpec::writer()
    };
    let opened = open_path(&dir.path().join("g.kgl"), &spec).unwrap();
    assert_eq!(opened.durability, DurabilityLevel::Full);
    assert_eq!(opened.degraded_from, None);
    assert!(opened.session.next_lsn().is_some(), "a log is attached");
}

#[test]
fn a_default_logging_level_degrades_on_disk_but_an_explicit_one_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let inherited = OpenSpec {
        storage: Some(StorageMode::Disk),
        durability: DurabilityLevel::Normal,
        durability_explicit: false,
        lease_timeout: Some(Duration::ZERO),
        valid_time_default: None,
    };
    let opened = open_path(&dir.path().join("a"), &inherited).unwrap();
    assert_eq!(opened.durability, DurabilityLevel::Off);
    assert_eq!(opened.degraded_from, Some(DurabilityLevel::Normal));
    assert_eq!(opened.live_mode, StorageMode::Disk);
    drop(opened);

    let explicit = OpenSpec {
        durability_explicit: true,
        ..inherited
    };
    let err = open_path(&dir.path().join("b"), &explicit)
        .err()
        .expect("explicit level on disk");
    assert!(matches!(err, OpenError::Session { .. }), "{err}");
    assert!(err.to_string().contains("disk"), "{err}");
}

#[test]
fn a_conversion_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("g.kgl");
    let opened = open_path(&path, &OpenSpec::writer()).unwrap();
    opened.session.save(&path.to_string_lossy(), true).unwrap();
    drop(opened);
    let spec = OpenSpec {
        storage: Some(StorageMode::Mapped),
        ..OpenSpec::writer()
    };
    let converted = open_path(&path, &spec).unwrap();
    assert_eq!(converted.converted_from, Some(StorageMode::Memory));
    assert_eq!(converted.live_mode, StorageMode::Mapped);
    assert_eq!(converted.disposition, OpenDisposition::Opened);
}
