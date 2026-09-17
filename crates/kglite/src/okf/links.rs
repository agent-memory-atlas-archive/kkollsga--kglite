//! Markdown link extraction with the edge-type inference ladder.
//!
//! A link from concept A to concept B becomes a directed edge. OKF links are
//! untyped (the relationship lives in prose), so the connection type is inferred
//! in three tiers, most-specific first:
//!  1. an explicit link **title** that looks like an edge type
//!     (`[customers](/tables/customers.md "JOINS_WITH")`),
//!  2. the enclosing **section header** (`# Joins` → `JOINS_WITH`,
//!     `# Citations` → `CITES`, …),
//!  3. the generic [`DEFAULT_CONN_TYPE`] (`LINKS_TO`).
//!
//! Links inside fenced code blocks and markdown image links (`![alt](src)`) are
//! ignored. External `http(s)` links are captured as `is_external` (they become
//! `Source` nodes in the builder); `mailto:`, anchors, and non-`.md` directory
//! links are skipped (directory structure is captured separately).
//!
//! What the vault profile adds on top (VAULT.md §5), each behind its own
//! [`Profile`] field so `okf`/`loose` bundles are untouched: `section` and
//! `anchor` edge properties, an `EMBEDS` edge for `![[Note]]`, and inline
//! `#tag` extraction. Frontmatter-valued edges (§4.3) are built from the
//! parsed frontmatter by `crate::okf::parse_file`, using [`wikilink_targets`]
//! and [`upper_snake`] from here.

use crate::datatypes::values::Value;
use crate::okf::model::{Link, Profile, DEFAULT_CONN_TYPE, EMBEDS_CONN_TYPE};
use regex::Regex;
use std::sync::OnceLock;

fn link_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // [text](dest) or [text](dest "title"). `text` may not contain ']'.
    RE.get_or_init(|| Regex::new(r#"\[[^\]]*\]\(([^)\s]+)(?:\s+"([^"]*)")?\)"#).unwrap())
}

fn wikilink_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // [[name]] or [[name|alias]].
    RE.get_or_init(|| Regex::new(r"\[\[([^\]|]+)(?:\|[^\]]*)?\]\]").unwrap())
}

/// A whole string that is nothing but one wikilink — the frontmatter
/// typed-edge discriminator (VAULT.md §4.3).
fn only_wikilink_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\s*\[\[([^\]|]+)(?:\|[^\]]*)?\]\]\s*$").unwrap())
}

/// Map a section heading to a connection type, or `None` to fall through.
fn conn_from_heading(heading: &str) -> Option<&'static str> {
    let h = heading.to_ascii_lowercase();
    if h.contains("citation") {
        Some("CITES")
    } else if h.contains("join") {
        Some("JOINS_WITH")
    } else if h.contains("reference") {
        Some("REFERENCES")
    } else if h.contains("related") {
        Some("RELATED")
    } else if h.contains("depend") {
        Some("DEPENDS_ON")
    } else {
        None
    }
}

/// The text of an ATX heading line (already left-trimmed), or `None` when the
/// line is not a heading. A heading is one to six `#` followed by a space, a
/// tab, or the end of the line; `#tag see [[Alice]]` is a tag line, and reading
/// it as a heading both invents a heading and drops every link on it.
pub(crate) fn heading_text(trimmed: &str) -> Option<&str> {
    let hashes = trimmed.len() - trimmed.trim_start_matches('#').len();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &trimmed[hashes..];
    if rest.is_empty() || rest.starts_with([' ', '\t']) {
        Some(rest.trim())
    } else {
        None
    }
}

/// A link title is honoured as an edge type only when it looks like one
/// (`SCREAMING_SNAKE_CASE`) — otherwise it's a human tooltip, not a type.
fn conn_from_title(title: &str) -> Option<String> {
    let t = title.trim();
    if !t.is_empty()
        && t.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        && t.chars().next().is_some_and(|c| c.is_ascii_uppercase())
    {
        Some(t.to_string())
    } else {
        None
    }
}

/// What one body yielded: its links, and the inline tags the vault profile
/// asks for. One pass, because both are per-line decisions gated by the same
/// fenced-code state.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Extraction {
    pub links: Vec<Link>,
    /// Inline `#tag` names in first-use order; always empty unless
    /// [`Profile::inline_tags`] is set.
    pub tags: Vec<String>,
}

