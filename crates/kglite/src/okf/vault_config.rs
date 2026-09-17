//! `.kglite/vault.yaml` — a vault's own declaration file (VAULT.md §7), plus
//! the skills and recipes it carries (§8).
//!
//! The file is read by **explicit path**: [`crate::okf::walk::discover`] prunes
//! every dot-directory, so `.kglite/` is invisible to the walk and its contents
//! never become notes. Two halves, applied at two moments:
//!
//! - the **profile overrides** ([`VaultConfig::apply_to_profile`]) reach the
//!   same [`Profile`] a dialect selects, before discovery and parsing, so the
//!   vault's declarations are indistinguishable from the dialect's defaults to
//!   every reader below;
//! - the **declarations** ([`VaultConfig::apply_post_build`]) — types, indexes,
//!   text indexes, ontology, embed targets — run against the finished graph.
//!
//! A broken file **fails the build** (`Err`), rather than being ignored with a
//! finding in the report. The re-application contract is what forces that: the
//! config is the only state that survives a rebuild, so a vault whose
//! `vault.yaml` stopped parsing would quietly lose its labels, hubs, indexes
//! and embed targets while still producing a graph that looks built. VAULT.md
//! §7 and §9 say so; `okf::validate` (P8) turns the `Err` back into the report
//! error §9 classifies.

use crate::datatypes::values::Value;
use crate::datatypes::PropMap;
use crate::graph::DirGraph;
use crate::okf::model::{
    BuildReport, FolderNoteDirection, HubSpec, LabelFrom, Profile, TAGGED_CONN_TYPE, TAG_LABEL,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The directory a vault keeps its machine-readable state in.
pub const CONFIG_DIR: &str = ".kglite";
/// The declaration file inside [`CONFIG_DIR`].
pub const CONFIG_FILE: &str = "vault.yaml";
/// Subdirectory of [`CONFIG_DIR`] holding `KgliteSkill` markdown (VAULT.md §8).
pub const SKILLS_DIR: &str = "skills";
/// Subdirectory of [`CONFIG_DIR`] holding `KgliteRecipe` markdown (VAULT.md §8).
pub const RECIPES_DIR: &str = "recipes";
/// The only `kglite_vault:` value this build understands.
pub const SUPPORTED_VERSION: i64 = 1;

/// The declared property types this format names (VAULT.md §7 `types:`) — the
/// blueprint vocabulary's spelling, minus the aliases a blueprint also takes.
/// Exactly these seven, so a typo is refused rather than silently ignored.
const TYPE_KEYWORDS: [&str; 7] = ["string", "int", "float", "bool", "date", "datetime", "list"];

/// Every key `vault.yaml` accepts at the top level. An unknown one is an error
/// (VAULT.md §7): a declaration the reader cannot place is never harmless —
/// a misspelled `heading_edge:` would leave every link typed by the ladder
/// with nothing to say so.
const TOP_LEVEL_KEYS: [&str; 13] = [
    "kglite_vault",
    "default_label",
    "label_from",
    "body",
    "skip_dirs",
    "folder_notes",
    "hubs",
    "heading_edges",
    "types",
    "indexes",
    "text_indexes",
    "ontology",
    "embed",
];

/// One entry of an `indexes:` list (VAULT.md §7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexDecl {
    /// A bare property name: the equality (hash) index.
    Equality(String),
    /// `{range: <prop>}` — the B-tree serving `<`, `>`, BETWEEN.
    Range(String),
    /// `{composite: [<prop>, …]}` — the multi-property equality index.
    Composite(Vec<String>),
}

