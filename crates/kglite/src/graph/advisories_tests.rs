use super::advisories::{compute_advisories, data_advisories, parse_version};
use crate::graph::dir_graph::DirGraph;
use crate::graph::io::file::{load_kgl_bytes, write_kgl_to, FileMetadata};
use crate::graph::schema::{ConnectionTypeInfo, ConnectivityTriple};
use std::sync::Arc;

fn roundtrip(graph: &DirGraph) -> Arc<DirGraph> {
    let mut bytes = Vec::new();
    write_kgl_to(graph, &mut bytes).unwrap();
    load_kgl_bytes(&bytes).unwrap()
}

fn stamped(oldest: &str) -> DirGraph {
    let mut graph = DirGraph::new();
    graph.save_metadata.oldest_writer = oldest.to_string();
    graph
}

fn with_connection(graph: &mut DirGraph, name: &str, sources: &[&str], prop: bool) {
    with_link(graph, name, sources, &[], prop);
}

fn with_link(graph: &mut DirGraph, name: &str, sources: &[&str], targets: &[&str], prop: bool) {
    let names = |list: &[&str]| list.iter().map(|s| s.to_string()).collect();
    let mut info = ConnectionTypeInfo {
        source_types: names(sources),
        target_types: names(targets),
        ..Default::default()
    };
    if prop {
        info.property_types.insert("since".into(), "Int64".into());
    }
    Arc::make_mut(&mut graph.connection_type_metadata).insert(name.to_string(), info);
}

#[test]
fn versions_parse_with_or_without_a_suffix_and_refuse_junk() {
    assert_eq!(parse_version("0.19.2"), Some((0, 19, 2)));
    assert_eq!(parse_version("0.19.2-dev"), Some((0, 19, 2)));
    assert_eq!(parse_version(""), None);
    assert_eq!(parse_version("0.19"), None);
    assert_eq!(parse_version("abc"), None);
}

/// A graph built and saved by one version writes no `oldest_writer` key (the
/// golden digests hold); one carrying an older writer writes it.
#[test]
fn oldest_writer_is_written_only_when_it_differs_from_the_saving_version() {
    let fresh = serde_json::to_string(&FileMetadata::from_graph(&DirGraph::new())).unwrap();
    assert!(!fresh.contains("oldest_writer"), "{fresh}");
    let current = env!("CARGO_PKG_VERSION");
    let same = serde_json::to_string(&FileMetadata::from_graph(&stamped(current))).unwrap();
    assert!(!same.contains("oldest_writer"), "{same}");
    let older = serde_json::to_string(&FileMetadata::from_graph(&stamped("0.18.0"))).unwrap();
    assert!(older.contains("\"oldest_writer\":\"0.18.0\""), "{older}");
}

/// The writer survives a re-save under a newer version (the laundering case):
/// load reads `oldest_writer` ahead of `library_version`, save keeps the
/// older of it and the running version.
#[test]
fn the_oldest_writer_survives_a_resave() {
    let loaded = roundtrip(&stamped("0.18.0"));
    assert_eq!(loaded.save_metadata.oldest_writer, "0.18.0");
    assert_eq!(
        loaded.save_metadata.library_version,
        env!("CARGO_PKG_VERSION")
    );
    let again = roundtrip(&loaded);
    assert_eq!(again.save_metadata.oldest_writer, "0.18.0");
}

/// A file without the field reads its writer as the stamped `library_version`.
#[test]
fn a_file_without_the_field_reads_its_library_version() {
    let loaded = roundtrip(&DirGraph::new());
    assert_eq!(
        loaded.save_metadata.oldest_writer,
        env!("CARGO_PKG_VERSION")
    );
}

#[test]
fn folded_history_needs_the_old_writer_and_the_multi_source_shape() {
    let mut graph = stamped("0.18.1");
    with_connection(&mut graph, "HAS_ROLE", &["Person", "Team"], true);
    let found = compute_advisories(&graph);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].code, "folded_history_edges");
    assert_eq!(found[0].affected, vec!["HAS_ROLE".to_string()]);

    // Same data, current writer: silent. Old writer, clean data: silent.
    let mut current = stamped("0.19.0");
    with_connection(&mut current, "HAS_ROLE", &["Person", "Team"], true);
    assert!(compute_advisories(&current).is_empty());
    let mut clean = stamped("0.18.1");
    with_connection(&mut clean, "HAS_ROLE", &["Person"], true);
    with_connection(&mut clean, "KNOWS", &["Person", "Team"], false);
    assert!(compute_advisories(&clean).is_empty());
}

/// An unknown writer raises nothing, whatever the data looks like.
#[test]
fn an_unparseable_writer_raises_nothing() {
    for writer in ["", "unknown"] {
        let mut graph = stamped(writer);
        with_connection(&mut graph, "HAS_ROLE", &["Person", "Team"], true);
        assert!(compute_advisories(&graph).is_empty(), "{writer:?}");
    }
}

fn triple(src: &str, conn: &str, tgt: &str, count: usize) -> ConnectivityTriple {
    ConnectivityTriple {
        src: src.into(),
        conn: conn.into(),
        tgt: tgt.into(),
        count,
    }
}

/// A graph whose metadata and connectivity say `Survey` links to
/// `SeismicSurvey` through each of `edges` with the given counts.
fn linked(writer: &str, parent: &str, edges: &[(&str, usize)]) -> DirGraph {
    let mut graph = stamped(writer);
    for (name, _) in edges {
        with_link(&mut graph, name, &["Survey"], &[parent], false);
    }
    graph.set_type_connectivity(
        edges
            .iter()
            .map(|(name, count)| triple("Survey", name, parent, *count))
            .collect(),
    );
    graph
}

#[test]
fn a_second_parent_edge_with_the_unsplit_name_is_flagged_for_the_one_writer() {
    let both = [("OF_SEISMIC_SURVEY", 100), ("OF_SEISMICSURVEY", 100)];
    let found = compute_advisories(&linked("0.19.2", "SeismicSurvey", &both));
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].code, "implicit_parent_edge_duplicates");
    assert_eq!(
        data_advisories(&linked("0.19.2", "SeismicSurvey", &both)),
        Vec::new(),
        "computed at load only"
    );

    // Other writers, a single-word parent, a lone edge, or counts that are
    // nowhere near each other: silent.
    assert!(compute_advisories(&linked("0.19.3", "SeismicSurvey", &both)).is_empty());
    let silent = [
        ("SeismicSurvey", vec![("OF_SEISMICSURVEY", 100)]),
        ("Wellbore", vec![("OF_WELLBORE", 100), ("BELONGS_TO", 100)]),
        (
            "SeismicSurvey",
            vec![("OF_SEISMIC_SURVEY", 100), ("OF_SEISMICSURVEY", 5)],
        ),
    ];
    for (parent, edges) in silent {
        assert!(
            compute_advisories(&linked("0.19.2", parent, &edges)).is_empty(),
            "{parent} {edges:?}"
        );
    }
}