/// Extract resolved outbound links (and, in a vault, inline tags) from a
/// concept body.
///
/// `source_dir` is the concept's directory (bundle-relative, `""` at root), used
/// to resolve relative link targets to bundle-relative concept-ids.
pub fn extract(body: &str, source_dir: &str, profile: &Profile) -> Extraction {
    let mut out = Extraction::default();
    let mut current_heading: Option<String> = None;
    let mut in_fence = false;

    for raw in body.lines() {
        let trimmed = raw.trim_start();
        // Toggle fenced code blocks (``` or ~~~).
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if let Some(h) = heading_text(trimmed) {
            // The heading's own `#`s are not a tag; a `#tag` written *in* the
            // heading text still is (VAULT.md §5.5).
            if profile.inline_tags {
                scan_tags(h, &mut out.tags);
            }
            current_heading = Some(h.to_string());
            continue;
        }
        if profile.inline_tags {
            scan_tags(raw, &mut out.tags);
        }

        let heading_conn = current_heading.as_deref().and_then(conn_from_heading);
        let section = current_heading.as_deref().filter(|h| !h.is_empty());

        for cap in link_re().captures_iter(raw) {
            let m = cap.get(0).unwrap();
            // Skip markdown image links: `![alt](src)`.
            if m.start() > 0 && raw.as_bytes()[m.start() - 1] == b'!' {
                continue;
            }
            let dest = cap.get(1).map(|d| d.as_str()).unwrap_or("");
            let conn = cap
                .get(2)
                .and_then(|t| conn_from_title(t.as_str()))
                .or_else(|| heading_conn.map(|c| c.to_string()))
                .unwrap_or_else(|| DEFAULT_CONN_TYPE.to_string());
            let props = edge_props(profile, section, fragment_of(dest));
            if is_external_url(dest) {
                // External http(s) link → a Source node (citation / reference).
                push_unique(
                    &mut out.links,
                    Link {
                        target: dest.to_string(),
                        conn_type: conn,
                        is_external: true,
                        props,
                        reverse: false,
                    },
                );
            } else if let Some(target) = resolve_target(dest, source_dir) {
                push_unique(
                    &mut out.links,
                    Link {
                        target,
                        conn_type: conn,
                        is_external: false,
                        props,
                        reverse: false,
                    },
                );
            }
        }

        if profile.wikilinks {
            for cap in wikilink_re().captures_iter(raw) {
                let m = cap.get(0).unwrap();
                let is_embed = m.start() > 0 && raw.as_bytes()[m.start() - 1] == b'!';
                // A `#heading` anchor never affects resolution:
                // `[[Note#Section]]` and `[[Note]]` reach the same node
                // (VAULT.md §5.4). The fragment is kept as an edge property.
                let raw_name = cap.get(1).unwrap().as_str();
                let (name, anchor) = match raw_name.split_once('#') {
                    Some((n, a)) => (n.trim(), Some(a.trim())),
                    None => (raw_name.trim(), None),
                };
                if name.is_empty() {
                    continue;
                }
                // strip a trailing `.md` if the wikilink included it
                let target = name.trim_end_matches(".md");
                let conn = if is_embed {
                    // `![[x]]` transcludes: a note becomes an EMBEDS edge, a
                    // file with any other extension is an attachment (§6) and
                    // is not a link at all. Without the extension check every
                    // embedded image minted a concept stub.
                    if !profile.embeds || !embeds_a_note(name) {
                        continue;
                    }
                    EMBEDS_CONN_TYPE.to_string()
                } else {
                    heading_conn
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| DEFAULT_CONN_TYPE.to_string())
                };
                push_unique(
                    &mut out.links,
                    Link {
                        target: target.to_string(),
                        conn_type: conn,
                        is_external: false,
                        props: edge_props(profile, section, anchor),
                        reverse: false,
                    },
                );
            }
        }
    }
    out
}

/// The `#fragment` of a path-link destination, without its `#` — `None` when
/// there is none or it is empty.
fn fragment_of(dest: &str) -> Option<&str> {
    let frag = dest.split_once('#')?.1;
    let frag = frag.split('?').next().unwrap_or(frag);
    (!frag.is_empty()).then_some(frag)
}