/// A parsed `.kglite/vault.yaml`.
///
/// Every profile-override field is an `Option`, so "declared" and "left to the
/// dialect" stay distinguishable: a `skip_dirs: []` prunes nothing *on
/// purpose*, which a `Vec::new()` could not express.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VaultConfig {
    pub default_label: Option<String>,
    pub label_from: Option<LabelFrom>,
    /// The property name the prose is stored under.
    pub body: Option<String>,
    pub skip_dirs: Option<Vec<String>>,
    pub folder_note_edge: Option<String>,
    pub folder_note_direction: Option<FolderNoteDirection>,
    /// Declared hubs, **merged over** the built-in `tags` hub rather than
    /// replacing it: §7 says `tags` "can be redeclared like any other", which
    /// only means anything if not redeclaring it leaves it standing.
    pub hubs: BTreeMap<String, HubSpec>,
    /// Heading text → edge type, merged over the built-in ladder (§5.3).
    pub heading_edges: BTreeMap<String, String>,
    /// Label → property → one of [`TYPE_KEYWORDS`].
    pub types: BTreeMap<String, BTreeMap<String, String>>,
    pub indexes: BTreeMap<String, Vec<IndexDecl>>,
    pub text_indexes: BTreeMap<String, Vec<String>>,
    /// The ontology document, already parsed — a malformed one is a config
    /// schema error, and a schema error belongs to loading, not to a graph
    /// that is by then half-declared.
    pub ontology: Option<crate::graph::ontology::OntologyStore>,
    /// `(label, property)` in declaration order. Core computes no vectors.
    pub embed: Vec<(String, String)>,
}

/// Where the declaration file lives under `root`.
pub fn config_path(root: &Path) -> PathBuf {
    config_dir(root).join(CONFIG_FILE)
}

/// `<root>/.kglite/` — the directory the walk prunes and the vault reads by
/// explicit path: the config, `skills/` and `recipes/` all live under it.
pub fn config_dir(root: &Path) -> PathBuf {
    root.join(CONFIG_DIR)
}

