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
//! The body is read through `okf::structure`'s block tree, one scan region at
//! a time (VAULT.md §5.1): a fenced code block and a `%%comment%%` are the two
//! regions that are never scanned, while indented code and HTML blocks are
//! read like prose. A region is one construct's source — a heading line, a
//! paragraph, a list item, a table cell — so link text may wrap across a line
//! but never across a paragraph, and the section every reference carries is
//! the heading the tree puts that region under.
//!
//! Markdown image links (`![alt](src)`) are never links. External `http(s)`
//! links are captured as `is_external` (they become `Source` nodes in the
//! builder); `mailto:`, anchors, and non-`.md` directory links are skipped
//! (directory structure is captured separately).
//!
//! What the vault profile adds on top (VAULT.md §5, §6), each behind its own
//! [`Profile`] field so `okf`/`loose` bundles are untouched: `section` and
//! `anchor` edge properties, an `EMBEDS` edge for `![[Note]]`, inline `#tag`
//! extraction, and `![alt](x.png)` / `![[x.png]]` attachment references
//! (resolved against the vault's files by the builder, not here).
//! Frontmatter-valued edges (§4.3) are built from the parsed frontmatter by
//! `crate::okf::parse_file`, using [`wikilink_targets`] and [`upper_snake`]
//! from here.

use crate::datatypes::values::Value;
use crate::okf::model::{AttachmentRef, Link, Profile, DEFAULT_CONN_TYPE, EMBEDS_CONN_TYPE};
use crate::okf::structure::{self, BlockTree};
use regex::{Captures, Match, Regex};
use std::borrow::Cow;
use std::sync::OnceLock;

/// `![alt](src)` / `![alt](src "title")` on its own. Used for the images
/// written **inside** a link's text, which the outer match consumes whole.
fn image_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"!\[([^\]]*)\]\(([^)\s]+)(?:\s+"([^"]*)")?\)"#).unwrap())
}

fn link_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // [text](dest) or [text](dest "title"). Group 1 is the text — the alt text
    // when a `!` precedes the whole match.
    //
    // The text is any run of non-`]` characters, **or a complete image**: that
    // second branch is the whole of `[![alt](thumb)](full)`, a thumbnail
    // linking to the full picture. Without it the first `]` ends the text and
    // the match stops at `[![alt](thumb)`, so `full` is never seen. The image
    // branch comes first because alternation is leftmost-*first*: taken the
    // other way round, `[^\]]` eats `![alt` and the match reverts to the short
    // one (`linked_image_yields_the_thumbnail_and_the_outer_link` pins it).
    //
    // Newlines are inside `[^\]]`, so a hard-wrapped `[Binary\nExtensions](url)`
    // matches; a scan region never spans a paragraph, which is what keeps that
    // from running two paragraphs' brackets together.
    RE.get_or_init(|| {
        Regex::new(r#"\[((?:!\[[^\]]*\]\([^)\s]*(?:\s+"[^"]*")?\)|[^\]])*)\]\(([^)\s]+)(?:\s+"([^"]*)")?\)"#)
            .unwrap()
    })
}

fn wikilink_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // [[name]] or [[name|alias]]. Group 2 is the alias — display text for a
    // note link (not stored), the alt text for an attachment embed (§6.1).
    // Neither half may hold a newline: Obsidian writes a wikilink on one line,
    // and allowing the span to wrap turns a stray `[[` and a later `]]` into a
    // link to whatever prose sits between them.
    RE.get_or_init(|| Regex::new(r"\[\[([^\]|\n]+)(?:\|([^\]\n]*))?\]\]").unwrap())
}

/// One `[[target#anchor|display text]]` split into its three parts.
pub(crate) struct WikiRef<'t> {
    pub name: &'t str,
    /// The `#fragment`, without its `#`; `None` when there is none.
    pub anchor: Option<&'t str>,
    /// The display text, which becomes the edge's `label` where `structure:`
    /// is declared (VAULT.md §5.1, §5.4).
    pub alias: Option<&'t str>,
}

/// Split one wikilink's two capture groups into target, anchor and display
/// text.
///
/// `\|` is Obsidian's escape for a pipe **inside a table cell**, and the
/// wikilink regex reads the pipe as the separator it is: the backslash is left
/// at the end of the target, where it belongs to the escape and not to the
/// note's name. Without this, `[[Usage\|the guide]]` in a cell names a note
/// spelled `Usage\` — 398 dangling links on one converted corpus — and the
/// display text is lost with it.
fn wikilink_parts<'t>(raw_name: &'t str, alias: Option<&'t str>) -> WikiRef<'t> {
    let raw = match alias {
        Some(_) => raw_name.strip_suffix('\\').unwrap_or(raw_name),
        None => raw_name,
    };
    let (name, anchor) = match raw.split_once('#') {
        Some((n, a)) => (n.trim(), Some(a.trim())),
        None => (raw.trim(), None),
    };
    WikiRef {
        name,
        anchor,
        alias: alias.map(str::trim).filter(|a| !a.is_empty()),
    }
}

/// The first `[[wikilink]]` in a string that is not an `![[embed]]` — how a
/// table cell states the target of an edge row (VAULT.md §7.1 `tables:`).
pub(crate) fn first_wikilink(text: &str) -> Option<WikiRef<'_>> {
    wikilink_re().captures_iter(text).find_map(|cap| {
        let m = cap.get(0).expect("the whole match");
        (m.start() == 0 || text.as_bytes()[m.start() - 1] != b'!').then(|| {
            wikilink_parts(
                cap.get(1).expect("the name group").as_str(),
                cap.get(2).map(|a| a.as_str()),
            )
        })
    })
}

/// A whole string that is nothing but one wikilink — the frontmatter
/// typed-edge discriminator (VAULT.md §4.3).
fn only_wikilink_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\s*\[\[([^\]|]+)(?:\|([^\]]*))?\]\]\s*$").unwrap())
}

