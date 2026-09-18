//! `structure:` — the vault's declaration of what to derive from a note's own
//! body (VAULT.md §7.1), parsed from `.kglite/vault.yaml`.
//!
//! Kept beside the derivation it configures rather than in
//! [`crate::okf::vault_config`]: every rule here names a construct the block
//! tree finds, and the two files move together. `vault_config` owns the
//! top-level schema and calls [`parse`] for this key.
//!
//! An unknown key **inside** `structure:` is an error exactly as an unknown
//! top-level key is (VAULT.md §7.1) — the block is a hard compatibility
//! boundary, so a rule this build has not shipped yet is refused by name
//! instead of being read as "derive nothing".

use crate::datatypes::values::Value;
use crate::datatypes::PropMap;
use crate::okf::vault_config::kind_of;
use regex::Regex;
use std::sync::OnceLock;

/// The `structure:` keys this build reads. A key VAULT.md names but this build
/// has not implemented is *not* here: it is refused with the same message an
/// invented key gets, which is what makes "a vault declaring `structure:`
/// needs a kglite that knows it" a loud failure rather than a quiet one.
const STRUCTURE_KEYS: [&str; 7] = [
    "sections",
    "chunks",
    "callouts",
    "code_fences",
    "ordered_lists",
    "inherit",
    "embed_text",
];

/// Properties a derived node defines itself, which `inherit:` may therefore
/// not name (VAULT.md §7.1): the alternative is a note's frontmatter silently
/// overwriting the structure it was read from. Names the whole vocabulary, P4's
/// and P5's included, so a vault written against the spec gets the spec's
/// answer whether or not this build derives that construct yet.
const DERIVED_PROPERTIES: [&str; 14] = [
    "title",
    "text",
    "level",
    "ordinal",
    "path",
    "note_id",
    "section_id",
    "kind",
    "fold",
    "lang",
    "code",
    "caption",
    "chunk_hash",
    "step_count",
];

/// VAULT.md §4.1's reserved frontmatter keys. `inherit:` may not name one
/// either — `id` and `type` are not properties at all, and `title` is a
/// derived node's own.
const RESERVED_FRONTMATTER: [&str; 7] = [
    "id", "type", "title", "aliases", "tags", "kg_skip", "parent",
];

/// The `embed_text:` placeholders (VAULT.md §7.1). Any other is an error: a
/// template is written once and read on every node, so a typo that rendered
/// literally would be baked into an entire corpus's embedding text.
const PLACEHOLDERS: [&str; 5] = ["title", "section_title", "heading_path", "text", "id"];

/// What a vault derives from its notes' bodies. `None` on a [`Profile`] — and
/// on every vault that declares no `structure:` — means the build is the one
/// 0.17.8 built, note for note and edge for edge.
///
/// [`Profile`]: crate::okf::model::Profile
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct StructureProfile {
    pub sections: Option<SectionRule>,
    pub chunks: Option<ChunkRule>,
    pub callouts: Option<CalloutRule>,
    pub code_fences: Option<FenceRule>,
    pub ordered_lists: Option<OrderedListRule>,
    /// Frontmatter properties copied verbatim onto every derived node.
    pub inherit: Vec<String>,
    /// The template materialised as a property of its own name on every
    /// derived node that carries `text`.
    pub embed_text: Option<String>,
}

/// `sections:` — one node per heading (VAULT.md §7.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SectionRule {
    pub label: String,
    /// Note → top-level section, and section → the sections directly inside it.
    pub edge: String,
    /// Child → parent, emitted only where a section has one.
    pub parent: String,
    /// Consecutive siblings under one parent, in document order.
    pub next: String,
}

/// `chunks:` — the retrieval unit (VAULT.md §7.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChunkRule {
    pub label: String,
    pub edge: String,
    pub next: String,
    pub max_words: usize,
    pub max_chars: usize,
}

/// `callouts:` — one node per callout (VAULT.md §7.1, §5.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CalloutRule {
    pub label: String,
    /// Enclosing section → callout, or note → callout, or callout → nested
    /// callout.
    pub edge: String,
}