/// Read `<root>/.kglite/vault.yaml`.
///
/// `Ok(None)` when the file is absent — a vault needs no config, and the
/// dialect's own defaults are a complete format. Every other failure (missing
/// or unknown `kglite_vault:`, unknown key, wrong shape, unreadable file) is
/// an `Err` naming the file and the rule.
pub fn load(root: &Path) -> Result<Option<VaultConfig>, String> {
    let path = config_path(root);
    if !path.is_file() {
        return Ok(None);
    }
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    parse(&text)
        .map(Some)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Parse the document's text. Split from [`load`] so the rules are testable
/// without a directory, and so the path prefixes exactly one message.
pub fn parse(text: &str) -> Result<VaultConfig, String> {
    let doc = crate::okf::frontmatter::parse_yaml(text)?;
    let map = match &doc {
        Value::Map(map) => map,
        Value::Null => return Err("empty document; `kglite_vault: 1` is required".to_string()),
        other => return Err(format!("must be a YAML mapping, not {}", kind_of(other))),
    };

    for (key, _) in map.iter() {
        if !TOP_LEVEL_KEYS.contains(&key) {
            return Err(format!(
                "unknown key `{key}`; this format accepts {}",
                TOP_LEVEL_KEYS.join(", ")
            ));
        }
    }
    match map.get("kglite_vault") {
        Some(Value::Int64(n)) if *n == SUPPORTED_VERSION => {}
        Some(Value::Int64(n)) => {
            return Err(format!(
                "`kglite_vault: {n}` is not a format version this build reads; \
                 the supported version is {SUPPORTED_VERSION}"
            ))
        }
        Some(other) => {
            return Err(format!(
                "`kglite_vault` must be the integer {SUPPORTED_VERSION}, not {}",
                kind_of(other)
            ))
        }
        None => return Err("`kglite_vault: 1` is required".to_string()),
    }

    let mut config = VaultConfig {
        default_label: opt_string(map, "default_label")?,
        body: opt_string(map, "body")?,
        ..VaultConfig::default()
    };
    if let Some(v) = map.get("label_from") {
        config.label_from = Some(match string_of(v, "label_from")?.as_str() {
            "type" => LabelFrom::Type,
            "folder" => LabelFrom::Folder,
            other => {
                return Err(format!(
                    "`label_from: {other}` is not a rung; use `type` or `folder`"
                ))
            }
        });
    }
    if let Some(v) = map.get("skip_dirs") {
        config.skip_dirs = Some(string_list(v, "skip_dirs")?);
    }
    if let Some(v) = map.get("folder_notes") {
        parse_folder_notes(v, &mut config)?;
    }
    if let Some(v) = map.get("hubs") {
        config.hubs = parse_hubs(v)?;
    }
    if let Some(v) = map.get("heading_edges") {
        for (heading, edge) in map_of(v, "heading_edges")?.iter() {
            config.heading_edges.insert(
                heading.to_string(),
                string_of(edge, &format!("heading_edges.{heading}"))?,
            );
        }
    }
    if let Some(v) = map.get("types") {
        config.types = parse_types(v)?;
    }
    if let Some(v) = map.get("indexes") {
        config.indexes = parse_indexes(v)?;
    }
    if let Some(v) = map.get("text_indexes") {
        for (label, props) in map_of(v, "text_indexes")?.iter() {
            config.text_indexes.insert(
                label.to_string(),
                string_list(props, &format!("text_indexes.{label}"))?,
            );
        }
    }
    if let Some(v) = map.get("ontology") {
        config.ontology = Some(
            crate::graph::ontology::ontology_from_value(v)
                .map_err(|e| format!("`ontology`: {e}"))?,
        );
    }
    if let Some(v) = map.get("embed") {
        for (label, prop) in map_of(v, "embed")?.iter() {
            config.embed.push((
                label.to_string(),
                string_of(prop, &format!("embed.{label}"))?,
            ));
        }
    }
    Ok(config)
}

fn parse_folder_notes(v: &Value, config: &mut VaultConfig) -> Result<(), String> {
    let map = map_of(v, "folder_notes")?;
    for (key, _) in map.iter() {
        if key != "edge" && key != "direction" {
            return Err(format!(
                "unknown key `folder_notes.{key}`; accepts `edge` and `direction`"
            ));
        }
    }
    config.folder_note_edge = opt_string(map, "edge")?;
    if let Some(d) = map.get("direction") {
        config.folder_note_direction =
            Some(match string_of(d, "folder_notes.direction")?.as_str() {
                "child_to_parent" => FolderNoteDirection::ChildToParent,
                "parent_to_child" => FolderNoteDirection::ParentToChild,
                other => {
                    return Err(format!(
                    "`folder_notes.direction: {other}`; use `child_to_parent` or `parent_to_child`"
                ))
                }
            });
    }
    Ok(())
}

fn parse_hubs(v: &Value) -> Result<BTreeMap<String, HubSpec>, String> {
    let mut out = BTreeMap::new();
    for (key, decl) in map_of(v, "hubs")?.iter() {
        let spec = map_of(decl, &format!("hubs.{key}"))?;
        for (field, _) in spec.iter() {
            if !["label", "edge", "case_insensitive"].contains(&field) {
                return Err(format!(
                    "unknown key `hubs.{key}.{field}`; accepts `label`, `edge`, `case_insensitive`"
                ));
            }
        }
        // The built-in tag hub's shape is the default for a hub that names
        // only what it changes, so `tags: {case_insensitive: true}` is the
        // whole redeclaration §7 promises.
        let label = opt_string(spec, "label")?.unwrap_or_else(|| TAG_LABEL.to_string());
        let edge = opt_string(spec, "edge")?.unwrap_or_else(|| TAGGED_CONN_TYPE.to_string());
        let case_insensitive = match spec.get("case_insensitive") {
            Some(Value::Boolean(b)) => *b,
            None | Some(Value::Null) => false,
            Some(other) => {
                return Err(format!(
                    "`hubs.{key}.case_insensitive` must be a boolean, not {}",
                    kind_of(other)
                ))
            }
        };
        out.insert(
            key.to_string(),
            HubSpec {
                label,
                edge,
                case_insensitive,
            },
        );
    }
    Ok(out)
}

fn parse_types(v: &Value) -> Result<BTreeMap<String, BTreeMap<String, String>>, String> {
    let mut out: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for (label, props) in map_of(v, "types")?.iter() {
        let mut declared = BTreeMap::new();
        for (property, keyword) in map_of(props, &format!("types.{label}"))?.iter() {
            let keyword = string_of(keyword, &format!("types.{label}.{property}"))?;
            if !TYPE_KEYWORDS.contains(&keyword.as_str()) {
                return Err(format!(
                    "`types.{label}.{property}: {keyword}` is not a declared type; \
                     use one of {}",
                    TYPE_KEYWORDS.join(", ")
                ));
            }
            declared.insert(property.to_string(), keyword);
        }
        out.insert(label.to_string(), declared);
    }
    Ok(out)
}

fn parse_indexes(v: &Value) -> Result<BTreeMap<String, Vec<IndexDecl>>, String> {
    let mut out: BTreeMap<String, Vec<IndexDecl>> = BTreeMap::new();
    for (label, entries) in map_of(v, "indexes")?.iter() {
        let ctx = format!("indexes.{label}");
        let Value::List(items) = entries else {
            return Err(format!(
                "`{ctx}` must be a list of index declarations, not {}",
                kind_of(entries)
            ));
        };
        let mut decls = Vec::with_capacity(items.len());
        for item in items.iter() {
            decls.push(match item {
                Value::String(property) => IndexDecl::Equality(property.clone()),
                Value::Map(spec) => {
                    let mut keys = spec.iter();
                    let (Some((kind, value)), None) = (keys.next(), keys.next()) else {
                        return Err(format!(
                            "`{ctx}` entry must be a single-key map: \
                             `{{range: <prop>}}` or `{{composite: [<prop>, …]}}`"
                        ));
                    };
                    match kind {
                        "range" => IndexDecl::Range(string_of(value, &format!("{ctx}.range"))?),
                        "composite" => {
                            let props = string_list(value, &format!("{ctx}.composite"))?;
                            if props.len() < 2 {
                                return Err(format!(
                                    "`{ctx}.composite` needs at least two properties; \
                                     one property is the plain equality index"
                                ));
                            }
                            IndexDecl::Composite(props)
                        }
                        other => {
                            return Err(format!(
                                "unknown index kind `{other}` in `{ctx}`; \
                                 use a bare property name, `range` or `composite`"
                            ))
                        }
                    }
                }
                other => {
                    return Err(format!(
                        "`{ctx}` entry must be a property name or a single-key map, not {}",
                        kind_of(other)
                    ))
                }
            });
        }
        out.insert(label.to_string(), decls);
    }
    Ok(out)
}

// ── Shape helpers ──────────────────────────────────────────────────────────

/// The word a message uses for a value of the wrong shape. Never the value
/// itself: a frontmatter body can be long, and the shape is the complaint.
fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "nothing",
        Value::Boolean(_) => "a boolean",
        Value::Int64(_) | Value::UniqueId(_) => "an integer",
        Value::Float64(_) => "a float",
        Value::String(_) => "a string",
        Value::List(_) => "a list",
        Value::Map(_) => "a mapping",
        _ => "that value",
    }
}

