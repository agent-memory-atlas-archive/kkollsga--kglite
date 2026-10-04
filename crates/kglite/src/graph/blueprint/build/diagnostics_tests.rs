//! Each advisory a blueprint build raises lands in the group its raise site
//! assigned, with a stable kind code.

use super::*;
use crate::graph::blueprint::schema::Blueprint;
use std::path::PathBuf;

fn fixture(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kglite_diag_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (file, text) in files {
        std::fs::write(dir.join(file), text).unwrap();
    }
    dir
}

fn report_for(name: &str, blueprint: &str, files: &[(&str, &str)]) -> BuildReport {
    let dir = fixture(name, files);
    let parsed: Blueprint = serde_json::from_str(blueprint).expect("fixture parses");
    let mut graph = DirGraph::new();
    let report = build(&mut graph, parsed, &dir, BuildInputs::default()).expect("builds");
    assert_eq!(
        graph.build_info.as_ref().map(|b| b.summary.clone()),
        Some(report.summary()),
        "the graph remembers the build's summary"
    );
    let _ = std::fs::remove_dir_all(&dir);
    report
}

fn group_of(report: &BuildReport, kind: &str) -> Option<DiagnosticGroup> {
    report
        .diagnostics
        .iter()
        .find(|d| d.kind == kind)
        .map(|d| d.group)
}

#[test]
fn raise_sites_assign_their_group() {
    let report = report_for(
        "groups",
        r#"{"nodes": {
            "Department": {"csv": "d.csv", "pk": "did", "title": "name",
                "properties": {"vf": "validFrom", "vt": "validTo"}},
            "Team": {"csv": "t.csv", "pk": "tid", "title": "name", "properties": {}},
            "Person": {"csv": "p.csv", "pk": "pid", "title": "name", "properties": {},
                "lables": ["Human"],
                "connections": {"fk_edges": {"MEMBER_OF": {"target": "Team", "fk": "team"}}}}
        }}"#,
        &[
            ("d.csv", "did,name,vf,vt\n1,Ops,2020-01-01,2021-01-01\n"),
            ("t.csv", "tid,name\nt1,Core\n"),
            (
                "p.csv",
                "pid,name,team\n1,Ann,t1\n1,Ann again,t1\n2,Bo,t9\n",
            ),
        ],
    );
    let expected = [
        ("typed_only_no_validity", DiagnosticGroup::Declarations),
        ("stubs_vivified", DiagnosticGroup::Stubs),
        ("unknown_key", DiagnosticGroup::DataShape),
        ("duplicate_id", DiagnosticGroup::DataQuality),
    ];
    for (kind, group) in expected {
        assert_eq!(
            group_of(&report, kind),
            Some(group),
            "{kind}: {report:?}",
            report = report.diagnostics
        );
    }
    assert_eq!(report.warnings.len(), report.diagnostics.len());
    for (warning, diagnostic) in report.warnings.iter().zip(&report.diagnostics) {
        assert_eq!(warning, &diagnostic.message);
    }
    let summary = report.summary();
    assert_eq!(summary.len(), 4, "{summary:?}");
    assert!(summary.values().all(|n| *n == 1), "{summary:?}");
}

#[test]
fn an_undeclared_column_read_twice_is_cosmetic() {
    let prepared = prepass::Prepared {
        resolved: [("age".to_string(), "int".to_string())]
            .into_iter()
            .collect(),
        resolved_ids: Default::default(),
        chunks: Box::new(std::iter::empty()),
        extra_pass: true,
    };
    let diagnostic = prepass::prepass_warning("node 'P'", &prepared).expect("warns");
    assert_eq!(diagnostic.group, DiagnosticGroup::Cosmetic);
    assert_eq!(diagnostic.kind, "undeclared_types_extra_read");
}