/// `code_fences:` — one node per fenced block (VAULT.md §7.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FenceRule {
    pub label: String,
    pub edge: String,
    /// Info-string first words, lowercased, that qualify. `None` — the key
    /// omitted — is **every** fence, including one carrying no info string,
    /// which is what a corpus whose converter dropped its languages needs.
    pub langs: Option<Vec<String>>,
}

/// `ordered_lists:` — a container node per qualifying top-level ordered list,
/// and a node per item (VAULT.md §7.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrderedListRule {
    /// The **step** label.
    pub label: String,
    /// The container label. Its edge from the section is
    /// `HAS_<UPPER_SNAKE(container)>`, which is therefore not declared.
    pub container: String,
    pub edge: String,
    pub next: String,
    /// Matched against the enclosing section's title; `None` reads every
    /// qualifying list, which is the default (VAULT.md §7.1).
    pub under_heading: Option<HeadingMatcher>,
    pub min_items: usize,
}

/// A compiled `under_heading:` regular expression.
///
/// Compiled once at load rather than per note: `derive` runs inside the
/// parallel parse, and a regex rebuilt per list would dominate the rule.
/// Two matchers are equal when their patterns are — which is what keeps
/// [`StructureProfile`] comparable, and `Regex` itself is not.
#[derive(Debug, Clone)]
pub(crate) struct HeadingMatcher(Regex);

impl HeadingMatcher {
    pub fn is_match(&self, title: &str) -> bool {
        self.0.is_match(title)
    }
}

impl PartialEq for HeadingMatcher {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_str() == other.0.as_str()
    }
}

impl Eq for HeadingMatcher {}

impl StructureProfile {
    /// Whether anything at all is derived. `inherit:`/`embed_text:` alone
    /// declare how derived nodes are decorated, not that there are any.
    pub fn derives_anything(&self) -> bool {
        self.sections.is_some()
            || self.chunks.is_some()
            || self.callouts.is_some()
            || self.code_fences.is_some()
            || self.ordered_lists.is_some()
    }
}

/// Parse the `structure:` value (VAULT.md §7.1). Every failure is a build
/// failure, reported with the key that caused it.
pub(crate) fn parse(v: &Value) -> Result<StructureProfile, String> {
    let map = match v {
        Value::Map(map) => map,
        Value::Null => return Ok(StructureProfile::default()),
        other => {
            return Err(format!(
                "`structure` must be a mapping, not {}",
                kind_of(other)
            ))
        }
    };
    for (key, _) in map.iter() {
        if !STRUCTURE_KEYS.contains(&key) {
            return Err(format!(
                "unknown key `structure.{key}`; this build accepts {}",
                STRUCTURE_KEYS.join(", ")
            ));
        }
    }
    let mut out = StructureProfile::default();
    if let Some(v) = map.get("sections") {
        out.sections = Some(section_rule(v)?);
    }
    if let Some(v) = map.get("chunks") {
        out.chunks = Some(chunk_rule(v)?);
    }
    if let Some(v) = map.get("callouts") {
        out.callouts = Some(callout_rule(v)?);
    }
    if let Some(v) = map.get("code_fences") {
        out.code_fences = Some(fence_rule(v)?);
    }
    if let Some(v) = map.get("ordered_lists") {
        out.ordered_lists = Some(ordered_list_rule(v)?);
    }
    if let Some(v) = map.get("inherit") {
        out.inherit = inherit_list(v)?;
    }
    if let Some(v) = map.get("embed_text") {
        let template = match v {
            Value::String(s) => s.clone(),
            other => {
                return Err(format!(
                    "`structure.embed_text` must be a string, not {}",
                    kind_of(other)
                ))
            }
        };
        check_placeholders(&template)?;
        out.embed_text = Some(template);
    }
    Ok(out)
}

fn section_rule(v: &Value) -> Result<SectionRule, String> {
    let spec = rule_map(v, "structure.sections")?;
    check_keys(
        &spec,
        "structure.sections",
        &["label", "edge", "parent", "next"],
    )?;
    Ok(SectionRule {
        label: field(&spec, "structure.sections", "label", "Section")?,
        edge: field(&spec, "structure.sections", "edge", "HAS_SECTION")?,
        parent: field(&spec, "structure.sections", "parent", "PARENT_SECTION")?,
        next: field(&spec, "structure.sections", "next", "NEXT_SECTION")?,
    })
}