fn map_of<'a>(v: &'a Value, ctx: &str) -> Result<&'a PropMap, String> {
    match v {
        Value::Map(map) => Ok(map),
        other => Err(format!("`{ctx}` must be a mapping, not {}", kind_of(other))),
    }
}

fn string_of(v: &Value, ctx: &str) -> Result<String, String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        other => Err(format!("`{ctx}` must be a string, not {}", kind_of(other))),
    }
}

fn opt_string(map: &PropMap, key: &str) -> Result<Option<String>, String> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => Ok(Some(string_of(v, key)?)),
    }
}

fn string_list(v: &Value, ctx: &str) -> Result<Vec<String>, String> {
    match v {
        Value::List(items) => items
            .iter()
            .map(|item| string_of(item, ctx))
            .collect::<Result<Vec<_>, _>>(),
        other => Err(format!(
            "`{ctx}` must be a list of strings, not {}",
            kind_of(other)
        )),
    }
}

// ── Application ────────────────────────────────────────────────────────────

impl VaultConfig {
    /// Overwrite the dialect's conventions with the vault's, before the walk.
    ///
    /// The vault's declaration wins over the caller's profile, deliberately:
    /// the file is the format's own statement about itself, and a rebuild that
    /// re-reads it must produce the same graph whatever the caller last set.
    pub fn apply_to_profile(&self, profile: &mut Profile) {
        if let Some(label) = &self.default_label {
            profile.default_label = Some(label.clone());
        }
        if let Some(from) = self.label_from {
            profile.label_from = from;
        }
        if let Some(body) = &self.body {
            profile.body_property = body.clone();
        }
        if let Some(dirs) = &self.skip_dirs {
            profile.skip_dirs = dirs.clone();
        }
        if let Some(edge) = &self.folder_note_edge {
            profile.folder_note_edge = edge.clone();
        }
        if let Some(direction) = self.folder_note_direction {
            profile.folder_note_direction = direction;
        }
        for (key, spec) in &self.hubs {
            profile.hubs.insert(key.clone(), spec.clone());
        }
        for (heading, edge) in &self.heading_edges {
            profile.heading_edges.insert(heading.clone(), edge.clone());
        }
    }

