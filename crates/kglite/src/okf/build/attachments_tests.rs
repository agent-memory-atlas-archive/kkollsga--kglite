//! The §6 ladder, the nodes it mints, and the edges they hang on.

use crate::datatypes::values::Value;
use crate::graph::storage::GraphRead;
use crate::graph::DirGraph;
use crate::okf::build::build;
use crate::okf::build::tests_support::{
    edges_of, nodes_with_titles, provisional_count, vault_build, write, EdgeFacts,
};
use crate::okf::model::{BuildOptions, ATTACHMENT_LABEL, DEFAULT_MIME, IMAGE_LABEL};
use std::fs;
use std::path::Path;
use tempfile::tempdir;

/// A tiny but real PNG header — enough for a fixture the build never
/// opens, and recognisably a PNG to anything that does.
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";

fn write_bytes(dir: &Path, rel: &str, bytes: &[u8]) {
    let p = dir.join(rel);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(p, bytes).unwrap();
}

/// `(id, [(key, rendered value)])` for every node carrying `label`, sorted.
fn nodes_with_props(g: &DirGraph, label: &str) -> Vec<(String, Vec<(String, String)>)> {
    let mut out: Vec<(String, Vec<(String, String)>)> = g
        .graph
        .node_indices()
        .filter_map(|n| {
            let nd = g.node_view(n)?;
            if nd.node_type_str(&g.interner) != label {
                return None;
            }
            let id = match nd.id().into_owned() {
                Value::String(s) => s,
                other => format!("{other:?}"),
            };
            let mut props: Vec<(String, String)> = nd
                .property_keys(&g.interner)
                .into_iter()
                .map(|k| {
                    (
                        k.to_string(),
                        match nd.get_property(k).map(std::borrow::Cow::into_owned) {
                            Some(Value::String(s)) => s,
                            other => format!("{other:?}"),
                        },
                    )
                })
                .collect();
            props.sort();
            Some((id, props))
        })
        .collect();
    out.sort();
    out
}

/// `(source, conn, target, props)` of the `HAS_IMAGE` / `HAS_ATTACHMENT`
/// edges only — a fixture's containment edges are not what §6 is about.
fn attachment_edges(g: &DirGraph) -> Vec<EdgeFacts> {
    edges_of(g)
        .into_iter()
        .filter(|(_, conn, _, _)| conn.starts_with("HAS_"))
        .collect()
}

/// One vault where each of VAULT.md §6.2's three rungs is the *only* rung
/// that answers: the two path-resolved files have a namesake elsewhere, so
/// the bare-filename rung below them is ambiguous and cannot stand in.
/// Without that, dropping rung 1 or 2 leaves every assertion green.
fn ladder_vault() -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    write_bytes(dir.path(), "img/faults.png", PNG);
    write_bytes(dir.path(), "other/faults.png", PNG);
    write_bytes(dir.path(), "notes/local.png", PNG);
    write_bytes(dir.path(), "other/local.png", PNG);
    write_bytes(dir.path(), "img/handbook.pdf", b"%PDF-1.4\n");
    write(
        dir.path(),
        "notes/a.md",
        concat!(
            "![near](local.png) is note-relative,\n",
            "![root](img/faults.png) is vault-root-relative,\n",
            "![[handbook.pdf]] is a bare filename.\n",
        ),
    );
    dir
}

#[test]
fn attachment_ladder_resolves_note_relative_root_relative_and_filename() {
    let dir = ladder_vault();
    let out = vault_build(dir.path());
    assert_eq!(
        attachment_edges(&out.graph)
            .into_iter()
            .map(|(s, c, t, _)| (s, c, t))
            .collect::<Vec<_>>(),
        vec![
            (
                "a".into(),
                "HAS_ATTACHMENT".into(),
                "img/handbook.pdf".into()
            ),
            ("a".into(), "HAS_IMAGE".into(), "img/faults.png".into()),
            ("a".into(), "HAS_IMAGE".into(), "notes/local.png".into()),
        ],
        "the stored value is always the vault-relative resolved path"
    );
    assert_eq!(
        out.report.missing_attachments, 0,
        "every reference found its rung"
    );
    assert_eq!(
        out.report.nodes_by_label.get(IMAGE_LABEL),
        Some(&2),
        "the two unreferenced namesakes are files, not nodes (§1.2)"
    );
}

