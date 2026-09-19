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
        match name {
            Some(name) => Dialect::from_name(name).unwrap_or(Dialect::Okf),
            None => Dialect::Okf,
        }
    }

    /// The dialect a name denotes, or `None` when it denotes none.
    ///
    /// The strict counterpart to [`Dialect::parse`], for the two callers that
    /// must not silently degrade to `Okf`: a terminal, where `--dialect
    /// obsidan` would read a vault as a bundle, and the build stamp, where an
    /// unrecognised name means the graph was written by a build that knows a
    /// dialect this one does not.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "okf" => Some(Dialect::Okf),
            "loose" => Some(Dialect::Loose),
            "obsidian" => Some(Dialect::Obsidian),
            _ => None,
        }
    }

    /// The name [`Dialect::from_name`] parses back, and the one the build
    /// stamp persists.
    pub fn name(self) -> &'static str {
        match self {
            Dialect::Okf => "okf",
            Dialect::Loose => "loose",
            Dialect::Obsidian => "obsidian",
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

/// Which way a `parent:` / folder-note edge points (VAULT.md §2.3, §4.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderNoteDirection {
    /// The note carrying `parent:` is the source: `child -[CHILD_OF]-> parent`.
    ChildToParent,
    /// The named parent is the source: `parent -[CONTAINS]-> child`.
    ParentToChild,
}

