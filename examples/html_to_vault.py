#!/usr/bin/env python3
"""Convert a directory of HTML documents into a KGLite vault (``VAULT.md``).

Dependencies — install them yourself; they are not kglite dependencies::

    pip install beautifulsoup4 markdownify

Usage::

    html_to_vault.py <html-dir> <out-vault> [--toc TOC.json] [--images-dir DIR]
                     [--default-label Article] [--dry-run]

It follows the converter checklist in ``VAULT.md`` §11: one ``.md`` per page;
the folder-note layout (``X.md`` beside ``X/``) mirroring the table of contents
when one is given, else every note flat under ``<out>/<default_label>/``; each
``<meta name=...>`` as a frontmatter key; a durable ``id:``; internal anchors as
``[[wikilinks]]``; the cross-reference block as ``parent:`` keys and a
``## Related topics`` section; ``<img src>`` references kept as written with the
files copied into the vault; and a ``.kglite/vault.yaml`` declaring the label,
the folder-note edge, the hubs, the heading map and the indexes and embed
targets you name.

The TOC file is JSON -- ``{"label": ..., "href": ..., "children": [...]}`` --
where ``href`` names an HTML file (leading directories ignored) and a node
without one is a grouping folder with no note of its own. The root's own label
names the collection, not a folder inside it.

Nothing is converted: an image is copied byte for byte, and one whose type the
MCP server does not deliver is counted so you can convert it at source.

It ends by running ``kglite.okf.validate`` and exits non-zero when the vault has
errors. Warnings do not fail it; ``--no-validate`` skips the pass.
"""

from __future__ import annotations

import argparse
from collections import defaultdict
from dataclasses import dataclass, field
import json
from pathlib import Path, PurePosixPath
import posixpath
import re
import shutil
import sys
import time
from typing import Any, Iterator

from bs4 import BeautifulSoup, NavigableString
from markdownify import markdownify

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


@dataclass(frozen=True)
class Placement:
    """Where the TOC puts one page: its directory, its stem, and its parent."""

    directory: str
    stem: str
    depth: int
    parent_file: str | None


@dataclass
class Page:
    """One source HTML file on its way to becoming one note."""

    source: Path
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


