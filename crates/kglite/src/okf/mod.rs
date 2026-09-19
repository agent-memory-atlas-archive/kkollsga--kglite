//! OKF (Open Knowledge Format) bundle ingestion — read-only, partial.
//!
//! Parses a directory of markdown files with YAML frontmatter, cross-linked by
//! markdown links (Google's OKF, but also Claude memory dirs, skills, and
//! Obsidian vaults), into a [`crate::graph::DirGraph`]. Conceptually a code-graph builder
//! for prose knowledge instead of source code.
//!
//! Ingestion is **partial** (like code-graph building): each concept becomes a node
//! carrying its frontmatter as properties plus a `file_path` pointer; the body
//! is read on demand and is *not* stored unless [`BuildOptions::with_body`] is
//! set. Links become typed edges; dangling link targets become `_provisional`
//! stub nodes. The result is a normal graph — every Cypher feature, algorithm
//! (`CALL leiden`/`pagerank`), and structural rule works over it with no extra
//! surface.
//!
//! This module is gated behind the `okf` Cargo feature (it pulls a YAML parser);
//! the Python wheel enables it, bare builds don't.

pub mod build;
pub mod cache;
pub(crate) mod directives;
pub mod export;
pub mod fingerprint;
pub mod frontmatter;
pub mod links;
pub mod model;
pub(crate) mod structure;
pub(crate) mod tags;
pub mod validate;
pub mod vault_config;
pub mod walk;

pub use build::{build, BuildOutput};
pub use cache::{is_cache_artifact, open, CachePolicy, Opened};
pub use export::{export, ExportOptions, ExportReport};
pub use fingerprint::{fingerprint, rebuild_if_changed, stamped_dialect};
pub use model::{
    BuildOptions, BuildReport, ConceptDoc, Dialect, FolderNoteDirection, IdScheme, LabelFrom, Link,
    Profile, RebuildOptions,
};
pub use validate::validate;
pub use vault_config::{IndexDecl, VaultConfig};

use crate::datatypes::values::Value;
use model::IdScheme as Ids;
use rayon::prelude::*;
use std::collections::BTreeMap;
use std::path::Path;

/// Read a concept's markdown body on demand (frontmatter stripped). The
/// counterpart to partial ingestion: the graph stores a `file_path` pointer, and
/// this resolves it to the prose when an agent has narrowed to one concept.
/// A file with no frontmatter returns its whole content.
pub fn read_body(path: &Path) -> Result<String, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let (_yaml, body) = frontmatter::split(&text);
    Ok(body)
}

/// Parse a bundle directory into [`ConceptDoc`]s (no graph yet). Files are read
/// and parsed in parallel; a file with malformed frontmatter degrades to a
/// body-only `Concept` rather than being dropped (permissive consumption).
pub fn parse_bundle(root: &Path, opts: &BuildOptions) -> Result<Vec<ConceptDoc>, String> {
    let walked = walk::discover(root, opts)?;
    Ok(parse_concepts(&walked.concepts, opts))
}

/// Parse already-discovered concept files into [`ConceptDoc`]s (parallel). Used
/// by [`parse_bundle`], the builder, and codingest's docs pass (which reuses
/// the OKF parser to ingest a repo's markdown).
pub fn parse_concepts(files: &[walk::DiscoveredFile], opts: &BuildOptions) -> Vec<ConceptDoc> {
    parse_concepts_reported(files, opts).0
}

/// What parsing found, in the `BuildReport` vocabulary: a collision that
/// forced ids to change is an error (VAULT.md §9), a case-only clash that a
/// case-insensitive filesystem would turn into one is a warning.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ParseFindings {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

/// [`parse_concepts`] plus the findings the build report wants. One
/// implementation, because id resolution *is* the collision detector: the
/// findings fall out of the pass that rewrites the colliding ids, and
/// recomputing them in the builder would be a second walk over the same map.
pub(crate) fn parse_concepts_reported(
    files: &[walk::DiscoveredFile],
    opts: &BuildOptions,
) -> (Vec<ConceptDoc>, ParseFindings) {
    let mut docs: Vec<ConceptDoc> = files
        .par_iter()
        .filter_map(|f| parse_file(f, opts).ok().flatten())
        .collect();
    // Stable order for reproducible builds / tests — and for a deterministic
    // collision pass, which reads the docs in this order.
    docs.sort_by(|a, b| a.concept_id.cmp(&b.concept_id));
    let mut findings = resolve_ids(&mut docs, opts);
    // After the id errors and before the builder's, in the docs' own sorted
    // order — a report whose findings moved with the filesystem's directory
    // order would make every golden assertion a coin flip.
    findings.errors.extend(
        docs.iter()
            .flat_map(|d| d.errors.iter().map(|e| format!("{}: {e}", d.file_path))),
    );
    findings.warnings.extend(
        docs.iter()
            .flat_map(|d| d.warnings.iter().map(|w| format!("{}: {w}", d.file_path))),
    );
    findings.warnings.extend(hub_key_edge_warnings(&docs));
    // The structure pass's own findings (VAULT.md §9): a duplicate derived id,
    // and nothing else this early — the ones that need the whole vault (a link
    // whose anchor names no heading, a rule that matched nothing anywhere) are
    // the builder's.
    findings.warnings.extend(docs.iter().flat_map(|d| {
        d.derived
            .warnings
            .iter()
            .map(|w| format!("{}: {w}", d.file_path))
    }));
    if !findings.errors.is_empty() {
        // Ids changed under the fallback; restore the ordering invariant.
        docs.sort_by(|a, b| a.concept_id.cmp(&b.concept_id));
    }
    (docs, findings)
}

