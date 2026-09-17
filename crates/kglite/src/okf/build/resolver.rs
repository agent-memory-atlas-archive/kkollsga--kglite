//! Link resolution: the forgiving ladder from a written target to the note it
//! means (VAULT.md §5.2), and the `aliases:` and slug indexes it walks.

use super::doc_path;
use crate::datatypes::values::Value;
use crate::okf::model::{ConceptDoc, Link, DEFAULT_LABEL};
use std::collections::HashMap;

/// A concept's `aliases:` entries — the names it also answers to in link
/// resolution (VAULT.md §5.2, rung 3). A scalar `aliases: Foo` is read as the
/// one-entry list it means.
fn doc_aliases(d: &ConceptDoc) -> Vec<&str> {
    d.props
        .iter()
        .filter(|(k, _)| k == "aliases")
        .flat_map(|(_, v)| match v {
            Value::List(items) => items
                .iter()
                .filter_map(|x| match x {
                    Value::String(s) => Some(s.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            Value::String(s) => vec![s.as_str()],
            _ => Vec::new(),
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// Normalize a name/path for forgiving link resolution: lowercase, unify
/// `_`/` `→`-`, collapse repeats, trim. So `Project-0-10` / `project_0_10` /
/// `Project 0 10` all match. Slashes are preserved (paths stay paths).
fn normalize_slug(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = false;
    for ch in s.chars() {
        let c = ch.to_ascii_lowercase();
        let c = if c == '_' || c == ' ' { '-' } else { c };
        if c == '-' {
            if prev_dash {
                continue;
            }
            prev_dash = true;
        } else {
            prev_dash = false;
        }
        out.push(c);
    }
    out.trim_matches('-').to_string()
}

/// Forgiving link → concept resolver, one ladder for path links and wikilinks
/// alike (VAULT.md §5.2). Tries, most-specific first: exact id → a
/// vault-relative then note-relative path → exact file stem → an `aliases:`
/// entry → normalized slug (full path or last segment) → normalized title.
/// Unresolved targets keep their raw id and the default label —
/// `add_connections` vivifies them as `_provisional` stubs.
pub(super) struct Resolver<'a> {
    pub(super) id_to_label: HashMap<&'a str, &'a str>,
    /// File path minus `.md` → id. Identical to `id_to_label`'s keys under the
    /// path id scheme; under the vault's stem ids it is what keeps a
    /// `[text](sub/note.md)` path link resolving (VAULT.md §5.2).
    path_to_id: HashMap<&'a str, &'a str>,
    stem_to_id: HashMap<&'a str, &'a str>,
    /// Empty unless [`crate::okf::model::Profile::alias_resolution`] is set.
    alias_to_id: HashMap<&'a str, &'a str>,
    slug_to_id: HashMap<String, &'a str>,
    title_to_id: HashMap<String, &'a str>,
}

impl<'a> Resolver<'a> {
    /// Build the ladder's indexes, and report the alias clashes found on the
    /// way: an alias that names another note's stem, or that two notes both
    /// claim, resolves to exactly one of them, so the vault's author needs to
    /// know (VAULT.md §9, a warning — the graph is still usable).
    pub(super) fn new(
        docs: &'a [ConceptDoc],
        profile: &crate::okf::model::Profile,
    ) -> (Self, Vec<String>) {
        let mut id_to_label = HashMap::new();
        let mut path_to_id = HashMap::new();
        let mut stem_to_id = HashMap::new();
        let mut alias_to_id = HashMap::new();
        let mut slug_to_id = HashMap::new();
        let mut title_to_id = HashMap::new();
        let mut warnings = Vec::new();
        for d in docs {
            id_to_label.insert(d.concept_id.as_str(), d.label.as_str());
            let path = doc_path(d);
            path_to_id.entry(path).or_insert(d.concept_id.as_str());
            // The stem the *editor* sees, from the filename — a note carrying
            // its own `id:` is still `[[Stem]]` in the vault.
            let stem = path.rsplit('/').next().unwrap_or(path);
            stem_to_id.entry(stem).or_insert(d.concept_id.as_str());
            slug_to_id
                .entry(normalize_slug(&d.concept_id))
                .or_insert(d.concept_id.as_str());
            slug_to_id
                .entry(normalize_slug(stem))
                .or_insert(d.concept_id.as_str());
            title_to_id
                .entry(normalize_slug(&d.title))
                .or_insert(d.concept_id.as_str());
        }
        if profile.alias_resolution {
            for d in docs {
                for alias in doc_aliases(d) {
                    // The note's own stem is not a clash — an alias repeating
                    // it is redundant, not ambiguous.
                    if let Some(&owner) = stem_to_id.get(alias) {
                        if owner != d.concept_id {
                            warnings.push(format!(
                                "alias `{alias}` on {} is already the file stem of `{owner}`; the stem wins",
                                d.file_path
                            ));
                        }
                        continue;
                    }
                    if let Some(&owner) = alias_to_id.get(alias) {
                        warnings.push(format!(
                            "alias `{alias}` is claimed by both `{owner}` and `{}`; the first wins",
                            d.concept_id
                        ));
                        continue;
                    }
                    alias_to_id.insert(alias, d.concept_id.as_str());
                }
            }
        }
        (
            Self {
                id_to_label,
                path_to_id,
                stem_to_id,
                alias_to_id,
                slug_to_id,
                title_to_id,
            },
            warnings,
        )
    }

    fn label_of(&self, id: &str) -> String {
        self.id_to_label
            .get(id)
            .copied()
            .unwrap_or(DEFAULT_LABEL)
            .to_string()
    }

    /// Resolve one link's target to `(id, label)`. `source_dir` is the linking
    /// note's directory, for the note-relative rung a `/`-bearing target gets
    /// after the vault-relative one (VAULT.md §5.2).
    pub(super) fn resolve(&self, link: &Link, source_dir: &str) -> (String, String) {
        let t = link.target.as_str();
        if let Some(lbl) = self.id_to_label.get(t) {
            return (t.to_string(), lbl.to_string());
        }
        if let Some(id) = self.path_to_id.get(t) {
            return ((*id).to_string(), self.label_of(id));
        }
        if t.contains('/') && !source_dir.is_empty() {
            if let Some(id) = self.path_to_id.get(format!("{source_dir}/{t}").as_str()) {
                return ((*id).to_string(), self.label_of(id));
            }
        }
        if let Some(id) = self.stem_to_id.get(t) {
            return ((*id).to_string(), self.label_of(id));
        }
        if let Some(id) = self.alias_to_id.get(t) {
            return ((*id).to_string(), self.label_of(id));
        }
        let norm = normalize_slug(t);
        if let Some(id) = self.slug_to_id.get(&norm) {
            return ((*id).to_string(), self.label_of(id));
        }
        if let Some(seg) = t.rsplit('/').next() {
            if let Some(id) = self.slug_to_id.get(&normalize_slug(seg)) {
                return ((*id).to_string(), self.label_of(id));
            }
        }
        if let Some(id) = self.title_to_id.get(&norm) {
            return ((*id).to_string(), self.label_of(id));
        }
        (t.to_string(), DEFAULT_LABEL.to_string())
    }
}

#[cfg(test)]
#[path = "resolver_tests.rs"]
mod resolver_tests;