    /// Install the declarations against the finished graph (VAULT.md §7).
    ///
    /// `types:` is not here — a declared type decides how a **column is
    /// built**, so it is applied where the frames are made
    /// (`build::build_nodes`); retyping an already-built column afterwards
    /// would be a second, weaker implementation of the same rule.
    ///
    /// Nothing in this pass fails the build. The config itself already parsed;
    /// what is left can only disagree with the vault's *content* — a label no
    /// note carries yet, a property nobody wrote — and a vault mid-authoring
    /// is the normal case, not a broken one.
    pub(crate) fn apply_post_build(&self, graph: &mut DirGraph, report: &mut BuildReport) {
        self.apply_indexes(graph, report);
        self.apply_text_indexes(graph, report);
        self.apply_ontology(graph, report);
        report.embed_targets = self.embed.clone();
        for (label, property) in &self.embed {
            if !graph.has_node_type(label) {
                report.warnings.push(format!(
                    "`vault.yaml` declares `embed: {label}.{property}`, but no note carries \
                     the label `{label}`"
                ));
            }
        }
    }

    fn apply_indexes(&self, graph: &mut DirGraph, report: &mut BuildReport) {
        for (label, decls) in &self.indexes {
            if !graph.has_node_type(label) {
                report.warnings.push(format!(
                    "`vault.yaml` declares indexes on `{label}`, but no note carries that label"
                ));
                continue;
            }
            for decl in decls {
                let outcome = match decl {
                    IndexDecl::Equality(property) => graph
                        .create_property_index_routed(label, property)
                        .map(|(count, _persistent)| count),
                    IndexDecl::Range(property) => Ok(graph.declare_range_index(label, property)),
                    IndexDecl::Composite(properties) => {
                        let refs: Vec<&str> = properties.iter().map(String::as_str).collect();
                        Ok(graph.declare_composite_index(label, &refs))
                    }
                };
                match outcome {
                    Ok(count) => {
                        report.indexes_declared += 1;
                        if count == 0 {
                            report.warnings.push(format!(
                                "`vault.yaml` index {} indexed no value — no note of label \
                                 `{label}` carries that property yet",
                                describe_index(label, decl)
                            ));
                        }
                    }
                    Err(reason) => report.warnings.push(format!(
                        "`vault.yaml` index {} was not installed: {reason}",
                        describe_index(label, decl)
                    )),
                }
            }
        }
    }