/// One hub declared in `hubs:` (VAULT.md §5.5, §7): a frontmatter list key
/// whose entries become shared nodes every note carrying them links to.
///
/// The built-in `tags` hub is one of these, so a vault that renames it or adds
/// `keywords:` alongside it is configuration rather than a second code path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubSpec {
    /// Label of the synthesized hub nodes (`Tag`, `Keyword`, …).
    pub label: String,
    /// Edge type from the note to the hub node (`TAGGED`, `HAS_KEYWORD`, …).
    pub edge: String,
    /// Fold the hub's identity to lowercase, so `Faults` and `faults` are one
    /// node. The node id is then the folded form and its `title` is the
    /// casing the vault used most often — ties settled alphabetically, so the
    /// title never depends on which note was read first.
    pub case_insensitive: bool,
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
    /// Read by [`crate::okf::links::extract`]: resolve `[[wikilink]]` syntax.
    /// [`BuildOptions::for_dialect`] sets it from [`Dialect::wikilinks`], so
    /// the dialect name stays the one place a user says "this vault uses
    /// wikilinks" and every reader below asks the profile.
    pub wikilinks: bool,
    /// Read by [`crate::okf::links::extract`]: carry `section` (the enclosing
    /// heading's text) and `anchor` (a link's `#fragment`) as edge properties
    /// on every body link (VAULT.md §5.4).
    pub link_edge_props: bool,
    /// Read by [`crate::okf::links::extract`]: `![[Note]]` is an
    /// [`EMBEDS_CONN_TYPE`] edge (VAULT.md §5.1). An embed naming a non-`.md`
    /// file stays dropped either way — it is an attachment, not a link.
    pub embeds: bool,
    /// Read by [`crate::okf::links::extract`]: an inline `#tag` in the body
    /// feeds the same `Tag` hub as `tags:` (VAULT.md §5.5). The `tags` list
    /// property keeps saying exactly what the frontmatter said.
    pub inline_tags: bool,
    /// Read by `crate::okf::parse_file`: a frontmatter key whose value is a
    /// wikilink string — or a list of nothing but wikilink strings — becomes
    /// edges typed `UPPER_SNAKE(key)` instead of a property (VAULT.md §4.3).
    pub frontmatter_edges: bool,
    /// Read by [`crate::okf::build::resolver::Resolver`]: a note's `aliases:` entries
    /// answer link resolution, between the stem and slug rungs (VAULT.md §5.2).
    pub alias_resolution: bool,
    /// Read by `crate::okf::parse_file` and
    /// [`crate::okf::build::folders::push_containment`]: the edge type a reserved
    /// `parent:` emits, and the one the folder layout joins a folder note's
    /// children by — one type, so a cross-listed note contributes exactly the
    /// edges the layout would have (VAULT.md §2.3, §4.3).
    pub folder_note_edge: String,
    /// Read by `crate::okf::parse_file`: which way that edge points.
    pub folder_note_direction: FolderNoteDirection,
    /// Read by [`crate::okf::build::folders::build_folders`]: `X.md` beside `X/`, or
    /// `X/X.md`, is that directory's **folder note** — it replaces the
    /// directory's `Folder` node and the notes inside are joined to it by
    /// [`Profile::folder_note_edge`] instead of `CONTAINS` (VAULT.md §2.3).
    pub folder_notes: bool,
    /// Read by [`crate::okf::build::hubs::build_hubs`]: frontmatter key → the hub its
    /// list entries join (VAULT.md §5.5, §7). Every dialect declares `tags` →
    /// `Tag`/`TAGGED` here; a vault adds its own in `.kglite/vault.yaml`.
    pub hubs: BTreeMap<String, HubSpec>,
    /// Read by [`crate::okf::links::conn_from_heading`]: heading text → edge
    /// type, merged *over* the built-in heading ladder and matched on the
    /// whole heading, case-insensitively (VAULT.md §5.3).
    pub heading_edges: BTreeMap<String, String>,
    /// Read by [`crate::okf::build::nodes::build_nodes`]: the property name a note's
    /// prose is stored under when `with_body` is set (VAULT.md §7 `body:`).
    /// `body` everywhere unless `.kglite/vault.yaml` renames it — a vault
    /// whose notes already carry a frontmatter `body:` key needs the prose
    /// somewhere else.
    pub body_property: String,
    /// Read by [`crate::okf::walk::discover`]: extra directories to prune,
    /// unioned with [`BuildOptions::skip_dirs`]. The profile's copy is what
    /// `.kglite/vault.yaml` declares; the options' copy is what the caller
    /// passed. Same gitignore-style matching for both.
    pub skip_dirs: Vec<String>,
    /// Read by `crate::okf::parse_file`: retype a frontmatter string that is
    /// an ISO `YYYY-MM-DD` date or an RFC 3339 timestamp as the matching
    /// temporal `Value`. Top-level scalars only — a list element keeps the
    /// type YAML gave it, so a tag literally named `2026-01-01` stays a tag.
    pub infer_temporal: bool,
    /// Read by [`crate::okf::links::extract`],
    /// [`crate::okf::walk::discover`] and
    /// [`crate::okf::build::attachments::build_attachments`]: an `![alt](x.png)` or
    /// `![[x.png]]` reference becomes an `Image` / `Attachment` node
    /// (VAULT.md §6). Off for `okf`/`loose`, which keep dropping the
    /// reference — and which therefore never pay the walk's `stat` per
    /// non-`.md` file either.
    pub attachments: bool,
    /// Read by `crate::okf::parse_file`: check VAULT.md §4.1's reserved keys
    /// for the shape their meaning depends on — a string `id:`/`type:`, a list
    /// `tags:`/`aliases:`, a boolean `kg_skip:`. Off for `okf`/`loose`, whose
    /// producers (codingest, Claude memories) write those keys to their own
    /// conventions.
    pub reserved_key_shapes: bool,
    /// Read by `crate::okf::parse_file`: frontmatter keys this profile drops
    /// outright — no property, no typed edge, no hub value (VAULT.md §4.1).
    /// Obsidian's `cssclasses:` styles how a note renders and says nothing
    /// about the knowledge in it; `okf`/`loose` have no such key and keep
    /// whatever a producer wrote.
    pub(crate) ignored_keys: &'static [&'static str],
    /// Read by [`crate::okf::structure::derive`]: what this vault derives from
    /// a note's own body — sections, chunks, callouts, fenced examples,
    /// ordered lists, and what decorates them (VAULT.md §7.1). `None` on every dialect and on every vault that
    /// declares no `structure:`, and a `None` here is the 0.17.8 build: note
    /// for note and edge for edge.
    pub(crate) structure: Option<crate::okf::structure::StructureProfile>,
    /// Read by [`crate::okf::build`]: `edge_defaults:` — properties that are
    /// constant for a whole edge type, pushed onto every edge of it the build
    /// emits (VAULT.md §7.2). Empty on every dialect; a vault declares them.
    pub(crate) edge_defaults: BTreeMap<String, Vec<(String, Value)>>,
    /// Read by [`crate::okf::links::extract`]: refuse a body reference that
    /// names a place the vault does not own — an absolute filesystem path, or
    /// one climbing above the root (VAULT.md §9). Off for `okf`/`loose`, where
    /// a sweep of many bundles under one parent legitimately links across
    /// roots and a `../` is how it is written.
    pub path_safety: bool,
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
            wikilinks: false,
            link_edge_props: false,
            embeds: false,
            inline_tags: false,
            frontmatter_edges: false,
            alias_resolution: false,
            folder_note_edge: FOLDER_NOTE_CONN_TYPE.to_string(),
            folder_note_direction: FolderNoteDirection::ChildToParent,
            folder_notes: false,
            hubs: default_hubs(false),
            heading_edges: BTreeMap::new(),
            skip_dirs: Vec::new(),
            body_property: DEFAULT_BODY_PROPERTY.to_string(),
            infer_temporal: false,
            attachments: false,
            path_safety: false,
            reserved_key_shapes: false,
            ignored_keys: &[],
            structure: None,
            edge_defaults: BTreeMap::new(),
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
            wikilinks: true,
            link_edge_props: true,
            embeds: true,
            inline_tags: true,
            frontmatter_edges: true,
            alias_resolution: true,
            folder_notes: true,
            index_as_folder_metadata: false,
            skip_log_files: false,
            infer_temporal: true,
            attachments: true,
            path_safety: true,
            reserved_key_shapes: true,
            ignored_keys: VAULT_IGNORED_KEYS,
            hubs: default_hubs(true),
            ..Profile::default()
        }
    }

    /// The profile a dialect selects. `wikilinks` comes from the dialect
    /// itself, which is why `Loose` is `Profile::default()` with that one
    /// field flipped rather than a profile of its own.
    pub fn for_dialect(dialect: Dialect) -> Self {
        let base = match dialect {
            Dialect::Okf | Dialect::Loose => Profile::default(),
            Dialect::Obsidian => Profile::obsidian(),
        };
        Profile {
            wikilinks: dialect.wikilinks(),
            ..base
        }
    }
}