/// Map a section heading to a connection type, or `None` to fall through
/// (VAULT.md §5.3, rungs 2 and 3).
///
/// A [`Profile::heading_edges`] entry is tried first and matches the **whole**
/// heading, case-insensitively — which is the point of declaring one: the
/// built-in ladder below matches a *substring*, so a corpus whose "Related
/// topics" sections mean `RELATED_TO` cannot get there by adding a rung, only
/// by overriding one. The map is a handful of entries, so a linear scan beats
/// lowercasing the heading to probe a map.
pub(crate) fn conn_from_heading(heading: &str, profile: &Profile) -> Option<String> {
    if let Some((_, edge)) = profile
        .heading_edges
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(heading))
    {
        return Some(edge.clone());
    }
    let h = heading.to_ascii_lowercase();
    let built_in = if h.contains("citation") {
        "CITES"
    } else if h.contains("join") {
        "JOINS_WITH"
    } else if h.contains("reference") {
        "REFERENCES"
    } else if h.contains("related") {
        "RELATED"
    } else if h.contains("depend") {
        "DEPENDS_ON"
    } else {
        return None;
    };
    Some(built_in.to_string())
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
/// asks for. One pass over the same scan regions, because both are gated by
/// the same fenced-code and comment extents.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Extraction {
    pub links: Vec<Link>,
    /// Inline `#tag` names in first-use order; always empty unless
    /// [`Profile::inline_tags`] is set.
    pub tags: Vec<String>,
    /// `![…]` references in body order; always empty unless
    /// [`Profile::attachments`] is set. Within one scan region the
    /// markdown-image spelling is collected before the wikilink-embed
    /// spelling, because the two syntaxes are scanned by separate passes —
    /// which only reorders references written in the same paragraph, and
    /// `ordinal` stays deterministic either way.
    pub attachments: Vec<AttachmentRef>,
    /// VAULT.md §9 path errors this body wrote: a reference naming an absolute
    /// filesystem path, or one climbing above the vault root. One entry per
    /// distinct offending target; the caller prefixes the note's own path.
    pub path_errors: Vec<String>,
}

/// Extract resolved outbound links (and, in a vault, inline tags) from a
/// concept body.
///
/// `source_dir` is the concept's directory (bundle-relative, `""` at root), used
/// to resolve relative link targets to bundle-relative concept-ids.
pub fn extract(body: &str, source_dir: &str, profile: &Profile) -> Extraction {
    extract_with_tree(body, &structure::parse_blocks(body), source_dir, profile)
}

/// [`extract`] against a block tree the caller already has, so a note that also
/// wants its title from the tree parses its body once.
pub(crate) fn extract_with_tree(
    body: &str,
    tree: &BlockTree,
    source_dir: &str,
    profile: &Profile,
) -> Extraction {
    let mut out = Extraction::default();
    // Masked once for the whole body: an inline code span is literal text, so
    // the constructs written inside one are hidden from every pass below while
    // the block around it stays one contiguous region (VAULT.md §5.1).
    let masked = structure::mask_code_spans(body, tree);
    for range in structure::scan_regions(body, tree) {
        let text = RegionText {
            masked: &masked[range.clone()],
            raw: &body[range.clone()],
        };
        // A region never crosses a heading, so the section is decided once for
        // the whole of it — including a heading's own line, which carries its
        // own heading (VAULT.md §5.4). An empty heading text (`### #`, whose
        // lone `#` is CommonMark's closing sequence) names no section.
        let heading = structure::heading_at(tree, range.start);
        let region = Region {
            source_dir,
            profile,
            section: heading.map(|h| h.text.as_str()).filter(|h| !h.is_empty()),
            heading_conn: heading.and_then(|h| conn_from_heading(&h.text, profile)),
        };
        if profile.inline_tags {
            // Per line: a tag is a line-local token, and an inline code span
            // opened on one line does not reach across to hide one on another.
            for line in text.masked.lines() {
                scan_tags(line, &mut out.tags);
            }
        }
        region.scan(text, &mut out);
    }
    out
}

/// One scan region's constants: the note's directory, the dialect, and the
/// heading the region sits under.
struct Region<'a> {
    source_dir: &'a str,
    profile: &'a Profile,
    section: Option<&'a str>,
    heading_conn: Option<String>,
}

/// One scan region's text in the two spellings the pass needs.
///
/// `masked` is what the regexes read — every link-, tag- and
/// attachment-spelling character inside an inline code span replaced by NUL, so
/// that ``​`[[Note]]`​`` matches nothing — and `raw` is the author's own bytes at
/// exactly the same offsets, which is what a match's label, target and title
/// are read from. The two are the same length by construction (only ASCII bytes
/// are replaced), so a match found in one indexes the other.
#[derive(Clone, Copy)]
struct RegionText<'t> {
    masked: &'t str,
    raw: &'t str,
}