def layout_toc(node: dict, directory: str = "", parent_file: str | None = None) -> Iterator[tuple[str, Placement]]:
    """Walk the TOC, yielding ``(html filename, placement)`` depth-first.

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
        href = child.get("href", "")
        filename = PurePosixPath(href).name if href else None
        stem = sanitize(child.get("label", ""))
        if stem.lower() in used:
            stem = f"{stem}_{index}"
        used.add(stem.lower())
        below = posixpath.join(directory, stem) if directory else stem
        if filename:
            yield filename, Placement(directory, stem, len(PurePosixPath(below).parts), parent_file)
        yield from layout_toc(child, below, filename or parent_file)


def load_toc(toc_path: Path) -> dict[str, list[Placement]]:
    root = json.loads(toc_path.read_text(encoding="utf-8"))
    placements: dict[str, list[Placement]] = defaultdict(list)
    for filename, placement in layout_toc(root):
        placements[filename].append(placement)
    return dict(placements)


def normalize_key(name: str) -> str:
    return re.sub(r"\s+", "_", name.strip().lower())


def extract_meta(soup: BeautifulSoup, table: dict[str, dict[str, str]], list_keys: frozenset[str]) -> dict[str, Any]:
    """Every named `<meta>` becomes a frontmatter key; the first spelling wins."""
    meta: dict[str, Any] = {}
    for tag in soup.find_all("meta"):
        key = normalize_key(tag.get("name") or "")
        content = (tag.get("content") or "").strip()
        if not key or not content or key in meta or key in BOILERPLATE_META or key.startswith("dc."):
            continue
        fold = table.get(key, {})
        if key in list_keys:
            items = [fold.get(p.strip().lower(), p.strip()) for p in content.split(",") if p.strip()]
            if items:
                meta[key] = items
        else:
            meta[key] = fold.get(content.lower(), content)
    return meta


def extract_id(soup: BeautifulSoup, source: Path, sources: list[str], pattern: re.Pattern | None) -> tuple[str, bool]:
    """The first durable identifier the page offers, and whether it is one.

    Returning the stem is the fallback VAULT.md §11.2 calls fragile: it is the
    id anyway, so the note simply does not declare it.
    """
    for spec in sources:
        if spec.startswith("meta:"):
            tag = soup.find("meta", attrs={"name": spec[5:]})
            candidate = (tag.get("content") or "").strip() if tag else ""
        elif spec == "body-id":
            body = soup.find("body")
            candidate = (body.get("id") or "").strip() if body else ""
        elif spec == "stem":
            candidate = source.stem
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
    return source.stem, False


def parse_page(source: Path, args: argparse.Namespace, table: dict[str, dict[str, str]]) -> Page | None:
    soup = BeautifulSoup(source.read_text(encoding="utf-8", errors="replace"), "html.parser")
    content = soup.select_one(args.content_selector) or soup.find("body")
    if content is None:
        return None
    for selector in args.strip:
        for junk in content.select(selector):
            junk.decompose()

    parent_files: list[str] = []
    related_files: list[str] = []
    for nav in content.select(args.related_selector):
        for link in nav.find_all("a"):
            href = (link.get("href") or "").split("#")[0]
            if not href or "://" in href:
                continue
            name = PurePosixPath(href).name
            if link.find_parent(class_=args.parent_link_class):
                parent_files.append(name)
            elif not link.find_parent(class_=args.child_link_class):
                related_files.append(name)
        nav.decompose()  # the links are edges now; leaving them would double every one

    title_tag = soup.find("title") or content.find(re.compile("^h[1-6]$"))
    doc_id, explicit = extract_id(soup, source, args.id_from, args.id_pattern)
    return Page(
        source=source,
        doc_id=doc_id,
        explicit_id=explicit,
        title=title_tag.get_text(strip=True) if title_tag else source.stem,
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
        placements = toc.get(note.source.name)
        if placements:
            directory, stem = placements[0].directory, placements[0].stem
            note.toc_depth = placements[0].depth
        else:
            directory, stem = loose, sanitize(note.title)
        candidate = posixpath.join(directory, stem) if directory else stem
        if candidate.lower() in used:
            candidate = f"{candidate}_{sanitize(note.doc_id)[:8]}"
        used.add(candidate.lower())
        note.out_path = candidate + ".md"


def canonical(pages: list[Page], toc: dict[str, list[Placement]]) -> Page:
    """One note per id, so a page cross-listed under several TOC paths is placed
    once: prefer a page the TOC places, then the most root-ward, then
    lexicographic — never source-scan order."""

    def key(page: Page) -> tuple[int, str, str]:
        placements = toc.get(page.source.name)
        if not placements:
            return (10_000, "", page.source.name)
        first = placements[0]
        return (first.depth, posixpath.join(first.directory, first.stem), page.source.name)

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


def rewrite_links(page: Page, notes_by_file: dict[str, Page], ambiguous: set[str]) -> int:
    """Turn internal anchors into wikilinks; return how many named no page.

    An anchor naming nothing in the output set becomes its own text: left as a
    markdown link it would name a file the vault does not own, which the loader
    reads as a path link or an attachment rather than as the prose it is.
    """
    unresolved = 0
    for link in list(page.content.find_all("a")):
        href = (link.get("href") or "").split("#")[0]
        if "://" in href:
            continue
        target = notes_by_file.get(PurePosixPath(href).name) if href else None
        text = link.get_text(strip=True)
        if target is None or target is page:
            unresolved += 1 if href and target is None else 0
            link.replace_with(NavigableString(text))
            continue
        name = link_target(target, ambiguous)
        link.replace_with(NavigableString(f"[[{name}]]" if text in ("", name) else f"[[{name}|{text}]]"))
    return unresolved


def collect_images(page: Page, images_root: Path | None, report: Report) -> list[tuple[Path, str]]:
    """Resolve every `<img src>` to (file on disk, vault-relative destination).

    The reference in the body is left exactly as written: it resolves
    note-relative for a note beside the images and vault-root-relative for one
    below them (VAULT.md §6.2), and copying to the same relative path keeps both
    readings true however the TOC reorganises the output.
    """
    copies: list[tuple[Path, str]] = []
    for img in page.content.find_all("img"):
        src = (img.get("src") or "").strip()
        if not src or "://" in src or src.startswith(("/", "data:")):
            continue
        rel = posixpath.normpath(src)
        if rel.startswith(".."):
            report.unplaceable_images.append(f"{page.source.name}: {src}")
            img.replace_with(NavigableString(img.get("alt") or ""))
            continue
        origin = (images_root / rel) if images_root else (page.source.parent / rel)
        if not origin.is_file():
            report.missing_images.add(rel)
            continue
        if PurePosixPath(rel).suffix.lower() not in DELIVERABLE_IMAGES:
            report.other_image_types[PurePosixPath(rel).suffix.lower()] += 1
        copies.append((origin, rel))
    return copies


def to_markdown(page: Page, related: list[str]) -> str:
    body = markdownify(
        str(page.content),
        heading_style="ATX",
        bullets="-",
        code_language="",
        # A wikilink is a note's name, not prose: escaping the `_` in
        # `[[Well_tops]]` spells a note nothing resolves to, and the link
        # silently becomes a dangling stub.
        escape_underscores=False,
        # markdownify reduces an image inside a heading or a table cell to its
        # alt text. A help corpus puts its button icons in exactly those two
        # places, and a converter that drops a picture has lost the picture.
        keep_inline_images_in=INLINE_IMAGE_PARENTS,
    ).strip()
    body = re.sub(r"\n{3,}", "\n\n", body)
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
            lines.extend(f"{pad}- {yaml_scalar(item)}" for item in value)
        else:
            lines.append(f"{pad}{key}: {yaml_scalar(value)}")
    return "\n".join(lines)


def vault_yaml(args: argparse.Namespace) -> str:
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
    return yaml_block(doc) + "\n"


class Report:
    """What the run saw, printed before the validator's own report."""

    def __init__(self) -> None:
        self.pages = self.notes = self.cross_listed = self.images = self.unresolved_links = 0
        self.missing_images: set[str] = set()
        self.unplaceable_images: list[str] = []
        self.other_image_types: defaultdict[str, int] = defaultdict(int)
        self.seconds = 0.0

    def render(self) -> str:
        counted = [
            ("pages scanned", self.pages),
            ("notes written", self.notes),
            ("cross-listed collapsed", self.cross_listed),
            ("images copied", self.images),
            ("missing image originals", len(self.missing_images)),
            ("unplaceable image refs", len(self.unplaceable_images)),
            ("unresolved internal links", self.unresolved_links),
        ]
        lines = [f"{label + ':':<27}{value}" for label, value in counted]
        lines.append(f"{'elapsed:':<27}{self.seconds:.1f}s")
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
    p.add_argument("--images-dir", type=Path, help="root <img src> paths resolve against (default: beside the page)")
    p.add_argument("--default-label", default="Note")
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
    p.add_argument("--no-validate", action="store_true", help="skip the closing okf.validate pass")
    p.add_argument("--dry-run", action="store_true", help="scan and report; write nothing")
    args = p.parse_args(argv)
    args.id_from = [s.strip() for s in args.id_from.split(",") if s.strip()]
    args.id_pattern = re.compile(args.id_pattern) if args.id_pattern else None
    # A hub reads a key's *list* entries and a scalar joins no hub (VAULT.md §7),
    # so a key the caller declared a hub for is written as a sequence even when
    # the source spells it as one value.
    args.list_keys = LIST_META | {spec.partition("=")[0] for spec in args.hub}
    return args