/// One warning per hub key that some note spent on the typed-edge rule
/// (VAULT.md §4.3 beats §7's `hubs:`), naming how many notes and the first of
/// them. Per *key*, not per note: the clash is a property of the vault's
/// declaration, and a corpus whose `keywords:` are all wikilinks would
/// otherwise bury the report under one copy per file.
fn hub_key_edge_warnings(docs: &[ConceptDoc]) -> Vec<String> {
    let mut by_key: BTreeMap<&str, (usize, &str)> = BTreeMap::new();
    for d in docs {
        for key in &d.hub_key_edges {
            let entry = by_key
                .entry(key.as_str())
                .or_insert((0, d.file_path.as_str()));
            entry.0 += 1;
        }
    }
    by_key
        .into_iter()
        .map(|(key, (count, first))| {
            format!(
                "hub key `{key}` holds wikilinks in {count} note(s) (first: {first}); \
                 the typed-edge rule wins and they join no hub"
            )
        })
        .collect()
}

/// The path-relative id a note falls back to: its vault-relative path minus
/// `.md`. Unique across a walk by construction, which is what makes it the
/// collision escape hatch.
fn path_id(doc: &ConceptDoc) -> String {
    doc.file_path
        .strip_suffix(".md")
        .unwrap_or(&doc.file_path)
        .to_string()
}

/// Settle id collisions among already-parsed notes (VAULT.md §3).
///
/// Only the stem-based scheme can collide — a path id is unique by
/// construction — so the whole pass is skipped for OKF bundles, leaving their
/// report byte-identical. Every note in a colliding group falls back to its
/// path id, including a group that collided because two notes declared the
/// same `id:`; leaving those merged would silently lose a note.
///
/// The loop re-checks because a fallback can itself collide (a note declaring
/// `id: notes/alpha` while `notes/alpha.md` exists). Path ids are unique, so a
/// group of them cannot re-form: two rounds always suffice, and the third is
/// the guard that says so.
fn resolve_ids(docs: &mut [ConceptDoc], opts: &BuildOptions) -> ParseFindings {
    let mut findings = ParseFindings::default();
    if opts.profile.id_scheme != Ids::FrontmatterOrStem {
        return findings;
    }
    for _ in 0..3 {
        let mut by_id: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, d) in docs.iter().enumerate() {
            by_id.entry(d.concept_id.clone()).or_default().push(i);
        }
        let colliding: Vec<(String, Vec<usize>)> =
            by_id.into_iter().filter(|(_, idx)| idx.len() > 1).collect();
        if colliding.is_empty() {
            break;
        }
        for (id, idx) in colliding {
            let paths: Vec<&str> = idx.iter().map(|&i| docs[i].file_path.as_str()).collect();
            findings.errors.push(format!(
                "id collision: {} notes resolve to id `{id}` ({}); each falls back to its path-relative id",
                idx.len(),
                paths.join(", ")
            ));
            for &i in &idx {
                let fallback = path_id(&docs[i]);
                docs[i].concept_id = fallback;
            }
        }
    }

    // Case-only clashes survive on a case-sensitive host and merge on a
    // case-insensitive one, so they are reported regardless of where the build
    // ran (VAULT.md §3).
    let mut folded: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, d) in docs.iter().enumerate() {
        folded
            .entry(d.concept_id.to_lowercase())
            .or_default()
            .push(i);
    }
    for (_, idx) in folded {
        let distinct: BTreeMap<&str, &str> = idx
            .iter()
            .map(|&i| (docs[i].concept_id.as_str(), docs[i].file_path.as_str()))
            .collect();
        if distinct.len() > 1 {
            let pairs: Vec<String> = distinct
                .iter()
                .map(|(id, path)| format!("`{id}` ({path})"))
                .collect();
            findings.warnings.push(format!(
                "case-insensitive id collision: {}",
                pairs.join(", ")
            ));
        }
    }
    findings
}