impl<'t> RegionText<'t> {
    /// The author's own text under a match.
    fn of(&self, m: Match<'_>) -> &'t str {
        &self.raw[m.start()..m.end()]
    }

    /// The author's own text under a capture group, `""` when it did not match.
    fn group(&self, cap: &Captures<'_>, index: usize) -> &'t str {
        cap.get(index).map_or("", |m| self.of(m))
    }

    /// One match's span as a region in its own right — a link's display text,
    /// which is scanned again for the pictures inside it.
    fn sub(&self, m: Match<'_>) -> RegionText<'t> {
        RegionText {
            masked: &self.masked[m.start()..m.end()],
            raw: self.of(m),
        }
    }
}

/// A reference the region's two syntaxes found, kept with its offset so both
/// spellings are handled in the order they were **written**.
enum Found<'t> {
    Markdown(Captures<'t>),
    Wikilink(Captures<'t>),
}

impl Region<'_> {
    /// Every `[text](dest)`, `![alt](src)`, `[[Note]]` and `![[embed]]` in one
    /// region, in document order.
    ///
    /// The two syntaxes are matched by separate regexes and then merged on
    /// offset, so a paragraph mixing them yields its references in the order a
    /// reader meets them — which is what makes an attachment's `ordinal`
    /// survive re-wrapping the prose around it.
    fn scan(&self, text: RegionText<'_>, out: &mut Extraction) {
        let mut found: Vec<Found<'_>> = link_re()
            .captures_iter(text.masked)
            .map(Found::Markdown)
            .collect();
        if self.profile.wikilinks {
            found.extend(
                wikilink_re()
                    .captures_iter(text.masked)
                    .map(Found::Wikilink),
            );
        }
        found.sort_by_key(Found::start);
        for one in found {
            match one {
                Found::Markdown(cap) => self.markdown(text, &cap, out),
                Found::Wikilink(cap) => self.wikilink(text, &cap, out),
            }
        }
    }

    /// `[text](dest)`, and the `![alt](src)` spelling of an attachment.
    fn markdown(&self, text: RegionText<'_>, cap: &Captures<'_>, out: &mut Extraction) {
        let m = cap.get(0).expect("the whole match");
        // The markdown spelling is the one a tool percent-encodes, so it
        // is decoded here — before resolution *and* before the §9 path
        // check, which `%2e%2e/` would otherwise walk straight past.
        let decoded = percent_decode(text.group(cap, 2));
        let dest = decoded.as_ref();
        // `![alt](src)` is never a link. Under the vault profile it is an
        // attachment reference instead (VAULT.md §6.1); otherwise it is
        // dropped, as it always was.
        if m.start() > 0 && text.masked.as_bytes()[m.start() - 1] == b'!' {
            let label = text.group(cap, 1);
            self.check_path(dest, out);
            self.attachment(dest, Some(label), out);
            return;
        }
        // A link whose text is a picture — `[![alt](thumb)](full)`. The
        // thumbnail is a reference in its own right and the outer half is an
        // ordinary link, so both are recorded and the reader's alt text comes
        // from the inner image rather than from its markdown source.
        let alt = match cap.get(1) {
            Some(label) => self.inner_images(text.sub(label), out),
            None => "",
        };
        if !is_external_url(dest) {
            self.check_path(dest, out);
        }
        let conn = cap
            .get(3)
            .and_then(|t| conn_from_title(text.of(t)))
            .or_else(|| self.heading_conn.clone())
            .unwrap_or_else(|| DEFAULT_CONN_TYPE.to_string());
        // A markdown link's text is prose around a path, not a display name for
        // a note: VAULT.md §5.4 gives `label` to the `[[Target|text]]` spelling
        // alone.
        let props = edge_props(self.profile, self.section, fragment_of(dest), None);
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
        } else if let Some(target) = resolve_target(dest, self.source_dir) {
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
        } else {
            // Everything a note link cannot be: a plain `[text](file.ext)`
            // naming a file the vault holds, which is a reference to that
            // file exactly as `![alt](file.ext)` is (VAULT.md §6.1), with
            // the link text as its alt. `attachment` drops what is left —
            // an in-page `#anchor`, a directory, a URL scheme — so those
            // stay the silent no-ops they always were.
            self.attachment(dest, Some(alt), out);
        }
    }

    /// `[[Note]]`, `[[Note|alias]]` and the `![[…]]` embed spelling.
    fn wikilink(&self, text: RegionText<'_>, cap: &Captures<'_>, out: &mut Extraction) {
        let m = cap.get(0).expect("the whole match");
        let is_embed = m.start() > 0 && text.masked.as_bytes()[m.start() - 1] == b'!';
        // A `#heading` anchor never affects resolution: `[[Note#Section]]` and
        // `[[Note]]` reach the same node (VAULT.md §5.4). The fragment is kept
        // as an edge property.
        let parts = wikilink_parts(
            text.of(cap.get(1).expect("the name group")),
            cap.get(2).map(|a| text.of(a)),
        );
        let (name, anchor) = (parts.name, parts.anchor);
        if name.is_empty() {
            return;
        }
        // A wikilink is a name, not a URL, so it is never decoded — but §5.2
        // tries a `/`-bearing one as a path, and §6 resolves every embed as
        // one, so both reach the §9 check.
        if self.profile.path_safety {
            if is_embed && !embeds_a_note(name) {
                // An embedded file is resolved as a path whatever it is
                // spelled like, so every spelling is checked.
                record_path_error(&mut out.path_errors, name, self.source_dir);
            } else {
                record_wikilink_path_error(&mut out.path_errors, name, self.source_dir);
            }
        }
        // strip a trailing `.md` if the wikilink included it
        let target = name.trim_end_matches(".md");
        let conn = if is_embed {
            // `![[x]]` transcludes: a note becomes an EMBEDS edge, a file with
            // any other extension is an attachment (§6) and is not a link at
            // all. Without the extension check every embedded image minted a
            // concept stub.
            if !embeds_a_note(name) {
                self.attachment(name, parts.alias, out);
                return;
            }
            if !self.profile.embeds {
                return;
            }
            EMBEDS_CONN_TYPE.to_string()
        } else {
            self.heading_conn
                .clone()
                .unwrap_or_else(|| DEFAULT_CONN_TYPE.to_string())
        };
        push_unique(
            &mut out.links,
            Link {
                target: target.to_string(),
                conn_type: conn,
                is_external: false,
                props: edge_props(self.profile, self.section, anchor, parts.alias),
                reverse: false,
            },
        );
    }

    /// Record every `![alt](src)` written *inside* a link's text and return the
    /// alt the outer link should carry.
    ///
    /// A link text that is nothing but one image reports that image's alt,
    /// because that is the text a reader sees; a text mixing prose and pictures
    /// keeps its own, and every picture in it is still a reference.
    fn inner_images<'t>(&self, label: RegionText<'t>, out: &mut Extraction) -> &'t str {
        let mut alt = label.raw;
        for image in image_re().captures_iter(label.masked) {
            let decoded = percent_decode(label.group(&image, 2));
            self.check_path(decoded.as_ref(), out);
            let inner = label.group(&image, 1);
            self.attachment(decoded.as_ref(), Some(inner), out);
            if image
                .get(0)
                .is_some_and(|m| m.as_str() == label.masked.trim())
            {
                alt = inner;
            }
        }
        alt
    }

    fn attachment(&self, dest: &str, alt: Option<&str>, out: &mut Extraction) {
        push_attachment(&mut out.attachments, self.profile, dest, alt, self.section);
    }

    fn check_path(&self, dest: &str, out: &mut Extraction) {
        if self.profile.path_safety {
            record_path_error(&mut out.path_errors, dest, self.source_dir);
        }
    }
}

impl Found<'_> {
    fn start(&self) -> usize {
        match self {
            Found::Markdown(cap) | Found::Wikilink(cap) => {
                cap.get(0).expect("the whole match").start()
            }
        }
    }
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
    label: Option<&str>,
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
    // Declaring `structure:` is what turns the display text into a property:
    // a vault that models the inside of its notes is one that wants its links
    // described, and there is no separate switch (VAULT.md §5.4, §7.1).
    if let Some(l) = label.filter(|l| !l.is_empty() && profile.structure.is_some()) {
        props.push(("label".to_string(), Value::String(l.to_string())));
    }
    props
}

