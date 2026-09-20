#!/usr/bin/env python3
"""Bounded, fixture-specific knowledge-base lifecycle using only stdlib.

This is deliberately a small auditable example, not a general HTML converter.
Every command writes only to the explicit --output path.
"""

from __future__ import annotations

import argparse
import hashlib
from html.parser import HTMLParser
import json
from pathlib import Path, PurePosixPath
import re
import shutil
import sys
import tempfile
import zipfile

HERE = Path(__file__).resolve().parent
MANIFEST = json.loads((HERE / "manifest.json").read_text(encoding="utf-8"))
PLACEHOLDER = re.compile(r"\b(?:TODO|TBD|PLACEHOLDER|lorem ipsum)\b", re.I)
API_SOURCE_HASH = "bf3070c666445ec55369edb8077984e766e0998407ddebba6b8229ffa560d63a"
API_SOURCE_SIGNATURE = 'connect(controller_id: str, timeout: int = 30, options: dict = {"mode": "safe"}) -> Session'


def table_row(*cells: str) -> str:
    return "| " + " | ".join(cells) + " |"


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()


class PageParser(HTMLParser):
    """Extract just the constructs present in the bounded synthetic corpus."""

    def __init__(self) -> None:
        super().__init__()
        self.title = ""
        self.blocks: list[dict[str, str]] = []
        self.references: list[str] = []
        self._tag = ""
        self._text: list[str] = []
        self._href: str | None = None
        self._rows: list[list[tuple[str, int, int]]] = []
        self._row: list[tuple[str, int, int]] | None = None
        self._cell: tuple[int, int] | None = None

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        values = dict(attrs)
        if tag in {"h1", "h2", "p", "li", "title"}:
            self._tag, self._text = tag, []
        elif tag == "a":
            self._href = values.get("href")
            if self._href:
                self.references.append(self._href)
        elif tag == "img" and values.get("src"):
            self.references.append(values["src"] or "")
        elif tag == "tr":
            self._row = []
        elif tag in {"th", "td"}:
            self._tag, self._text = tag, []
            self._cell = (int(values.get("rowspan") or 1), int(values.get("colspan") or 1))

    def handle_data(self, data: str) -> None:
        if self._tag:
            self._text.append(data)

    def handle_endtag(self, tag: str) -> None:
        text = " ".join("".join(self._text).split())
        if tag == "title" and self._tag == tag:
            self.title = text
        elif tag in {"h1", "h2", "p", "li"} and self._tag == tag and text:
            kind = "warning" if tag == "p" and "warning" in self.get_starttag_text().lower() else tag
            self.blocks.append({"kind": kind, "text": text})
        elif tag in {"th", "td"} and self._tag == tag and self._row is not None:
            rowspan, colspan = self._cell or (1, 1)
            self._row.append((text, rowspan, colspan))
            self._cell = None
        elif tag == "tr" and self._row is not None:
            self._rows.append(self._row)
            self._row = None
        elif tag == "table" and self._rows:
            grid = logical_grid(self._rows)
            self.blocks.append({"kind": "table", "text": json.dumps(grid)})
            self._rows = []
        if tag == self._tag:
            self._tag, self._text = "", []


def logical_grid(rows: list[list[tuple[str, int, int]]]) -> list[list[str]]:
    """Expand HTML row/column spans into an explicit rectangular logical grid."""
    grid: list[list[str | None]] = []
    pending: dict[tuple[int, int], str] = {}
    for row_no, cells in enumerate(rows):
        row: list[str | None] = []
        grid.append(row)
        col = 0
        for text, rowspan, colspan in cells:
            while (row_no, col) in pending:
                row.append(pending[(row_no, col)])
                col += 1
            for dc in range(colspan):
                row.append(text)
                for dr in range(1, rowspan):
                    pending[(row_no + dr, col + dc)] = text
            col += colspan
        while (row_no, col) in pending:
            row.append(pending[(row_no, col)])
            col += 1
    width = max(map(len, grid))
    return [[cell or "" for cell in row] + [""] * (width - len(row)) for row in grid]