#[test]
fn a_rooted_path_skips_the_note_relative_rung() {
    let dir = tempdir().unwrap();
    write_bytes(dir.path(), "shot.png", PNG);
    write_bytes(dir.path(), "notes/shot.png", PNG);
    write(dir.path(), "notes/a.md", "![x](/shot.png)");
    let out = vault_build(dir.path());
    assert_eq!(
        attachment_edges(&out.graph)
            .into_iter()
            .map(|(_, _, t, _)| t)
            .collect::<Vec<_>>(),
        vec!["shot.png".to_string()],
        "a leading `/` means the vault root, as it does for a path link"
    );
}

#[test]
fn an_ambiguous_filename_does_not_resolve_and_names_both_candidates() {
    let dir = tempdir().unwrap();
    write_bytes(dir.path(), "a/shot.png", PNG);
    write_bytes(dir.path(), "b/shot.png", PNG);
    write(dir.path(), "note.md", "![[shot.png]]");
    let out = vault_build(dir.path());
    assert_eq!(out.report.missing_attachments, 1);
    assert_eq!(out.report.ambiguous_attachments, 1);
    let w = out
        .report
        .warnings
        .iter()
        .find(|w| w.contains("shot.png"))
        .expect("an ambiguity is reported");
    assert!(
        w.contains("`a/shot.png`") && w.contains("`b/shot.png`"),
        "the warning names both candidates: {w}"
    );
    assert_eq!(provisional_count(&out.graph), 1, "it resolved to nothing");
}

#[test]
fn the_extension_decides_the_label_and_the_mime_type() {
    let dir = tempdir().unwrap();
    for rel in [
        "f/a.png", "f/b.JPG", "f/c.gif", "f/d.webp", "f/e.svg", "f/g.tiff", "f/h.pdf", "f/i.qqq",
    ] {
        write_bytes(dir.path(), rel, PNG);
    }
    write(
        dir.path(),
        "note.md",
        "![](f/a.png) ![](f/b.JPG) ![](f/c.gif) ![](f/d.webp)\n\
         ![](f/e.svg) ![](f/g.tiff) ![](f/h.pdf) ![](f/i.qqq)",
    );
    let out = vault_build(dir.path());
    let mimes = |label: &str| -> Vec<(String, String)> {
        nodes_with_props(&out.graph, label)
            .into_iter()
            .map(|(id, props)| {
                let mime = props
                    .iter()
                    .find(|(k, _)| k == "mime")
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default();
                (id, mime)
            })
            .collect()
    };
    assert_eq!(
        mimes(IMAGE_LABEL),
        vec![
            ("f/a.png".to_string(), "image/png".to_string()),
            ("f/b.JPG".to_string(), "image/jpeg".to_string()),
            ("f/c.gif".to_string(), "image/gif".to_string()),
            ("f/d.webp".to_string(), "image/webp".to_string()),
        ],
        "`Image` is exactly the four types the MCP server delivers, and the \
         extension match is case-insensitive"
    );
    assert_eq!(
        mimes(ATTACHMENT_LABEL),
        vec![
            ("f/e.svg".to_string(), "image/svg+xml".to_string()),
            ("f/g.tiff".to_string(), "image/tiff".to_string()),
            ("f/h.pdf".to_string(), "application/pdf".to_string()),
            ("f/i.qqq".to_string(), DEFAULT_MIME.to_string()),
        ],
        "a picture that cannot be delivered is still an Attachment, and an \
         unknown extension gets a MIME type rather than none"
    );
}