/// Record one `![…]` reference (VAULT.md §6.1), or drop it when the profile
/// does not read attachments — which is what keeps `okf`/`loose` on the
/// behaviour they have always had.
///
/// A target carrying **any** URI scheme is somebody else's file: it is not in
/// the vault, no `stat` describes it, and §6.2's ladder has no rung for it.
/// `http(s)` is only the common one — `[mail](mailto:a@b.com)` reaches here
/// too now that a plain link does, and `mailto:a@b.com` ends in `.com`, so
/// without the scheme check it would resolve as a file named `com`. A target
/// with no extension or a `.md` one is a note embed, handled by the caller.
fn push_attachment(
    out: &mut Vec<AttachmentRef>,
    profile: &Profile,
    dest: &str,
    alt: Option<&str>,
    section: Option<&str>,
) {
    if !profile.attachments || has_uri_scheme(dest) {
        return;
    }
    let target = dest.split(['#', '?']).next().unwrap_or(dest).trim();
    if target.is_empty() || embeds_a_note(target) {
        return;
    }
    out.push(AttachmentRef {
        target: target.to_string(),
        alt: alt
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .map(str::to_string),
        section: section.map(str::to_string),
    });
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
        let cap = only_wikilink_re().captures(s)?;
        // The same `\|` escape a table cell writes (VAULT.md §5.1): one
        // spelling of a wikilink, read one way wherever it is written.
        let parts = wikilink_parts(cap.get(1)?.as_str(), cap.get(2).map(|a| a.as_str()));
        let name = parts.name.trim_end_matches(".md");
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

/// Whether a target opens with a URI scheme (`mailto:`, `ftp://`, `obsidian:`)
/// and so names something outside the vault's file tree.
///
/// A scheme is at least **two** characters, which is what keeps a Windows
/// drive letter out: `C:/x.png` is one character before the colon, and §9
/// refuses it as an absolute filesystem path rather than as a URL.
fn has_uri_scheme(dest: &str) -> bool {
    let Some((scheme, _)) = dest.split_once(':') else {
        return false;
    };
    scheme.len() >= 2
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
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

/// Decode `%XX` escapes in a **markdown-style** target (VAULT.md §5.1, §6.1).
///
/// Obsidian writes `img/a%20b.png` for a file named `a b.png` in the
/// `[text](target)` and `![alt](target)` spellings, so resolving the literal
/// text looks for a file whose name contains a percent sign and finds nothing.
/// A `%` not followed by two hex digits is itself, so a filename that really
/// contains one survives; an escape sequence that is not UTF-8 is left alone
/// rather than replaced. Wikilink targets are names, not URLs, and are never
/// decoded — `[[a%20b]]` names a note spelled that way.
pub(crate) fn percent_decode(target: &str) -> Cow<'_, str> {
    if !target.contains('%') {
        return Cow::Borrowed(target);
    }
    let bytes = target.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let decoded = (bytes[i] == b'%' && i + 2 < bytes.len())
            .then(|| {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
                u8::from_str_radix(hex, 16).ok()
            })
            .flatten();
        match decoded {
            Some(byte) => {
                out.push(byte);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    match String::from_utf8(out) {
        Ok(text) => Cow::Owned(text),
        Err(_) => Cow::Borrowed(target),
    }
}

/// Why a path-shaped reference names a place the vault does not own
/// (VAULT.md §9), or `None` when it stays inside.
///
/// `source_dir` is the referencing note's directory, because how far `..` has
/// to climb before it leaves depends on where it was written. A leading `/` is
/// deliberately **not** absolute here: §6.2 fixes it as vault-root-relative,
/// which is the only reading under which a vault is relocatable.
pub(crate) fn path_error(target: &str, source_dir: &str) -> Option<String> {
    if is_absolute_fs_path(target) {
        return Some(format!("`{target}` is an absolute filesystem path"));
    }
    escapes_root(target, source_dir).then(|| format!("`{target}` escapes the vault root"))
}

/// A target naming a filesystem location instead of a vault-relative one: a
/// Windows drive or UNC path, a `~` home path, or a `file:` URL. A vault is a
/// directory that gets copied, zipped and served from somewhere else, so none
/// of these can mean anything portable.
fn is_absolute_fs_path(target: &str) -> bool {
    let bytes = target.as_bytes();
    let drive = matches!(bytes, [letter, b':', sep, ..]
        if letter.is_ascii_alphabetic() && (*sep == b'/' || *sep == b'\\'));
    drive
        || target.starts_with('\\')
        || target == "~"
        || target.starts_with("~/")
        || target.len() >= 5 && target[..5].eq_ignore_ascii_case("file:")
}

/// Whether `..` segments climb above the vault root. [`normalize_path_parts`]
/// pops at the root and carries on, so `../../etc/passwd` silently *becomes*
/// `etc/passwd` there — the escape is only visible while the segments are
/// still being counted.
fn escapes_root(target: &str, source_dir: &str) -> bool {
    let mut depth: isize = if target.starts_with('/') || source_dir.is_empty() {
        0
    } else {
        source_dir.split('/').filter(|p| !p.is_empty()).count() as isize
    };
    for part in target.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                depth -= 1;
                if depth < 0 {
                    return true;
                }
            }
            _ => depth += 1,
        }
    }
    false
}

/// Record one §9 path error for a **wikilink** target, wherever it was
/// written — in the prose or as a typed-edge key's value (VAULT.md §4.3).
///
/// A wikilink is a name, not a path, so only one spelled like a path is
/// checked: §5.2 resolves a `/`-bearing target as a vault-relative id and then
/// as a note-relative path, which is the rung `[[../../etc/passwd]]` would
/// otherwise reach. One routine for both sources, so a spelling refused in the
/// body cannot be accepted in frontmatter.
pub(crate) fn record_wikilink_path_error(out: &mut Vec<String>, name: &str, source_dir: &str) {
    if name.contains('/') || is_absolute_fs_path(name) {
        record_path_error(out, name, source_dir);
    }
}

/// Record one §9 path error, once per distinct target: a note that references
/// the same escaping path in five places has one problem, not five.
fn record_path_error(out: &mut Vec<String>, target: &str, source_dir: &str) {
    if let Some(message) = path_error(target, source_dir) {
        if !out.contains(&message) {
            out.push(message);
        }
    }
}