/// The one hub every dialect has: `tags:` → `Tag` nodes joined by `TAGGED`.
///
/// Folded in a **vault**, where `#Seismic` and `#seismic` are one tag as they
/// are in Obsidian (VAULT.md §5.5), and case-**sensitive** in an `okf`/`loose`
/// bundle, where tag identity has always been the string the frontmatter
/// spelled and folding it would merge two existing `Tag` nodes in every bundle
/// already built. Either default is redeclarable in `.kglite/vault.yaml` with
/// `hubs: {tags: {case_insensitive: …}}`.
fn default_hubs(case_insensitive: bool) -> BTreeMap<String, HubSpec> {
    BTreeMap::from([(
        "tags".to_string(),
        HubSpec {
            label: TAG_LABEL.to_string(),
            edge: TAGGED_CONN_TYPE.to_string(),
            case_insensitive,
        },
    )])
}

/// The built-in hub a `.kglite/vault.yaml` redeclaration merges over, when
/// the key names one (VAULT.md §7). A redeclaration states only what it
/// changes, so `hubs: {tags: {label: Topic}}` has to keep the vault default's
/// folding rather than silently returning tag identity to case-sensitive.
pub(crate) fn vault_builtin_hub(key: &str) -> Option<HubSpec> {
    default_hubs(true).get(key).cloned()
}