fn chunk_rule(v: &Value) -> Result<ChunkRule, String> {
    let spec = rule_map(v, "structure.chunks")?;
    check_keys(
        &spec,
        "structure.chunks",
        &["label", "edge", "next", "max_words", "max_chars"],
    )?;
    Ok(ChunkRule {
        label: field(&spec, "structure.chunks", "label", "Chunk")?,
        edge: field(&spec, "structure.chunks", "edge", "HAS_CHUNK")?,
        next: field(&spec, "structure.chunks", "next", "NEXT_CHUNK")?,
        max_words: limit(&spec, "structure.chunks", "max_words", 650)?,
        max_chars: limit(&spec, "structure.chunks", "max_chars", 6000)?,
    })
}

fn callout_rule(v: &Value) -> Result<CalloutRule, String> {
    let spec = rule_map(v, "structure.callouts")?;
    check_keys(&spec, "structure.callouts", &["label", "edge"])?;
    Ok(CalloutRule {
        label: field(&spec, "structure.callouts", "label", "Note")?,
        edge: field(&spec, "structure.callouts", "edge", "HAS_NOTE")?,
    })
}

fn fence_rule(v: &Value) -> Result<FenceRule, String> {
    let spec = rule_map(v, "structure.code_fences")?;
    check_keys(&spec, "structure.code_fences", &["label", "edge", "langs"])?;
    Ok(FenceRule {
        label: field(&spec, "structure.code_fences", "label", "Example")?,
        edge: field(&spec, "structure.code_fences", "edge", "HAS_EXAMPLE")?,
        langs: match spec.get("langs") {
            None | Some(Value::Null) => None,
            Some(v) => Some(lang_list(v)?),
        },
    })
}

fn ordered_list_rule(v: &Value) -> Result<OrderedListRule, String> {
    const CTX: &str = "structure.ordered_lists";
    let spec = rule_map(v, CTX)?;
    check_keys(
        &spec,
        CTX,
        &[
            "label",
            "container",
            "edge",
            "next",
            "under_heading",
            "min_items",
        ],
    )?;
    Ok(OrderedListRule {
        label: field(&spec, CTX, "label", "ProcedureStep")?,
        container: field(&spec, CTX, "container", "Procedure")?,
        edge: field(&spec, CTX, "edge", "HAS_STEP")?,
        next: field(&spec, CTX, "next", "NEXT_STEP")?,
        under_heading: heading_matcher(spec.get("under_heading"))?,
        min_items: limit(&spec, CTX, "min_items", 2)?,
    })
}

/// `under_heading:` — compiled at load, so a broken pattern fails the build
/// once rather than being rebuilt against every list in the vault.
fn heading_matcher(v: Option<&Value>) -> Result<Option<HeadingMatcher>, String> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(pattern)) => Regex::new(pattern)
            .map(|re| Some(HeadingMatcher(re)))
            .map_err(|e| {
                format!("`structure.ordered_lists.under_heading` is not a regular expression: {e}")
            }),
        Some(other) => Err(format!(
            "`structure.ordered_lists.under_heading` must be a string, not {}",
            kind_of(other)
        )),
    }
}

/// A rule's own mapping. A bare `sections:` with nothing under it is the rule
/// with every default — the spelling a vault uses when it wants the construct
/// and not an opinion about its names.
fn rule_map<'a>(v: &'a Value, ctx: &str) -> Result<std::borrow::Cow<'a, PropMap>, String> {
    match v {
        Value::Map(map) => Ok(std::borrow::Cow::Borrowed(map)),
        Value::Null => Ok(std::borrow::Cow::Owned(PropMap::new())),
        other => Err(format!("`{ctx}` must be a mapping, not {}", kind_of(other))),
    }
}

fn check_keys(map: &PropMap, ctx: &str, allowed: &[&str]) -> Result<(), String> {
    for (key, _) in map.iter() {
        if !allowed.contains(&key) {
            return Err(format!(
                "unknown key `{ctx}.{key}`; accepts {}",
                allowed.join(", ")
            ));
        }
    }
    Ok(())
}

