#!/usr/bin/env python3
"""Convert a directory of HTML documents into a KGLite vault (``VAULT.md``).

Dependencies — install them yourself; they are not kglite dependencies::

    pip install beautifulsoup4 markdownify

Usage::

    html_to_vault.py <html-dir> <out-vault> [--toc TOC.json] [--images-dir DIR]
                     [--default-label Article] [--dry-run]

A page is identified by its **route** — its path below ``<html-dir>``, with a
``<dir>/index.html`` naming ``<dir>`` — so a corpus that gives every page its
own directory is one note per page rather than one note called ``index``.
Everything a page names is resolved the way a browser reads it: a ``/``-absolute
``src`` or ``href`` against the source root, a relative one against the page's
own directory, and never by filename alone.

It follows the converter checklist in ``VAULT.md`` §11: one ``.md`` per page;
the folder-note layout (``X.md`` beside ``X/``) mirroring the table of contents
when one is given, else every note flat under ``<out>/<default_label>/``; each
``<meta name=...>`` as a frontmatter key; a durable ``id:``; internal anchors as
``[[wikilinks]]``; the cross-reference block as ``parent:`` keys and a
``## Related topics`` section; ``<img src>`` references kept as written with the
files copied into the vault; and a ``.kglite/vault.yaml`` declaring the label,
the folder-note edge, the hubs, the heading map and the indexes and embed
targets you name.

It also emits the constructs ``VAULT.md`` §13 asks for, because HTML a markdown
converter flattens arrives in the graph as nothing (§13.2):

* an admonition — ``class="admonition note"``, ``class="note tip note_tip"``,
  ``class="versionadded"`` — becomes an Obsidian callout, keeping the source's
  own word for the kind rather than folding it into ``note`` (§5.7);
* a ``<table>`` becomes a **GFM** pipe table, and one under a heading matching
  ``--param-heading`` has its first column renamed ``name`` so a
  ``tables: {under_heading: Parameters, key_column: name}`` rule keys its rows;
* a ``<dl>`` whose ``<dt>`` ids name their own terms — the Sphinx API shape —
  becomes a heading per symbol, so ``key_from_heading:`` can relabel it; any
  other ``<dl>``, including one whose ids are a generator's own anchors,
  becomes ``**term**`` paragraphs;
* a docutils **field list** — ``<th>`` name beside ``<td>`` value, with no
  header row anywhere — is given a synthesised ``Field | Value`` header,
  whatever ``--table-header`` says: its first row is data, so promoting it
  would name the columns after one field and lose it, and leaving the header
  blank names nothing at all;
* a ``[[`` the source itself wrote is escaped with backslashes, because the
  converter mints every wikilink in the output and one that arrives from the
  page is text a reader is meant to see -- an API page's
  ``(e.g., [["Tables", "Table1"]])`` read as a link mints a note named after a
  Python literal (inside ``<code>`` it is left alone: a code span is not
  scanned at all, §5.1);
* a link to an ``id`` **inside** a page — a Sphinx ``<span id=...>``, a section
  ``<div id=...>`` — becomes ``[[Note#Heading#Subheading]]``, the heading path
  that id sits under, so an anchor reaches the section a reader lands on rather
  than the top of the page;
* a ``<pre>`` becomes a fenced block carrying the language its
  ``highlight-<lang>`` / ``language-<lang>`` class names;
* with ``--block-ids``, a ``<p id=...>`` gains the trailing `` ^id`` that makes
  it a citable passage (§5.7, §13.1);
* nested lists keep their nesting, indented four spaces per level.

Two losses are deliberate and worth knowing. A **merged cell** is flattened: a
``colspan`` writes its value once and leaves the cells it spanned empty, and a
``rowspan`` writes its value in its first row only — GFM has no merge, so the
shape survives and the span does not. And a **nested table** inside a cell is
reduced to its text, because a pipe table cannot contain one.

``--emit-structure`` adds the matching ``structure:`` block to the generated
``.kglite/vault.yaml``. It is off by default because ``structure:`` is a hard
compatibility boundary: a kglite that does not know the key fails the build with
an unknown-key error (§7.1), so the block is written only when you are running a
kglite that reads it.

The TOC file is JSON -- ``{"label": ..., "href": ..., "children": [...]}`` --
where ``href`` names an HTML file — as a path, matched to the page whose route
it ends in, so a TOC written relative to its own directory places the corpus
just as one written from the root does — and a node without one is a grouping
folder with no note of its own. The root's own label
names the collection, not a folder inside it.

Nothing is converted: an image is copied byte for byte to its path below the
source root -- so two pages in two directories that both write ``img/logo.png``
stay two pictures -- and one whose type the MCP server does not deliver is
counted so you can convert it at source.

It ends by running ``kglite.okf.validate`` and exits non-zero when the vault has
errors. Warnings do not fail it; ``--no-validate`` skips the pass.
"""

from __future__ import annotations

import argparse
from collections import defaultdict
from dataclasses import dataclass, field, replace
import json
from pathlib import Path, PurePosixPath
import posixpath
import re
import shutil
import sys
import time
from typing import Any, Iterator

from bs4 import BeautifulSoup, NavigableString
from markdownify import MarkdownConverter

# `<meta name=…>` tags describing the generator rather than the document. Names
# are lowercased and whitespace-normalised first, and the Dublin Core namespace
# goes with them.
BOILERPLATE_META = frozenset(
    {"generator", "copyright", "viewport", "robots", "content-type", "content-language", "content-style-type"}
)
# Meta keys whose comma-separated value is a YAML sequence, not one string.
LIST_META = frozenset({"keywords", "tags", "aliases"})
# Value spellings to fold, per meta key — a corpus with typo'd metadata extends
# this, or passes a JSON file of the same shape to `--meta-map`. Lookups are
# lowercased; a value the table does not name is kept exactly as written.
META_NORMALIZERS: dict[str, dict[str, str]] = {}
# Extensions the bundled MCP server delivers as images (VAULT.md §6).
DELIVERABLE_IMAGES = frozenset({".png", ".jpg", ".jpeg", ".gif", ".webp"})
# Direct parents an image may have inside a heading or a table cell and still be
# written out as an image rather than collapsed to its alt text. The match is on
# the image's *direct* parent, so `figure` — the element HTML defines for a
# picture with a caption — has to be named even though `figcaption` is.
INLINE_IMAGE_PARENTS = [
    *(f"h{n}" for n in range(1, 7)),
    "a",
    "b",
    "caption",
    "div",
    "dd",
    "dt",
    "em",
    "figcaption",
    "figure",
    "i",
    "li",
    "p",
    "span",
    "strong",
    "sub",
    "sup",
    "td",
    "th",
]

_YAML_INDICATORS = "-?:,[]{}#&*!|>'\"%@`"
_YAML_BOOLISH = frozenset("true false yes no on off y n null".split())
_DATEISH = re.compile(r"^\d{4}-\d{2}-\d{2}([T ]\d{2}:\d{2}.*)?$")
_UNSAFE_NAME = re.compile(r"[<>:\"/\\|?*\x00-\x1f‘’“”]")

