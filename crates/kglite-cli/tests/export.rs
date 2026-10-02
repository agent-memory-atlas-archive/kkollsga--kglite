use std::path::Path;
use std::process::{Command, Output};

fn kglite(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kglite"))
        .args(args)
        .output()
        .unwrap()
}

/// An HR graph saved at `dir/hr.kgl`.
fn hr_graph(dir: &Path) -> String {
    let graph = dir.join("hr.kgl").to_string_lossy().to_string();
    let out = kglite(&[
        "write",
        &graph,
        "CREATE (a:Person {id: 1, title: 'Ada'}), (d:Department {id: 10, title: 'Platform'}), \
         (a)-[:WORKS_IN {since: 2020}]->(d)",
        "--save",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    graph
}

#[test]
fn rdf_formats_are_inferred_from_the_extension_or_named() {
    let temp = tempfile::tempdir().unwrap();
    let graph = hr_graph(temp.path());
    let nq = temp.path().join("hr.nq");
    let trig = temp.path().join("hr.out");
    for (output, flag) in [(&nq, None), (&trig, Some("trig"))] {
        let mut args = vec!["export", graph.as_str(), output.to_str().unwrap()];
        if let Some(format) = flag {
            args.extend(["--format", format]);
        }
        let out = kglite(&args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(String::from_utf8_lossy(&out.stderr).contains("statements"));
    }
    let nq_text = std::fs::read_to_string(&nq).unwrap();
    assert!(nq_text.contains("kg#manifest"), "{nq_text}");
    assert!(nq_text.contains("Ada"), "{nq_text}");
    // TriG groups the manifest into a named graph block.
    let trig_text = std::fs::read_to_string(&trig).unwrap();
    assert!(
        trig_text.contains('{') && trig_text.contains("kg#manifest"),
        "{trig_text}"
    );
}

#[test]
fn csv_writes_the_lossless_tree() {
    let temp = tempfile::tempdir().unwrap();
    let graph = hr_graph(temp.path());
    let tree = temp.path().join("tree");
    let out = kglite(&["export", &graph, tree.to_str().unwrap(), "--format", "csv"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(tree.join("blueprint.json").is_file());
    assert!(tree.join("manifest.json").is_file());
}

#[test]
fn refusals_name_the_flag_or_the_cause() {
    let temp = tempfile::tempdir().unwrap();
    let graph = hr_graph(temp.path());
    let dest = temp.path().join("dump");
    let out = kglite(&["export", &graph, dest.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--format"));

    let out = kglite(&[
        "export",
        &graph,
        dest.to_str().unwrap(),
        "--format",
        "csv",
        "--schema-org",
    ]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("RDF formats only"));

    let nq = temp.path().join("x.nq");
    let out = kglite(&[
        "export",
        &graph,
        nq.to_str().unwrap(),
        "--base",
        "http://schema.org/",
    ]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("well-known"));
}