/// The edge properties a body link carries (VAULT.md §5.4). Empty for every
/// dialect that does not ask for them, which keeps their connection frames the
/// two-column shape they have always been.
fn edge_props(
    profile: &Profile,
    section: Option<&str>,
    anchor: Option<&str>,
) -> Vec<(String, Value)> {
    if !profile.link_edge_props {
        return Vec::new();
    }
    let mut props = Vec::new();
    if let Some(s) = section {
        props.push(("section".to_string(), Value::String(s.to_string())));
    }
    if let Some(a) = anchor.filter(|a| !a.is_empty()) {
        props.push(("anchor".to_string(), Value::String(a.to_string())));
    }
    props
}

/// Whether an embed's target names a note rather than an attachment. A target
/// with no extension, or with `.md`, is a note; `![[diagram.png]]` is P5's
/// attachment and never a link. A note whose *filename* contains a dot is
/// therefore read as an attachment — write `![[Note.md]]` for it.
fn embeds_a_note(name: &str) -> bool {
    let file = name.rsplit('/').next().unwrap_or(name);
    match file.rsplit_once('.') {
        None => true,
        Some((_, ext)) => ext.eq_ignore_ascii_case("md"),
    }
}

/// Collect inline `#tag` names from one line (VAULT.md §5.5). A tag starts at
/// a `#` that opens the line or follows whitespace, so a URL fragment
/// (`…/x#frag`) and a wikilink anchor (`[[Note#Sec]]`) are not tags; inline
/// code spans are masked out first, and fenced blocks never reach here.
fn scan_tags(line: &str, out: &mut Vec<String>) {
    let masked = mask_code_spans(line);
    let mut cursor = 0;
    while let Some(pos) = masked[cursor..].find('#') {
        let at = cursor + pos;
        cursor = at + 1;
        if at > 0
            && !masked[..at]
                .chars()
                .next_back()
                .is_some_and(char::is_whitespace)
        {
            continue;
        }
        let name: String = masked[at + 1..]
            .chars()
            .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '/'))
            .collect();
        // A bare `#` and a purely numeric `#2026` are not tags — the latter is
        // an issue reference far more often than a tag.
        if name.is_empty() || !name.chars().any(char::is_alphabetic) {
            continue;
        }
        cursor = at + 1 + name.len();
        if !out.contains(&name) {
            out.push(name);
        }
    }
}

/// Replace inline code spans (and their backticks) with NUL, so a `#` inside
/// one is invisible to [`scan_tags`] and a `#` glued to a span's closing
/// backtick still counts as glued rather than as following whitespace.
fn mask_code_spans(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '`' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let open = i;
        while i < chars.len() && chars[i] == '`' {
            i += 1;
        }
        let run = i - open;
        let mut j = i;
        let close = loop {
            if j >= chars.len() {
                break None;
            }
            if chars[j] == '`' {
                let s = j;
                while j < chars.len() && chars[j] == '`' {
                    j += 1;
                }
                if j - s == run {
                    break Some(j);
                }
            } else {
                j += 1;
            }
        };
        // An unmatched run masks only itself: the rest of the line is prose.
        let end = close.unwrap_or(i);
        for _ in open..end {
            out.push('\u{0}');
        }
        i = end;
    }
    out
}

/// The wikilink targets of a frontmatter value, when the whole value is one
/// wikilink string or a list of nothing but wikilink strings (VAULT.md §4.3).
/// `None` means the key stays an ordinary property — including a list that
/// mixes wikilinks with plain strings, which the rule never splits.
pub(crate) fn wikilink_targets(v: &Value) -> Option<Vec<String>> {
    let one = |s: &str| -> Option<String> {
        let name = only_wikilink_re().captures(s)?.get(1)?.as_str();
        let name = name.split('#').next().unwrap_or(name).trim();
        let name = name.trim_end_matches(".md");
        (!name.is_empty()).then(|| name.to_string())
    };
    match v {
        Value::String(s) => one(s).map(|t| vec![t]),
        Value::List(items) => {
            if items.is_empty() {
                return None;
            }
            items
                .iter()
                .map(|x| match x {
                    Value::String(s) => one(s),
                    _ => None,
                })
                .collect()
        }
        _ => None,
    }
}

/// `depends_on` → `DEPENDS_ON`: the edge type a frontmatter key spells
/// (VAULT.md §4.3). Runs of non-alphanumerics collapse to one `_`; a key with
/// no alphanumerics at all yields `""` and emits no edge.
pub(crate) fn upper_snake(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    let mut pending_sep = false;
    for ch in key.chars() {
        if ch.is_alphanumeric() {
            if pending_sep && !out.is_empty() {
                out.push('_');
            }
            pending_sep = false;
            out.extend(ch.to_uppercase());
        } else {
            pending_sep = true;
        }
    }
    out
}