/// VAULT.md §4.1's ignored keys: frontmatter a vault writes for Obsidian
/// itself, which describes the *rendering* rather than the knowledge. Storing
/// `cssclasses:` would put a stylesheet name on the node and — its value being
/// a list — offer it to the typed-edge rule as well.
const VAULT_IGNORED_KEYS: &[&str] = &["cssclasses"];

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

/// What to read a *rebuild* with, when the dialect may be the graph's own.
///
/// [`BuildOptions`] cannot express this: its `dialect` is a value, and a
/// rebuild has to distinguish "read it as a vault" from "read it as whatever
/// this graph was built as". The distinction is the whole of the trap the
/// build stamp closes — the fingerprint is dialect-dependent, so a caller who
/// meant "am I current?" and got the wrong dialect is told "changed", every
/// time, and rebuilds into a graph of a different shape.
///
/// The non-dialect knobs are the ones [`BuildOptions`] takes from the dialect:
/// `None` keeps whatever the resolved dialect chose, a value overrides it in
/// both directions. Any field added to `BuildOptions` that a dialect does
/// *not* set belongs here too, or a rebuild cannot set it.
#[derive(Debug, Clone)]
pub struct RebuildOptions {
    /// The dialect to read the directory with, or `None` to use the one the
    /// graph's build stamped (`DirGraph::source_dialect`).
    pub dialect: Option<Dialect>,
    /// The conventions to read with, or `None` for the resolved dialect's
    /// own — the one knob whose default cannot be named before the dialect
    /// is.
    pub profile: Option<Profile>,
    pub require_frontmatter: Option<bool>,
    pub respect_skip: bool,
    pub skip_dirs: Vec<String>,
    pub with_body: Option<bool>,
}

impl Default for RebuildOptions {
    fn default() -> Self {
        RebuildOptions {
            dialect: None,
            profile: None,
            require_frontmatter: None,
            respect_skip: true,
            skip_dirs: Vec::new(),
            with_body: None,
        }
    }
}

impl RebuildOptions {
    /// Read the directory as `dialect`, with nothing else asked for.
    pub fn for_dialect(dialect: Dialect) -> Self {
        RebuildOptions {
            dialect: Some(dialect),
            ..RebuildOptions::default()
        }
    }