def parse_page(path: Path) -> PageParser:
    parser = PageParser()
    parser.feed(path.read_text(encoding="utf-8"))
    return parser


def require_regular_within(path: Path, root: Path) -> None:
    """Reject links and paths outside the declared input/output root."""
    if path.is_symlink() or not path.is_file() or not path.resolve().is_relative_to(root.resolve()):
        raise ValueError(f"path must be a regular file inside {root}: {path}")


def render_note(page: dict[str, str], parsed: PageParser) -> str:
    lines = ["---", f"id: {page['id']}", "type: Article", f"source: Sources/{page['source']}", "---", ""]
    for block in parsed.blocks:
        kind, value = block["kind"], block["text"]
        if kind == "h1":
            lines += [f"# {value}", ""]
        elif kind == "h2":
            if page["id"] == "client-api" and value == "Client.connect":
                lines += [
                    "## Client." + API_SOURCE_SIGNATURE.replace(" -> ", " → "),
                    "",
                ]
                lines += [
                    "<!-- kglite owner: Client -->",
                    "<!-- kglite returns: Session -->",
                    f"<!-- kglite source_signature: {API_SOURCE_SIGNATURE} -->",
                    "<!-- kglite provenance: Sources/api.html#connect -->",
                    f"<!-- kglite source_sha256: {API_SOURCE_HASH} -->",
                    "",
                ]
            else:
                lines += [f"## {value}", ""]
        elif kind == "li":
            lines.append(f"1. {value}")
        elif kind == "warning":
            lines += [f"> [!warning] {value}", ""]
        elif kind == "table":
            grid = json.loads(value)
            lines += ["<!-- logical-grid: expanded rowspan/colspan; original HTML retained under Sources/ -->"]
            lines += ["| " + " | ".join(row) + " |" for row in grid]
            lines.insert(len(lines) - len(grid) + 1, "| " + " | ".join("---" for _ in grid[0]) + " |")
            lines.append("")
        else:
            lines += [value, ""]
    if page["id"] == "client-api":
        lines += [
            "### Parameters",
            "",
            table_row("name", "type", "default", "owner", "source_anchor", "source_sha256"),
            table_row("---", "---", "---", "---", "---", "---"),
            table_row("controller_id", "str", "required", "Client", "api.html#connect", API_SOURCE_HASH),
            table_row("timeout", "int", "30", "Client", "api.html#connect", API_SOURCE_HASH),
            table_row("options", "dict", '{"mode": "safe"}', "Client", "api.html#connect", API_SOURCE_HASH),
            "",
            "### Returns",
            "",
            table_row("name", "type", "owner", "source_anchor", "source_sha256"),
            table_row("---", "---", "---", "---", "---"),
            table_row("return", "Session", "Client", "api.html#connect", API_SOURCE_HASH),
            "",
            "### Exceptions",
            "",
            table_row("condition_id", "condition", "exception", "owner", "source_anchor", "source_sha256"),
            table_row("---", "---", "---", "---", "---", "---"),
            table_row(
                "value-error-empty-id",
                "controller_id is empty",
                "ValueError",
                "Client",
                "api.html#connect",
                API_SOURCE_HASH,
            ),
            table_row(
                "value-error-negative-timeout",
                "timeout is negative",
                "ValueError",
                "Client",
                "api.html#connect",
                API_SOURCE_HASH,
            ),
            table_row(
                "connection-error-unreachable",
                "controller cannot be reached",
                "ConnectionError",
                "Client",
                "api.html#connect",
                API_SOURCE_HASH,
            ),
            "",
        ]
    if parsed.references:
        lines += ["## Referenced source members", ""]
        lines += [f"- `{target}`" for target in parsed.references]
        lines.append("")
    return "\n".join(lines).rstrip() + "\n"