# Classes that mark an element as an admonition. Sphinx writes
# `<div class="admonition note">` and `<div class="versionadded">`; DITA writes
# `<div class="note tip note_tip">`. Extend the list with `--admonition-classes`.
ADMONITION_CLASSES = "admonition,note,warning,tip,important,caution,seealso,versionadded,versionchanged,deprecated"
# `admonition` names the box and `note` is what a source calls one it has no
# better word for, so either loses to a class that says which kind it is.
GENERIC_ADMONITION = frozenset({"admonition", "note"})
# Where an admonition keeps its title: Sphinx, DITA, and DITA's older spelling.
ADMONITION_TITLE = ".admonition-title, .note__title, .notetitle"
# `highlight-python` sits on the wrapper div Sphinx emits; `language-python` on
# the `<code>` a GFM-flavoured generator emits.
CODE_LANGUAGE = re.compile(r"^(?:highlight|language)-([\w+#.-]+)$")
HEADING = re.compile(r"^h[1-6]$")
# Filenames that name their directory rather than themselves: `guide/index.html`
# is the page a reader reaches at `guide/`, so its route is `guide`.
INDEX_STEMS = frozenset({"index"})
# `[[Note#A#B]]` has no escape for any of these (VAULT.md §13.2), so a heading
# carrying one cannot be the anchor half of a link, however it is spelled.
UNLINKABLE_IN_HEADING = frozenset("[]|#")
# A block id is Latin letters, digits and dashes only (VAULT.md §5.7), so
# `<p id="para_1">` names the block `^para-1` and not `^para_1`, which is text.
_BLOCK_ID_JUNK = re.compile(r"[^A-Za-z0-9-]+")
_LIST_BULLET = re.compile(r"(?:[-*+]|\d+\.) +")
# A wikilink's display text inside a table cell. The cell's `|` separators are
# escaped `\|`, which is what Obsidian's own help writes in a table — but
# VAULT.md §5.1 reads a wikilink target up to the first `|` and never unescapes
# one, so `[[Usage\|the guide]]` names a note called `Usage\` and the edge
# dangles. The target is what a link is for, so the cell keeps it and drops the
# display text, which §5.1 stores only where `structure:` is declared.
_CELL_WIKILINK = re.compile(r"\[\[([^\[\]|]+)\|[^\[\]]*\]\]")
# Tags that already stand on their own line, so a `<dd>` holding one needs no
# paragraph wrapper when it is unwrapped.
BLOCK_TAGS = ["blockquote", "div", "dl", "h1", "h2", "h3", "h4", "h5", "h6", "ol", "p", "pre", "table", "ul"]
# Elements an admonition class may sit on.
ADMONITION_TAGS = ("aside", "blockquote", "div", "p", "section")

# Tags are minted from one scratch document; bs4 moves them into any tree.
_SOUP = BeautifulSoup("", "html.parser")


class VaultMarkdown(MarkdownConverter):
    """markdownify, with a list item's continuation indented a fixed four spaces.

    markdownify indents by the width of the item's own bullet, so `1.` nests
    three spaces and `10.` nests four and the indent of a line no longer says
    how deep it is. Four is a multiple of every bullet width and still short of
    the indented-code threshold at each level.
    """

    def convert_li(self, el, text, parent_tags):  # noqa: D102 -- markdownify's own hook
        item = super().convert_li(el, text, parent_tags)
        bullet = _LIST_BULLET.match(item)
        if bullet is None:
            return item
        width = bullet.end()
        first, _, rest = item.partition("\n")
        lines = [first]
        for line in rest.split("\n"):
            indented = line.strip() and not line[:width].strip()
            lines.append("    " + line[width:] if indented else line)
        return "\n".join(lines)


MD_OPTIONS: dict[str, Any] = {
    "heading_style": "ATX",
    "bullets": "-",
    "code_language": "",
    # A wikilink is a note's name, not prose: escaping the `_` in
    # `[[Well_tops]]` spells a note nothing resolves to, and the link silently
    # becomes a dangling stub.
    "escape_underscores": False,
    # markdownify reduces an image inside a heading or a table cell to its alt
    # text. A help corpus puts its button icons in exactly those two places, and
    # a converter that drops a picture has lost the picture.
    "keep_inline_images_in": INLINE_IMAGE_PARENTS,
}


def to_md(html: str) -> str:
    return VaultMarkdown(**MD_OPTIONS).convert(html)


def inner_md(element: Any) -> str:
    """The element's children as markdown; the element's own tag is not read."""
    return to_md("".join(str(child) for child in element.contents))


def tidy(text: str) -> str:
    return re.sub(r"\n{3,}", "\n\n", text).strip("\n")


def term_text(term: Any) -> str:
    """A `<dt>`'s own words.

    No separator: a signature is spelled in adjacent inline spans, and joining
    them with a space writes `func ( a, b )`, which the `key_from_heading:`
    default regex does not match.
    """
    return re.sub(r"\s+", " ", term.get_text()).strip()


def qualified_name(term: Any) -> str:
    """The dotted name a `<dt>`'s id gives its text, or "" when it names something else.

    Sphinx writes `<dt id="pkg.mod.func">func(a, b)</dt>`, so the id is the
    qualified name and the text carries the signature. Every other generator
    writes an id of its own devising — DITA's `GUID-…__DLENTRY_…` — and reading
    one as the heading would replace the term with an anchor, which is what a
    definition list is *for*. So the id is used only when it ends in the term's
    own leading name.
    """
    identifier = (term.get("id") or "").strip()
    name = term_text(term)
    head = name.split("(")[0].strip()
    if not identifier or not head or not (identifier == head or identifier.endswith(f".{head}")):
        return ""
    opened, closed = name.find("("), name.rfind(")")
    return identifier + (name[opened : closed + 1] if 0 <= opened < closed else "")


def heading_text(element: Any) -> str:
    """A heading's text as the markdown will spell it.

    `get_text()` is not it: a heading holding `<strong>` renders as `**Bold**`,
    and an anchor written against the plain words would name a heading the note
    does not have — a VAULT.md §9 warning per link.
    """
    return re.sub(r"\s+", " ", inner_md(element)).strip()


def definition_levels(content: Any, rules: BodyRules) -> list[tuple[Any, int, bool]]:
    """Every `<dl>` in document order, with the heading level its terms take and
    whether those terms are symbols — decided before any of them is rewritten.

    Both halves have to be settled up front. `rewrite_definition_lists` turns a
    `<dt>` into a heading, so asking a later sibling `<dl>` for "the previous
    heading" answers with the *symbol* the list before it just wrote: three
    sibling symbols under one `##` came out as `###`, `####` and `#####`, each
    nested inside the one before it, and the whole page's heading paths with
    them. A `<dl>` inside a `<dd>` is genuinely one deeper, and that is the only
    thing that nests.
    """
    spec: list[tuple[Any, int, bool]] = []
    levels: dict[int, int] = {}
    for definitions in content.find_all("dl"):
        parent = definitions.find_parent("dl")
        if parent is not None and id(parent) in levels:
            level = min(6, levels[id(parent)] + 1)
        else:
            heading = previous_heading(definitions, content)
            level = min(6, int(heading.name[1]) + 1) if heading is not None else 2
        levels[id(definitions)] = level
        spec.append((definitions, level, definition_symbols(definitions, rules)))
    return spec


def definition_symbols(definitions: Any, rules: BodyRules) -> bool:
    """Whether a `<dl>`'s terms are API symbols, and so become headings.

    `auto` asks whether an id *names its own term*, not merely whether there is
    one: DITA gives every `<dt>` a generated id, and reading those as symbols
    turned 275 of the Petrel corpus's GUI labels into headings.
    """
    if rules.dl_mode != "auto":
        return rules.dl_mode == "headings"
    return any(qualified_name(term) for term in definitions.find_all("dt") if term.find_parent("dl") is definitions)