    /// The full options once the dialect is known: that dialect's own
    /// defaults, with these overrides applied over them.
    pub fn resolve(&self, dialect: Dialect) -> BuildOptions {
        let mut opts = BuildOptions::for_dialect(dialect);
        if let Some(profile) = &self.profile {
            // Same order `for_dialect` uses: the profile carries the two
            // defaults, and an explicit override below still wins.
            opts.require_frontmatter = profile.require_frontmatter;
            opts.with_body = profile.store_body;
            opts.profile = profile.clone();
        }
        if let Some(value) = self.require_frontmatter {
            opts.require_frontmatter = value;
        }
        opts.respect_skip = self.respect_skip;
        opts.skip_dirs = self.skip_dirs.clone();
        if let Some(value) = self.with_body {
            opts.with_body = value;
        }
        opts
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
    /// `_provisional` stubs included. The `KgliteSkill` / `KgliteRecipe` nodes
    /// a vault carries (VAULT.md §8) are **not** here: they are content the
    /// build imported rather than notes it read, and `skills_imported` /
    /// `recipes_imported` count them.
    pub nodes_by_label: BTreeMap<String, usize>,
    /// Connection rows emitted per edge type. The mutator collapses duplicate
    /// source/target pairs, so the built graph can hold fewer edges than this.
    pub edges_by_type: BTreeMap<String, usize>,
    /// Link targets that matched no concept and vivified as `_provisional`
    /// stubs.
    pub dangling: usize,
    /// Directories whose `Folder` node a folder note replaced (VAULT.md §2.3).
    pub folder_notes: usize,
    /// Distinct attachment references that matched no file and vivified as
    /// `missing: true` stubs (VAULT.md §6.6). An ambiguous filename is one of
    /// these: it did not resolve.
    pub missing_attachments: usize,
    /// The subset of [`BuildReport::missing_attachments`] that failed *because*
    /// the bare filename named two or more files (VAULT.md §6.2) — a vault
    /// whose references need qualifying, not one whose files are absent.
    pub ambiguous_attachments: usize,
    /// `(label, property)` pairs `.kglite/vault.yaml`'s `embed:` declared
    /// (VAULT.md §7), in declaration order. **The core never embeds**: it
    /// links no embedder, so it reports the targets and leaves the vectors to
    /// whoever has one — the wheel's rebuild helper, the MCP vault producer.
    /// Empty for a vault that declares none and for every non-vault build.
    pub embed_targets: Vec<(String, String)>,
    /// Index declarations `.kglite/vault.yaml` installed — equality, range and
    /// composite counted together, one per declared entry.
    pub indexes_declared: usize,
    /// BM25 text indexes built from `text_indexes:`.
    pub text_indexes_built: usize,
    /// Skills imported from `.kglite/skills/` (VAULT.md §8).
    pub skills_imported: usize,
    /// Recipe queries imported from `.kglite/recipes/`.
    pub recipes_imported: usize,
    /// Chunk boundaries `structure.chunks`' caps forced *inside* one block —
    /// a list, table or paragraph too big to be a chunk on its own (VAULT.md
    /// §7.1). Zero for a vault whose blocks all fit, so a non-zero count is
    /// the signal that the source has passages the caps had to cut blind.
    pub forced_splits: usize,
    /// Problems that leave the build's output untrustworthy — a caller that
    /// gates on the report fails on a non-empty list.
    pub errors: Vec<String>,
    /// Problems worth surfacing that still leave a usable graph.
    pub warnings: Vec<String>,
}

impl BuildReport {
    /// Whether the vault meets VAULT.md §9: no errors, and under `strict` no
    /// warnings either. The exit code of `kglite okf check` and the `ok`
    /// attribute of `okf.validate`'s report are both this — one rule, so a
    /// converter's test suite and an operator's terminal cannot disagree.
    pub fn is_ok(&self, strict: bool) -> bool {
        self.errors.is_empty() && (!strict || self.warnings.is_empty())
    }

