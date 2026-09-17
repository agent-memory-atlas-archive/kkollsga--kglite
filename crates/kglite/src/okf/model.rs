//! Data model for OKF bundle ingestion.
//!
//! A bundle is a directory tree of markdown files with YAML frontmatter,
//! cross-linked by markdown links. Each non-reserved `.md` file becomes one
//! [`ConceptDoc`]; the links within become edges. The model is deliberately
//! *partial* — the body is not retained unless [`BuildOptions::with_body`] is
//! set, mirroring code-graph builders (store structure + a `file_path` pointer; read the
//! body on demand).

use crate::datatypes::values::Value;
use std::collections::BTreeMap;

/// Which link / frontmatter conventions to honour when parsing a bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    /// Strict OKF: bundle-relative `[text](/path.md "TYPE")` markdown links.
    /// A missing `type` still degrades to the `Concept` label (OKF mandates
    /// permissive consumption) rather than erroring.
    Okf,
    /// Loose: everything `Okf` does, **plus** Obsidian-style `[[wikilink]]`
    /// resolution (by file stem). For memory dirs / vaults that aren't strict
    /// OKF bundles.
    Loose,
    /// Obsidian vault: `Loose`'s wikilinks plus the vault conventions carried
    /// by [`Profile::obsidian`].
    Obsidian,
}

impl Dialect {
    /// Parse a dialect name. `None`, `"okf"` → [`Dialect::Okf`]; `"loose"` →
    /// [`Dialect::Loose`]; `"obsidian"` → [`Dialect::Obsidian`]. Unknown
    /// strings fall back to `Okf`.
    pub fn parse(name: Option<&str>) -> Self {
        match name.map(|s| s.to_ascii_lowercase()).as_deref() {
            Some("loose") => Dialect::Loose,
            Some("obsidian") => Dialect::Obsidian,
            _ => Dialect::Okf,
        }
    }

    /// Whether `[[wikilink]]` syntax is resolved in this dialect.
    pub fn wikilinks(self) -> bool {
        matches!(self, Dialect::Loose | Dialect::Obsidian)
    }
}

/// Which rung of the label ladder is tried first (VAULT.md §2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelFrom {
    /// Frontmatter `type:` first, the folder rung after the default label.
    Type,
    /// The top-level folder first, so moving a note between folders relabels
    /// it even when it carries a `type:`. The remaining rungs keep their order.
    Folder,
}

/// How a note's id is chosen (VAULT.md §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdScheme {
    /// Bundle-relative path minus `.md` — unique by construction, and it
    /// changes whenever the file moves.
    Path,
    /// Frontmatter `id:` → the filename stem. A vault's link namespace *is* the
    /// stem, and an id that survives a folder move is what embedding carry and
    /// external references key on. Colliding stems fall back to the path id;
    /// see [`crate::okf::model::BuildReport::errors`].
    FrontmatterOrStem,
}

/// The conventions a dialect brings, as data rather than as `match` arms.
///
/// A dialect name selects a `Profile`, and behaviour reads the profile's
/// fields; the dialect itself is interpreted only here and in
/// [`Dialect::wikilinks`]. That keeps a convention from threading through the
/// walker, parser and builder as a value each of them re-interprets, and gives
/// a vault's own declaration file one struct to override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    /// Read by [`crate::okf::walk::discover`]: a directory's `index.md`
    /// describes the directory, so it is diverted to that directory's `Folder`
    /// node instead of becoming a concept of its own.
    pub index_as_folder_metadata: bool,
    /// Read by [`crate::okf::walk::discover`]: `log.md` is a running journal,
    /// not a concept — drop it from the walk.
    pub skip_log_files: bool,
    /// Read by `crate::okf::parse_file`: which label rung is tried first.
    pub label_from: LabelFrom,
    /// Read by `crate::okf::parse_file`: the label for a note whose
    /// frontmatter names none, tried *before* the folder rung. `None` here; a
    /// vault declares one in `.kglite/vault.yaml`.
    pub default_label: Option<String>,
    /// Read by `crate::okf::parse_file`: honour a Claude-memory
    /// `metadata.type` as a label rung after `type:`. Off in a vault, where
    /// `metadata:` is ordinary frontmatter.
    pub metadata_type_label: bool,
    /// Read by `crate::okf::parse_file`: use the note's **top-level** folder
    /// name (verbatim — no singularising, no case change) as a label rung.
    pub folder_label: bool,
    /// Read by `crate::okf::parse_file`: the label when no rung yielded one.
    pub fallback_label: String,
    /// Read by `crate::okf::parse_file` and `crate::okf::resolve_ids`.
    pub id_scheme: IdScheme,
    /// Read by [`crate::okf::build::column_value`]: keep sequences and nested
    /// maps as `Value::List` / `Value::Map` columns. Off for OKF bundles,
    /// which JSON-encode them into String columns (codingest's convention).
    pub native_collections: bool,
    /// Read by [`BuildOptions::for_dialect`]: the dialect's
    /// `require_frontmatter` default. A vault is mostly plain notes; the
    /// frontmatter discriminator exists for sweeping a mixed tree. The option
    /// set on [`BuildOptions`] after that call still wins, in both directions.
    pub require_frontmatter: bool,
    /// Read by [`BuildOptions::for_dialect`]: the dialect's `with_body`
    /// default. A vault stores the prose (BM25 and embeddings want it); an OKF
    /// sweep keeps the `file_path` pointer and reads bodies on demand. The
    /// option set on [`BuildOptions`] after that call still wins, in both
    /// directions.
    pub store_body: bool,
    /// Read by `crate::okf::parse_file`: retype a frontmatter string that is
    /// an ISO `YYYY-MM-DD` date or an RFC 3339 timestamp as the matching
    /// temporal `Value`. Top-level scalars only — a list element keeps the
    /// type YAML gave it, so a tag literally named `2026-01-01` stays a tag.
    pub infer_temporal: bool,
}

