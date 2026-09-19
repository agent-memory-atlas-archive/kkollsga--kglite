//! Markdown link extraction with the edge-type inference ladder.
//!
//! A link from concept A to concept B becomes a directed edge. OKF links are
//! untyped (the relationship lives in prose), so the connection type is inferred
//! in four tiers, most-specific first:
//!  0. a `{type}` written straight after a wikilink's `]]`
//!     (`[[Customers]]{joins-with}`) — vault only,
//!  1. an explicit link **title** that looks like an edge type
//!     (`[customers](/tables/customers.md "JOINS_WITH")`),
//!  2. the enclosing **section header** (`# Joins` → `JOINS_WITH`,
//!     `# Citations` → `CITES`, …),
//!  3. the generic [`DEFAULT_CONN_TYPE`] (`LINKS_TO`).
//!
//! Rungs 0 and 1 are the two link spellings' own: a wikilink has no title and
//! a markdown link takes no brace.
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
//! `anchor` edge properties, an `EMBEDS` edge for `![[Note]]`, rung 0's
//! `{type}` suffix, inline `#tag` extraction, and `![alt](x.png)` /
//! `![[x.png]]` attachment references
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
    /// Inline `#tag` names in first-use order, deduplicated — the hub's view
    /// of the body (VAULT.md §5.5). Always empty unless
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
    /// Build-report warnings this body wrote: a `{…}` written against a
    /// wikilink that names no edge type (VAULT.md §5.3). The caller prefixes
    /// the note's own path, as it does for [`Extraction::path_errors`].
    pub(crate) warnings: Vec<String>,
    /// **Every** inline `#tag` occurrence with the byte range of its own
    /// token, in body order — what [`Extraction::tags`] is deduplicated from.
    /// A tag is attributed to the innermost derived node whose range contains
    /// it (VAULT.md §7.1), and a name alone cannot say which node that is.
    pub(crate) tag_spans: Vec<TagRef>,
}

/// One inline `#tag` where it was written.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TagRef {
    /// The tag name, `#` excluded, exactly as [`Extraction::tags`] spells it.
    pub name: String,
    /// The `#tag` token itself, `#` included, as a range into the body.
    pub range: std::ops::Range<usize>,
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
            // `split_inclusive` and not `lines`, because the running offset
            // has to count the newline each line ends with.
            let mut at = range.start;
            for line in text.masked.split_inclusive('\n') {
                scan_tags(line, at, &mut out.tag_spans);
                at += line.len();
            }
        }
        region.scan(text, &mut out);
    }
    // The hub's list is the span list deduplicated, so the two can never
    // disagree about what the body says (VAULT.md §5.5).
    for tag in &out.tag_spans {
        if !out.tags.contains(&tag.name) {
            out.tags.push(tag.name.clone());
        }
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

/// What a wikilink's `{type}` suffix yielded (VAULT.md §5.3 rung 0).
///
/// [`TypeSuffix::Absent`] and [`TypeSuffix::Unusable`] both leave the brace as
/// prose and differ only in whether the author was plainly reaching for a
/// type: a space before the brace, or no closing one, is prose somebody wrote
/// beside a link, while `{see also}` is a suffix that failed and says so in
/// the build report.
enum TypeSuffix<'t> {
    Absent,
    Named(String),
    Unusable(&'t str),
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
            match self.type_suffix(text, m.end()) {
                TypeSuffix::Named(conn) => conn,
                TypeSuffix::Unusable(suffix) => {
                    // Once per distinct spelling, as a path error is: the same
                    // mistake written in five places is one thing to fix.
                    let warning = format!(
                        "`{}{suffix}`: a link type holds no whitespace and must normalise to a \
                         name that does not start with a digit — the brace is left as prose",
                        text.of(m)
                    );
                    if !out.warnings.contains(&warning) {
                        out.warnings.push(warning);
                    }
                    self.untyped_conn()
                }
                TypeSuffix::Absent => self.untyped_conn(),
            }
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

    /// The edge type a wikilink with no `{type}` of its own takes: the
    /// enclosing heading's, else [`DEFAULT_CONN_TYPE`] (VAULT.md §5.3 rungs
    /// 2–4).
    fn untyped_conn(&self) -> String {
        self.heading_conn
            .clone()
            .unwrap_or_else(|| DEFAULT_CONN_TYPE.to_string())
    }

    /// The `{type}` suffix written against the wikilink that ends at `at`
    /// (VAULT.md §5.3 rung 0).
    ///
    /// The extent is read from the masked text, so a `}` written inside an
    /// inline code span closes nothing; the name itself is the author's own
    /// bytes, which is what the warning has to quote back.
    fn type_suffix<'t>(&self, text: RegionText<'t>, at: usize) -> TypeSuffix<'t> {
        if !self.profile.typed_links {
            return TypeSuffix::Absent;
        }
        let Some(rest) = text.masked.get(at..).filter(|r| r.starts_with('{')) else {
            return TypeSuffix::Absent;
        };
        // A brace with no `}` before the line ends is not a suffix at all: a
        // lone `{` is ordinary prose, and warning about one would fire on
        // every note that writes a brace after a link.
        let line = &rest[..rest.find('\n').unwrap_or(rest.len())];
        let Some(close) = line.find('}') else {
            return TypeSuffix::Absent;
        };
        let suffix = &text.raw[at..at + close + 1];
        let inner = &text.raw[at + 1..at + close];
        let conn = upper_snake(inner);
        if inner.contains(char::is_whitespace)
            || conn.is_empty()
            || conn.starts_with(|c: char| c.is_ascii_digit())
        {
            return TypeSuffix::Unusable(suffix);
        }
        TypeSuffix::Named(conn)
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

/// Collect inline `#tag` occurrences from one line (VAULT.md §5.5). A tag
/// starts at a `#` that opens the line or follows whitespace, so a URL
/// fragment (`…/x#frag`) and a wikilink anchor (`[[Note#Sec]]`) are not tags;
/// inline code spans are masked out first, and fenced blocks never reach here.
///
/// `base` is the line's own offset in the body, so every range collected here
/// indexes the body and not the line.
fn scan_tags(line: &str, base: usize, out: &mut Vec<TagRef>) {
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
        out.push(TagRef {
            range: base + at..base + cursor,
            name,
        });
    }
}

/// Replace inline code spans (and their backticks) with NUL, so a `#` inside
/// one is invisible to [`scan_tags`] and a `#` glued to a span's closing
/// backtick still counts as glued rather than as following whitespace.
///
/// One NUL **per byte**, not per character: [`scan_tags`] reports the byte
/// range of every tag it finds, so a mask that shortened a span holding a
/// multi-byte character would move every tag after it.
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
        for masked in &chars[open..end] {
            for _ in 0..masked.len_utf8() {
                out.push('\u{0}');
            }
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
        // `get`, not a slice: byte 5 lands inside a character whenever the
        // target opens with one, and slicing there panics the whole build.
        || target
            .get(..5)
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file:"))
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
#[path = "links_tests.rs"]
mod links_tests;