/// Parse one discovered file into a [`ConceptDoc`]. Returns `Ok(None)` when the
/// file is skipped (no frontmatter while `require_frontmatter` is set).
fn parse_file(f: &walk::DiscoveredFile, opts: &BuildOptions) -> Result<Option<ConceptDoc>, String> {
    let text = std::fs::read_to_string(&f.abs_path)
        .map_err(|e| format!("reading {}: {e}", f.abs_path.display()))?;

    let (yaml, body) = frontmatter::split(&text);
    // Plain markdown (no frontmatter) is skipped by default — the discriminator
    // between structured knowledge (OKF concepts / memories) and normal md.
    if opts.require_frontmatter && yaml.is_none() {
        return Ok(None);
    }

    let profile = &opts.profile;
    let doc_path = f.rel_path.strip_suffix(".md").unwrap_or(&f.rel_path);
    // One block tree per note, shared by the title ladder and the link pass so
    // the two can never disagree about where a heading is (VAULT.md §5.4).
    let tree = structure::parse_blocks(&body);

    // Malformed YAML degrades to an empty frontmatter map (the concept still
    // becomes a node — losing the file entirely would be worse) and is
    // reported: VAULT.md §4 makes it an error precisely because the degrade is
    // silent otherwise, and a converter emitting broken YAML would ship it.
    let mut errors: Vec<String> = Vec::new();
    let mut fm = match frontmatter::parse(&text) {
        Ok(map) => map,
        Err(reason) => {
            errors.push(reason);
            BTreeMap::new()
        }
    };
    if profile.reserved_key_shapes {
        errors.extend(reserved_key_errors(&fm));
    }

    // Honor the `kg_skip: true` opt-out marker (excludes the file from the sweep).
    if opts.respect_skip && matches!(fm.get(model::SKIP_KEY), Some(Value::Boolean(true))) {
        return Ok(None);
    }

    // The keys a profile ignores outright (VAULT.md §4.1) leave before the
    // typed-edge rule, the hubs and `props` can see them — with the dotted
    // keys a nested-map spelling flattened to, so "not stored" holds whatever
    // shape the value had.
    if !profile.ignored_keys.is_empty() {
        fm.retain(|key, _| {
            !profile.ignored_keys.iter().any(|ignored| {
                key.as_str() == *ignored
                    || key
                        .strip_prefix(ignored)
                        .is_some_and(|rest| rest.starts_with('.'))
            })
        });
    }

    // Id (VAULT.md §3): the path, or — in a vault — a declared `id:` falling
    // back to the filename stem. A declared id is the node's identity, not a
    // property, so it leaves the frontmatter map; `resolve_ids` settles any
    // collision once every note is parsed.
    let concept_id = match profile.id_scheme {
        Ids::Path => doc_path.to_string(),
        Ids::FrontmatterOrStem => fm
            .remove("id")
            .map(value_to_display)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| stem(doc_path).to_string()),
    };

    // Label ladder (VAULT.md §2.1): `type` → `metadata.type` (Claude memories)
    // → the profile's default label → the top-level folder → the fallback.
    // `label_from: Folder` moves the folder rung to the front, so a folder move
    // relabels a note that carries a `type:` too. `type` leaves the map either
    // way: the label is derived, never stored.
    let declared = fm
        .remove("type")
        .map(value_to_display)
        .filter(|s| !s.is_empty());
    let meta_type = profile
        .metadata_type_label
        .then(|| {
            fm.get("metadata.type")
                .cloned()
                .map(value_to_display)
                .filter(|s| !s.is_empty())
        })
        .flatten();
    let folder = profile
        .folder_label
        .then(|| top_folder(label_path(doc_path, profile)).map(str::to_string))
        .flatten();
    let ladder = match profile.label_from {
        LabelFrom::Type => [declared, meta_type, profile.default_label.clone(), folder],
        LabelFrom::Folder => [folder, declared, meta_type, profile.default_label.clone()],
    };
    let label = ladder
        .into_iter()
        .flatten()
        .next()
        .unwrap_or_else(|| profile.fallback_label.clone());

    // Title: `title` → `name` (Claude memories) → the body's first heading, of
    // any level (so a frontmatter-less README/doc gets its real title, not the
    // file stem) → file stem. The stem comes from the *path*, not the id, so a
    // note with a declared `id:` is still titled after its file.
    let title = fm
        .remove("title")
        .map(value_to_display)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            fm.get("name")
                .cloned()
                .map(value_to_display)
                .filter(|s| !s.is_empty())
        })
        .or_else(|| first_heading_of(&tree))
        .unwrap_or_else(|| stem(doc_path).to_string());

    // Wikilink-valued keys become edges, not properties (VAULT.md §4.3) —
    // before `props` is built, because the rule *removes* the key.
    let (fm_links, hub_key_edges) =
        frontmatter_edges(&mut fm, profile, parent_dir(doc_path), &mut errors);

    let mut props: Vec<(String, Value)> = fm
        .into_iter()
        .map(|(k, v)| {
            let v = if profile.infer_temporal {
                frontmatter::infer_temporal(v)
            } else {
                v
            };
            (k, v)
        })
        .collect();
    let source_dir = parent_dir(doc_path);
    let extracted = links::extract_with_tree(&body, &tree, source_dir, profile);
    errors.extend(extracted.path_errors);
    // The block model's own §9 findings (a `<!-- kglite heading -->` with
    // nothing to promote) join the link pass's, before either depends on
    // whether this vault declares `structure:`.
    let mut warnings = tree.warnings.clone();
    warnings.extend(extracted.warnings);
    let mut all_links = extracted.links;
    for link in fm_links {
        links::push_unique(&mut all_links, link);
    }
    // The structure pass reads the same tree the link pass just read
    // (VAULT.md §7.1). Nothing is derived unless the vault declared a rule, so
    // a profile without one costs one `Option` test per note.
    let mut derived = match &profile.structure {
        Some(structure) if structure.derives_anything() => {
            structure::derive(&body, &tree, &title, &label, structure)
        }
        _ => structure::Derived::default(),
    };
    // An edge table's rows are links, not derived edges: their targets are
    // notes the resolver has yet to find (VAULT.md §7.1 `tables:`), so they
    // join the note's own links and travel the ladder every prose link
    // travels — stub, `edge_defaults:` and all.
    for link in std::mem::take(&mut derived.links) {
        links::push_unique(&mut all_links, link);
    }
    // `<!-- kglite key: value -->` reaches the section derive just made, or
    // the note (VAULT.md §5.8). After the derive, before the doc is sealed.
    directives::apply(
        &tree,
        &mut derived,
        &mut directives::NoteSide {
            profile,
            source_dir,
            props: &mut props,
            links: &mut all_links,
            errors: &mut errors,
        },
    );
    // Inline tags read as placed, once the nodes a tag can sit in exist: the
    // `tags` list on each of them, and what `tag_labels:` claimed (§5.5).
    let typed_tags = tags::apply(&extracted.tag_spans, &props, &mut derived, profile);
    let body = if opts.with_body { Some(body) } else { None };

    Ok(Some(ConceptDoc {
        concept_id,
        file_path: f.rel_path.clone(),
        label,
        title,
        props,
        links: all_links,
        inline_tags: extracted.tags,
        typed_tags,
        attachments: extracted.attachments,
        hub_key_edges,
        errors,
        warnings,
        body,
        derived,
    }))
}

