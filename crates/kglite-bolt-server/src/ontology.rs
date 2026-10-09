//! `--ontology FILE`: the operator-supplied, locked ontology.
//!
//! [`apply`] runs once at startup, before the first client connects. It
//! declares the file's ontology through the same `define_ontology` path every
//! other declaration takes (so declare-over-violating-data refusal and the WAL
//! record both apply) and then locks the session's graph: from then on no
//! client can redeclare or clear it, because the operator owns the ontology.

use std::collections::BTreeSet;
use std::path::Path;

use kglite::api::session::Session;
use kglite::api::{ontology_from_json, OntologyStore};

/// What startup did with the ontology file.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Applied {
    /// No ontology was stored; the file's was declared.
    Declared { warnings: Vec<String> },
    /// The stored ontology already equals the file's.
    Unchanged,
    /// `--ontology-replace` replaced a differing stored ontology.
    Replaced { warnings: Vec<String> },
}

/// Declare `file`'s ontology on `session` and lock it.
///
/// A stored ontology that differs from the file is refused with a short diff
/// unless `replace` is set; a declaration that stored data already breaks at an
/// `error` rule is refused with the report. Both leave the graph untouched.
pub(crate) fn apply(session: &Session, file: &Path, replace: bool) -> Result<Applied, String> {
    let text =
        std::fs::read_to_string(file).map_err(|e| format!("--ontology {}: {e}", file.display()))?;
    let wanted =
        ontology_from_json(&text).map_err(|e| format!("--ontology {}: {e}", file.display()))?;
    let snapshot = session.snapshot();
    let stored: &OntologyStore = &snapshot.ontology;
    let outcome = if *stored == wanted {
        Applied::Unchanged
    } else {
        let replacing = !stored.is_empty();
        if replacing && !replace {
            return Err(format!(
                "the stored ontology differs from --ontology {}:\n  {}\nRestart with \
                 --ontology-replace to replace the stored declaration with the file's, or \
                 point --ontology at the file that matches it",
                file.display(),
                diff(stored, &wanted).join("\n  ")
            ));
        }
        let mut tx = session.begin();
        let warnings = tx
            .working_mut()
            .map_err(|e| format!("--ontology {}: {e}", file.display()))?
            .define_ontology(wanted)
            .map_err(|e| format!("--ontology {}: {e}", file.display()))?;
        match session.commit(tx, true) {
            kglite::api::session::CommitOutcome::Committed { .. } => {}
            other => {
                return Err(format!(
                    "--ontology {}: the declaration could not be committed ({other:?})",
                    file.display()
                ))
            }
        }
        if replacing {
            Applied::Replaced { warnings }
        } else {
            Applied::Declared { warnings }
        }
    };
    drop(snapshot);
    session.lock_ontology();
    Ok(outcome)
}

/// [`apply`] when `--ontology` was given, then log what happened and any
/// `warn`-level findings. A refusal is the startup error.
pub(crate) fn apply_cli(session: &Session, cli: &crate::Cli) -> anyhow::Result<()> {
    let Some(file) = cli.ontology.as_deref() else {
        return Ok(());
    };
    let (outcome, warnings) =
        match apply(session, file, cli.ontology_replace).map_err(anyhow::Error::msg)? {
            Applied::Declared { warnings } => ("declared", warnings),
            Applied::Replaced { warnings } => ("replaced", warnings),
            Applied::Unchanged => ("unchanged", Vec::new()),
        };
    for warning in &warnings {
        tracing::warn!("{warning}");
    }
    tracing::info!(file = %file.display(), outcome, "ontology locked");
    Ok(())
}