def anchor_paths(content: Any, rules: BodyRules) -> dict[str, list[str]]:
    """Every `id` in a page's body, mapped to the heading path it sits under.

    A generator anchors its cross-references at the element they point to — a
    Sphinx `<span id="…">` before a paragraph, a section `<div id="…">` — and
    the markdown keeps no trace of either. The heading path is what survives the
    conversion, is what `[[Note#A#B]]` addresses, and is what a `structure:`
    build turns into a `Section` node, so it is what an anchor resolves to. An
    id *on* a heading names that heading itself.

    Read from the page before anything rewrites it, and through the same two
    decisions the body transform makes, so both passes name one heading tree.
    """
    spec = definition_levels(content, rules)
    symbols = {id(definitions): (level, is_symbol) for definitions, level, is_symbol in spec}
    stack: list[tuple[int, str]] = []
    paths: dict[str, list[str]] = {}
    for element in content.find_all(True):
        identifier = (element.get("id") or "").strip()
        if HEADING.match(element.name):
            level, text = int(element.name[1]), heading_text(element)
        elif element.name == "dt" and symbols.get(id(element.find_parent("dl")), (0, False))[1]:
            level, text = symbols[id(element.find_parent("dl"))][0], (qualified_name(element) or term_text(element))
        else:
            if identifier:
                paths.setdefault(identifier, [text for _, text in stack])
            continue
        while stack and stack[-1][0] >= level:
            stack.pop()
        stack.append((level, text))
        if identifier:
            paths.setdefault(identifier, [text for _, text in stack])
    return {identifier: path for identifier, path in paths.items() if path}


def previous_heading(element: Any, content: Any) -> Any:
    """The heading a block sits under, or None when it sits above all of them."""
    found = element.find_previous(HEADING)
    if found is None or not (found is content or content in found.parents):
        return None
    return found


def to_route(path: str) -> str:
    """The route a path below the source root names.

    A route is the page's identity: `guide/install.html` is `guide/install`,
    `guide/index.html` and the directory link `guide/` are both `guide`, and the
    root's own `index.html` is `index`. A corpus that gives every page its own
    directory therefore has one route per page, where the filename alone has
    `index` for all of them.
    """
    trimmed = PurePosixPath(path.rstrip("/"))
    if trimmed.suffix:
        parent = str(trimmed.parent)
        trimmed = trimmed.parent if trimmed.stem in INDEX_STEMS and parent != "." else trimmed.with_suffix("")
    return "" if str(trimmed) in (".", "") else str(trimmed)


def resolve_href(href: str, page_dir: str) -> str:
    """One `href` or `src` as a path below the source root, `""` when it leaves it.

    The browser's own rule, which is the only one that holds for a corpus of
    more than one directory: a leading `/` is the source root and everything
    else is relative to the page that wrote it. Resolving by filename instead
    reaches a *different* page whenever two directories carry the same name.
    """
    path = href.split("#")[0].split("?")[0].strip()
    if not path:
        return ""
    joined = path[1:] if path.startswith("/") else posixpath.join(page_dir, path)
    resolved = posixpath.normpath(joined)
    return "" if resolved == "." or resolved.startswith("..") else resolved


@dataclass(frozen=True)
class Placement:
    """Where the TOC puts one page: its directory, its stem, and its parent.

    ``parent`` is the parent entry's ``href`` as the TOC wrote it until
    :func:`place_toc` resolves it, and the parent page's route after.
    """

    directory: str
    stem: str
    depth: int
    parent: str | None


@dataclass
class Page:
    """One source HTML file on its way to becoming one note."""

    source: Path
    #: The source file's path below the source root, and the route it names.
    rel: str
    route: str
    doc_id: str
    explicit_id: bool
    title: str
    meta: dict[str, Any]
    content: Any
    related_files: list[str]
    parent_files: list[str]
    out_path: str = ""
    parents: list[str] = field(default_factory=list)
    toc_depth: int | None = None


def sanitize(name: str, max_len: int = 120) -> str:
    """Turn a heading into a path segment, over VAULT.md §10.2's character set."""
    name = re.sub(r"\s+", "_", _UNSAFE_NAME.sub("", name).strip())
    name = re.sub(r"_+", "_", name).strip("_.")
    return name[:max_len] if name else "untitled"


def layout_toc(node: dict, directory: str = "", parent: str | None = None) -> Iterator[tuple[str, Placement]]:
    """Walk the TOC, yielding ``(href, placement)`` depth-first.

    A node's ``href`` is read as the route it names, not as a filename: a TOC
    over a corpus of ``<page>/index.html`` directories otherwise places every
    one of its entries on the same page.

    A node's children live in the directory named by the node's own stem, so a
    node that has both an ``href`` and children becomes a folder note. Sibling
    stems are made unique here rather than per file, so the directory a child
    lands in and the file its parent becomes never disagree — and the directory's
    own name is taken, so a child labelled like its parent cannot land on
    ``X/X.md``, the second folder-note spelling for a directory ``X.md``
    already speaks for (VAULT.md §2.3 makes declaring both an error).
    """
    used: set[str] = {PurePosixPath(directory).name.lower()} if directory else set()
    for index, child in enumerate(node.get("children", [])):
        href = child.get("href", "") or None
        stem = sanitize(child.get("label", ""))
        if stem.lower() in used:
            stem = f"{stem}_{index}"
        used.add(stem.lower())
        below = posixpath.join(directory, stem) if directory else stem
        if href:
            yield href, Placement(directory, stem, len(PurePosixPath(below).parts), parent)
        yield from layout_toc(child, below, href or parent)


def load_toc(toc_path: Path) -> list[tuple[str, Placement]]:
    """Every entry the TOC places, in depth-first order, keyed by its own href."""
    return list(layout_toc(json.loads(toc_path.read_text(encoding="utf-8"))))


def match_route(href: str, routes: set[str]) -> str:
    """The route of the page an ``href`` names, or ``""`` when the corpus has none.

    A table of contents is written relative to wherever it lives — an exported
    one names `html/guide.html` for a corpus whose root *is* `html/` — so the
    full path is tried first and then each shorter tail of it. Reading only the
    filename instead, which is what a flat corpus lets you get away with, puts
    every `<page>/index.html` of a route-per-directory corpus on one entry.
    """
    route = to_route(resolve_href(href, ""))
    while route and route not in routes:
        route = route.partition("/")[2]
    return route


def place_toc(entries: list[tuple[str, Placement]], routes: set[str]) -> dict[str, list[Placement]]:
    """The TOC as placements keyed by the route each entry reaches."""
    resolved = {href: match_route(href, routes) for href, _ in entries}
    placements: dict[str, list[Placement]] = defaultdict(list)
    for href, placement in entries:
        route = resolved.get(href)
        if route:
            parent = resolved.get(placement.parent or "") or None
            placements[route].append(replace(placement, parent=parent))
    return dict(placements)


def normalize_key(name: str) -> str:
    return re.sub(r"\s+", "_", name.strip().lower())


def extract_meta(soup: BeautifulSoup, table: dict[str, dict[str, str]], list_keys: frozenset[str]) -> dict[str, Any]:
    """Every named `<meta>` becomes a frontmatter key.

    A page may spell one key more than once. For a scalar key the first tag
    wins, because the later ones are alternatives to it; for a key whose value
    is a list they are *more of the same list*, so the entries are appended in
    document order and either "wins" rule would throw data away.
    """
    meta: dict[str, Any] = {}
    for tag in soup.find_all("meta"):
        key = normalize_key(tag.get("name") or "")
        content = (tag.get("content") or "").strip()
        if not key or not content or key in BOILERPLATE_META or key.startswith("dc."):
            continue
        fold = table.get(key, {})
        if key in list_keys:
            entries = meta.setdefault(key, [])
            for part in content.split(","):
                item = fold.get(part.strip().lower(), part.strip())
                if item and item not in entries:
                    entries.append(item)
            if not entries:
                del meta[key]
        elif key not in meta:
            meta[key] = fold.get(content.lower(), content)
    return meta