impl Default for Profile {
    /// The OKF-bundle conventions, which `Okf` and `Loose` both use.
    fn default() -> Self {
        Profile {
            index_as_folder_metadata: true,
            skip_log_files: true,
            label_from: LabelFrom::Type,
            default_label: None,
            metadata_type_label: true,
            folder_label: false,
            fallback_label: DEFAULT_LABEL.to_string(),
            id_scheme: IdScheme::Path,
            native_collections: false,
            require_frontmatter: true,
            store_body: false,
            infer_temporal: false,
        }
    }
}

impl Profile {
    /// The conventions of [`Dialect::Obsidian`], normative in `VAULT.md`.
    pub fn obsidian() -> Self {
        Profile {
            label_from: LabelFrom::Type,
            default_label: None,
            metadata_type_label: false,
            folder_label: true,
            fallback_label: VAULT_DEFAULT_LABEL.to_string(),
            id_scheme: IdScheme::FrontmatterOrStem,
            native_collections: true,
            require_frontmatter: false,
            store_body: true,
            infer_temporal: true,
            ..Profile::default()
        }
    }

    /// The profile a dialect selects.
    pub fn for_dialect(dialect: Dialect) -> Self {
        match dialect {
            Dialect::Okf | Dialect::Loose => Profile::default(),
            Dialect::Obsidian => Profile::obsidian(),
        }
    }
}

/// Options controlling a bundle build.
#[derive(Debug, Clone)]
pub struct BuildOptions {
    pub dialect: Dialect,
    /// The dialect's conventions, as values the walker/parser/builder read.
    /// [`BuildOptions::for_dialect`] keeps it in step with `dialect`; setting
    /// `dialect` alone does not.
    pub profile: Profile,
    /// Only ingest `.md` files that have a YAML frontmatter block — the
    /// discriminator between *structured* knowledge (OKF concepts, Claude
    /// memories) and plain markdown (READMEs, notes). On by default, so pointing
    /// at a large mixed tree (e.g. a parent of many projects) sweeps out only the
    /// structured files. Set false to ingest every `.md` (vault-style).
    pub require_frontmatter: bool,
    /// Honor the `kg_skip: true` frontmatter marker (exclude that file from the
    /// sweep). On by default; set false to ingest skip-marked files anyway.
    pub respect_skip: bool,
    /// Directories to prune from the walk (the directory **and its whole
    /// subtree**). gitignore-style: an entry without a `/` matches a directory by
    /// **name** at any depth (`"node_modules"`, `"target"`); an entry with a `/`
    /// is an anchored **bundle-relative path** (`"vendor/repos"`). For excluding
    /// cloned / vendored trees you don't own.
    pub skip_dirs: Vec<String>,
    /// Store each concept's markdown body as a `body` property. Off by default
    /// (partial ingestion — read bodies on demand via the file pointer).
    pub with_body: bool,
    /// Reserved for the opt-in embedder pass (stores body vectors for
    /// `text_score`). Not wired in the core loader; honoured by the wheel.
    pub embed: bool,
}

impl Default for BuildOptions {
    fn default() -> Self {
        BuildOptions {
            dialect: Dialect::Okf,
            profile: Profile::default(),
            require_frontmatter: true,
            respect_skip: true,
            skip_dirs: Vec::new(),
            with_body: false,
            embed: false,
        }
    }
}

impl BuildOptions {
    /// Default options for a dialect, with the matching [`Profile`]. The
    /// intended constructor: set the remaining fields on the result rather than
    /// spelling out a struct literal, so a new field keeps its dialect default.
    pub fn for_dialect(dialect: Dialect) -> Self {
        let profile = Profile::for_dialect(dialect);
        BuildOptions {
            dialect,
            require_frontmatter: profile.require_frontmatter,
            with_body: profile.store_body,
            profile,
            ..BuildOptions::default()
        }
    }
}