def write_note(note: Page, out_root: Path, related: list[str]) -> None:
    front: dict[str, Any] = dict(note.meta)
    if note.explicit_id:
        front["id"] = note.doc_id
    if note.title != PurePosixPath(note.out_path).stem:
        front["title"] = note.title
    if note.parents:
        front["parent"] = [f"[[{name}]]" for name in note.parents]
    if note.toc_depth is not None:
        front["toc_depth"] = note.toc_depth
    body = to_markdown(note, related)
    destination = out_root / note.out_path
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(f"---\n{yaml_block(front)}\n---\n\n{body}" if front else body, encoding="utf-8")


def plan(
    args: argparse.Namespace,
    table: dict[str, dict[str, str]],
    toc: dict[str, list[Placement]],
    report: Report,
) -> tuple[list[Page], dict[str, Page], set[str]]:
    """Read every page and decide, before a byte is written, what each note is:
    where it lands, which name reaches it, and which notes are its parents."""
    sources = sorted({p for pattern in ("*.html", "*.htm") for p in args.html_dir.rglob(pattern)})
    scanned = [page for page in (parse_page(s, args, table) for s in sources) if page]
    report.pages = len(scanned)

    by_id: dict[str, list[Page]] = defaultdict(list)
    for page in scanned:
        by_id[page.doc_id].append(page)
    notes = sorted((canonical(group, toc) for group in by_id.values()), key=lambda p: p.source.name)
    report.cross_listed = len(scanned) - len(notes)
    assign_paths(notes, toc, args.default_label)

    stems: defaultdict[str, int] = defaultdict(int)
    for note in notes:
        stems[PurePosixPath(note.out_path).stem.lower()] += 1
    ambiguous = {stem for stem, n in stems.items() if n > 1}
    # A collapsed duplicate is still a link destination: it is reached through
    # the note that survived it.
    notes_by_file = {page.source.name: canonical(by_id[page.doc_id], toc) for page in scanned}

    for note in notes:
        pages = by_id[note.doc_id]
        parent_files = {p.parent_file for page in pages for p in toc.get(page.source.name, []) if p.parent_file}
        parent_files.update(f for page in pages for f in page.parent_files)
        parents = {link_target(notes_by_file[f], ambiguous) for f in parent_files if f in notes_by_file}
        note.parents = sorted(parents - {link_target(note, ambiguous)})
    return notes, notes_by_file, ambiguous