    /// The report as stable text: counts, then errors, then warnings.
    ///
    /// One rendering for `kglite okf check` and Python's `str(report)`, so
    /// what an operator reads in a terminal and what a converter prints from
    /// its own harness are the same lines. Deterministic — the count maps are
    /// `BTreeMap`s and the findings keep the order the build made them in.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let counted = |map: &BTreeMap<String, usize>| {
            if map.is_empty() {
                "none".to_string()
            } else {
                map.iter()
                    .map(|(k, v)| format!("{k} {v}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        };
        out.push_str(&format!("files scanned: {}\n", self.files_scanned));
        out.push_str(&format!("concepts: {}\n", self.concepts));
        out.push_str(&format!("nodes: {}\n", counted(&self.nodes_by_label)));
        out.push_str(&format!("edges: {}\n", counted(&self.edges_by_type)));
        out.push_str(&format!("dangling links: {}\n", self.dangling));
        out.push_str(&format!("folder notes: {}\n", self.folder_notes));
        out.push_str(&format!(
            "missing attachments: {} ({} ambiguous)\n",
            self.missing_attachments, self.ambiguous_attachments
        ));
        out.push_str(&format!("indexes declared: {}\n", self.indexes_declared));
        out.push_str(&format!(
            "text indexes built: {}\n",
            self.text_indexes_built
        ));
        out.push_str(&format!("skills imported: {}\n", self.skills_imported));
        out.push_str(&format!("recipes imported: {}\n", self.recipes_imported));
        out.push_str(&format!("forced chunk splits: {}\n", self.forced_splits));
        out.push_str(&format!(
            "embed targets: {}\n",
            if self.embed_targets.is_empty() {
                "none".to_string()
            } else {
                self.embed_targets
                    .iter()
                    .map(|(label, property)| format!("{label}.{property}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        ));
        for (name, findings) in [("errors", &self.errors), ("warnings", &self.warnings)] {
            if findings.is_empty() {
                out.push_str(&format!("{name}: none\n"));
                continue;
            }
            out.push_str(&format!("{name} ({}):\n", findings.len()));
            for finding in findings {
                out.push_str(&format!("  - {finding}\n"));
            }
        }
        out
    }
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
    /// True when `target` is an external `http(s)` URL — becomes a `Source` node
    /// rather than resolving to a concept.
    pub is_external: bool,
    /// Properties carried onto the edge itself (VAULT.md §5.4): `section` and
    /// `anchor` for a body link under [`Profile::link_edge_props`]. The
    /// builder emits them as extra columns on the connection frame, which is
    /// the path P5's attachment `alt`/`ordinal` reuses.
    pub props: Vec<(String, Value)>,
    /// The edge runs target → this concept instead of the other way: a
    /// `parent:` under [`FolderNoteDirection::ParentToChild`]. Two links
    /// differing only here are two different edges, so it belongs to the
    /// link's identity.
    pub reverse: bool,
}

impl Link {
    /// A plain outbound link with no edge properties — every link that is not
    /// a vault body link, spelled once.
    pub(crate) fn plain(target: String, conn_type: String, is_external: bool) -> Self {
        Link {
            target,
            conn_type,
            is_external,
            props: Vec::new(),
            reverse: false,
        }
    }
}

/// One `![alt](x.png)` / `![[x.png|alt]]` reference in a note's body
/// (VAULT.md §6.1), before it is resolved against the vault's files.
///
/// Resolution needs the whole file list, which the parser does not have — so
/// extraction records the reference as written and
/// [`crate::okf::build::attachments::build_attachments`] walks the ladder (§6.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentRef {
    /// The target exactly as written, minus any `#fragment` / `?query`.
    pub target: String,
    /// The alt text: the `[…]` of a markdown image, or the `|…` of a wikilink
    /// embed. `None` when empty — VAULT.md §6.4 omits the edge property.
    pub alt: Option<String>,
    /// The enclosing heading's text, as body links carry it (VAULT.md §5.4).
    pub section: Option<String>,
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
    /// Inline `#tag` names found in the body, in first-use order (VAULT.md
    /// §5.5). They join the `tags` frontmatter list at the `Tag` hub and
    /// nowhere else — the `tags` property still reports only the frontmatter.
    pub inline_tags: Vec<String>,
    /// Hub keys (VAULT.md §7 `hubs:`) this note spent on the typed-edge rule
    /// instead: a `keywords:` holding nothing but wikilinks is edges, and its
    /// hub gets nothing from this note. Drained into the build report's
    /// warnings, which is the only place the clash is visible.
    pub hub_key_edges: Vec<String>,
    /// `![…]` references found in the body, in body order (VAULT.md §6).
    /// Always empty unless [`Profile::attachments`] is set.
    pub attachments: Vec<AttachmentRef>,
    /// VAULT.md §9 errors this one file produced — unparseable frontmatter, a
    /// reserved key of the wrong shape, a reference naming an absolute or
    /// escaping path. Drained into the build report's errors, prefixed with
    /// this note's `file_path`.
    pub errors: Vec<String>,
    /// Body markdown — `Some` only when `with_body` was requested.
    pub body: Option<String>,
    /// What `structure:` derived from this note's body (VAULT.md §7.1), with
    /// every id held as the **suffix** of the note's own — `resolve_ids` can
    /// still move `concept_id` after this doc is parsed, and the builder is
    /// what joins the two. Empty unless the vault declares a rule.
    pub(crate) derived: crate::okf::structure::Derived,
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
/// Edge type for an `![[embed]]` of one note in another (VAULT.md §5.1).
pub const EMBEDS_CONN_TYPE: &str = "EMBEDS";
/// Default edge type for a folder note / reserved `parent:` key (VAULT.md §2.3).
pub const FOLDER_NOTE_CONN_TYPE: &str = "CHILD_OF";
/// Node label for synthesized tag nodes; edge type concept → tag.
pub const TAG_LABEL: &str = "Tag";
pub const TAGGED_CONN_TYPE: &str = "TAGGED";
/// Node label for synthesized external-source (URL) nodes.
pub const SOURCE_LABEL: &str = "Source";
/// Node label for synthesized directory nodes (the bundle's folder hierarchy).
pub const FOLDER_LABEL: &str = "Folder";
/// Property a note's prose is stored under by default (VAULT.md §7 `body:`).
pub const DEFAULT_BODY_PROPERTY: &str = "body";
/// Frontmatter key that opts a file out of the sweep (`kg_skip: true`).
pub const SKIP_KEY: &str = "kg_skip";
/// Node label for a referenced file whose MIME type the bundled MCP server
/// delivers as an image (VAULT.md §6.3).
pub const IMAGE_LABEL: &str = "Image";
/// Node label for every other referenced non-`.md` file.
pub const ATTACHMENT_LABEL: &str = "Attachment";
/// Edge type note → [`IMAGE_LABEL`] (VAULT.md §6.4).
pub const HAS_IMAGE_CONN_TYPE: &str = "HAS_IMAGE";
/// Edge type note → [`ATTACHMENT_LABEL`].
pub const HAS_ATTACHMENT_CONN_TYPE: &str = "HAS_ATTACHMENT";
/// MIME type for an extension the table below does not name.
pub const DEFAULT_MIME: &str = "application/octet-stream";

/// The extension → MIME table, the one place a file type is named
/// (VAULT.md §6.3). Extensions arrive lowercased.
///
/// Not exhaustive and not meant to be: it covers what a vault actually holds,
/// and everything else is [`DEFAULT_MIME`], which is a correct answer rather
/// than a missing one.
pub(crate) fn mime_for_extension(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "avif" => "image/avif",
        "heic" => "image/heic",
        "ico" => "image/vnd.microsoft.icon",
        "pdf" => "application/pdf",
        "md" | "markdown" => "text/markdown",
        "txt" => "text/plain",
        "csv" => "text/csv",
        "tsv" => "text/tab-separated-values",
        "html" | "htm" => "text/html",
        "json" => "application/json",
        "yaml" | "yml" => "application/yaml",
        "xml" => "application/xml",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "doc" | "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" | "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" | "pptx" => {
            "application/vnd.openxmlformats-officedocument.presentationml.presentation"
        }
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "m4a" => "audio/mp4",
        "ttf" => "font/ttf",
        "woff2" => "font/woff2",
        _ => DEFAULT_MIME,
    }
}

/// The label a referenced file's MIME type earns (VAULT.md §6.3).
///
/// [`IMAGE_LABEL`] is exactly the four types the bundled MCP server can hand
/// back as an image block, so `Image` means "deliverable", not "picture": SVG
/// and TIFF are pictures and are [`ATTACHMENT_LABEL`]s, which is why §6
/// tells converters to rasterise.
pub(crate) fn label_for_mime(mime: &str) -> &'static str {
    match mime {
        "image/png" | "image/jpeg" | "image/gif" | "image/webp" => IMAGE_LABEL,
        _ => ATTACHMENT_LABEL,
    }
}

/// The lowercased extension of a file path, or `""` when it has none.
pub(crate) fn extension_of(path: &str) -> String {
    let file = path.rsplit('/').next().unwrap_or(path);
    file.rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default()
}

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
        // Loose is the default profile with wikilinks on — the one convention
        // the dialect name itself carries.
        assert_eq!(
            BuildOptions::for_dialect(Dialect::Loose).profile,
            Profile {
                wikilinks: true,
                ..Profile::default()
            }
        );
        assert!(!BuildOptions::for_dialect(Dialect::Okf).profile.wikilinks);
    }
}