fn field(map: &PropMap, ctx: &str, key: &str, default: &str) -> Result<String, String> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(default.to_string()),
        Some(Value::String(s)) if !s.is_empty() => Ok(s.clone()),
        Some(Value::String(_)) => Err(format!("`{ctx}.{key}` must not be empty")),
        Some(other) => Err(format!(
            "`{ctx}.{key}` must be a string, not {}",
            kind_of(other)
        )),
    }
}

fn limit(map: &PropMap, ctx: &str, key: &str, default: usize) -> Result<usize, String> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Int64(n)) if *n > 0 => Ok(*n as usize),
        Some(Value::Int64(n)) => Err(format!(
            "`{ctx}.{key}: {n}` must be a positive integer; a limit of zero would \
             make every block its own chunk"
        )),
        Some(other) => Err(format!(
            "`{ctx}.{key}` must be an integer, not {}",
            kind_of(other)
        )),
    }
}

/// `inherit:` (VAULT.md §7.1), with both refusals the spec names.
fn inherit_list(v: &Value) -> Result<Vec<String>, String> {
    let Value::List(items) = v else {
        return Err(format!(
            "`structure.inherit` must be a list of strings, not {}",
            kind_of(v)
        ));
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items.iter() {
        let Value::String(name) = item else {
            return Err(format!(
                "`structure.inherit` must be a list of strings, not {}",
                kind_of(item)
            ));
        };
        if DERIVED_PROPERTIES.contains(&name.as_str()) {
            return Err(format!(
                "`structure.inherit: [{name}]` names a property a derived node defines \
                 itself; inheriting it would overwrite the structure the note was read from"
            ));
        }
        if RESERVED_FRONTMATTER.contains(&name.as_str()) {
            return Err(format!(
                "`structure.inherit: [{name}]` names a reserved frontmatter key (VAULT.md §4.1)"
            ));
        }
        out.push(name.clone());
    }
    Ok(out)
}

/// `langs:` — the info-string first words that qualify, lowercased so a
/// declaration and a fence written `Python` agree.
fn lang_list(v: &Value) -> Result<Vec<String>, String> {
    let Value::List(items) = v else {
        return Err(format!(
            "`structure.code_fences.langs` must be a list of strings, not {}",
            kind_of(v)
        ));
    };
    items
        .iter()
        .map(|item| match item {
            Value::String(s) => Ok(s.to_lowercase()),
            other => Err(format!(
                "`structure.code_fences.langs` must be a list of strings, not {}",
                kind_of(other)
            )),
        })
        .collect()
}

fn placeholder_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\{([A-Za-z_][A-Za-z0-9_]*)\}").unwrap())
}

fn check_placeholders(template: &str) -> Result<(), String> {
    for caps in placeholder_re().captures_iter(template) {
        let name = caps.get(1).map(|m| m.as_str()).unwrap_or_default();
        if !PLACEHOLDERS.contains(&name) {
            return Err(format!(
                "`structure.embed_text` uses `{{{name}}}`, which is not a placeholder \
                 this format names; use {}",
                PLACEHOLDERS
                    .iter()
                    .map(|p| format!("{{{p}}}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    Ok(())
}

/// Render `embed_text:` for one derived node. Placeholders are substituted in
/// one left-to-right pass, so a value that happens to contain `{text}` is
/// never re-read as a placeholder.
pub(crate) fn render_embed_text(
    template: &str,
    note_title: &str,
    section_title: &str,
    heading_path: &[String],
    text: &str,
    id: &str,
) -> String {
    placeholder_re()
        .replace_all(template, |caps: &regex::Captures| {
            match caps.get(1).map(|m| m.as_str()).unwrap_or_default() {
                "title" => note_title.to_string(),
                "section_title" => section_title.to_string(),
                "heading_path" => heading_path.join(" > "),
                "text" => text.to_string(),
                "id" => id.to_string(),
                // `parse` refused every other name; a template that reached
                // here cannot carry one.
                _ => caps[0].to_string(),
            }
        })
        .into_owned()
}

#[cfg(test)]
#[path = "profile_tests.rs"]
mod profile_tests;
