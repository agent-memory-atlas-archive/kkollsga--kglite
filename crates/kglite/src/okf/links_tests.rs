//! Link-extraction cases: the edge-type ladder, the two link spellings,
//! inline tags, attachment references and the §9 path checks.

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
    let body = "# Citations\n[site](https://example.com) and [dir](subdir/) and [doc](./pic.png)";
    let links = extract_links(body, "", Dialect::Okf);
    // the http link becomes an external (Source) link; dir/ and .png are skipped
    assert_eq!(links.len(), 1);
    assert!(links[0].is_external);
    assert_eq!(links[0].target, "https://example.com");
    assert_eq!(links[0].conn_type, "CITES");
}

#[test]
fn images_and_fenced_code_skipped() {
    let body = "![alt](/tables/x.md)\n```sql\nSELECT [a](/tables/y.md)\n```\n[real](/tables/z.md)";
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

/// A target is arbitrary text, so the `file:` scheme test cannot slice it
/// at a fixed byte: one page of a converted help corpus links to a note
/// whose title carries an em dash, byte 5 landed in the middle of it, and
/// every thread reading that vault panicked.
#[test]
fn a_target_whose_fifth_byte_is_inside_a_character_is_read_not_sliced() {
    let got = extract(
        "see [[RMS_—_RMS_API_1.13_documentation]] and [x](a_—_b.md)\n",
        "",
        &vault(),
    );
    assert_eq!(
        got.links
            .iter()
            .map(|l| l.target.as_str())
            .collect::<Vec<_>>(),
        vec!["RMS_—_RMS_API_1.13_documentation", "a_—_b"]
    );
    assert!(got.path_errors.is_empty());
    // …and the scheme it is testing for is still refused.
    let refused = extract("see [x](FILE:///etc/passwd)\n", "", &vault());
    assert_eq!(refused.path_errors.len(), 1);
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

    // A code span may be opened on one line and closed on the next. The
    // line-local tag mask cannot see that — each line carries one
    // unmatched backtick, which masks only itself — so the second line's
    // `#hidden` is a tag to it and not to the block tree.
    let wrapped = extract(
        "`[[ghost]]\n#hidden still code` then [[atlas]] #kept\n",
        "",
        &vault(),
    );
    assert_eq!(
        wrapped
            .links
            .iter()
            .map(|l| l.target.as_str())
            .collect::<Vec<_>>(),
        vec!["atlas"]
    );
    assert_eq!(wrapped.tags, vec!["kept"]);

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
/// so it is not a tag (VAULT.md §5.5). The mask must not change that, and
/// it cannot while it leaves the backticks alone and writes a character
/// that is not whitespace — a mask that did both would turn this line's
/// `#glued` into a tag. This is what pins the pair.
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

// ── typed inline links, `[[Target]]{type}` (VAULT.md §5.3 rung 0) ──────────

/// `(target, conn type)` of every link one body states.
fn typed(body: &str) -> Vec<(String, String)> {
    extract(body, "", &vault())
        .links
        .into_iter()
        .map(|l| (l.target, l.conn_type))
        .collect()
}

#[test]
fn a_brace_after_a_wikilink_names_the_edge_type() {
    assert_eq!(
        typed("see [[Atlas]]{see-also} and [[Api]]{api}\n"),
        vec![
            ("Atlas".to_string(), "SEE_ALSO".to_string()),
            ("Api".to_string(), "API".to_string())
        ]
    );
    assert_eq!(
        typed("[[Atlas]]{SEE_ALSO}\n"),
        typed("[[Atlas]]{see-also}\n"),
        "the suffix normalises, so the two spellings are one type"
    );
}

#[test]
fn a_typed_link_outranks_the_heading_it_sits_under() {
    assert_eq!(
        typed("## Related work\n\n[[Atlas]]{see-also} and [[Api]]\n"),
        vec![
            ("Atlas".to_string(), "SEE_ALSO".to_string()),
            ("Api".to_string(), "RELATED".to_string())
        ],
        "rung 0 beats the built-in ladder, and only for the link that wrote it"
    );
    let mut profile = vault();
    profile
        .heading_edges
        .insert("Related work".to_string(), "RELATED_TO".to_string());
    let got = extract("## Related work\n\n[[Atlas]]{see-also}\n", "", &profile);
    assert_eq!(
        got.links[0].conn_type, "SEE_ALSO",
        "and it beats `heading_edges:` too"
    );
}

#[test]
fn a_typed_link_keeps_its_display_text_and_its_anchor() {
    let got = extract(
        "## Notes\n\n[[Atlas#Overview|the atlas]]{see-also}\n",
        "",
        &structured_vault(),
    );
    assert_eq!(got.links[0].target, "Atlas");
    assert_eq!(got.links[0].conn_type, "SEE_ALSO");
    assert_eq!(
        props_of(&got.links[0]),
        vec![
            ("section", "Notes"),
            ("anchor", "Overview"),
            ("label", "the atlas")
        ]
    );
}

#[test]
fn a_typed_link_is_read_inside_a_table_cell() {
    let got = extract(
        "| topic | link |\n|---|---|\n| a | [[Atlas\\|the atlas]]{see-also} |\n",
        "",
        &structured_vault(),
    );
    assert_eq!(
        got.links
            .iter()
            .map(|l| (l.target.as_str(), l.conn_type.as_str(), props_of(l)))
            .collect::<Vec<_>>(),
        vec![("Atlas", "SEE_ALSO", vec![("label", "the atlas")])],
        "the `\\|` escape belongs to the cell, and the brace still follows the `]]`"
    );
}

#[test]
fn a_brace_that_names_no_type_stays_prose_and_warns() {
    for suffix in ["{}", "{see also}", "{3d}", "{-}"] {
        let body = format!("[[Atlas]]{suffix}\n");
        let got = extract(&body, "", &vault());
        assert_eq!(
            got.links[0].conn_type, "LINKS_TO",
            "`{suffix}` names no type, so the link keeps its default"
        );
        assert_eq!(got.warnings.len(), 1, "`{suffix}`: {:?}", got.warnings);
        assert!(
            got.warnings[0].contains(suffix) && got.warnings[0].contains("[[Atlas]]"),
            "the warning names what was written: {}",
            got.warnings[0]
        );
    }
}

#[test]
fn a_brace_the_link_does_not_touch_is_ordinary_prose() {
    for body in [
        "[[Atlas]] {see-also}\n",
        "[[Atlas]]{see-also\n",
        "[[Atlas]]{see-\nalso}\n",
    ] {
        let got = extract(body, "", &vault());
        assert_eq!(got.links[0].conn_type, "LINKS_TO", "{body:?}");
        assert!(
            got.warnings.is_empty(),
            "a brace that is not attached to the link says nothing about it: {:?}",
            got.warnings
        );
    }
}

#[test]
fn one_mistake_written_twice_is_one_warning() {
    let got = extract(
        "[[Atlas]]{see also} again [[Atlas]]{see also}\n",
        "",
        &vault(),
    );
    assert_eq!(got.warnings.len(), 1, "{:?}", got.warnings);
}

#[test]
fn a_tag_after_a_link_is_a_tag_and_not_a_link_type() {
    let got = extract("[[Atlas]] #see-also\n", "", &vault());
    assert_eq!(got.links[0].conn_type, "LINKS_TO");
    assert_eq!(got.tags, vec!["see-also"], "it is still a tag");
    assert!(got.warnings.is_empty());
}

#[test]
fn an_embed_keeps_embeds_and_the_brace_after_it_is_prose() {
    let got = extract("![[Atlas]]{see-also}\n", "", &vault());
    assert_eq!(
        got.links
            .iter()
            .map(|l| (l.target.as_str(), l.conn_type.as_str()))
            .collect::<Vec<_>>(),
        vec![("Atlas", "EMBEDS")],
        "a transclusion's type is what it is"
    );
    assert!(got.warnings.is_empty());
}

#[test]
fn only_the_vault_dialect_reads_a_typed_link() {
    let got = extract(
        "[[Atlas]]{see-also}\n",
        "",
        &Profile::for_dialect(Dialect::Loose),
    );
    assert_eq!(got.links[0].conn_type, "LINKS_TO");
    assert!(got.warnings.is_empty());
}

// ---- tag offsets (VAULT.md §5.5; the anchor a derived node attaches by) ----

/// `(name, the `#tag` token sliced back out of the body)` for every
/// occurrence, so a wrong range is a wrong string and not a wrong number.
fn tag_spans<'b>(body: &'b str, got: &'b Extraction) -> Vec<(&'b str, &'b str)> {
    got.tag_spans
        .iter()
        .map(|t| (t.name.as_str(), &body[t.range.clone()]))
        .collect()
}

#[test]
fn every_tag_occurrence_carries_the_range_of_its_own_token() {
    let body = concat!(
        "# Heading #inhead\n",
        "A #plain tag and a #kebab-case/nested one.\n",
        "#plain again is the same tag written twice.\n",
    );
    let got = extract(body, "", &vault());
    assert_eq!(
        tag_spans(body, &got),
        vec![
            ("inhead", "#inhead"),
            ("plain", "#plain"),
            ("kebab-case/nested", "#kebab-case/nested"),
            ("plain", "#plain"),
        ],
        "every occurrence, in body order"
    );
    assert_eq!(
        got.tags,
        vec!["inhead", "plain", "kebab-case/nested"],
        "the deduplicated names are unchanged"
    );
}

#[test]
fn tag_ranges_survive_a_table_cell_and_a_callout() {
    let body = concat!(
        "> [!warning] Careful\n",
        "> Tagged #incallout here.\n",
        "\n",
        "| a | b |\n",
        "|---|---|\n",
        "| #incell | plain |\n",
    );
    let got = extract(body, "", &vault());
    assert_eq!(
        tag_spans(body, &got),
        vec![("incallout", "#incallout"), ("incell", "#incell")]
    );
}

/// A code span is masked before the scan, and the mask must be byte-for-byte
/// as long as what it hides — otherwise every offset after a span holding a
/// multi-byte character is wrong.
#[test]
fn a_multibyte_code_span_does_not_shift_the_tags_after_it() {
    let body = "Prose `café #nope` then #after it.\n";
    let got = extract(body, "", &vault());
    assert_eq!(tag_spans(body, &got), vec![("after", "#after")]);
}