def extract_id(soup: BeautifulSoup, route: str, sources: list[str], pattern: re.Pattern | None) -> tuple[str, bool]:
    """The first durable identifier the page offers, and whether it is one.

    Falling back to the **route** is what VAULT.md §11.2 calls fragile: it is
    the note's identity anyway, so the note simply does not declare it. It is
    the route rather than the filename stem because pages are grouped by this
    id — and a corpus of `<page>/index.html` directories has one stem for the
    whole corpus, which collapses every page onto the first.
    """
    for spec in sources:
        if spec.startswith("meta:"):
            tag = soup.find("meta", attrs={"name": spec[5:]})
            candidate = (tag.get("content") or "").strip() if tag else ""
        elif spec == "body-id":
            body = soup.find("body")
            candidate = (body.get("id") or "").strip() if body else ""
        elif spec == "stem":
            candidate = route
        else:
            raise SystemExit(f"--id-from: unknown source {spec!r} (meta:<name>, body-id or stem)")
        if not candidate:
            continue
        if pattern is not None:
            found = pattern.search(candidate)
            if not found:
                continue
            candidate = found.group(1) if found.groups() else found.group(0)
        return candidate, spec != "stem"
    return route, False


def escape_wikilink_syntax(content: Any, report: Report) -> None:
    r"""Escape a `[[` the *source* wrote, which is text and not a link.

    The converter mints every wikilink in the output itself, so a `[[` that
    survives from the page is something a reader is meant to see: an API page
    writes `(e.g., [["Tables", "Table1"]])` in its prose, and read as a link it
    mints a stub note named after a Python literal. `\[\[` is the escape
    VAULT.md §5.1 names. Text inside `<pre>` or `<code>` is left exactly as
    written — it becomes a fence or a code span, neither of which is scanned
    for links, and a backslash there would be code the source never had.
    """
    for text in list(content.find_all(string=True)):
        if "[[" not in text or text.find_parent(["pre", "code"]) is not None:
            continue
        text.replace_with(NavigableString(text.replace("[[", "\\[\\[")))
        report.literal_brackets += 1


def parse_page(source: Path, args: argparse.Namespace, table: dict[str, dict[str, str]], report: Report) -> Page | None:
    rel = PurePosixPath(source.relative_to(args.html_dir).as_posix())
    page_dir = str(rel.parent) if str(rel.parent) != "." else ""
    soup = BeautifulSoup(source.read_text(encoding="utf-8", errors="replace"), "html.parser")
    content = soup.select_one(args.content_selector) or soup.find("body")
    if content is None:
        return None
    for selector in args.strip:
        for junk in content.select(selector):
            junk.decompose()
    escape_wikilink_syntax(content, report)

    parent_files: list[str] = []
    related_files: list[str] = []
    for nav in content.select(args.related_selector):
        for link in nav.find_all("a"):
            href = link.get("href") or ""
            if "://" in href:
                continue
            name = to_route(resolve_href(href, page_dir))
            if not name:
                continue
            if link.find_parent(class_=args.parent_link_class):
                parent_files.append(name)
            elif not link.find_parent(class_=args.child_link_class):
                related_files.append(name)
        nav.decompose()  # the links are edges now; leaving them would double every one

    title_tag = soup.find("title") or content.find(re.compile("^h[1-6]$"))
    route = to_route(str(rel))
    doc_id, explicit = extract_id(soup, route, args.id_from, args.id_pattern)
    return Page(
        source=source,
        rel=str(rel),
        route=route,
        doc_id=doc_id,
        explicit_id=explicit,
        title=title_tag.get_text(strip=True) if title_tag else PurePosixPath(route).name,
        meta=extract_meta(soup, table, args.list_keys),
        content=content,
        related_files=related_files,
        parent_files=parent_files,
    )


def assign_paths(notes: list[Page], toc: dict[str, list[Placement]], default_label: str) -> None:
    """Give every note its vault-relative ``out_path``.

    A page the TOC places sits where the TOC put it, so a page with TOC children
    is the folder note for the directory holding them. A page the TOC never
    names has no known position: it lands at the vault root beside the tree, or
    flat under the default label when there is no TOC at all.
    """
    loose = "" if toc else default_label
    used: set[str] = set()
    for note in notes:
        placements = toc.get(note.route)
        if placements:
            directory, stem = placements[0].directory, placements[0].stem
            note.toc_depth = placements[0].depth
        else:
            directory, stem = loose, sanitize(note.title)
        candidate = posixpath.join(directory, stem) if directory else stem
        # Two pages may carry one title — a route-per-directory corpus names
        # every page `index.html` and titles many of them the same word — so the
        # route, which is unique by construction, disambiguates. The counter is
        # the floor under that: without it a third clash overwrites a note that
        # is already written, and the run reports one note per page either way.
        if candidate.lower() in used:
            candidate = f"{candidate}_{sanitize(note.route.replace('/', '_'))}"
        base, suffix = candidate, 1
        while candidate.lower() in used:
            suffix += 1
            candidate = f"{base}_{suffix}"
        used.add(candidate.lower())
        note.out_path = candidate + ".md"


def canonical(pages: list[Page], toc: dict[str, list[Placement]]) -> Page:
    """One note per id, so a page cross-listed under several TOC paths is placed
    once: prefer a page the TOC places, then the most root-ward, then
    lexicographic — never source-scan order."""

    def key(page: Page) -> tuple[int, str, str]:
        placements = toc.get(page.route)
        if not placements:
            return (10_000, "", page.rel)
        first = placements[0]
        return (first.depth, posixpath.join(first.directory, first.stem), page.rel)

    return sorted(pages, key=key)[0]


def link_target(page: Page, ambiguous: set[str]) -> str:
    """The wikilink that reaches this note.

    Its stem, unless two notes share one — and then the id it will carry: the
    declared one, or the path-relative fallback VAULT.md §3 gives a collision.
    """
    stem = PurePosixPath(page.out_path).stem
    if stem.lower() not in ambiguous:
        return stem
    return page.doc_id if page.explicit_id else page.out_path[: -len(".md")]


def rewrite_links(
    page: Page,
    notes_by_route: dict[str, Page],
    ambiguous: set[str],
    anchors: dict[str, dict[str, list[str]]],
    report: Report,
) -> int:
    """Turn internal links into wikilinks; return how many named no page.

    A link naming nothing in the output set becomes its own text: left as a
    markdown link it would name a file the vault does not own, which the loader
    reads as a path link or an attachment rather than as the prose it is.

    A `#fragment` that names an id *inside* the target page becomes the heading
    path that id sits under — `[[Note#Section#Subsection]]`, which is the
    spelling Obsidian and VAULT.md §5.1 both read — so a generator's anchor
    lands the reader where the link meant, and a `structure:` build retargets
    the edge onto that section. A heading carrying `[`, `]`, `|` or `#` cannot
    be written as an anchor at all (§13.2), so the link is left on the note.
    """
    unresolved = 0
    page_dir = posixpath.dirname(page.rel)
    for link in list(page.content.find_all("a")):
        href = (link.get("href") or "").strip()
        if "://" in href:
            continue
        path, _, fragment = href.partition("#")
        route = to_route(resolve_href(path, page_dir)) if path else ""
        target = notes_by_route.get(route) if route else None
        text = link.get_text(strip=True)
        if target is None or target is page:
            unresolved += 1 if path and target is None else 0
            link.replace_with(NavigableString(text))
            continue
        name = link_target(target, ambiguous)
        heading = anchors.get(target.route, {}).get(fragment, []) if fragment else []
        if heading and not any(set(part) & UNLINKABLE_IN_HEADING for part in heading):
            name = f"{name}#" + "#".join(heading)
            report.anchored_links += 1
        link.replace_with(NavigableString(f"[[{name}]]" if text in ("", name) else f"[[{name}|{text}]]"))
    return unresolved