def write_vault(
    notes: list[Page],
    notes_by_file: dict[str, Page],
    ambiguous: set[str],
    args: argparse.Namespace,
    report: Report,
) -> None:
    out_root = args.out_vault
    out_root.mkdir(parents=True, exist_ok=True)
    copies: dict[str, Path] = {}
    for note in notes:
        report.unresolved_links += rewrite_links(note, notes_by_file, ambiguous)
        for origin, rel in collect_images(note, args.images_dir, report):
            copies[rel] = origin
        self_link = link_target(note, ambiguous)
        related = {link_target(notes_by_file[f], ambiguous) for f in note.related_files if f in notes_by_file}
        write_note(note, out_root, sorted(related - {self_link}))
        report.notes += 1
    for rel, origin in sorted(copies.items()):
        destination = out_root / rel
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(origin, destination)
        report.images += 1
    config = out_root / ".kglite"
    config.mkdir(exist_ok=True)
    (config / "vault.yaml").write_text(vault_yaml(args), encoding="utf-8")


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    started = time.monotonic()
    report = Report()
    table = dict(META_NORMALIZERS)
    if args.meta_map:
        for key, mapping in json.loads(args.meta_map.read_text(encoding="utf-8")).items():
            table[normalize_key(key)] = {k.lower(): v for k, v in mapping.items()}

    toc = load_toc(args.toc) if args.toc else {}
    notes, notes_by_file, ambiguous = plan(args, table, toc, report)
    if args.dry_run:
        report.notes = len(notes)
        report.seconds = time.monotonic() - started
        print(report.render())
        return 0

    out_root = args.out_vault
    write_vault(notes, notes_by_file, ambiguous, args, report)
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