/// VAULT.md §4.1's reserved keys, checked for the *shape* their meaning
/// depends on. Every one of these fails quietly otherwise: a scalar `tags:`
/// joins no hub, a list-valued `id:` is stringified into an identity nobody
/// links to, and a `kg_skip: "true"` excludes nothing.
///
/// The nested-map spelling is invisible here by construction — `tags: {a: 1}`
/// flattened to `tags.a` before this ran — which is why the message names the
/// shape found rather than claiming the key is absent.
fn reserved_key_errors(fm: &BTreeMap<String, Value>) -> Vec<String> {
    let mut out = Vec::new();
    let mut require = |key: &str, shape: &str, ok: fn(&Value) -> bool| {
        if let Some(value) = fm.get(key) {
            if !ok(value) {
                out.push(format!(
                    "reserved key `{key}:` must be {shape}, not {}",
                    value.type_name()
                ));
            }
        }
    };
    require("id", "a string", |v| matches!(v, Value::String(_)));
    require("type", "a string", |v| matches!(v, Value::String(_)));
    require("tags", "a list", |v| matches!(v, Value::List(_)));
    require("aliases", "a list", |v| matches!(v, Value::List(_)));
    require(model::SKIP_KEY, "a boolean", |v| {
        matches!(v, Value::Boolean(_))
    });
    out
}

/// The keys §4.3's typed-edge rule never touches: `id`/`type`/`title` have
/// already left the map, and these three are reserved for other meanings
/// (VAULT.md §4.1). `parent` is reserved too, and is handled first — with a
/// fixed edge type rather than one spelled by its key.
const NON_EDGE_KEYS: [&str; 3] = ["aliases", "tags", model::SKIP_KEY];

/// Drain the frontmatter keys whose value is a wikilink string, or a list of
/// nothing but wikilink strings, into edges (VAULT.md §4.3). The raw property
/// is not kept: storing both would duplicate it on export.
///
/// Returns the edges and the [`Profile::hubs`] keys among them — a key that is
/// both a hub and wikilink-valued goes to the typed-edge rule, and the caller
/// reports the clash rather than leaving the hub silently short.
///
/// Every target goes through the §9 path check the body's wikilinks go
/// through, on the same routine: `depends_on: "[[../../etc/passwd]]"` names a
/// place the vault does not own exactly as the same spelling in the prose
/// does, and the reader that follows it does not care which half of the file
/// it was written in.
fn frontmatter_edges(
    fm: &mut BTreeMap<String, Value>,
    profile: &Profile,
    source_dir: &str,
    errors: &mut Vec<String>,
) -> (Vec<Link>, Vec<String>) {
    let mut out = Vec::new();
    let mut hub_keys = Vec::new();
    if !profile.frontmatter_edges {
        return (out, hub_keys);
    }
    // `parent:` is the reserved key that follows the rule with the folder
    // note's edge type and direction, so a cross-listed note contributes the
    // same edges the folder layout would have (VAULT.md §2.3, §4.3). It leaves
    // the map whatever its shape: the key is reserved, never a property.
    if let Some(v) = fm.remove("parent") {
        if let Some(targets) = links::wikilink_targets(&v) {
            let reverse =
                profile.folder_note_direction == model::FolderNoteDirection::ParentToChild;
            for target in targets {
                if profile.path_safety {
                    links::record_wikilink_path_error(errors, &target, source_dir);
                }
                out.push(Link {
                    target,
                    conn_type: profile.folder_note_edge.clone(),
                    is_external: false,
                    props: Vec::new(),
                    reverse,
                });
            }
        }
    }
    let edge_keys: Vec<String> = fm
        .iter()
        .filter(|(k, v)| {
            !NON_EDGE_KEYS.contains(&k.as_str()) && links::wikilink_targets(v).is_some()
        })
        .map(|(k, _)| k.clone())
        .collect();
    for key in edge_keys {
        let conn_type = links::upper_snake(&key);
        let value = fm.remove(&key).expect("key came from this map");
        if conn_type.is_empty() {
            continue;
        }
        if profile.hubs.contains_key(&key) {
            hub_keys.push(key);
        }
        for target in links::wikilink_targets(&value).expect("filtered on Some above") {
            if profile.path_safety {
                links::record_wikilink_path_error(errors, &target, source_dir);
            }
            out.push(Link::plain(target, conn_type.clone(), false));
        }
    }
    (out, hub_keys)
}

/// The path a note is labelled from (VAULT.md §2.1 rung 3, §2.3).
///
/// A folder note stands in for its directory, so `X/X.md` is labelled from
/// where `X/` sits — its parent — and both spellings of a folder note give one
/// label. The `X.md`-beside-`X/` spelling already lives there, so only this one
/// needs moving, and it is detectable from the path alone: no directory listing
/// tells you anything the stem and its parent do not.
fn label_path<'a>(doc_path: &'a str, profile: &Profile) -> &'a str {
    if profile.folder_notes {
        let dir = parent_dir(doc_path);
        if !dir.is_empty() && stem(doc_path) == stem(dir) {
            return dir;
        }
    }
    doc_path
}