def collect_images(page: Page, root: Path, images_root: Path | None, report: Report) -> list[tuple[Path, str]]:
    """Resolve every `<img src>` to (file on disk, vault-relative destination).

    A picture's place in the vault is its path below the **source root**: a
    `/`-absolute `src` already names one and a relative one is resolved against
    the page that wrote it, which is how a browser reads both. That is what
    keeps two pages in two directories that each write `img/logo.png` as two
    pictures — reading every `src` as written puts them on one path, and the
    second copy overwrites the first.

    The reference is left exactly as written wherever the page's own directory
    did not change what it means, so a flat corpus writes what it always wrote;
    where it did, the reference becomes the `/`-absolute one VAULT.md §6.2
    resolves from the vault root, which no reorganisation of the output can
    break.
    """
    copies: list[tuple[Path, str]] = []
    page_dir = posixpath.dirname(page.rel)
    for img in page.content.find_all("img"):
        src = (img.get("src") or "").strip()
        if not src or "://" in src or src.startswith("data:"):
            continue
        rel = resolve_href(src, page_dir)
        if not rel:
            report.unplaceable_images.append(f"{page.rel}: {src}")
            img.replace_with(NavigableString(img.get("alt") or ""))
            continue
        if rel != posixpath.normpath(src.lstrip("/")):
            img["src"] = "/" + rel
        origin = (images_root or root) / rel
        if not origin.is_file():
            report.missing_images.add(rel)
            continue
        if PurePosixPath(rel).suffix.lower() not in DELIVERABLE_IMAGES:
            report.other_image_types[PurePosixPath(rel).suffix.lower()] += 1
        copies.append((origin, rel))
    return copies


@dataclass(frozen=True)
class BodyRules:
    """What the body transform reads off the command line."""

    admonition_classes: frozenset[str]
    param_heading: re.Pattern
    procedure_heading: re.Pattern
    dl_mode: str
    block_ids: bool
    table_header: str


class Body:
    """One page's body on its way to markdown.

    HTML the markdown converter would flatten is rendered here instead — a
    table to GFM, an admonition to a callout, a `<pre>` to a fence carrying its
    language — and replaced in the DOM by a token that survives the markdown
    pass untouched. ``expand`` puts the markdown back at the end, indented to
    the line the token sat on, so a construct inside a list item stays inside
    it; and because the blank-line tidy runs while the tokens are still in
    place, it cannot reach inside a fence and close a gap the code meant.
    """

    def __init__(self, rules: BodyRules, report: Report) -> None:
        self.rules = rules
        self.report = report
        self.blocks: dict[str, str] = {}
        self.ids: set[str] = set()

    def render(self, content: Any) -> str:
        """Markdown for the whole body, with every rendered block still a token."""
        if self.rules.block_ids:
            self.name_blocks(content)
        self.rewrite_definition_lists(content)
        self.rewrite_tables(content)
        self.rewrite_fences(content)
        self.rewrite_admonitions(content)
        # Last: what is left in the DOM is what will be a *top-level* list, and
        # a list inside a callout is not one (VAULT.md §7.1).
        self.count_ordered_lists(content)
        return to_md(str(content)).strip()

    def token(self, markdown: str) -> str:
        name = f"KGLITEBLOCK{len(self.blocks):04d}X"
        self.blocks[name] = markdown
        return name

    def replace(self, element: Any, markdown: str) -> None:
        holder = _SOUP.new_tag("p")
        holder.string = self.token(markdown)
        element.replace_with(holder)

    def expand(self, text: str) -> str:
        lines: list[str] = []
        for line in text.split("\n"):
            block = self.blocks.get(line.strip())
            if block is None:
                lines.append(line)
                continue
            pad = line[: len(line) - len(line.lstrip())]
            lines.extend(pad + part if part else "" for part in block.split("\n"))
        return "\n".join(lines)

    def name_blocks(self, content: Any) -> None:
        """A `<p id=...>` becomes a citable passage: VAULT.md §5.7's ` ^id`.

        A paragraph inside a table is skipped — the id would land in a cell,
        where it is text and not a name.
        """
        for para in content.find_all("p"):
            slug = _BLOCK_ID_JUNK.sub("-", (para.get("id") or "").strip()).strip("-")
            if not slug or para.find_parent("table"):
                continue
            candidate, suffix = slug, 1
            while candidate in self.ids:
                suffix += 1
                candidate = f"{slug}-{suffix}"
            self.ids.add(candidate)
            para.append(NavigableString(f" ^{candidate}"))
            self.report.block_ids += 1

    def rewrite_definition_lists(self, content: Any) -> None:
        """A `<dl>` is prose to the reader (§13.2), so it becomes headings or terms.

        Headings mode is for the Sphinx shape — a `<dt>` whose id is the
        symbol's qualified name — because a heading is what `key_from_heading:`
        relabels and what `[[Note#pkg.mod.func(a, b)]]` reaches. Everything else
        becomes a bold term above its own paragraph.

        The level each list's terms take is decided for the whole page first
        (`definition_levels`), because rewriting one list changes what "the
        previous heading" answers for the next.
        """
        for definitions, level, symbols in definition_levels(content, self.rules):
            for part in definitions.find_all(["dt", "dd"]):
                if part.find_parent("dl") is not definitions:
                    continue  # a nested list's own term, handled when we reach it
                if part.name == "dt":
                    self.rewrite_term(part, level, symbols)
                else:
                    self.open_definition_body(part, level if symbols else 0)
            definitions.unwrap()

    def rewrite_term(self, term: Any, level: int, symbols: bool) -> None:
        name = term_text(term)
        if not symbols:
            paragraph, strong = _SOUP.new_tag("p"), _SOUP.new_tag("strong")
            strong.string = name
            paragraph.append(strong)
            term.replace_with(paragraph)
            self.report.definition_terms += 1
            return
        qualified = qualified_name(term)
        heading = _SOUP.new_tag(f"h{level}")
        heading.string = qualified or name
        term.replace_with(heading)
        self.report.symbol_headings += 1 if qualified else 0

    def open_definition_body(self, body: Any, under: int) -> None:
        """Unwrap a `<dd>`, demoting the headings it holds to sit below `under`.

        Left at their own level they would *close* the symbol's section instead
        of nesting in it, and the parameter table would sit under the wrong
        heading path.
        """
        levels = [int(tag.name[1]) for tag in body.find_all(HEADING)]
        if under and levels:
            shift = under + 1 - min(levels)
            for tag in body.find_all(HEADING):
                tag.name = f"h{min(6, max(1, int(tag.name[1]) + shift))}"
        if body.find(BLOCK_TAGS, recursive=False) is None:
            wrapper = _SOUP.new_tag("p")
            for child in list(body.contents):
                wrapper.append(child.extract())
            body.append(wrapper)
        body.unwrap()

    def rewrite_tables(self, content: Any) -> None:
        for table in content.find_all("table"):
            if table.parent is None or table.find_parent("table") is not None:
                # A nested table left the tree when its cell flattened it, and
                # a detached element cannot be replaced.
                continue
            heading = previous_heading(table, content)
            title = heading.get_text(" ", strip=True) if heading is not None else ""
            parameters = bool(self.rules.param_heading.search(title))
            header, rows = self.table_shape(table)
            if header is None:
                table.decompose()
                continue
            self.replace(table, self.gfm(header, rows, parameters))
            self.report.tables += 1
            self.report.table_rows += len(rows)
            self.report.param_tables += 1 if parameters else 0

    def table_shape(self, table: Any) -> tuple[list[str] | None, list[list[str]]]:
        """The header row and the body rows, or ``(None, [])`` for an empty table."""
        rows: list[tuple[bool, list[str], list[str]]] = []
        for line in table.find_all("tr"):
            if line.find_parent("table") is not table:
                continue
            cells = [cell for cell in line.find_all(["th", "td"]) if cell.find_parent(["th", "td"]) is None]
            if not cells:
                continue
            heading_row = all(cell.name == "th" for cell in cells) or line.find_parent("thead") is not None
            names = [cell.name for cell in cells]
            rows.append((heading_row, [text for cell in cells for text in self.cell_texts(cell)], names))
        if not rows:
            return None, []
        body = [cells for _, cells, _ in rows]
        if rows[0][0]:
            return rows[0][1], body[1:]
        # A docutils field list: a `<th>` naming the field beside a `<td>`
        # holding it, and no header row anywhere. Both `--table-header` answers
        # are wrong for it — promoting the first row names the columns
        # "Parameters | …" and loses that field, leaving it blank names nothing
        # — so the shape gets the header it means, whichever the flag says.
        if all(names == ["th", "td"] for _, _, names in rows):
            self.report.field_lists += 1
            return ["Field", "Value"], body
        if self.rules.table_header == "first-row":
            return rows[0][1], body[1:]
        # No `<th>` anywhere: promoting the first row would name the columns
        # after one row's data and lose that row, so the header is left blank
        # and the count says how many tables need one written at source.
        self.report.tables_without_header += 1
        return [""] * max(len(cells) for cells in body), body

    def cell_texts(self, cell: Any) -> list[str]:
        """One cell's markdown, plus an empty cell for each column it spans.

        A row is one line, so every newline a cell would carry — a `<br>`, a
        second `<p>`, a list — is collapsed to a space, and a nested table to
        its text, because a pipe table cannot hold one.
        """
        for nested in cell.find_all("table"):
            nested.replace_with(NavigableString(nested.get_text(" ", strip=True)))
        text = re.sub(r"\s+", " ", inner_md(cell)).strip()
        text, aliases = _CELL_WIKILINK.subn(r"[[\1]]", text)
        self.report.cell_link_aliases += aliases
        text = text.replace("|", "\\|")
        span = cell.get("colspan", "1")
        return [text] + [""] * (int(span) - 1 if str(span).isdigit() else 0)

    def gfm(self, header: list[str], rows: list[list[str]], parameters: bool) -> str:
        width = max(len(line) for line in [header, *rows])
        if parameters and header:
            # `tables: {under_heading: Parameters, key_column: name}` keys each
            # row on this column (VAULT.md §7.1), whatever the source called it.
            header = ["name" if index == 0 else cell for index, cell in enumerate(header)]

        def line(cells: list[str]) -> str:
            return "| " + " | ".join(cells + [""] * (width - len(cells))) + " |"

        return "\n".join([line(header), line(["---"] * width), *(line(cells) for cells in rows)])

    def rewrite_fences(self, content: Any) -> None:
        for block in content.find_all("pre"):
            language = self.fence_language(block)
            code = block.get_text().strip("\n")
            runs = [len(run) for run in re.findall(r"`+", code)]
            ticks = "`" * max(3, max(runs, default=0) + 1)
            self.replace(block, f"{ticks}{language}\n{code}\n{ticks}")
            self.report.fences[language or "(none)"] += 1

    def fence_language(self, block: Any) -> str:
        """`<div class="highlight-python"><div class="highlight"><pre>` is Sphinx's
        shape, so the class is looked for on the block, its wrappers and its code."""
        holders = [block, *list(block.parents)[:2], *block.find_all("code", limit=1)]
        for holder in holders:
            for name in holder.get("class", None) or []:
                found = CODE_LANGUAGE.match(name)
                if found:
                    return found.group(1)
        return ""

    def is_admonition(self, element: Any) -> bool:
        if getattr(element, "name", None) not in ADMONITION_TAGS:
            return False
        classes = {name.lower() for name in element.get("class", None) or []}
        return bool(classes & self.rules.admonition_classes)

    def rewrite_admonitions(self, content: Any) -> None:
        """Innermost first, so a callout inside a callout is already `> ` lines
        when the outer one prefixes its body and Obsidian's nesting falls out."""
        while True:
            pending = [el for el in content.find_all(self.is_admonition) if el.find(self.is_admonition) is None]
            if not pending:
                return
            for element in pending:
                self.replace(element, self.callout(element))

    def callout(self, element: Any) -> str:
        kind = self.callout_kind(element)
        title = ""
        marked = element.select_one(ADMONITION_TITLE)
        if marked is not None:
            title = re.sub(r"\s+", " ", marked.get_text(" ", strip=True)).strip().rstrip(":")
            marked.decompose()
            if title.lower() in GENERIC_ADMONITION | {kind}:
                title = ""  # Obsidian displays the type; a title repeating it says nothing
        # DITA writes the title inline — `<span class="note__title">Tip:</span>
        # text` — so removing it leaves the space that followed it.
        body = self.expand(tidy(inner_md(element))).lstrip(" \t")
        marker = f"> [!{kind}] {title}" if title else f"> [!{kind}]"
        self.report.callouts[kind] += 1
        if not body:
            return marker
        return "\n".join([marker, *(f"> {line}".rstrip() for line in body.split("\n"))])

    def callout_kind(self, element: Any) -> str:
        """The source's own word for the kind, which VAULT.md §5.7 keeps verbatim."""
        classes = [name.lower() for name in element.get("class", None) or []]
        named = [name for name in classes if name in self.rules.admonition_classes]
        specific = [name for name in named if name not in GENERIC_ADMONITION]
        if specific:
            return specific[0]
        return "note" if "note" in named or not named else named[0].replace("admonition", "note")

    def count_ordered_lists(self, content: Any) -> None:
        """Census only: `ordered_lists:` reads every top-level list of two or more
        items, and the count under a `--procedure-heading` says whether narrowing
        the rule with `under_heading:` would be worth declaring."""
        for listing in content.find_all("ol"):
            if listing.find_parent("li") is not None or len(listing.find_all("li", recursive=False)) < 2:
                continue
            self.report.ordered_lists += 1
            heading = previous_heading(listing, content)
            title = heading.get_text(" ", strip=True) if heading is not None else ""
            if self.rules.procedure_heading.search(title):
                self.report.procedure_lists += 1