/// Normalize a path: drop `.`/empty segments, pop on `..`, join with `/`.
pub(crate) fn normalize_path_parts<'a>(parts: impl Iterator<Item = &'a str>) -> String {
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

    /// The heading rule — one to six `#` then a space, a tab or the end of the
    /// line — read through the `section` a link under it carries, which is the
    /// only thing the rule is *for* now that the tree owns it.
    #[test]
    fn heading_rule_decides_the_section_a_link_carries() {
        let section_under = |heading: &str| {
            let body = format!("{heading}\nsee [[Alice]]");
            let links = extract(&body, "", &vault()).links;
            assert_eq!(links.len(), 1, "{heading}");
            props_of(&links[0])
                .into_iter()
                .find(|(k, _)| *k == "section")
                .map(|(_, v)| v.to_string())
        };
        assert_eq!(section_under("# Joins").as_deref(), Some("Joins"));
        assert_eq!(section_under("###\tDeps").as_deref(), Some("Deps"));
        assert_eq!(
            section_under("#"),
            None,
            "an empty heading names no section"
        );
        assert_eq!(section_under("#related"), None, "a `#tag` line is prose");
        assert_eq!(section_under("####### Deep"), None, "seven hashes is prose");
        assert_eq!(section_under("plain"), None);
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
                // Document order, both spellings merged: before P2 the
                // markdown pass ran over a whole line ahead of the wikilink
                // pass, so `tables/y` came out first although it is written
                // third.
                ("other-note", "LINKS_TO", true),
                ("tables/y", "LINKS_TO", true),
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

    /// A heading line is a body line: every reference written on it counts,
    /// and the section it carries is that heading. Before this, `links.rs`
    /// set the heading and moved on, so `## Figures ![map](img/x.png)`
    /// produced no node, no edge and no warning (8 pictures in the Petrel
    /// corpus, found at P14).
    #[test]
    fn a_heading_line_states_its_own_links_and_pictures() {
        let got = extract(
            concat!(
                "## Gallery ![in a heading](img/x.png) beside [[atlas]]\n",
                "Below it: [[bob]].\n",
            ),
            "",
            &vault(),
        );
        assert_eq!(
            attach(&got),
            vec![(
                "img/x.png",
                Some("in a heading"),
                Some("Gallery ![in a heading](img/x.png) beside [[atlas]]")
            )],
            "the picture is referenced, and its section is the heading it sits in"
        );
        let links: Vec<(&str, Vec<(&str, &str)>)> = got
            .links
            .iter()
            .map(|l| (l.target.as_str(), props_of(l)))
            .collect();
        assert_eq!(
            links,
            vec![
                (
                    "atlas",
                    vec![(
                        "section",
                        "Gallery ![in a heading](img/x.png) beside [[atlas]]"
                    )]
                ),
                (
                    "bob",
                    vec![(
                        "section",
                        "Gallery ![in a heading](img/x.png) beside [[atlas]]"
                    )]
                ),
            ],
            "one section string for the heading's own link and for the line \
             below it — two values would split one section into two edge groups"
        );
    }

    /// The same for the other two spellings, plus the edge-type ladder and the
    /// §9 path check, each of which the heading line used to skip.
    #[test]
    fn a_heading_lines_markdown_link_takes_the_headings_own_edge_type() {
        let got = extract(
            "## Related work, see [Alice](people/alice.md)\n",
            "",
            &vault(),
        );
        assert_eq!(got.links.len(), 1);
        assert_eq!(got.links[0].target, "people/alice");
        assert_eq!(
            got.links[0].conn_type, "RELATED",
            "the heading types the link written on it, as it types the ones below"
        );

        let escaping = extract("## See ![map](../../etc/passwd.png)\n", "", &vault());
        assert_eq!(
            escaping.path_errors,
            vec!["`../../etc/passwd.png` escapes the vault root".to_string()],
            "a reference on a heading meets §9 like any other"
        );
    }

    /// `okf`/`loose` gain the links a heading states and nothing else: the
    /// three vault-only reads stay off.
    #[test]
    fn a_heading_line_is_scanned_in_every_dialect() {
        for dialect in [Dialect::Okf, Dialect::Loose] {
            let got = extract(
                "## See [Alice](people/alice.md) #tag ![map](img/x.png)\n",
                "",
                &Profile::for_dialect(dialect),
            );
            assert_eq!(
                got.links
                    .iter()
                    .map(|l| l.target.as_str())
                    .collect::<Vec<_>>(),
                vec!["people/alice"],
                "{dialect:?} reads the link and drops the image, as it does in prose"
            );
            assert!(got.attachments.is_empty(), "{dialect:?}");
            assert!(got.tags.is_empty(), "{dialect:?}");
            assert!(got.links[0].props.is_empty(), "{dialect:?}");
        }
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

    // ---- vault attachments (VAULT.md §6.1) ----

    fn attach(got: &Extraction) -> Vec<(&str, Option<&str>, Option<&str>)> {
        got.attachments
            .iter()
            .map(|a| (a.target.as_str(), a.alt.as_deref(), a.section.as_deref()))
            .collect()
    }

    #[test]
    fn vault_captures_all_three_attachment_spellings() {
        let got = extract(
            concat!(
                "![](img/bare.png) and ![[plain.png]]\n",
                "## Figures\n",
                "![Fault map](../img/faults.png) then ![[diagram.png|A diagram]]\n",
                "and ![  ](img/blank.png) has no alt, ![[notes/deep.md]] is a note,\n",
                "![[nameless]] is a note too, and ![remote](https://ex.com/x.png)\n",
                "and ![frag](img/frag.png#page=2) drops its fragment.\n",
                "![doc](notes/other.md) and ![dir](subdir) are neither.\n",
            ),
            "notes",
            &vault(),
        );
        assert_eq!(
            attach(&got),
            vec![
                ("img/bare.png", None, None),
                ("plain.png", None, None),
                ("../img/faults.png", Some("Fault map"), Some("Figures")),
                ("diagram.png", Some("A diagram"), Some("Figures")),
                ("img/blank.png", None, Some("Figures")),
                ("img/frag.png", Some("frag"), Some("Figures")),
            ],
            "an external URL, a `.md` target and an extension-less one are not \
             attachments — in either spelling"
        );
        assert!(
            !got.links.iter().any(|l| l.target.contains("other")),
            "and `![doc](notes/other.md)` is not a link either: the `!` still \
             disqualifies it"
        );
        let embeds: Vec<&str> = got
            .links
            .iter()
            .filter(|l| l.conn_type == EMBEDS_CONN_TYPE)
            .map(|l| l.target.as_str())
            .collect();
        assert_eq!(
            embeds,
            vec!["notes/deep", "nameless"],
            "the note embeds on those lines are still links"
        );
    }

    /// A plain `[text](file.ext)` link names a file the vault holds, and the
    /// only thing a vault can do with one is the thing it does with
    /// `![…](…)`: an `Image`/`Attachment` node and its edge (VAULT.md §6.1).
    /// Before this it resolved to nothing at all — no node, no edge, no
    /// warning — which is how 48 download links (`.rmspy`, `.plugin`, `.zip`)
    /// left the P16 probe's corpus silently.
    #[test]
    fn vault_plain_link_to_a_file_is_an_attachment_reference() {
        let got = extract(
            concat!(
                "## Downloads\n",
                "[The handbook](../img/handbook.pdf) and [a map](../img/faults.png),\n",
                "[back to top](#downloads) and [the note](other.md) are not,\n",
                "and neither are [mail](mailto:a@b.com), [dir](sub/) or\n",
                "[site](https://example.com/x.zip).\n",
            ),
            "notes",
            &vault(),
        );
        assert_eq!(
            attach(&got),
            vec![
                (
                    "../img/handbook.pdf",
                    Some("The handbook"),
                    Some("Downloads")
                ),
                ("../img/faults.png", Some("a map"), Some("Downloads")),
            ],
            "the link text is the reference's alt, exactly as `![alt](…)`'s is"
        );
        let links: Vec<&str> = got.links.iter().map(|l| l.target.as_str()).collect();
        assert_eq!(
            links,
            vec!["notes/other", "https://example.com/x.zip"],
            "a `.md` target is still a note link and an http one still a Source"
        );
        assert!(
            got.path_errors.is_empty(),
            "an in-page anchor, a mailto and a directory link are silent no-ops"
        );
    }

    #[test]
    fn okf_and_loose_drop_a_plain_link_to_a_file() {
        for dialect in [Dialect::Okf, Dialect::Loose] {
            let got = extract("[handbook](img/h.pdf)", "", &Profile::for_dialect(dialect));
            assert!(
                got.attachments.is_empty() && got.links.is_empty(),
                "{dialect:?} reads no attachments, so the link stays dropped"
            );
        }
    }

    /// VAULT.md §5.1: `\[\[` is the escape, and it is the *regex* that gives
    /// it — two `[` separated by a backslash are not a wikilink opener. Pinned
    /// because the spec now promises it to converter authors.
    #[test]
    fn an_escaped_wikilink_is_literal_text() {
        let got = extract(r"Write \[\[atlas]] to mean the literal text.", "", &vault());
        assert!(
            got.links.is_empty() && got.attachments.is_empty(),
            "an escaped wikilink names nothing"
        );
    }

    /// VAULT.md §5.1: indented code is **not** exempt — only a fence and a
    /// comment are. The block tree marks an `IndentedCode` block, and
    /// `scan_regions` deliberately keeps scanning it: honouring CommonMark's
    /// rule here would swallow every list continuation line, which is where a
    /// converter writes most of its links.
    #[test]
    fn an_indented_code_block_is_scanned_like_prose() {
        let got = extract(
            "## Example\n\n    [[atlas]] and ![m](img/x.png)\n",
            "",
            &vault(),
        );
        assert_eq!(
            got.links
                .iter()
                .map(|l| l.target.as_str())
                .collect::<Vec<_>>(),
            vec!["atlas"],
            "four-space indentation exempts nothing; fence it instead"
        );
        assert_eq!(
            attach(&got),
            vec![("img/x.png", Some("m"), Some("Example"))]
        );
    }

    /// VAULT.md §1.4/§5.1: an HTML tag carries no meaning, but a line holding
    /// one is still prose and the markdown syntax written inside it is read.
    #[test]
    fn markdown_syntax_inside_an_html_block_is_scanned() {
        let got = extract(
            concat!(
                "## Gallery\n",
                "<div><a href=\"bob.md\">bob</a> <img src=\"img/y.png\"></div>\n",
                "<div>[[atlas]] and ![m](img/x.png)</div>\n",
            ),
            "",
            &vault(),
        );
        assert_eq!(
            got.links
                .iter()
                .map(|l| l.target.as_str())
                .collect::<Vec<_>>(),
            vec!["atlas"],
            "`<a href>` is not a link; the wikilink beside it is"
        );
        assert_eq!(
            attach(&got),
            vec![("img/x.png", Some("m"), Some("Gallery"))],
            "`<img src>` is not a reference; the markdown image beside it is"
        );
    }

    #[test]
    fn okf_and_loose_capture_no_attachments() {
        for dialect in [Dialect::Okf, Dialect::Loose] {
            let got = extract(
                "![alt](img/x.png) and ![[y.png]]",
                "",
                &Profile::for_dialect(dialect),
            );
            assert!(
                got.attachments.is_empty(),
                "{dialect:?} still drops every image reference"
            );
            assert!(got.links.is_empty(), "{dialect:?} mints no link either");
        }
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

    #[test]
    fn percent_decode_leaves_a_literal_percent_alone() {
        assert_eq!(percent_decode("img/a%20b.png"), "img/a b.png");
        assert_eq!(percent_decode("img/50%25.png"), "img/50%.png");
        assert_eq!(percent_decode("notes/r%C3%A5data.md"), "notes/rådata.md");
        // Not an escape: nothing to decode, so the name survives as written.
        assert_eq!(percent_decode("img/100%.png"), "img/100%.png");
        assert_eq!(percent_decode("img/%zz.png"), "img/%zz.png");
        assert_eq!(percent_decode("img/a%2.png"), "img/a%2.png");
        // `%FF` alone is not UTF-8; decoding it would corrupt the target, so
        // the whole string is left as written.
        assert_eq!(percent_decode("img/%FF.png"), "img/%FF.png");
        assert!(matches!(percent_decode("img/plain.png"), Cow::Borrowed(_)));
    }

    #[test]
    fn a_rooted_target_is_vault_relative_not_absolute() {
        // VAULT.md §6.2: a leading `/` means the vault root, which is what
        // makes a vault relocatable — it is never a §9 absolute path.
        assert_eq!(path_error("/img/x.png", "notes"), None);
        assert!(path_error("/../img/x.png", "notes").is_some());
        assert_eq!(path_error("../img/x.png", "notes"), None);
        assert_eq!(path_error("../../img/x.png", "notes/deep"), None);
        assert!(path_error("../../../img/x.png", "notes/deep").is_some());
        assert!(path_error("\\\\server\\share\\x.png", "").is_some());
        assert!(path_error("D:\\vault\\x.md", "").is_some());
        assert_eq!(
            path_error("C:notes/x.md", ""),
            None,
            "no separator, no drive"
        );
    }

    /// The pre-P2 scanner toggled one `in_fence` boolean on **any** fence
    /// line, so a `~~~` written inside a ``` block turned scanning back on and
    /// the rest of the code block became links. The tree closes the block at
    /// its own delimiter.
    #[test]
    fn a_tilde_line_inside_a_backtick_fence_does_not_resume_scanning() {
        let got = extract(
            concat!(
                "```text\n",
                "~~~\n",
                "[[atlas]] and ![m](img/x.png) and #buried\n",
                "```\n",
                "[real](/notes/z.md)\n",
            ),
            "",
            &vault(),
        );
        assert_eq!(
            got.links
                .iter()
                .map(|l| l.target.as_str())
                .collect::<Vec<_>>(),
            vec!["notes/z"],
            "everything between the ``` delimiters is code"
        );
        assert!(attach(&got).is_empty());
        assert!(got.tags.is_empty());
    }

    /// `[![alt](thumb)](full)` — a thumbnail linking to the full picture. The
    /// old regex stopped its text at the inner `]`, matched `[![alt](thumb)`
    /// and never saw `full` at all (217 of these in the RMS corpus).
    #[test]
    fn linked_image_yields_the_thumbnail_and_the_outer_link() {
        let got = extract(
            "## Figures\n\n[![Fault map](img/thumb.png)](img/faults.png)\n",
            "",
            &vault(),
        );
        assert_eq!(
            attach(&got),
            vec![
                ("img/thumb.png", Some("Fault map"), Some("Figures")),
                ("img/faults.png", Some("Fault map"), Some("Figures")),
            ],
            "both halves are references; the outer one wears the inner alt"
        );
        assert!(got.links.is_empty(), "neither half is a note");
    }

    /// The same shape with a `.md` outer target: the picture is a reference and
    /// the link around it is an ordinary note link.
    #[test]
    fn a_linked_image_pointing_at_a_note_is_still_a_link() {
        let got = extract("[![map](img/x.png)](/notes/atlas.md)\n", "", &vault());
        assert_eq!(
            got.links
                .iter()
                .map(|l| l.target.as_str())
                .collect::<Vec<_>>(),
            vec!["notes/atlas"]
        );
        assert_eq!(attach(&got), vec![("img/x.png", Some("map"), None)]);
    }

    /// A link text hard-wrapped by an editor is one link, and two paragraphs'
    /// brackets are never run together into one.
    #[test]
    fn link_text_may_wrap_a_line_but_never_a_paragraph() {
        let got = extract(
            "see the [Binary\nExtensions](/notes/atlas.md) page\n",
            "",
            &vault(),
        );
        assert_eq!(
            got.links
                .iter()
                .map(|l| l.target.as_str())
                .collect::<Vec<_>>(),
            vec!["notes/atlas"]
        );

        let across = extract("a [open\n\nclose](/notes/atlas.md) b\n", "", &vault());
        assert!(
            across.links.is_empty(),
            "a blank line ends the paragraph, and the bracket with it"
        );
    }

    /// VAULT.md §5.7: a comment's text is never scanned — inline or spanning
    /// lines — and it is the only region besides a fence with that property.
    #[test]
    fn a_comment_hides_links_tags_and_attachments() {
        let inline = extract(
            "## Notes\n\nkeep [[atlas]] %%drop [[ghost]] ![g](img/g.png) #hidden%% here\n",
            "",
            &vault(),
        );
        assert_eq!(
            inline
                .links
                .iter()
                .map(|l| l.target.as_str())
                .collect::<Vec<_>>(),
            vec!["atlas"]
        );
        assert!(attach(&inline).is_empty());
        assert!(inline.tags.is_empty());

        let block = extract(
            "%%\n[[ghost]] ![g](img/g.png) #hidden\n%%\n\n[[atlas]] #kept\n",
            "",
            &vault(),
        );
        assert_eq!(
            block
                .links
                .iter()
                .map(|l| l.target.as_str())
                .collect::<Vec<_>>(),
            vec!["atlas"]
        );
        assert!(attach(&block).is_empty());
        assert_eq!(block.tags, vec!["kept"]);

        // …and a commented-out heading names no section and titles nothing:
        // CommonMark reports it, but VAULT.md §5.7 says nothing is read out of
        // a comment.
        let heading = extract("%%\n# Hidden\n%%\n\n[[atlas]]\n", "", &vault());
        assert_eq!(props_of(&heading.links[0]), Vec::new());
        assert_eq!(crate::okf::first_heading("%%\n# Hidden\n%%\n"), None);
    }

    /// VAULT.md §5.1: an inline code span is rendered literally, in every
    /// dialect, so nothing written inside one is a link, a tag or an
    /// attachment. A cold agent's converter minted eleven stub notes from
    /// Python subscripts a page had written as code.
    #[test]
    fn a_code_span_hides_links_tags_and_attachments() {
        let got = extract(
            "keep [[atlas]] `[[ghost]] ![g](img/g.png) [t](x.md) #hidden` here #kept\n",
            "",
            &vault(),
        );
        assert_eq!(
            got.links
                .iter()
                .map(|l| l.target.as_str())
                .collect::<Vec<_>>(),
            vec!["atlas"]
        );
        assert!(attach(&got).is_empty());
        assert_eq!(got.tags, vec!["kept"]);

        // A code span may be opened on one line and closed on the next, which
        // the line-local tag mask cannot see and the block tree can.
        let wrapped = extract("`[[ghost]]\nstill code` then [[atlas]]\n", "", &vault());
        assert_eq!(
            wrapped
                .links
                .iter()
                .map(|l| l.target.as_str())
                .collect::<Vec<_>>(),
            vec!["atlas"]
        );

        // CommonMark, not an Obsidian rule: the okf dialect reads a code span
        // the same way.
        let okf = extract_links(
            "see `[t](/tables/x.md)` and [u](/tables/y.md)",
            "",
            Dialect::Okf,
        );
        assert_eq!(
            okf.iter().map(|l| l.target.as_str()).collect::<Vec<_>>(),
            vec!["tables/y"]
        );
    }

    /// The mask hides what is written *inside* a span, never the constructs
    /// written around one: `[`file.md`](file.md)` is the spelling a generated
    /// help corpus uses thirteen thousand times, and cutting the span out of
    /// the scan the way a fence is cut would lose every one of those links —
    /// along with the author's own display text, which is read from the
    /// unmasked body at the same offsets.
    #[test]
    fn a_code_span_can_still_be_a_links_display_text() {
        let got = extract(
            "see [`atlas.md`](atlas.md) and [[beta|`inline`]]\n",
            "",
            &vault(),
        );
        assert_eq!(
            got.links
                .iter()
                .map(|l| l.target.as_str())
                .collect::<Vec<_>>(),
            vec!["atlas", "beta"]
        );

        // A bracket inside the span is masked for the match and restored for
        // the text: the link survives and its label is what was written.
        let subscript = extract("see [the `rows[0]` case](atlas.md)\n", "", &vault());
        assert_eq!(subscript.links.len(), 1);
        assert_eq!(subscript.links[0].target, "atlas");

        // An image whose alt is code is still one reference, alt intact.
        let picture = extract("![a `b[0]` c](img/x.png)\n", "", &vault());
        assert_eq!(
            picture
                .attachments
                .iter()
                .map(|a| (a.target.as_str(), a.alt.as_deref().unwrap_or("")))
                .collect::<Vec<_>>(),
            vec![("img/x.png", "a `b[0]` c")]
        );
    }

    /// A `#` glued to a closing backtick is glued, not preceded by whitespace,
    /// so it is not a tag. That survives the mask because the mask leaves the
    /// backticks themselves alone and writes NUL, not a space: either choice
    /// reversed and the glued `#` reads as a tag.
    #[test]
    fn a_hash_glued_to_a_code_span_is_still_not_a_tag() {
        let got = extract("`code`#glued and #free\n", "", &vault());
        assert_eq!(got.tags, vec!["free"]);
    }

    /// A setext heading was invisible to the line scanner, so everything under
    /// one carried no `section` at all. One heading model, one answer.
    #[test]
    fn a_setext_heading_names_the_section_below_it() {
        let got = extract(
            "Related work\n============\n\nsee [[atlas]] and ![m](img/x.png)\n",
            "",
            &vault(),
        );
        assert_eq!(props_of(&got.links[0]), vec![("section", "Related work")]);
        assert_eq!(
            attach(&got),
            vec![("img/x.png", Some("m"), Some("Related work"))]
        );
        assert_eq!(
            got.links[0].conn_type, "RELATED",
            "the heading ladder reads a setext heading too"
        );
    }

    /// Every `section` a body's references carry must name a heading the block
    /// tree actually holds — the two passes cannot disagree, because there is
    /// only one of them.
    #[test]
    fn every_sections_value_names_a_heading_of_the_same_tree() {
        let bodies = [
            "# Joins\n[x](/tables/y.md)\n## Deep\nsee [[Alice]]\n",
            "Setext\n------\n\n![m](img/x.png)\n\n### #\n[[atlas]]\n",
            "no heading at all: [[atlas]]\n",
            "## Figures\n\n| a | b |\n|---|---|\n| [[atlas]] | ![m](img/x.png) |\n",
            "## Steps\n\n- one [[atlas]]\n  - two ![m](img/x.png)\n",
            "## Quote\n\n> [!note] Title\n> see [[atlas]]\n",
        ];
        for body in bodies {
            let tree = crate::okf::structure::parse_blocks(body);
            let got = extract(body, "", &vault());
            let headings: Vec<&str> = tree.headings.iter().map(|h| h.text.as_str()).collect();
            let sections = got
                .links
                .iter()
                .flat_map(|l| l.props.iter())
                .filter(|(k, _)| k == "section")
                .map(|(_, v)| match v {
                    Value::String(s) => s.clone(),
                    other => panic!("section is a string, got {other:?}"),
                })
                .chain(got.attachments.iter().filter_map(|a| a.section.clone()));
            for section in sections {
                assert!(
                    headings.contains(&section.as_str()),
                    "{body:?}: section {section:?} is not a heading of the tree {headings:?}"
                );
            }
        }
    }

    /// A vault that declares `structure:` — the one switch that turns a
    /// wikilink's display text into the edge's `label` (VAULT.md §5.4, §7.1).
    fn structured_vault() -> Profile {
        let mut profile = vault();
        profile.structure = Some(crate::okf::structure::StructureProfile::default());
        profile
    }

    #[test]
    fn display_text_is_the_edge_label_only_where_structure_is_declared() {
        let body = "# Notes\n\nSee [[atlas|the atlas]] and [[atlas#Maps|its maps]].\n";
        let plain = extract(body, "", &vault()).links;
        assert_eq!(
            plain.iter().map(props_of).collect::<Vec<_>>(),
            vec![
                vec![("section", "Notes")],
                vec![("section", "Notes"), ("anchor", "Maps")],
            ],
            "without `structure:` the display text is dropped, as it always was"
        );
        let structured = extract(body, "", &structured_vault()).links;
        assert_eq!(
            structured.iter().map(props_of).collect::<Vec<_>>(),
            vec![
                vec![("section", "Notes"), ("label", "the atlas")],
                vec![
                    ("section", "Notes"),
                    ("anchor", "Maps"),
                    ("label", "its maps")
                ],
            ]
        );
        // A markdown link's text is prose around a path, not a display name.
        let markdown = extract("[the atlas](atlas.md)\n", "", &structured_vault()).links;
        assert_eq!(
            markdown.iter().map(props_of).collect::<Vec<_>>(),
            vec![vec![]]
        );
    }

    /// Obsidian's `\|` is the pipe a wikilink writes inside a table cell. The
    /// regex reads the pipe as the separator it is, so the backslash is left
    /// on the target — and a corpus of tables resolved to notes named `Usage\`.
    #[test]
    fn an_escaped_pipe_in_a_table_cell_names_the_note_and_keeps_its_text() {
        let body = "| topic | guide |\n|---|---|\n| chunking | [[Usage\\|the guide]] |\n";
        let got = extract(body, "", &structured_vault()).links;
        assert_eq!(
            got.iter()
                .map(|l| (l.target.as_str(), props_of(l)))
                .collect::<Vec<_>>(),
            vec![("Usage", vec![("label", "the guide")])]
        );
        // The same escape wherever a wikilink is read, including an anchored
        // one and the frontmatter typed-edge rule (VAULT.md §4.3).
        let anchored = extract(
            "| a | [[Usage#Sub\\|text]] |\n|---|---|\n| b | c |\n",
            "",
            &vault(),
        )
        .links;
        assert_eq!(
            anchored
                .iter()
                .map(|l| (l.target.as_str(), props_of(l)))
                .collect::<Vec<_>>(),
            vec![("Usage", vec![("anchor", "Sub")])]
        );
        assert_eq!(
            wikilink_targets(&Value::String("[[Usage\\|the guide]]".to_string())),
            Some(vec!["Usage".to_string()])
        );
    }
}