/// Coerce a frontmatter scalar to a display string for label/title use.
fn value_to_display(v: Value) -> String {
    match v {
        Value::String(s) => s,
        Value::Int64(i) => i.to_string(),
        Value::Float64(f) => f.to_string(),
        Value::Boolean(b) => b.to_string(),
        other => format!("{other:?}"),
    }
}

/// The body's first markdown heading, of **any** level, used as a title
/// fallback for frontmatter-less docs. Reads the same block tree the link pass
/// does, so a `#` inside a fenced code block is not a heading here either.
///
/// Levels below `#` count deliberately: a note whose prose opens with an `##`
/// still has a better title there than in its filename stem, and this ladder
/// is shared with the `okf` and `loose` dialects, whose producers write no
/// `# H1` at all. VAULT.md §3 says the same.
pub(crate) fn first_heading(body: &str) -> Option<String> {
    first_heading_of(&structure::parse_blocks(body))
}

/// [`first_heading`] against a block tree the caller already parsed.
///
/// A heading whose text is empty is skipped, not returned: `##` titles
/// nothing, and neither does `### #`, whose lone `#` is CommonMark's closing
/// sequence rather than a title. The ladder falls through to the file stem.
fn first_heading_of(tree: &structure::BlockTree) -> Option<String> {
    tree.headings
        .iter()
        .find(|h| !h.text.is_empty())
        .map(|h| h.text.clone())
}

/// Last path component of a concept-id (the file stem).
fn stem(concept_id: &str) -> &str {
    concept_id.rsplit('/').next().unwrap_or(concept_id)
}

/// The **top-level** folder of a bundle-relative path — the label rung in
/// VAULT.md §2.1, so a note nested three deep is still labelled by the folder
/// its branch hangs off. `None` for a note at the root.
fn top_folder(doc_path: &str) -> Option<&str> {
    doc_path.split_once('/').map(|(head, _)| head)
}

/// Directory portion of a concept-id (`""` at the bundle root). `pub(crate)` so
/// codingest's docs pass reuses it to resolve relative markdown links.
pub(crate) fn parent_dir(concept_id: &str) -> &str {
    match concept_id.rfind('/') {
        Some(i) => &concept_id[..i],
        None => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write(dir: &Path, rel: &str, content: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, content).unwrap();
    }

    #[test]
    fn parses_concepts_and_skips_reserved() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "tables/orders.md",
            "---\ntype: BigQuery Table\ntitle: Orders\ntags:\n- sales\n---\nPart of [sales](../datasets/sales.md \"PART_OF\").",
        );
        write(
            dir.path(),
            "datasets/sales.md",
            "---\ntype: BigQuery Dataset\n---\nThe sales dataset.",
        );
        write(
            dir.path(),
            "index.md",
            "# Listing\n* [orders](tables/orders.md)",
        );
        write(dir.path(), "notes.txt", "not markdown");

        let docs = parse_bundle(dir.path(), &BuildOptions::default()).unwrap();
        assert_eq!(docs.len(), 2, "index.md reserved, notes.txt non-md");

        let orders = docs
            .iter()
            .find(|d| d.concept_id == "tables/orders")
            .unwrap();
        assert_eq!(orders.label, "BigQuery Table");
        assert_eq!(orders.title, "Orders");
        assert_eq!(orders.links.len(), 1);
        assert_eq!(orders.links[0].target, "datasets/sales");
        assert_eq!(orders.links[0].conn_type, "PART_OF");

        let sales = docs
            .iter()
            .find(|d| d.concept_id == "datasets/sales")
            .unwrap();
        assert_eq!(sales.label, "BigQuery Dataset");
        assert_eq!(sales.title, "sales", "title falls back to file stem");
    }

    #[test]
    fn no_frontmatter_skipped_by_default_degrades_when_allowed() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "plain.md",
            "# Just a note\n\nNo frontmatter here.",
        );
        // Default: structured-only → plain markdown is skipped.
        let docs = parse_bundle(dir.path(), &BuildOptions::default()).unwrap();
        assert_eq!(docs.len(), 0);
        // Opt out → it degrades to a Concept.
        let opts = BuildOptions {
            require_frontmatter: false,
            ..BuildOptions::default()
        };
        let docs = parse_bundle(dir.path(), &opts).unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].label, "Concept");
        // No frontmatter title/name → falls back to the body's first heading.
        assert_eq!(docs[0].title, "Just a note");
    }

    #[test]
    fn tag_line_is_not_taken_for_the_title() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "readme.md",
            "#project-x\n\n# My Project\n\nIntro.",
        );
        let opts = BuildOptions {
            require_frontmatter: false,
            ..BuildOptions::default()
        };
        let docs = parse_bundle(dir.path(), &opts).unwrap();
        assert_eq!(docs[0].title, "My Project");
    }

    #[test]
    fn title_from_first_heading_when_no_frontmatter_fields() {
        let dir = tempdir().unwrap();
        write(dir.path(), "readme.md", "# My Project\n\nIntro text.");
        let opts = BuildOptions {
            require_frontmatter: false,
            ..BuildOptions::default()
        };
        let docs = parse_bundle(dir.path(), &opts).unwrap();
        assert_eq!(docs[0].title, "My Project");
    }

    /// VAULT.md §3's heading rung is "the first heading", not "the first `# H1`":
    /// a producer that opens every note with an `##` would otherwise be titled
    /// by its filenames.
    #[test]
    fn a_heading_below_h1_still_titles_the_note() {
        let dir = tempdir().unwrap();
        write(dir.path(), "readme.md", "## My Project\n\nIntro text.");
        let opts = BuildOptions {
            require_frontmatter: false,
            ..BuildOptions::default()
        };
        let docs = parse_bundle(dir.path(), &opts).unwrap();
        assert_eq!(docs[0].title, "My Project");
    }

    /// CommonMark reads the lone `#` in `### #` as the optional **closing**
    /// sequence, so that heading titles nothing and the ladder falls through
    /// to the file stem. The pre-P2 line scanner kept the `#` and titled the
    /// note `#` (one heading in 8 917 across the RMS corpus).
    #[test]
    fn a_heading_that_is_only_a_closing_sequence_titles_nothing() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "readme.md",
            "### #