def to_markdown(page: Page, related: list[str], rules: BodyRules, report: Report) -> str:
    writer = Body(rules, report)
    body = writer.expand(tidy(writer.render(page.content)))
    if related:
        links = "\n".join(f"- [[{name}]]" for name in related)
        body = f"{body}\n\n## Related topics\n\n{links}" if body else f"## Related topics\n\n{links}"
    return body + "\n"


def yaml_scalar(value: Any) -> str:
    """VAULT.md §10.4: quote a string only when bare it would read as something
    else — a number, a boolean, a date, a flow sequence (every wikilink)."""
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float)):
        return str(value)
    text = str(value)
    quote = (
        not text
        or text != text.strip()
        or text[0] in _YAML_INDICATORS
        or ": " in text
        or " #" in text
        or "\n" in text
        or text.endswith(":")
        or text.lower() in _YAML_BOOLISH
        or _DATEISH.match(text) is not None
    )
    if not quote:
        try:
            float(text)
            quote = True
        except ValueError:
            pass
    return '"' + text.replace("\\", "\\\\").replace('"', '\\"') + '"' if quote else text


def yaml_item(value: Any) -> str:
    """One sequence entry. A mapping is written inline — `{range: toc_depth}` is
    the shape VAULT.md §7 gives a range or composite index, and stringifying the
    mapping instead spells a *property name* nothing carries."""
    if isinstance(value, dict):
        return "{" + ", ".join(f"{key}: {yaml_item(value[key])}" for key in value) + "}"
    if isinstance(value, list):
        return "[" + ", ".join(yaml_item(item) for item in value) + "]"
    return yaml_scalar(value)


def yaml_block(data: dict[str, Any], indent: int = 0) -> str:
    pad = " " * indent
    lines = []
    for key in sorted(data):
        value = data[key]
        if isinstance(value, dict):
            lines.append(f"{pad}{key}:")
            lines.append(yaml_block(value, indent + 2))
        elif isinstance(value, list):
            lines.append(f"{pad}{key}:")
            lines.extend(f"{pad}- {yaml_item(item)}" for item in value)
        else:
            lines.append(f"{pad}{key}: {yaml_scalar(value)}")
    return "\n".join(lines)