def build(output: Path, source_root: Path = HERE) -> None:
    manifest_path = source_root / "manifest.json"
    require_regular_within(manifest_path, source_root)
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    if output.exists() and any(output.iterdir()):
        raise ValueError(f"output must be absent or empty: {output}")
    output.mkdir(parents=True, exist_ok=True)
    inventory, mappings, refs, index = [], [], [], []
    originals = source_root / "originals"
    for source in sorted(p for p in originals.rglob("*") if p.is_file()):
        require_regular_within(source, originals)
        relative = source.relative_to(originals)
        mirror = output / "Sources" / relative
        mirror.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, mirror)
        inventory.append(
            {"path": f"Sources/{relative.as_posix()}", "size": source.stat().st_size, "sha256": sha256(source)}
        )
    for page in manifest["pages"]:
        source = originals / page["source"]
        parsed = parse_page(source)
        note = output / page["output"]
        note.parent.mkdir(parents=True, exist_ok=True)
        note.write_text(render_note(page, parsed), encoding="utf-8")
        mappings.append({"source": page["source"], "note": page["output"], "id": page["id"]})
        refs.extend({"source": page["source"], "target": target} for target in parsed.references)
        index.append(
            {
                "id": page["id"],
                "title": parsed.title,
                "source": page["source"],
                "note": page["output"],
                "blocks": parsed.blocks,
            }
        )
    private_source = source_root / "private" / "memory.md"
    if private_source.exists():
        require_regular_within(private_source, source_root)
        private_out = output / "Memories" / "memory.md"
        private_out.parent.mkdir(parents=True)
        shutil.copy2(private_source, private_out)
    workflow_out = output / "Workflows" / "reset-review.md"
    workflow_out.parent.mkdir(parents=True)
    workflow_source = source_root / "workflows" / "reset-review.md"
    require_regular_within(workflow_source, source_root)
    shutil.copy2(workflow_source, workflow_out)
    annotation_source = source_root / "overlays" / "reset-annotation.json"
    require_regular_within(annotation_source, source_root)
    annotation = json.loads(annotation_source.read_text(encoding="utf-8"))
    annotation_out = output / "Annotations" / "reset-annotation.json"
    annotation_out.parent.mkdir(parents=True)
    annotation_out.write_text(json.dumps(annotation, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    recipe_out = output / ".kglite" / "recipes" / "article-index.md"
    recipe_out.parent.mkdir(parents=True)
    recipe_source = source_root / ".kglite" / "recipes" / "article-index.md"
    require_regular_within(recipe_source, source_root)
    shutil.copy2(recipe_source, recipe_out)
    vault_config_source = source_root / ".kglite" / "vault.yaml"
    require_regular_within(vault_config_source, source_root)
    shutil.copy2(vault_config_source, output / ".kglite" / "vault.yaml")
    mcp_source = source_root / "mcp.yaml"
    require_regular_within(mcp_source, source_root)
    shutil.copy2(mcp_source, output / "mcp.yaml")
    report = {"inventory": inventory, "mappings": mappings, "references": refs, "index": index}
    (output / "build-report.json").write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    check(output)


def check(vault: Path) -> None:
    report = json.loads((vault / "build-report.json").read_text(encoding="utf-8"))
    errors: list[str] = []
    expected_source_paths = {
        path.relative_to(vault).as_posix() for path in (vault / "Sources").rglob("*") if path.is_file()
    }
    actual_source_paths = {item["path"] for item in report["inventory"]}
    declared_inventory = {item["path"]: item for item in MANIFEST["source_inventory"]}
    if actual_source_paths != expected_source_paths or actual_source_paths != set(declared_inventory):
        errors.append("source inventory path set mismatch")
    for item in report["inventory"]:
        path = vault / item["path"]
        if not path.is_file() or path.stat().st_size != item["size"] or sha256(path) != item["sha256"]:
            errors.append(f"inventory mismatch: {item['path']}")
        if item != declared_inventory.get(item["path"]):
            errors.append(f"inventory differs from independent fixture contract: {item['path']}")
    actual_mappings = {(x["source"], x["note"], x["id"]) for x in report["mappings"]}
    expected_mappings = {(x["source"], x["output"], x["id"]) for x in MANIFEST["pages"]}
    if actual_mappings != expected_mappings:
        errors.append("page mapping mismatch")
    actual_refs = {(x["source"], x["target"]) for x in report["references"]}
    expected_refs = {tuple(x) for x in MANIFEST["required_references"]}
    missing = expected_refs - actual_refs
    if missing:
        errors.append(f"lost references: {sorted(missing)}")
    unexpected = actual_refs - expected_refs
    if unexpected:
        errors.append(f"unaccounted references: {sorted(unexpected)}")
    searchable = "\n".join((vault / x["note"]).read_text(encoding="utf-8") for x in report["mappings"])
    if PLACEHOLDER.search(searchable):
        errors.append("placeholder text remains")
    expected_grid = [["Mode", "Allowed range", "Allowed range"], ["Mode", "Minimum", "Maximum"], ["Safe", "1", "5"]]
    procedure_text = (vault / "Articles" / "reset-controller.md").read_text(encoding="utf-8")
    expected_grid_lines = ["| " + " | ".join(row) + " |" for row in expected_grid]
    if not all(line in procedure_text for line in expected_grid_lines):
        errors.append("merged table logical grid changed")
    api_text = (vault / "Articles" / "client-api.md").read_text(encoding="utf-8")
    expected_signature = (
        "Owner: Client. Signature: connect(controller_id: str, timeout: int = 30, "
        'options: dict = {"mode": "safe"}) -> Session.'
    )
    if api_text.count(expected_signature) != 1:
        errors.append("quoted or nested signature default changed")
    expected_value_errors = {
        "Raises ValueError when controller_id is empty.",
        "Raises ValueError when timeout is negative.",
    }
    if any(api_text.count(condition) != 1 for condition in expected_value_errors):
        errors.append("repeated exception conditions were merged or lost")
    if "> [!warning] Warning: do not remove power after Apply until Ready appears." not in procedure_text:
        errors.append("procedure warning missing")
    for source, target in expected_refs:
        note = next(item["note"] for item in report["mappings"] if item["source"] == source)
        if f"- `{target}`" not in (vault / note).read_text(encoding="utf-8"):
            errors.append(f"rendered reference missing: {source} -> {target}")
    annotation = json.loads((vault / "Annotations" / "reset-annotation.json").read_text(encoding="utf-8"))
    source = vault / "Sources" / annotation["source"]
    if annotation["source_sha256"] != sha256(source):
        errors.append("reviewed annotation has stale source evidence")
    if errors:
        raise ValueError("; ".join(errors))
    print(
        f"checked {len(report['inventory'])} originals, {len(actual_refs)} references, {len(report['mappings'])} pages"
    )


def query(vault: Path, topic: str, page_size: int) -> None:
    if page_size < 1 or page_size > 20:
        raise ValueError("page-size must be between 1 and 20")
    report = json.loads((vault / "build-report.json").read_text(encoding="utf-8"))
    wanted = "reset-controller" if topic == "procedure" else "client-api"
    page = next(x for x in report["index"] if x["id"] == wanted)
    blocks = page["blocks"]
    pages = [blocks[i : i + page_size] for i in range(0, len(blocks), page_size)]
    payload = {
        "source": page["source"],
        "total": len(blocks),
        "returned": len(blocks),
        "truncated": False,
        "pages": pages,
    }
    print(json.dumps(payload, indent=2))


def share(vault: Path, output: Path) -> None:
    check(vault)
    if output.exists():
        raise ValueError(f"output already exists: {output}")
    members: list[Path] = []
    for root in MANIFEST["public_roots"]:
        path = vault / root
        if not path.exists() or path.is_symlink():
            raise ValueError(f"required public root missing or unsafe: {root}")
        members.extend([path] if path.is_file() else sorted(p for p in path.rglob("*") if p.is_file()))
    for path in members:
        require_regular_within(path, vault)
    with zipfile.ZipFile(output, "x", zipfile.ZIP_DEFLATED) as archive:
        for path in members:
            archive.write(path, path.relative_to(vault))
    verify_share(output)


def verify_share(archive_path: Path) -> None:
    canary = MANIFEST["private_canary"].encode()
    with zipfile.ZipFile(archive_path) as archive:
        names = archive.namelist()
        if len(names) != len(set(names)) or any(
            PurePosixPath(n).is_absolute() or ".." in PurePosixPath(n).parts for n in names
        ):
            raise ValueError("unsafe or duplicate archive member")
        for name in names:
            derived = name == ".kglite/graph.kgl" or name.startswith(".kglite/graph.kgl.")
            if name.startswith("Memories/") or derived or canary in archive.read(name):
                raise ValueError(f"private or derived content leaked: {name}")
        with tempfile.TemporaryDirectory(prefix="kglite-kb-share-") as temp:
            root = Path(temp)
            archive.extractall(root)
            report = json.loads((root / "build-report.json").read_text(encoding="utf-8"))
            for item in report["inventory"]:
                member = root / item["path"]
                if member.stat().st_size != item["size"] or sha256(member) != item["sha256"]:
                    raise ValueError(f"extracted source mismatch: {item['path']}")
            check(root)
    print(f"verified public archive: {len(names)} members; private canary absent")


def kglite_query(vault: Path, topic: str, page_size: int) -> None:
    try:
        from kglite import okf
    except ImportError as exc:
        raise RuntimeError("install kglite to use kglite-query; the other commands need only Python") from exc
    graph = okf.open(vault, dialect="obsidian")
    if page_size < 1 or page_size > 20:
        raise ValueError("page-size must be between 1 and 20")
    total = graph.cypher("MATCH (n:Article) RETURN count(n) AS total").to_list()[0]["total"]
    pages, warnings = [], []
    for offset in range(0, total, page_size):
        result = graph.cypher(
            "MATCH (n:Article) RETURN n.id AS id, n.title AS title ORDER BY id SKIP $offset LIMIT $limit",
            params={"offset": offset, "limit": page_size},
        )
        pages.append(result.to_list())
        warnings.extend(result.warnings)
    selected = "reset-controller" if topic == "procedure" else "client-api"
    flattened = [row for page in pages for row in page]
    if not any(str(row["id"]).endswith(selected) for row in flattened):
        raise ValueError(f"topic not found in source-backed graph: {topic}")
    recipe = graph.get_recipe("help", "article_index")
    payload = {
        "topic": topic,
        "total": total,
        "returned": len(flattened),
        "truncated": False,
        "pages": pages,
        "warnings": warnings,
        "recipe": {"name": "help.article_index", "parameters": recipe["parameters"]},
    }
    print(json.dumps(payload, indent=2))


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    commands = root.add_subparsers(dest="command", required=True)
    p = commands.add_parser("build")
    p.add_argument("--output", type=Path, required=True)
    p.add_argument("--source", type=Path, default=HERE)
    p = commands.add_parser("check")
    p.add_argument("--vault", type=Path, required=True)
    for name in ("query", "kglite-query"):
        p = commands.add_parser(name)
        p.add_argument("--vault", type=Path, required=True)
        p.add_argument("topic", choices=("procedure", "api"))
        p.add_argument("--page-size", type=int, default=2)
    p = commands.add_parser("share")
    p.add_argument("--vault", type=Path, required=True)
    p.add_argument("--output", type=Path, required=True)
    p = commands.add_parser("verify-share")
    p.add_argument("--archive", type=Path, required=True)
    return root


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        if args.command == "build":
            build(args.output, args.source)
        elif args.command == "check":
            check(args.vault)
        elif args.command == "query":
            query(args.vault, args.topic, args.page_size)
        elif args.command == "kglite-query":
            kglite_query(args.vault, args.topic, args.page_size)
        elif args.command == "share":
            share(args.vault, args.output)
        else:
            verify_share(args.archive)
    except (OSError, ValueError, RuntimeError, KeyError, json.JSONDecodeError, zipfile.BadZipFile) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