#[test]
fn an_attachment_node_carries_its_stat_metadata_and_nothing_read() {
    let dir = tempdir().unwrap();
    write_bytes(dir.path(), "img/d.png", b"0123456789");
    write_bytes(dir.path(), "img/empty.png", b"");
    write(dir.path(), "note.md", "![d](img/d.png) ![e](img/empty.png)");
    let out = vault_build(dir.path());
    let props = nodes_with_props(&out.graph, IMAGE_LABEL);
    let of = |id: &str, key: &str| -> String {
        props
            .iter()
            .find(|(i, _)| i == id)
            .unwrap()
            .1
            .iter()
            .find(|(k, _)| k == key)
            .unwrap_or_else(|| panic!("{id} has no {key}"))
            .1
            .clone()
    };
    assert_eq!(of("img/d.png", "mime"), "image/png");
    assert_eq!(of("img/d.png", "size_bytes"), "Some(Int64(10))");
    assert_eq!(
        of("img/empty.png", "size_bytes"),
        "Some(Int64(0))",
        "an empty file has a size, not a missing one"
    );
    assert_eq!(
        of("img/empty.png", "mime"),
        "image/png",
        "the type comes from the name — nothing sniffed the zero bytes"
    );
    assert!(
        of("img/d.png", "mtime").starts_with("Some(Timestamp("),
        "`mtime` is a UTC datetime, not a number: {}",
        of("img/d.png", "mtime")
    );
    assert_eq!(
        nodes_with_titles(&out.graph, IMAGE_LABEL),
        vec![
            ("img/d.png".to_string(), "d.png".to_string()),
            ("img/empty.png".to_string(), "empty.png".to_string()),
        ],
        "the id is the vault-relative path and the title is the filename"
    );
}

/// VAULT.md §6.5: the build stats attachments and never opens them. A file
/// the process cannot read proves it — `stat` needs no read permission, so
/// a build that opened the file would fail where this one succeeds.
#[cfg(unix)]
#[test]
fn attachment_bytes_are_never_read() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempdir().unwrap();
    write_bytes(dir.path(), "img/secret.png", b"0123456789");
    let p = dir.path().join("img/secret.png");
    fs::set_permissions(&p, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&p).is_ok() {
        // A privileged user bypasses the mode bits, so the file is not
        // actually unreadable and this test can prove nothing here.
        return;
    }
    write(dir.path(), "note.md", "![s](img/secret.png)");
    let out = vault_build(dir.path());
    assert_eq!(out.report.missing_attachments, 0);
    assert_eq!(
        nodes_with_props(&out.graph, IMAGE_LABEL)
            .into_iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>(),
        vec!["img/secret.png".to_string()],
    );
    fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
}

#[test]
fn image_text_collects_alts_and_note_titles_in_first_use_order() {
    let dir = tempdir().unwrap();
    write_bytes(dir.path(), "img/f.png", PNG);
    write_bytes(dir.path(), "img/h.pdf", b"%PDF-1.4\n");
    write(
        dir.path(),
        "a.md",
        "---\ntitle: Alpha\n---\n![Fault map](img/f.png)\n\n## More\n\n![Fault map](img/f.png)",
    );
    write(
        dir.path(),
        "b.md",
        "---\ntitle: Beta\n---\n![Fault map](img/f.png) and ![Handbook](img/h.pdf)",
    );
    let out = vault_build(dir.path());
    let text = nodes_with_props(&out.graph, IMAGE_LABEL)[0]
        .1
        .iter()
        .find(|(k, _)| k == "text")
        .unwrap()
        .1
        .clone();
    assert_eq!(
        text, "Fault map\nAlpha\nBeta",
        "distinct alts and using-note titles, first-use order"
    );
    assert!(
        !nodes_with_props(&out.graph, ATTACHMENT_LABEL)[0]
            .1
            .iter()
            .any(|(k, _)| k == "text"),
        "only an Image carries `text` (VAULT.md §6.3)"
    );
}

#[test]
fn attachment_edges_carry_alt_section_and_a_per_kind_ordinal() {
    let dir = tempdir().unwrap();
    write_bytes(dir.path(), "img/one.png", PNG);
    write_bytes(dir.path(), "img/two.png", PNG);
    write_bytes(dir.path(), "img/h.pdf", b"%PDF-1.4\n");
    write(
        dir.path(),
        "note.md",
        concat!(
            "![](img/one.png)\n",
            "![Doc](img/h.pdf)\n",
            "## Figures\n",
            "![Second](img/two.png)\n",
        ),
    );
    let out = vault_build(dir.path());
    assert_eq!(
        attachment_edges(&out.graph),
        vec![
            (
                "note".into(),
                "HAS_ATTACHMENT".into(),
                "img/h.pdf".into(),
                vec![
                    ("alt".to_string(), "Doc".to_string()),
                    ("ordinal".to_string(), "Some(Int64(0))".to_string()),
                ]
            ),
            (
                "note".into(),
                "HAS_IMAGE".into(),
                "img/one.png".into(),
                vec![("ordinal".to_string(), "Some(Int64(0))".to_string())]
            ),
            (
                "note".into(),
                "HAS_IMAGE".into(),
                "img/two.png".into(),
                vec![
                    ("alt".to_string(), "Second".to_string()),
                    ("ordinal".to_string(), "Some(Int64(1))".to_string()),
                    ("section".to_string(), "Figures".to_string()),
                ]
            ),
        ],
        "`alt` is absent when empty, `section` when above the first heading, \
         and the two kinds number independently"
    );
}