Intro text.",
        );
        write(
            dir.path(),
            "after.md",
            "### #

## Real one
",
        );
        let opts = BuildOptions {
            require_frontmatter: false,
            ..BuildOptions::default()
        };
        let docs = parse_bundle(dir.path(), &opts).unwrap();
        let titles: Vec<(&str, &str)> = docs
            .iter()
            .map(|d| (d.concept_id.as_str(), d.title.as_str()))
            .collect();
        assert_eq!(
            titles,
            vec![("after", "Real one"), ("readme", "readme")],
            "an empty heading is skipped, not used as a title"
        );
    }

    #[test]
    fn kg_skip_excludes_by_default_and_respects_override() {
        let dir = tempdir().unwrap();
        write(dir.path(), "keep.md", "---\ntype: Note\n---\nkeep me");
        write(
            dir.path(),
            "scratch.md",
            "---\ntype: Note\nkg_skip: true\n---\nignore me",
        );
        // Default: kg_skip files are excluded.
        let docs = parse_bundle(dir.path(), &BuildOptions::default()).unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].concept_id, "keep");
        // respect_skip=false ingests them anyway.
        let opts = BuildOptions {
            respect_skip: false,
            ..BuildOptions::default()
        };
        let docs = parse_bundle(dir.path(), &opts).unwrap();
        assert_eq!(docs.len(), 2);
    }

    #[test]
    fn skip_dirs_prunes_by_name_and_by_path() {
        let dir = tempdir().unwrap();
        write(dir.path(), "keep/a.md", "---\ntype: Note\n---\nkeep");
        write(
            dir.path(),
            "vendor/repos/b.md",
            "---\ntype: Note\n---\nclone",
        );
        write(dir.path(), "deep/cache/c.md", "---\ntype: Note\n---\ndep");

        // bare name matches at any depth; path entry is anchored to the subtree.
        let opts = BuildOptions {
            skip_dirs: vec!["cache".to_string(), "vendor/repos".to_string()],
            ..BuildOptions::default()
        };
        let ids: Vec<String> = parse_bundle(dir.path(), &opts)
            .unwrap()
            .into_iter()
            .map(|d| d.concept_id)
            .collect();
        assert_eq!(ids, vec!["keep/a"]);

        // without skip_dirs all three are ingested.
        let all = parse_bundle(dir.path(), &BuildOptions::default()).unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn label_and_title_fall_back_to_metadata_type_and_name() {
        let dir = tempdir().unwrap();
        // A Claude-memory-shaped file: no top-level `type`/`title`.
        write(
            dir.path(),
            "feedback_x.md",
            "---\nname: Cypher First\nmetadata:\n  type: feedback\n---\nbody",
        );
        let docs = parse_bundle(dir.path(), &BuildOptions::default()).unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(
            docs[0].label, "feedback",
            "label falls back to metadata.type"
        );
        assert_eq!(docs[0].title, "Cypher First", "title falls back to name");
    }

    /// Parse a vault with the obsidian profile, returning docs + findings.
    fn vault(dir: &Path) -> (Vec<ConceptDoc>, ParseFindings) {
        let opts = BuildOptions::for_dialect(Dialect::Obsidian);
        let walked = walk::discover(dir, &opts).unwrap();
        parse_concepts_reported(&walked.concepts, &opts)
    }

    fn doc<'a>(docs: &'a [ConceptDoc], id: &str) -> &'a ConceptDoc {
        docs.iter()
            .find(|d| d.concept_id == id)
            .unwrap_or_else(|| panic!("no doc with id `{id}` in {:?}", ids(docs)))
    }

    fn ids(docs: &[ConceptDoc]) -> Vec<&str> {
        docs.iter().map(|d| d.concept_id.as_str()).collect()
    }

    #[test]
    fn vault_label_ladder_walks_type_then_folder_then_note() {
        let dir = tempdir().unwrap();
        write(dir.path(), "root.md", "Just prose at the vault root.");
        write(dir.path(), "Geology/faults.md", "A note in a folder.");
        write(
            dir.path(),
            "Geology/deep/nested.md",
            "Three deep, still Geology.",
        );
        write(
            dir.path(),
            "Geology/typed.md",
            "---\ntype: Initiative\n---\nAn explicit type wins.",
        );
        let (docs, _) = vault(dir.path());
        assert_eq!(doc(&docs, "root").label, "Note", "no folder, no type");
        assert_eq!(doc(&docs, "faults").label, "Geology", "top-level folder");
        assert_eq!(
            doc(&docs, "nested").label,
            "Geology",
            "the TOP-level folder, not the immediate parent"
        );
        assert_eq!(doc(&docs, "typed").label, "Initiative");
    }

    #[test]
    fn vault_default_label_sits_between_type_and_folder() {
        let dir = tempdir().unwrap();
        write(dir.path(), "root.md", "root prose");
        write(dir.path(), "Geology/faults.md", "folder prose");
        write(
            dir.path(),
            "Geology/typed.md",
            "---\ntype: Initiative\n---\nprose",
        );
        let mut opts = BuildOptions::for_dialect(Dialect::Obsidian);
        opts.profile.default_label = Some("Article".to_string());
        let walked = walk::discover(dir.path(), &opts).unwrap();
        let docs = parse_concepts(&walked.concepts, &opts);
        assert_eq!(doc(&docs, "root").label, "Article");
        assert_eq!(
            doc(&docs, "faults").label,
            "Article",
            "the default label outranks the folder rung"
        );
        assert_eq!(doc(&docs, "typed").label, "Initiative");
    }

    #[test]
    fn label_from_folder_puts_the_folder_rung_first() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "Geology/typed.md",
            "---\ntype: Initiative\n---\nprose",
        );
        write(dir.path(), "root.md", "---\ntype: Initiative\n---\nprose");
        let mut opts = BuildOptions::for_dialect(Dialect::Obsidian);
        opts.profile.label_from = LabelFrom::Folder;
        let walked = walk::discover(dir.path(), &opts).unwrap();
        let docs = parse_concepts(&walked.concepts, &opts);
        assert_eq!(
            doc(&docs, "typed").label,
            "Geology",
            "the folder wins over an explicit type"
        );
        assert_eq!(
            doc(&docs, "root").label,
            "Initiative",
            "a root note has no folder rung, so type is next"
        );
    }

    #[test]
    fn vault_ignores_the_metadata_type_rung() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "memo.md",
            "---\nmetadata:\n  type: feedback\n---\nprose",
        );
        let (docs, _) = vault(dir.path());
        assert_eq!(docs[0].label, "Note");
        assert_eq!(
            docs[0].props.iter().find(|(k, _)| k == "metadata.type"),
            Some(&(
                "metadata.type".to_string(),
                Value::String("feedback".into())
            )),
            "still an ordinary property"
        );
    }

    #[test]
    fn vault_id_is_the_stem_or_the_declared_id() {
        let dir = tempdir().unwrap();
        write(dir.path(), "notes/meeting.md", "---\nid: mtg-1\n---\nprose");
        write(dir.path(), "notes/plain.md", "prose");
        write(dir.path(), "notes/numeric.md", "---\nid: 4711\n---\nprose");
        let (docs, findings) = vault(dir.path());
        assert_eq!(ids(&docs), vec!["4711", "mtg-1", "plain"]);
        // The unquoted id is usable — and reported, because YAML would have
        // turned `id: 007` into `7` just as quietly (VAULT.md §4.1).
        assert_eq!(
            findings.errors,
            vec!["notes/numeric.md: reserved key `id:` must be a string, not Int64".to_string()]
        );
        assert!(findings.warnings.is_empty(), "{:?}", findings.warnings);
        assert!(
            !doc(&docs, "mtg-1").props.iter().any(|(k, _)| k == "id"),
            "a declared id is identity, not a property"
        );
        assert_eq!(
            doc(&docs, "mtg-1").title,
            "meeting",
            "title still falls back to the FILE stem"
        );
    }

    #[test]
    fn okf_dialect_keeps_path_ids_and_an_id_property() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "notes/meeting.md",
            "---\ntype: Note\nid: mtg-1\n---\nprose",
        );
        let opts = BuildOptions::default();
        let docs = parse_bundle(dir.path(), &opts).unwrap();
        assert_eq!(ids(&docs), vec!["notes/meeting"]);
        assert!(docs[0].props.iter().any(|(k, _)| k == "id"));
    }

    #[test]
    fn colliding_stems_fall_back_to_path_ids_and_report_an_error() {
        let dir = tempdir().unwrap();
        write(dir.path(), "projects/alpha.md", "prose");
        write(dir.path(), "archive/alpha.md", "prose");
        write(dir.path(), "solo.md", "prose");
        let (docs, findings) = vault(dir.path());
        assert_eq!(ids(&docs), vec!["archive/alpha", "projects/alpha", "solo"]);
        assert_eq!(findings.errors.len(), 1);
        assert!(
            findings.errors[0].contains("`alpha`")
                && findings.errors[0].contains("archive/alpha.md")
                && findings.errors[0].contains("projects/alpha.md"),
            "the error names the id and both paths: {}",
            findings.errors[0]
        );
        assert!(findings.warnings.is_empty());
    }

    #[test]
    fn two_notes_declaring_one_id_also_fall_back() {
        let dir = tempdir().unwrap();
        write(dir.path(), "a.md", "---\nid: shared\n---\nprose");
        write(dir.path(), "b.md", "---\nid: shared\n---\nprose");
        let (docs, findings) = vault(dir.path());
        assert_eq!(ids(&docs), vec!["a", "b"], "neither note is lost");
        assert_eq!(findings.errors.len(), 1);
    }

    #[test]
    fn a_fallback_that_collides_again_is_settled_too() {
        let dir = tempdir().unwrap();
        // `alpha` collides; the fallback `archive/alpha` is what `pin.md`
        // declared, so a single pass would merge those two instead.
        write(dir.path(), "projects/alpha.md", "prose");
        write(dir.path(), "archive/alpha.md", "prose");
        write(dir.path(), "pin.md", "---\nid: archive/alpha\n---\nprose");
        let (docs, findings) = vault(dir.path());
        assert_eq!(
            ids(&docs),
            vec!["archive/alpha", "pin", "projects/alpha"],
            "every note keeps a distinct id"
        );
        assert_eq!(findings.errors.len(), 2, "both rounds are reported");
    }

    #[test]
    fn case_only_clashes_warn_without_changing_ids() {
        let dir = tempdir().unwrap();
        write(dir.path(), "projects/Roadmap.md", "prose");
        write(dir.path(), "notes/roadmap.md", "prose");
        let (docs, findings) = vault(dir.path());
        assert_eq!(ids(&docs), vec!["Roadmap", "roadmap"], "ids are untouched");
        assert!(findings.errors.is_empty());
        assert_eq!(findings.warnings.len(), 1);
        assert!(
            findings.warnings[0].contains("`Roadmap` (projects/Roadmap.md)")
                && findings.warnings[0].contains("`roadmap` (notes/roadmap.md)"),
            "the warning names both: {}",
            findings.warnings[0]
        );
    }

    #[test]
    fn okf_dialect_reports_no_collisions_at_all() {
        let dir = tempdir().unwrap();
        write(dir.path(), "projects/Alpha.md", "---\ntype: N\n---\nprose");
        write(dir.path(), "archive/alpha.md", "---\ntype: N\n---\nprose");
        let opts = BuildOptions::default();
        let walked = walk::discover(dir.path(), &opts).unwrap();
        let (_, findings) = parse_concepts_reported(&walked.concepts, &opts);
        assert_eq!(findings, ParseFindings::default());
    }

    #[test]
    fn vault_infers_iso_dates_and_rfc3339_stamps() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "a.md",
            "---\nupdated: 2026-01-15\nreviewed: '2026-01-15T09:30:00+02:00'\n\
             version: '2026-1-5'\nepoch: '1609459200000'\ntags:\n- 2026-01-15\n---\nprose",
        );
        let (docs, _) = vault(dir.path());
        let props: BTreeMap<&str, &Value> =
            docs[0].props.iter().map(|(k, v)| (k.as_str(), v)).collect();
        assert_eq!(
            props["updated"],
            &Value::DateTime(chrono::NaiveDate::from_ymd_opt(2026, 1, 15).unwrap())
        );
        assert_eq!(
            props["reviewed"],
            &Value::Timestamp(
                chrono::NaiveDate::from_ymd_opt(2026, 1, 15)
                    .unwrap()
                    .and_hms_opt(7, 30, 0)
                    .unwrap()
            ),
            "an offset normalises to UTC"
        );
        assert_eq!(
            props["version"],
            &Value::String("2026-1-5".into()),
            "not ISO YYYY-MM-DD"
        );
        assert_eq!(
            props["epoch"],
            &Value::String("1609459200000".into()),
            "epoch millis are not a date spelling"
        );
        assert_eq!(
            props["tags"],
            &Value::List(vec![Value::String("2026-01-15".into())]),
            "list elements keep the type YAML gave them"
        );
    }

    #[test]
    fn okf_dialect_leaves_date_strings_alone() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "a.md",
            "---\ntype: Note\nupdated: 2026-01-15\n---\nprose",
        );
        let docs = parse_bundle(dir.path(), &BuildOptions::default()).unwrap();
        assert_eq!(
            docs[0]
                .props
                .iter()
                .find(|(k, _)| k == "updated")
                .unwrap()
                .1,
            Value::String("2026-01-15".into())
        );
    }

    #[test]
    fn vault_stores_bodies_and_ingests_plain_notes_by_default() {
        let dir = tempdir().unwrap();
        write(dir.path(), "plain.md", "No frontmatter, just prose.");
        let opts = BuildOptions::for_dialect(Dialect::Obsidian);
        assert!(!opts.require_frontmatter, "a vault is mostly plain notes");
        assert!(opts.with_body);
        let docs = parse_bundle(dir.path(), &opts).unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].body.as_deref(), Some("No frontmatter, just prose."));

        // The explicit option still wins, in both directions.
        let mut off = BuildOptions::for_dialect(Dialect::Obsidian);
        off.with_body = false;
        off.require_frontmatter = true;
        assert!(
            parse_bundle(dir.path(), &off).unwrap().is_empty(),
            "an explicit require_frontmatter=true skips the plain note again"
        );

        let okf = BuildOptions::for_dialect(Dialect::Okf);
        assert!(okf.require_frontmatter, "the OKF sweep discriminator stays");
        assert!(!okf.with_body, "OKF ingestion stays partial");
        let mut on = okf;
        on.with_body = true;
        on.require_frontmatter = false;
        assert_eq!(
            parse_bundle(dir.path(), &on).unwrap()[0].body.as_deref(),
            Some("No frontmatter, just prose.")
        );
    }

    #[test]
    fn with_body_retains_body() {
        let dir = tempdir().unwrap();
        write(dir.path(), "a.md", "---\ntype: Note\n---\nbody content");
        let opts = BuildOptions {
            with_body: true,
            ..BuildOptions::default()
        };
        let docs = parse_bundle(dir.path(), &opts).unwrap();
        assert_eq!(docs[0].body.as_deref(), Some("body content"));
        let docs2 = parse_bundle(dir.path(), &BuildOptions::default()).unwrap();
        assert_eq!(docs2[0].body, None);
    }
}