/// What a build saw — the counts a caller reports, gates on, or prints.
///
/// Every number comes from data the build already computed on its way to the
/// graph; nothing here costs a second pass over the bundle.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildReport {
    /// `.md` files the walk handed to the parser.
    pub files_scanned: usize,
    /// Files that became concept nodes. Lower than `files_scanned` when
    /// `require_frontmatter` or `kg_skip` excluded some.
    pub concepts: usize,
    /// Nodes added per label, synthesized `Folder`/`Tag`/`Source` nodes and
    /// `_provisional` stubs included.
    pub nodes_by_label: BTreeMap<String, usize>,
    /// Connection rows emitted per edge type. The mutator collapses duplicate
    /// source/target pairs, so the built graph can hold fewer edges than this.
    pub edges_by_type: BTreeMap<String, usize>,
    /// Link targets that matched no concept and vivified as `_provisional`
    /// stubs.
    pub dangling: usize,
    /// Problems that leave the build's output untrustworthy — a caller that
    /// gates on the report fails on a non-empty list.
    pub errors: Vec<String>,
    /// Problems worth surfacing that still leave a usable graph.
    pub warnings: Vec<String>,
}

/// A resolved cross-link from a concept to another concept or an external URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// Target concept-id (bundle-relative path minus `.md`) for path links, the
    /// raw wikilink name (resolved in the builder), or the URL for external
    /// links.
    pub target: String,
    /// Edge type from the inference ladder: explicit link title → section
    /// header → `LINKS_TO`.
    pub conn_type: String,
    /// True when `target` is a wikilink awaiting builder resolution.
    pub is_wikilink: bool,
    /// True when `target` is an external `http(s)` URL — becomes a `Source` node
    /// rather than resolving to a concept.
    pub is_external: bool,
}

/// One parsed concept document. Partial by default: `body` is `None` unless
/// `with_body` was requested.
#[derive(Debug, Clone)]
pub struct ConceptDoc {
    /// Bundle-relative path minus `.md`, forward-slashed (e.g. `tables/users`).
    /// Used as the node id and as the link-resolution target.
    pub concept_id: String,
    /// Bundle-relative path to the source file (the on-demand body pointer).
    pub file_path: String,
    /// Node label: frontmatter `type`, or `Concept` when absent.
    pub label: String,
    /// Display title: frontmatter `title`, or the file stem.
    pub title: String,
    /// Flattened frontmatter (excluding `type`/`title`): scalars direct, `tags`
    /// and other sequences as `Value::List`, nested maps flattened to dotted
    /// keys (`metadata.type`).
    pub props: Vec<(String, Value)>,
    /// Resolved outbound links (becoming edges).
    pub links: Vec<Link>,
    /// Body markdown — `Some` only when `with_body` was requested.
    pub body: Option<String>,
}

/// Default edge type when no title or section header gives a more specific one.
pub const DEFAULT_CONN_TYPE: &str = "LINKS_TO";
/// Structural edge type for directory containment (parent dir → child concept).
pub const CONTAINS_CONN_TYPE: &str = "CONTAINS";
/// Node label assigned to concepts with no frontmatter `type`.
pub const DEFAULT_LABEL: &str = "Concept";
/// Node label a vault note falls back to when no rung of the label ladder
/// (VAULT.md §2.1) named one — a root-level note with no `type:` and no
/// `default_label:`.
pub const VAULT_DEFAULT_LABEL: &str = "Note";
/// Node label for synthesized tag nodes; edge type concept → tag.
pub const TAG_LABEL: &str = "Tag";
pub const TAGGED_CONN_TYPE: &str = "TAGGED";
/// Node label for synthesized external-source (URL) nodes.
pub const SOURCE_LABEL: &str = "Source";
/// Node label for synthesized directory nodes (the bundle's folder hierarchy).
pub const FOLDER_LABEL: &str = "Folder";
/// Frontmatter key that opts a file out of the sweep (`kg_skip: true`).
pub const SKIP_KEY: &str = "kg_skip";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialect_names_map_to_variants() {
        assert_eq!(Dialect::parse(None), Dialect::Okf);
        assert_eq!(Dialect::parse(Some("okf")), Dialect::Okf);
        assert_eq!(Dialect::parse(Some("loose")), Dialect::Loose);
        assert_eq!(Dialect::parse(Some("Obsidian")), Dialect::Obsidian);
        assert_eq!(Dialect::parse(Some("nonsense")), Dialect::Okf);
        assert!(!Dialect::Okf.wikilinks());
        assert!(Dialect::Loose.wikilinks());
        assert!(Dialect::Obsidian.wikilinks());
    }

    #[test]
    fn for_dialect_pairs_the_profile_with_the_dialect() {
        let opts = BuildOptions::for_dialect(Dialect::Obsidian);
        assert_eq!(opts.dialect, Dialect::Obsidian);
        assert_eq!(opts.profile, Profile::obsidian());
        assert_eq!(
            BuildOptions::for_dialect(Dialect::Loose).profile,
            Profile::default()
        );
    }
}