#[test]
fn two_references_to_one_file_are_one_edge_unless_they_differ() {
    let dir = tempdir().unwrap();
    write_bytes(dir.path(), "img/f.png", PNG);
    write(
        dir.path(),
        "note.md",
        concat!(
            "![Map](img/f.png) and again ![Map](img/f.png)\n",
            "## Figures\n",
            "![Map](img/f.png) and ![Other caption](img/f.png)\n",
        ),
    );
    let out = vault_build(dir.path());
    let ordinals: Vec<String> = attachment_edges(&out.graph)
        .into_iter()
        .map(|(_, _, _, props)| {
            props
                .iter()
                .find(|(k, _)| k == "ordinal")
                .unwrap()
                .1
                .clone()
        })
        .collect();
    assert_eq!(
        ordinals,
        vec![
            "Some(Int64(0))".to_string(),
            "Some(Int64(1))".to_string(),
            "Some(Int64(2))".to_string()
        ],
        "the repeat in one section folds away, and the ordinals left behind \
         have no hole in them"
    );
    assert_eq!(out.report.edges_by_type.get("HAS_IMAGE"), Some(&3));
}

#[test]
fn a_missing_attachment_is_a_provisional_stub_and_a_warning() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "note.md",
        "![gone](./img/gone.png) and ![[nowhere.pdf]]",
    );
    let out = vault_build(dir.path());
    assert_eq!(out.report.missing_attachments, 2);
    assert_eq!(out.report.ambiguous_attachments, 0, "both simply absent");
    assert_eq!(
        nodes_with_props(&out.graph, IMAGE_LABEL),
        vec![(
            "img/gone.png".to_string(),
            vec![
                (
                    "_provisional".to_string(),
                    "Some(Boolean(true))".to_string()
                ),
                ("missing".to_string(), "Some(Boolean(true))".to_string()),
            ]
        )],
        "the stub is keyed by the normalised reference and carries no stat"
    );
    assert_eq!(
        nodes_with_props(&out.graph, ATTACHMENT_LABEL)
            .into_iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>(),
        vec!["nowhere.pdf".to_string()],
        "the extension still decides the label of a file that is not there"
    );
    let mut warnings: Vec<&String> = out
        .report
        .warnings
        .iter()
        .filter(|w| w.starts_with("missing attachment"))
        .collect();
    warnings.sort();
    assert_eq!(
        warnings,
        vec![
            &"missing attachment: `img/gone.png`".to_string(),
            &"missing attachment: `nowhere.pdf`".to_string()
        ]
    );
    assert_eq!(out.graph.graph.edge_count(), 2, "the edges reach the stubs");
}

#[test]
fn okf_and_loose_mint_no_attachment_nodes() {
    for dialect in [
        crate::okf::model::Dialect::Okf,
        crate::okf::model::Dialect::Loose,
    ] {
        let dir = tempdir().unwrap();
        write_bytes(dir.path(), "img/f.png", PNG);
        write(
            dir.path(),
            "note.md",
            "---\ntype: Note\n---\n![f](img/f.png) and ![[f.png]]",
        );
        let mut opts = BuildOptions::for_dialect(dialect);
        opts.require_frontmatter = false;
        let out = build(dir.path(), &opts).unwrap();
        assert_eq!(
            out.report.nodes_by_label.get(IMAGE_LABEL),
            None,
            "{dialect:?} still drops every image reference"
        );
        assert_eq!(out.report.nodes_by_label.get(ATTACHMENT_LABEL), None);
        assert_eq!(out.graph.graph.edge_count(), 0, "{dialect:?}");
        assert_eq!(out.report.missing_attachments, 0);
    }
}