    fn apply_text_indexes(&self, graph: &mut DirGraph, report: &mut BuildReport) {
        for (label, properties) in &self.text_indexes {
            for property in properties {
                match crate::graph::text_indexes::build_text_index(graph, label, property, None) {
                    Ok(_) => report.text_indexes_built += 1,
                    Err(reason) => report.warnings.push(format!(
                        "`vault.yaml` text index `{label}.{property}` was not built: {reason}"
                    )),
                }
            }
        }
    }

    fn apply_ontology(&self, graph: &mut DirGraph, report: &mut BuildReport) {
        let Some(store) = &self.ontology else { return };
        match graph.define_ontology(store.clone()) {
            Ok(warnings) => report.warnings.extend(
                warnings
                    .into_iter()
                    .map(|w| format!("`vault.yaml` ontology: {w}")),
            ),
            Err(reason) => report
                .errors
                .push(format!("`vault.yaml` ontology was not installed: {reason}")),
        }
    }
}

/// `Label.property` / `Label.(a,b)` — how a message names one declaration.
fn describe_index(label: &str, decl: &IndexDecl) -> String {
    match decl {
        IndexDecl::Equality(property) => format!("`{label}.{property}`"),
        IndexDecl::Range(property) => format!("`{label}.{property}` (range)"),
        IndexDecl::Composite(properties) => format!("`{label}.({})`", properties.join(",")),
    }
}

// ── Declared property types ────────────────────────────────────────────────

/// Coerce one value to a declared type, or `None` when it cannot be
/// (VAULT.md §7 `types:`).
///
/// `Null` is never a failure: a property absent from one note is not that
/// note's disagreement with the declaration. String parsing reuses the
/// blueprint's *declared*-type grammar (`blueprint::typing::scalar`), which is
/// deliberately wider than `frontmatter::infer_temporal`'s inference grammar —
/// a declaration is the author asking for the value to be read that way.
pub(crate) fn coerce(value: &Value, declared: &str) -> Option<Value> {
    use crate::graph::blueprint::typing::scalar;
    if matches!(value, Value::Null) {
        return Some(Value::Null);
    }
    match declared {
        // Every value has a string spelling, so this rung cannot fail — which
        // is the point: `string` is how a vault turns inference *off*.
        "string" => Some(Value::String(crate::datatypes::values::raw_string(value))),
        "int" => match value {
            Value::Int64(n) => Some(Value::Int64(*n)),
            Value::UniqueId(n) => Some(Value::Int64(i64::from(*n))),
            Value::Float64(f) if f.fract() == 0.0 && f.is_finite() => Some(Value::Int64(*f as i64)),
            Value::String(s) => scalar::parse_integer(s).map(Value::Int64),
            _ => None,
        },
        "float" => match value {
            Value::Float64(f) => Some(Value::Float64(*f)),
            Value::Int64(n) => Some(Value::Float64(*n as f64)),
            Value::String(s) => scalar::parse_float(s).map(Value::Float64),
            _ => None,
        },
        "bool" => match value {
            Value::Boolean(b) => Some(Value::Boolean(*b)),
            Value::String(s) => scalar::parse_boolean(s).map(Value::Boolean),
            _ => None,
        },
        "date" => match value {
            Value::DateTime(d) => Some(Value::DateTime(*d)),
            Value::Timestamp(ts) => Some(Value::DateTime(ts.date())),
            Value::String(s) => scalar::parse_date(s).map(Value::DateTime),
            _ => None,
        },
        "datetime" => match value {
            Value::Timestamp(ts) => Some(Value::Timestamp(*ts)),
            // Midnight UTC, the same widening `date('x') < datetime(...)`
            // comparisons already take.
            Value::DateTime(d) => Some(Value::Timestamp(d.and_hms_opt(0, 0, 0)?)),
            Value::String(s) => parse_datetime(s),
            _ => None,
        },
        // A scalar is *not* silently wrapped in a one-element list: a
        // `keywords: seismic` that meant a list is an authoring mistake the
        // warning should surface, and a hub reads a key's list only (§7).
        "list" => match value {
            Value::List(items) => Some(Value::List(items.clone())),
            _ => None,
        },
        _ => None,
    }
}