pub(crate) fn push_unique(out: &mut Vec<Link>, link: Link) {
    if !out.contains(&link) {
        out.push(link);
    }
}

/// An external link target — `http(s)` only (`mailto:` and other schemes are not
/// turned into Source nodes).
fn is_external_url(dest: &str) -> bool {
    dest.starts_with("http://") || dest.starts_with("https://")
}

/// Resolve a raw markdown link destination to a bundle-relative concept-id, or
/// `None` if it isn't an in-bundle `.md` target (external URL, anchor-only,
/// directory/index link, …).
fn resolve_target(dest: &str, source_dir: &str) -> Option<String> {
    // Drop fragment / query.
    let dest = dest.split(['#', '?']).next().unwrap_or(dest);
    if dest.is_empty() {
        return None;
    }
    // External or non-relative schemes.
    if dest.contains("://") || dest.starts_with("mailto:") {
        return None;
    }
    // Only markdown concepts become edges (directory/index links are handled by
    // structural CONTAINS edges).
    if !dest.ends_with(".md") {
        return None;
    }
    let stem = &dest[..dest.len() - 3]; // strip ".md"

    let normalized = if let Some(abs) = stem.strip_prefix('/') {
        normalize_path_parts(abs.split('/'))
    } else {
        // Relative to the source concept's directory.
        let mut parts: Vec<&str> = if source_dir.is_empty() {
            Vec::new()
        } else {
            source_dir.split('/').collect()
        };
        let combined = parts
            .drain(..)
            .chain(stem.split('/'))
            .collect::<Vec<_>>()
            .join("/");
        normalize_path_parts(combined.split('/'))
    };
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

/// Normalize a path: drop `.`/empty segments, pop on `..`, join with `/`.
fn normalize_path_parts<'a>(parts: impl Iterator<Item = &'a str>) -> String {
    let mut stack: Vec<&str> = Vec::new();
    for p in parts {
        match p {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            other => stack.push(other),
        }
    }
    stack.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::model::Dialect;

    /// Links only, for the assertions that predate `Extraction`.
    fn extract_links(body: &str, source_dir: &str, dialect: Dialect) -> Vec<Link> {
        extract(body, source_dir, &Profile::for_dialect(dialect)).links
    }

    fn vault() -> Profile {
        Profile::for_dialect(Dialect::Obsidian)
    }

    fn props_of(link: &Link) -> Vec<(&str, &str)> {
        link.props
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str(),
                    match v {
                        Value::String(s) => s.as_str(),
                        _ => panic!("edge props are strings"),
                    },
                )
            })
            .collect()
    }

    #[test]
    fn titled_link_yields_typed_edge() {
        let body = "Joined with [customers](/tables/customers.md \"JOINS_WITH\") here.";
        let links = extract_links(body, "tables", Dialect::Okf);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "tables/customers");
        assert_eq!(links[0].conn_type, "JOINS_WITH");
    }

    #[test]
    fn section_header_inference() {
        let body = "# Citations\n[1] [src](/references/x.md)\n# Joins\nsee [y](/tables/y.md)";
        let links = extract_links(body, "tables", Dialect::Okf);
        let by_target: std::collections::HashMap<_, _> = links
            .iter()
            .map(|l| (l.target.as_str(), l.conn_type.as_str()))
            .collect();
        assert_eq!(by_target.get("references/x"), Some(&"CITES"));
        assert_eq!(by_target.get("tables/y"), Some(&"JOINS_WITH"));
    }

    #[test]
    fn untyped_link_defaults_to_links_to() {
        let body = "See [other](./other.md) for details.";
        let links = extract_links(body, "tables", Dialect::Okf);
        assert_eq!(links[0].target, "tables/other");
        assert_eq!(links[0].conn_type, "LINKS_TO");
    }

    #[test]
    fn relative_parent_paths_resolve() {
        let body = "Part of the [sales dataset](../datasets/sales.md).";
        let links = extract_links(body, "tables", Dialect::Okf);
        assert_eq!(links[0].target, "datasets/sales");
    }

    #[test]
    fn tooltip_title_is_not_a_type() {
        let body = "See [customers](/tables/customers.md \"the customers table\").";
        let links = extract_links(body, "tables", Dialect::Okf);
        assert_eq!(links[0].conn_type, "LINKS_TO");
    }

    #[test]
    fn external_captured_non_md_skipped() {
        let body =
            "# Citations\n[site](https://example.com) and [dir](subdir/) and [doc](./pic.png)";
        let links = extract_links(body, "", Dialect::Okf);
        // the http link becomes an external (Source) link; dir/ and .png are skipped
        assert_eq!(links.len(), 1);
        assert!(links[0].is_external);
        assert_eq!(links[0].target, "https://example.com");
        assert_eq!(links[0].conn_type, "CITES");
    }

    #[test]
    fn images_and_fenced_code_skipped() {
        let body =
            "![alt](/tables/x.md)\n```sql\nSELECT [a](/tables/y.md)\n```\n[real](/tables/z.md)";
        let links = extract_links(body, "tables", Dialect::Okf);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "tables/z");
    }

    #[test]
    fn wikilinks_only_in_loose_dialect() {
        let body = "See [[other-note]] and [[sub/thing|alias]].";
        assert!(extract_links(body, "", Dialect::Okf).is_empty());
        let links = extract_links(body, "", Dialect::Loose);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].target, "other-note");
        assert_eq!(links[1].target, "sub/thing");
    }

    #[test]
    fn embedded_wikilink_is_not_a_link() {
        // `![[img.png]]` renders an attachment; it used to mint a concept stub.
        let links = extract_links(
            "![[diagram.png]] and ![[note|alias]] but [[real-note]]",
            "",
            Dialect::Loose,
        );
        let targets: Vec<&str> = links.iter().map(|l| l.target.as_str()).collect();
        assert_eq!(targets, vec!["real-note"]);
    }

    #[test]
    fn tag_line_is_not_a_heading_and_keeps_its_links() {
        let links = extract_links("#project see [[Alice]]", "", Dialect::Loose);
        assert_eq!(links.len(), 1, "a `#tag` line is prose, not a heading");
        assert_eq!(links[0].target, "Alice");
        // …and it does not leak a heading into the edge-type ladder.
        assert_eq!(links[0].conn_type, "LINKS_TO");
    }

    #[test]
    fn heading_text_requires_a_space_and_at_most_six_hashes() {
        assert_eq!(heading_text("# Joins"), Some("Joins"));
        assert_eq!(heading_text("###\tDeps"), Some("Deps"));
        assert_eq!(heading_text("#"), Some(""));
        assert_eq!(heading_text("#related"), None);
        assert_eq!(heading_text("####### Deep"), None);
        assert_eq!(heading_text("plain"), None);
    }

    #[test]
    fn real_heading_still_types_the_links_below_it() {
        let links = extract_links(
            "# Related work\n#seealso\nsee [[Alice]]",
            "",
            Dialect::Loose,
        );
        assert_eq!(links.len(), 1);
        assert_eq!(
            links[0].conn_type, "RELATED",
            "heading ladder still applies"
        );
    }

    #[test]
    fn loose_dialect_link_set_is_unchanged() {
        let body = "see [[other-note]], ![[img.png]], [x](/tables/y.md) and #tag [[Alice]]";
        let got = extract(body, "tables", &Profile::for_dialect(Dialect::Loose));
        let links: Vec<(&str, &str, bool)> = got
            .links
            .iter()
            .map(|l| (l.target.as_str(), l.conn_type.as_str(), l.props.is_empty()))
            .collect();
        assert_eq!(
            links,
            vec![
                ("tables/y", "LINKS_TO", true),
                ("other-note", "LINKS_TO", true),
                ("Alice", "LINKS_TO", true),
            ]
        );
        assert!(got.tags.is_empty(), "inline tags are a vault rule only");
    }

    #[test]
    fn wikilink_anchor_is_stripped() {
        let links = extract_links(
            "see [[Design Notes#Goals]] and [[api#parse]]",
            "",
            Dialect::Loose,
        );
        let targets: Vec<&str> = links.iter().map(|l| l.target.as_str()).collect();
        assert_eq!(targets, vec!["Design Notes", "api"]);
    }

    // ---- vault link semantics (VAULT.md §5) ----

    #[test]
    fn vault_body_link_carries_its_section() {
        let got = extract(
            "Above every heading: [[atlas]].\n\n## Deep dive\n\nBelow one: [[bob]].",
            "",
            &vault(),
        );
        assert_eq!(
            props_of(&got.links[0]),
            Vec::new(),
            "above the first heading"
        );
        assert_eq!(props_of(&got.links[1]), vec![("section", "Deep dive")]);
    }

    #[test]
    fn vault_fragment_link_carries_its_anchor() {
        let got = extract(
            "## Notes\n[[atlas#Overview]] and [x](sub/b.md#usage) and [[bob#^b-12]]",
            "",
            &vault(),
        );
        let by_target: std::collections::HashMap<&str, Vec<(&str, &str)>> = got
            .links
            .iter()
            .map(|l| (l.target.as_str(), props_of(l)))
            .collect();
        assert_eq!(
            by_target["atlas"],
            vec![("section", "Notes"), ("anchor", "Overview")]
        );
        assert_eq!(
            by_target["sub/b"],
            vec![("section", "Notes"), ("anchor", "usage")],
            "a path link's fragment is an anchor too, and never part of the target"
        );
        assert_eq!(
            by_target["bob"],
            vec![("section", "Notes"), ("anchor", "^b-12")],
            "a block reference keeps its caret"
        );
    }

    #[test]
    fn vault_embed_of_a_note_is_an_edge_of_a_file_is_not() {
        let got = extract("![[old]] ![[notes/deep.md]] ![[diagram.png]]", "", &vault());
        let got: Vec<(&str, &str)> = got
            .links
            .iter()
            .map(|l| (l.target.as_str(), l.conn_type.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![("old", "EMBEDS"), ("notes/deep", "EMBEDS")],
            "`.png` is an attachment, not a link"
        );
    }

    #[test]
    fn loose_still_drops_every_embed() {
        let links = extract_links("![[old]] and ![[diagram.png]]", "", Dialect::Loose);
        assert!(links.is_empty(), "EMBEDS is a vault rule");
    }

    #[test]
    fn vault_inline_tags_honour_the_four_exclusions() {
        let body = concat!(
            "# Heading #inhead\n",
            "A #plain tag and a #kebab-case/nested one.\n",
            "Not in a `span with #incode in it`, not in https://ex.com/p#frag,\n",
            "not in [[Note#Section]], and #2026 is not a tag.\n",
            "```\n#infence\n```\n",
            "#plain again is not a second tag.\n",
        );
        let got = extract(body, "", &vault());
        assert_eq!(
            got.tags,
            vec!["inhead", "plain", "kebab-case/nested"],
            "a heading's own `#` is not a tag, but a tag written in one is"
        );
    }

    #[test]
    fn loose_extracts_no_inline_tags() {
        assert!(
            extract("A #plain tag.", "", &Profile::for_dialect(Dialect::Loose))
                .tags
                .is_empty()
        );
    }

    #[test]
    fn frontmatter_wikilink_values_name_their_targets() {
        let one = Value::String("[[Seismic interpretation]]".to_string());
        assert_eq!(
            wikilink_targets(&one),
            Some(vec!["Seismic interpretation".to_string()])
        );
        let list = Value::List(vec![
            Value::String("[[A]]".to_string()),
            Value::String("  [[sub/B.md#frag|shown]]  ".to_string()),
        ]);
        assert_eq!(
            wikilink_targets(&list),
            Some(vec!["A".to_string(), "sub/B".to_string()])
        );
        // The rule never splits a key: a mixed list stays a property.
        let mixed = Value::List(vec![
            Value::String("[[A]]".to_string()),
            Value::String("plain".to_string()),
        ]);
        assert_eq!(wikilink_targets(&mixed), None);
        assert_eq!(
            wikilink_targets(&Value::String("see [[A]] there".to_string())),
            None,
            "a wikilink inside prose is not a typed-edge value"
        );
        assert_eq!(wikilink_targets(&Value::List(Vec::new())), None);
        assert_eq!(wikilink_targets(&Value::Int64(3)), None);
    }

    #[test]
    fn upper_snake_spells_the_edge_type() {
        assert_eq!(upper_snake("depends_on"), "DEPENDS_ON");
        assert_eq!(upper_snake("see also"), "SEE_ALSO");
        assert_eq!(upper_snake("metadata.source"), "METADATA_SOURCE");
        assert_eq!(upper_snake("--x--"), "X");
        assert_eq!(upper_snake("---"), "");
    }
}