def structure_yaml(args: argparse.Namespace, report: Report) -> str:
    """The `structure:` block for what this run actually emitted (VAULT.md §7.1).

    A rule that matched nothing is the §13.4 warning the author is told to look
    for, so a rule is declared only when the corpus carries the construct it
    reads: no `callouts:` where the source had no admonitions, no `tables:`
    where no table sat under a parameter heading.
    """
    if not args.emit_structure or not report.structured():
        return ""
    block: dict[str, Any] = {
        "sections": {"label": "Section", "edge": "HAS_SECTION", "parent": "PARENT_SECTION", "next": "NEXT_SECTION"},
        "chunks": {"label": "Chunk", "edge": "HAS_CHUNK", "next": "NEXT_CHUNK", "max_words": 650, "max_chars": 6000},
    }
    if report.callouts:
        block["callouts"] = {"label": "Note", "edge": "HAS_NOTE"}
    if report.fences:
        # No `langs:` filter: a fence the source left unlabelled is still an
        # example, and naming languages here would drop it (VAULT.md §13.2).
        block["code_fences"] = {"label": "Example", "edge": "HAS_EXAMPLE"}
    if report.ordered_lists:
        block["ordered_lists"] = {
            "label": "ProcedureStep",
            "container": "Procedure",
            "edge": "HAS_STEP",
            "next": "NEXT_STEP",
        }
    if report.param_tables:
        block["tables"] = [
            {
                "under_heading": args.param_heading.pattern,
                "label": "ApiParameter",
                "key_column": "name",
                "edge": "HAS_PARAMETER",
            }
        ]
    if report.symbol_headings and args.api_label:
        # `under_label:` is one of the rule's two required gates, so without
        # `--api-label` there is no rule to declare (VAULT.md §7.1).
        block["key_from_heading"] = {
            "label": "ApiSymbol",
            "property": "qualified_name",
            "under_label": args.api_label,
        }
    lines = ["structure:"]
    for key in sorted(block):
        value = block[key]
        if isinstance(value, list):
            lines.append(f"  {key}:")
            lines.extend(f"  - {yaml_item(entry)}" for entry in value)
        else:
            lines.append(f"  {key}: {yaml_item(value)}")
    return "\n".join(lines) + "\n"


def vault_yaml(args: argparse.Namespace, report: Report) -> str:
    doc: dict[str, Any] = {
        "kglite_vault": 1,
        "default_label": args.default_label,
        "folder_notes": {"edge": "CHILD_OF", "direction": "child_to_parent"},
        "heading_edges": {"Related topics": "RELATED_TO"},
    }
    hubs = {}
    for spec in args.hub:
        key, _, rest = spec.partition("=")
        parts = rest.split(":")
        if not key or len(parts) < 2 or not parts[0] or not parts[1]:
            raise SystemExit(f"--hub: expected key=Label:EDGE[:ci], got {spec!r}")
        hubs[key] = {"label": parts[0], "edge": parts[1], "case_insensitive": parts[2:] == ["ci"]}
    indexes: dict[str, list[Any]] = defaultdict(list)
    for spec in args.index:
        label, _, tail = spec.partition(".")
        prop, _, kind = tail.partition(":")
        if not label or not prop:
            raise SystemExit(f"--index: expected Label.property[:range], got {spec!r}")
        indexes[label].append({"range": prop} if kind == "range" else prop)
    embed = {}
    for spec in args.embed:
        label, _, prop = spec.partition(".")
        if not label or not prop:
            raise SystemExit(f"--embed: expected Label.property, got {spec!r}")
        embed[label] = prop
    for key, value in (("hubs", hubs), ("indexes", dict(indexes)), ("embed", embed)):
        if value:
            doc[key] = value
    return yaml_block(doc) + "\n" + structure_yaml(args, report)


class Report:
    """What the run saw, printed before the validator's own report."""

    def __init__(self) -> None:
        self.pages = self.notes = self.cross_listed = self.images = self.unresolved_links = 0
        self.missing_images: set[str] = set()
        self.unplaceable_images: list[str] = []
        self.other_image_types: defaultdict[str, int] = defaultdict(int)
        self.tables = self.table_rows = self.tables_without_header = self.param_tables = 0
        self.field_lists = self.anchored_links = self.literal_brackets = 0
        self.ordered_lists = self.procedure_lists = self.block_ids = self.cell_link_aliases = 0
        self.symbol_headings = self.definition_terms = 0
        self.callouts: defaultdict[str, int] = defaultdict(int)
        self.fences: defaultdict[str, int] = defaultdict(int)
        self.seconds = 0.0

    def structured(self) -> bool:
        """Whether the run produced anything a `structure:` block would read."""
        return bool(
            self.callouts or self.fences or self.tables or self.ordered_lists or self.symbol_headings or self.block_ids
        )

    def render(self) -> str:
        counted = [
            ("pages scanned", self.pages),
            ("notes written", self.notes),
            ("cross-listed collapsed", self.cross_listed),
            ("images copied", self.images),
            ("missing image originals", len(self.missing_images)),
            ("unplaceable image refs", len(self.unplaceable_images)),
            ("unresolved internal links", self.unresolved_links),
            ("links resolved to a heading", self.anchored_links),
            ("literal [[ escaped", self.literal_brackets),
            ("callouts", sum(self.callouts.values())),
            ("GFM tables", self.tables),
            ("GFM table rows", self.table_rows),
            ("tables with no header", self.tables_without_header),
            ("field lists", self.field_lists),
            ("cell link aliases dropped", self.cell_link_aliases),
            ("parameter tables", self.param_tables),
            ("ordered lists", self.ordered_lists),
            ("under a procedure heading", self.procedure_lists),
            ("fenced code blocks", sum(self.fences.values())),
            ("symbol headings", self.symbol_headings),
            ("definition terms", self.definition_terms),
            ("block ids", self.block_ids),
        ]
        lines = [f"{label + ':':<27}{value}" for label, value in counted]
        lines.append(f"{'elapsed:':<27}{self.seconds:.1f}s")
        for what, census in (("callouts by kind", self.callouts), ("fences by language", self.fences)):
            if census:
                lines.append(f"{what}: " + ", ".join(f"{k} x{n}" for k, n in sorted(census.items())))
        if self.other_image_types:
            kinds = ", ".join(f"{ext} x{n}" for ext, n in sorted(self.other_image_types.items()))
            lines.append(f"non-deliverable image types: {kinds} -- convert these at source (VAULT.md 6)")
        lines.extend(f"  missing original: {name}" for name in sorted(self.missing_images)[:10])
        lines.extend(f"  unplaceable: {ref}" for ref in sorted(self.unplaceable_images)[:10])
        return "\n".join(lines)


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    p = argparse.ArgumentParser(description="Convert a directory of HTML pages into a KGLite vault.")
    p.add_argument("html_dir", type=Path)
    p.add_argument("out_vault", type=Path)
    p.add_argument("--toc", type=Path, help="JSON table of contents driving the folder layout")
    p.add_argument(
        "--images-dir", type=Path, help="root the resolved <img src> paths are read from (default: html_dir)"
    )
    p.add_argument(
        "--default-label",
        default="Note",
        help="label for every note; outranks the folder name (VAULT.md 2.1)",
    )
    p.add_argument("--content-selector", default="div.pageData", help="CSS selector for the body; falls back to <body>")
    p.add_argument("--strip", action="append", default=[], metavar="SELECTOR", help="delete from the body; repeatable")
    p.add_argument("--related-selector", default="nav.related-links", help="CSS selector for the cross-reference block")
    p.add_argument("--parent-link-class", default="parentlink", help="class marking a parent link inside that block")
    p.add_argument("--child-link-class", default="ulchildlink", help="class marking a child link, which is dropped")
    p.add_argument("--id-from", default="meta:id,body-id,stem", help="comma list of meta:<name> / body-id / stem")
    p.add_argument("--id-pattern", help="regex pulling the id out of the candidate (group 1, or the whole match)")
    p.add_argument("--meta-map", type=Path, metavar="FILE", help='JSON {"<key>": {"<lowercased value>": "<fold to>"}}')
    p.add_argument("--hub", action="append", default=[], metavar="KEY=Label:EDGE[:ci]")
    p.add_argument("--index", action="append", default=[], metavar="Label.property[:range]")
    p.add_argument("--embed", action="append", default=[], metavar="Label.property")
    p.add_argument("--admonition-classes", default=ADMONITION_CLASSES, metavar="LIST", help="comma list of classes")
    p.add_argument("--param-heading", default=r"^(Parameters|Arguments|Fields)$", metavar="REGEX")
    p.add_argument("--procedure-heading", default=r"^(Steps|Procedure|To .*)", metavar="REGEX")
    p.add_argument("--dl-mode", choices=("headings", "terms", "auto"), default="auto", help="how a <dl> is written")
    p.add_argument("--table-header", choices=("th", "first-row"), default="th", help="where a header comes from")
    p.add_argument("--block-ids", action="store_true", help="a <p id=...> gets the trailing ` ^id` that cites it")
    p.add_argument("--api-label", metavar="LABEL", help="under_label: for the key_from_heading rule")
    p.add_argument("--emit-structure", action="store_true", help="write structure: into the generated vault.yaml")
    p.add_argument("--no-validate", action="store_true", help="skip the closing okf.validate pass")
    p.add_argument("--dry-run", action="store_true", help="scan and report; write nothing")
    args = p.parse_args(argv)
    args.id_from = [s.strip() for s in args.id_from.split(",") if s.strip()]
    args.id_pattern = re.compile(args.id_pattern) if args.id_pattern else None
    args.param_heading = re.compile(args.param_heading)
    args.procedure_heading = re.compile(args.procedure_heading)
    args.rules = BodyRules(
        admonition_classes=frozenset(c.strip().lower() for c in args.admonition_classes.split(",") if c.strip()),
        param_heading=args.param_heading,
        procedure_heading=args.procedure_heading,
        dl_mode=args.dl_mode,
        block_ids=args.block_ids,
        table_header=args.table_header,
    )
    # A hub reads a key's *list* entries and a scalar joins no hub (VAULT.md §7),
    # so a key the caller declared a hub for is written as a sequence even when
    # the source spells it as one value.
    args.list_keys = LIST_META | {spec.partition("=")[0] for spec in args.hub}
    return args