/// Names that differ between two stores: classes and relationships added,
/// removed or changed, and the store-level settings.
fn diff(stored: &OntologyStore, wanted: &OntologyStore) -> Vec<String> {
    let mut lines = Vec::new();
    if stored.closed_labels != wanted.closed_labels {
        lines.push(format!(
            "closed_labels: stored {}, file {}",
            stored.closed_labels, wanted.closed_labels
        ));
    }
    if stored.enforcement != wanted.enforcement {
        lines.push(format!(
            "enforcement: stored {}, file {}",
            stored.enforcement.as_str(),
            wanted.enforcement.as_str()
        ));
    }
    section(&mut lines, "class", &stored.classes, &wanted.classes);
    section(
        &mut lines,
        "relationship",
        &stored.relationships,
        &wanted.relationships,
    );
    if lines.is_empty() {
        lines.push("version or other settings differ".to_string());
    }
    lines
}

fn section<T: PartialEq>(
    lines: &mut Vec<String>,
    kind: &str,
    stored: &std::collections::BTreeMap<String, T>,
    wanted: &std::collections::BTreeMap<String, T>,
) {
    let names: BTreeSet<&String> = stored.keys().chain(wanted.keys()).collect();
    for name in names {
        match (stored.get(name), wanted.get(name)) {
            (Some(_), None) => lines.push(format!("{kind} '{name}': only in the stored ontology")),
            (None, Some(_)) => lines.push(format!("{kind} '{name}': only in the file")),
            (Some(a), Some(b)) if a != b => {
                lines.push(format!("{kind} '{name}': declaration differs"))
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kglite::api::DirGraph;

    fn file(tag: &str, name: &str, json: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("kglite-bolt-ontology-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, json).unwrap();
        path
    }

    const A: &str =
        r#"{"classes": {"Person": {"required_properties": ["id"], "enforcement": "error"}}}"#;
    const B: &str = r#"{"classes": {"Person": {"required_properties": ["name"], "enforcement": "error"}, "Org": {}}}"#;

    #[test]
    fn declare_lock_unchanged_differs_and_replace() {
        let (a, b) = (file("ab", "a.json", A), file("ab", "b.json", B));
        let session = Session::new(DirGraph::new());

        assert!(matches!(
            apply(&session, &a, false),
            Ok(Applied::Declared { .. })
        ));
        assert!(session.snapshot().ontology_locked());
        // The operator's own lock refuses a later declare from any client.
        let mut tx = session.begin();
        let refused = tx
            .working_mut()
            .unwrap()
            .define_ontology(ontology_from_json(B).unwrap());
        assert!(refused.unwrap_err().to_string().contains("--ontology"));

        // A fresh session over the same graph state: equal file is Unchanged.
        let mut graph = DirGraph::new();
        graph
            .define_ontology(ontology_from_json(A).unwrap())
            .unwrap();
        let stored = Session::new(graph);
        assert_eq!(apply(&stored, &a, false).unwrap(), Applied::Unchanged);
        let err = apply(&stored, &b, false).unwrap_err();
        assert!(
            err.contains("differs") && err.contains("class 'Org': only in the file"),
            "{err}"
        );
        assert!(err.contains("class 'Person': declaration differs"), "{err}");
        // `--ontology-replace` on a later start (a session not yet locked).
        let mut graph = DirGraph::new();
        graph
            .define_ontology(ontology_from_json(A).unwrap())
            .unwrap();
        let restarted = Session::new(graph);
        assert!(matches!(
            apply(&restarted, &b, true),
            Ok(Applied::Replaced { .. })
        ));
        assert!(restarted.snapshot().ontology.classes.contains_key("Org"));
    }

    #[test]
    fn violating_data_refuses_the_declaration_and_leaves_the_graph_alone() {
        let a = file(
            "violating",
            "a.json",
            r#"{"classes": {"Person": {"required_properties": ["email"], "enforcement": "error"}}}"#,
        );
        let mut graph = DirGraph::new();
        let params = std::collections::HashMap::new();
        kglite::api::session::execute_mut(
            &mut graph,
            "CREATE (:Person {name: 'x'})",
            &kglite::api::session::ExecuteOptions::eager(&params),
        )
        .unwrap();
        let session = Session::new(graph);
        let err = apply(&session, &a, false).unwrap_err();
        assert!(err.contains("Person.required_properties"), "{err}");
        assert!(session.snapshot().ontology.is_empty());
        assert!(!session.snapshot().ontology_locked());
    }
}