fn parse_datetime(text: &str) -> Option<Value> {
    let s = text.trim();
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(Value::Timestamp(dt.naive_utc()));
    }
    for format in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M"] {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, format) {
            return Some(Value::Timestamp(dt));
        }
    }
    crate::graph::blueprint::typing::scalar::parse_date(s)
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(Value::Timestamp)
}

// ── Carried skills and recipes (VAULT.md §8) ───────────────────────────────

/// Import `.kglite/skills/*.md` and `.kglite/recipes/*.md` into the graph's
/// own skill and recipe layers.
///
/// Per §8 a file that fails validation is **skipped with a warning naming the
/// file and the rule, and its siblings load**: the directory is vault content,
/// authored by hand, and one unfinished skill must not cost an agent the other
/// nine. The build is not failed for the same reason — nothing that reached
/// the graph is wrong, there is simply less of it than the author intended.
pub(crate) fn import_carried(root: &Path, graph: &mut DirGraph, report: &mut BuildReport) {
    let base = root.join(CONFIG_DIR);
    for file in markdown_files(&base.join(SKILLS_DIR)) {
        match read_and_set_skill(graph, &file) {
            Ok(()) => report.skills_imported += 1,
            Err(reason) => report.warnings.push(format!(
                "`.kglite/skills/{}` was skipped: {reason}",
                file_name(&file)
            )),
        }
    }
    import_carried_recipes(&base.join(RECIPES_DIR), graph, report);
}

/// `.kglite/recipes/*.md`, with §8's group-description inheritance resolved
/// across the **whole directory** before any file is stored.
///
/// A group's description is written once, by whichever sibling says it. Doing
/// that per file as it was read made it depend on filename order: the P16
/// usability probe wrote it in one of six siblings and the five sorting ahead
/// of it were skipped for "expected a non-empty group description". A file
/// that fails for any other reason is still skipped alone, with its own
/// warning, and its siblings load.
fn import_carried_recipes(dir: &Path, graph: &mut DirGraph, report: &mut BuildReport) {
    let mut parsed: Vec<(PathBuf, crate::graph::recipes::RecipeRecord)> = Vec::new();
    for file in markdown_files(dir) {
        match std::fs::read_to_string(&file)
            .map_err(|e| e.to_string())
            .and_then(|text| {
                crate::graph::recipes::parse_markdown(&text).map_err(|e| e.to_string())
            }) {
            Ok(record) => parsed.push((file, record)),
            Err(reason) => report.warnings.push(skipped_recipe(&file, &reason)),
        }
    }
    let mut records: Vec<_> = parsed.iter().map(|(_, record)| record.clone()).collect();
    crate::graph::recipes::inherit_group_descriptions(&mut records, graph);
    // A group nothing described leaves every member without one, and each is
    // refused by `set`'s own validation — so the warning still names the file
    // the author has to edit, which is §8's contract for carried content.
    for ((file, _), record) in parsed.iter().zip(records) {
        match crate::graph::recipes::set(graph, &record) {
            Ok(_) => report.recipes_imported += 1,
            Err(error) => report
                .warnings
                .push(skipped_recipe(file, &error.to_string())),
        }
    }
}

fn skipped_recipe(file: &Path, reason: &str) -> String {
    format!(
        "`.kglite/recipes/{}` was skipped: {reason}",
        file_name(file)
    )
}

fn read_and_set_skill(graph: &mut DirGraph, file: &Path) -> Result<(), String> {
    let text = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
    let record = crate::graph::skills::parse_markdown(&text).map_err(|e| e.to_string())?;
    crate::graph::skills::set(graph, &record)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// The `.md` files directly inside `dir`, sorted — so the upsert order, and
/// therefore the report's warning order, is the same on every platform. A
/// missing directory is an empty list, not an error: §8's directories are
/// optional.
fn markdown_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().is_some_and(|ext| ext == "md"))
        .collect();
    files.sort();
    files
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "vault_config_tests.rs"]
mod tests;