def write_note(note: Page, out_root: Path, related: list[str], rules: BodyRules, report: Report) -> None:
    front: dict[str, Any] = dict(note.meta)
    if note.explicit_id:
        front["id"] = note.doc_id
    if note.title != PurePosixPath(note.out_path).stem:
        front["title"] = note.title
    if note.parents:
        front["parent"] = [f"[[{name}]]" for name in note.parents]
    if note.toc_depth is not None:
        front["toc_depth"] = note.toc_depth
    body = to_markdown(note, related, rules, report)
    destination = out_root / note.out_path
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(f"---\n{yaml_block(front)}\n---\n\n{body}" if front else body, encoding="utf-8")


def plan(
    args: argparse.Namespace,
    table: dict[str, dict[str, str]],
    entries: list[tuple[str, Placement]],
    report: Report,
) -> tuple[list[Page], dict[str, Page], set[str], dict[str, dict[str, list[str]]]]:
    """Read every page and decide, before a byte is written, what each note is:
    where it lands, which name reaches it, which notes are its parents, and
    what heading each of its anchors sits under."""
    sources = sorted({p for pattern in ("*.html", "*.htm") for p in args.html_dir.rglob(pattern)})
    scanned = [page for page in (parse_page(s, args, table, report) for s in sources) if page]
    report.pages = len(scanned)
    # The TOC is resolved against the corpus, so it has to wait for it.
    toc = place_toc(entries, {page.route for page in scanned})

    by_id: dict[str, list[Page]] = defaultdict(list)
    for page in scanned:
        by_id[page.doc_id].append(page)
    notes = sorted((canonical(group, toc) for group in by_id.values()), key=lambda p: p.rel)
    report.cross_listed = len(scanned) - len(notes)
    assign_paths(notes, toc, args.default_label)

    stems: defaultdict[str, int] = defaultdict(int)
    for note in notes:
        stems[PurePosixPath(note.out_path).stem.lower()] += 1
    ambiguous = {stem for stem, n in stems.items() if n > 1}
    # A collapsed duplicate is still a link destination: it is reached through
    # the note that survived it.
    notes_by_route = {page.route: canonical(by_id[page.doc_id], toc) for page in scanned}
    # Read before anything rewrites a body, and for the notes that survive: a
    # link's anchor names an id in the page it *reaches*, not in the page that
    # wrote it.
    anchors = {note.route: anchor_paths(note.content, args.rules) for note in notes}

    for note in notes:
        pages = by_id[note.doc_id]
        parent_routes = {p.parent for page in pages for p in toc.get(page.route, []) if p.parent}
        parent_routes.update(f for page in pages for f in page.parent_files)
        parents = {link_target(notes_by_route[f], ambiguous) for f in parent_routes if f in notes_by_route}
        note.parents = sorted(parents - {link_target(note, ambiguous)})
    return notes, notes_by_route, ambiguous, anchors


def write_vault(
    notes: list[Page],
    notes_by_route: dict[str, Page],
    ambiguous: set[str],
    anchors: dict[str, dict[str, list[str]]],
    args: argparse.Namespace,
    report: Report,
) -> None:
    out_root = args.out_vault
    out_root.mkdir(parents=True, exist_ok=True)
    copies: dict[str, Path] = {}
    for note in notes:
        report.unresolved_links += rewrite_links(note, notes_by_route, ambiguous, anchors, report)
        for origin, rel in collect_images(note, args.html_dir, args.images_dir, report):
            copies[rel] = origin
        self_link = link_target(note, ambiguous)
        related = {link_target(notes_by_route[f], ambiguous) for f in note.related_files if f in notes_by_route}
        write_note(note, out_root, sorted(related - {self_link}), args.rules, report)
        report.notes += 1
    for rel, origin in sorted(copies.items()):
        destination = out_root / rel
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(origin, destination)
        report.images += 1
    config = out_root / ".kglite"
    config.mkdir(exist_ok=True)
    (config / "vault.yaml").write_text(vault_yaml(args, report), encoding="utf-8")


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    started = time.monotonic()
    report = Report()
    table = dict(META_NORMALIZERS)
    if args.meta_map:
        for key, mapping in json.loads(args.meta_map.read_text(encoding="utf-8")).items():
            if not isinstance(mapping, dict):
                raise SystemExit(f'--meta-map: "{key}" must map value spellings to their fold, got {mapping!r}')
            table[normalize_key(key)] = {k.lower(): v for k, v in mapping.items()}

    notes, notes_by_route, ambiguous, anchors = plan(args, table, load_toc(args.toc) if args.toc else [], report)
    if args.dry_run:
        report.notes = len(notes)
        report.seconds = time.monotonic() - started
        print(report.render())
        return 0

    out_root = args.out_vault
    write_vault(notes, notes_by_route, ambiguous, anchors, args, report)
    report.seconds = time.monotonic() - started
    print(report.render())
    if args.no_validate:
        return 0
    from kglite import okf

    result = okf.validate(str(out_root), dialect="obsidian")
    print(result)
    return 1 if result.errors else 0


if __name__ == "__main__":
    sys.exit(main())
