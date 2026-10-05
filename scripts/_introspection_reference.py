"""Introspection reference tables for docs/python/introspection-reference.md.

Keys come from the Rust code that builds the dicts and the `.pyi` docstring
that describes them; types, presence and the schema/describe wording live
here. Every table is cross-checked, so a key added, renamed or removed in the
source fails `render_docs_facts.py --check` until this file and the page
agree.
"""

from __future__ import annotations

from pathlib import Path
import re

REPO_ROOT = Path(__file__).resolve().parents[1]
PYAPI = REPO_ROOT / "crates" / "kglite-py" / "src" / "graph" / "pyapi"
INTROSPECTION_RS = PYAPI / "kg_introspection.rs"
CORE_RS = PYAPI / "kg_core.rs"
DESCRIBE_DIR = REPO_ROOT / "crates" / "kglite" / "src" / "graph" / "introspection"
STUB = REPO_ROOT / "kglite" / "__init__.pyi"

# key -> (type, present). Order of the rendered table follows the Rust source.
GRAPH_INFO: dict[str, tuple[str, str]] = {
    "node_count": ("int", "always"),
    "node_capacity": ("int", "always"),
    "node_tombstones": ("int", "always"),
    "edge_count": ("int", "always"),
    "edge_capacity": ("int", "always"),
    "edge_tombstones": ("int", "always"),
    "fragmentation_ratio": ("float", "always"),
    "type_count": ("int", "always"),
    "property_index_count": ("int", "always"),
    "composite_index_count": ("int", "always"),
    "format_version": ("int", "always"),
    "library_version": ("str", "always"),
    "user_schema_version": ("int", "always"),
    "storage_mode": ("str", "always"),
    "columnar_heap_bytes": ("int", "always"),
    "columnar_is_mapped": ("bool", "always"),
    "edges_mapped": ("bool", "always"),
    "edge_property_overlay_rows": ("int", "always"),
    "memory_limit": ("int or None", "always"),
    "columnar_total_rows": ("int", "always"),
    "columnar_live_rows": ("int", "always"),
    "auto_vacuum_threshold": ("float or None", "always"),
    "auto_vacuums_run": ("int", "always"),
    "valid_time_default": ("dict", "always"),
    "build": ("dict", "Graph built by `from_blueprint()`"),
    "advisories": ("list of dict", "Graph loaded from a file whose data shows an older build's defect"),
}

# Keys whose pyi bullet is a structure description, not a one-line meaning.
GRAPH_INFO_MEANING = {
    "valid_time_default": "The default an undated statement reads; see the nested keys below.",
    "build": "Advisories of the build, by group; see the nested keys below. Saved with the graph and restored on load.",
    "advisories": "Known-defect findings from the load. Each entry also raised one `UserWarning`.",
}

# (builder variable, python name) -> nested keys: (type, meaning)
NESTED: dict[str, tuple[str, dict[str, tuple[str, str]]]] = {
    "default_dict": (
        "valid_time_default",
        {
            "effective": ("str", "Default in force now: `'today'`, `'all'` or a date."),
            "stored": ("str", "Part of the default saved in the file."),
        },
    ),
    "out": (
        "build",
        {
            "summary": ("dict[str, int]", "Count of advisories per group, over all of them."),
            "diagnostics": ("list of dict", "At most the 100 most severe advisories."),
        },
    ),
    "entry:build": (
        "build.diagnostics[]",
        {
            "group": ("str", "`declarations`, `stubs`, `data_shape`, `data_quality` or `cosmetic`."),
            "kind": ("str", "Stable advisory kind."),
            "message": ("str", "Human-readable text."),
        },
    ),
    "entry:advisories": (
        "advisories[]",
        {
            "code": ("str", "Stable advisory code."),
            "writer": ("str", "Oldest kglite version that wrote the data."),
            "message": ("str", "Human-readable text."),
            "affected": ("list of str", "Matched node types."),
        },
    ),
}

SCHEMA_TOP = {
    "node_types": ("dict", "`{type_name: node type entry}`."),
    "connection_types": ("dict", "`{relationship_type: relationship type entry}`."),
    "indexes": ("list of str", 'One `"Type.property"` string per index.'),
    "node_count": ("int", "Total live nodes."),
    "edge_count": ("int", "Total live edges."),
}
SCHEMA_NODE = {
    "count": ("int", "Nodes of the type."),
    "properties": ("dict[str, str]", '`{property_name: type_name}`, for example `{"age": "Int64"}`.'),
}
SCHEMA_CONN = {
    "count": ("int", "Relationships of the type."),
    "source_types": ("list of str", "Node types at the start of the relationship."),
    "target_types": ("list of str", "Node types at the end of the relationship."),
    "properties": (
        "dict[str, str]",
        "`{property_name: type_name}` of the relationship's own properties; `{}` if none.",
    ),
}

# Elements of the describe() XML: (element, appears in, attributes / content).
DESCRIBE_ELEMENTS: list[tuple[str, str, str]] = [
    ("graph", "every call", "Root. `kglite_version`; the overview adds `nodes`, `edges`."),
    ("conventions", "overview", "Text: node `.id` and `.title`, and the special property kinds present."),
    ("read-only", "overview", "Text; present when `read_only(True)` is set."),
    ("schema-locked", "overview", "Text; present after `lock_schema()`."),
    ("user-schema-version", "overview", "Text; present when `schema_version` is not 0."),
    ("data-advisory", "overview", "`code`, `writer`; text is the message. One per load advisory."),
    (
        "valid-time-default",
        "overview",
        "`effective`, `stored`; present when the default is not `today` on a graph that declares validity.",
    ),
    ("ontology", "overview, `types=`", "`classes`, `relationships`; the declared semantic layer."),
    ("types", "overview", "Wraps one `type` per node type."),
    ("type", "overview, `types=`", "`name`, `count`; children below."),
    ("properties", "inside `type`, `<conn>` deep-dive", "One `prop` per property."),
    ("prop", "inside `properties`", "`name`, `type`, `unique`, `coverage` (only if under 100%), `vals`."),
    ("example", "inside `type`, `types=`", "`query`: a Cypher query anchored on the type's identifier."),
    ("connections", "overview, `connections=`", "Wraps relationship entries; inside `type` it wraps `out` and `in`."),
    ("conn", "overview, `connections=True`", "`type`, `count`, `from`, `to`, `properties` (`name:Type` list)."),
    ("out", "inside `connections`", "`type`, `target`, `count`."),
    ("in", "inside `connections`", "`type`, `source`, `count`."),
    ("samples", "inside `type`, `connections=[...]`", "Sample `node` or `edge` elements."),
    (
        "endpoints",
        "`connections=[...]`",
        "One `pair` (`from`, `to`, `count`) per endpoint pair; `<more pairs= edges=/>` marks a hidden tail.",
    ),
    ("type_search", "`type_search=`", "`pattern`, `matches`, `depth`; one `match` per type plus a `hint`."),
    ("embeddings", "inside `type` or a relationship", "`text_col`, `dim`, `count`."),
    ("text_index", "inside `type`", "`text_col`: a BM25 text index."),
    ("skills", "overview", "Graph-carried skills; present only when the graph carries some."),
    ("recipes", "overview", "Graph-carried recipe queries; present only when the graph carries some."),
    (
        "extensions",
        "overview",
        "Hint elements: `algorithms`, `rules`, `cypher`, `fluent_api`, `connections`, `temporal`, "
        "`bug_report`, `indexing`; `timeseries` and `spatial` when used.",
    ),
    ("type_distribution", "extreme-scale overview", "`by_size` buckets and a `top` list."),
    ("connection_summary", "extreme-scale overview", "`count`; a `top` list and a `more` marker."),
    ("exploration_hints", "overview", "Lists `disconnected` types and `join_candidates`."),
    ("more", "any truncated list", "`count` (or `pairs`/`edges`, `out`/`in`) and a `hint` for the next call."),
]

DESCRIBE_MODES: list[tuple[str, str]] = [
    ("`describe()`", "Overview: inventory of types and connections; large graphs get a summary."),
    ("`describe(types=[...])`", "Detail for named types: properties, example, connections, samples."),
    ("`describe(type_search='x')`", "Types whose name contains `x`, with neighbours."),
    ("`describe(connections=True)`", "Every relationship type with counts, endpoint types and properties."),
    ("`describe(connections=[...])`", "Deep dive per relationship type: endpoint pairs, property stats, samples."),
    (
        "`describe(cypher=...)`, `describe(fluent=...)`",
        "Reference text for the Cypher dialect or the fluent API, or one topic of each.",
    ),
]


def _fn_body(source: str, signature: str) -> str:
    start = source.index(signature)
    depth = 0
    began = False
    for i in range(source.index("{", start), len(source)):
        if source[i] == "{":
            depth += 1
            began = True
        elif source[i] == "}":
            depth -= 1
            if began and depth == 0:
                return source[start : i + 1]
    raise ValueError(f"unterminated fn: {signature}")


def _keys(body: str, var: str) -> list[str]:
    return re.findall(rf'\b{re.escape(var)}\s*\.set_item\(\s*"(\w+)"', body)


def _same(what: str, found: list[str], declared: dict) -> None:
    if set(found) != set(declared) or len(found) != len(set(found)):
        raise ValueError(
            f"{what}: source keys {sorted(set(found) ^ set(declared))} differ from "
            f"scripts/_introspection_reference.py (source {found})"
        )


def _role_free(text: str) -> str:
    text = re.sub(r":(?:meth|func|attr|class|ref):`~?([^`]+)`", lambda m: f"`{m.group(1)}`", text)
    return text.replace("``", "`").replace("|", "\\|")


def _stub_meanings() -> dict[str, str]:
    stub = STUB.read_text(encoding="utf-8")
    start = stub.index("    def graph_info(self)")
    end = stub.index("\n    def ", start + 10)
    doc = stub[start:end]
    meanings: dict[str, str] = {}
    for m in re.finditer(r"^ {16}- ``(\w+)``: (.*(?:\n {18,}\S.*)*)", doc, re.M):
        text = " ".join(m.group(2).split())
        first = re.split(r"(?<=[a-z)`])\.\s+(?=[A-Z])", text, maxsplit=1)[0].rstrip(".")
        meanings[m.group(1)] = (lambda t: t[:1].upper() + t[1:])(_role_free(first)) + "."
    return meanings


def _table(header: list[str], rows: list[list[str]]) -> str:
    lines = ["| " + " | ".join(header) + " |", "|" + "---|" * len(header)]
    lines += ["| " + " | ".join(r) + " |" for r in rows]
    return "\n".join(lines)


def graph_info_tables() -> str:
    source = INTROSPECTION_RS.read_text(encoding="utf-8")
    body = _fn_body(source, "fn graph_info(&self)")
    keys = _keys(body, "dict")
    _same("graph_info() keys", keys, GRAPH_INFO)
    meanings = _stub_meanings()
    _same("graph_info() stub bullets", list(meanings), GRAPH_INFO)
    rows = []
    for key in keys:
        kind, present = GRAPH_INFO[key]
        meaning = GRAPH_INFO_MEANING.get(key, meanings[key])
        rows.append([f"`{key}`", kind, present, meaning])
    out = [_table(["Key", "Type", "Present", "Meaning"], rows), ""]

    build_body = _fn_body(source, "fn build_info_dict")
    nested_rows = []
    sources = {
        "default_dict": (body, "default_dict"),
        "out": (build_body, "out"),
        "entry:build": (build_body, "entry"),
        "entry:advisories": (body, "entry"),
    }
    for var, (path, declared) in NESTED.items():
        found = _keys(*sources[var])
        _same(f"graph_info() nested {path}", found, declared)
        for key in found:
            kind, meaning = declared[key]
            nested_rows.append([f"`{path}`", f"`{key}`", kind, meaning])
    out.append(_table(["Under", "Key", "Type", "Meaning"], nested_rows))
    return "\n".join(out)


def schema_tables() -> str:
    body = _fn_body(CORE_RS.read_text(encoding="utf-8"), "fn schema(&self)")
    _same("schema() top-level keys", _keys(body, "result"), SCHEMA_TOP)
    _same("schema() node type entry", _keys(body, "type_dict"), SCHEMA_NODE)
    _same("schema() relationship type entry", _keys(body, "ct_dict"), SCHEMA_CONN)

    def rows(found: list[str], declared: dict) -> list[list[str]]:
        return [[f"`{k}`", declared[k][0], declared[k][1]] for k in found]

    header = ["Key", "Type", "Meaning"]
    return "\n\n".join(
        [
            "Top level:",
            _table(header, rows(_keys(body, "result"), SCHEMA_TOP)),
            "Node type entry, `schema()['node_types'][name]`:",
            _table(header, rows(_keys(body, "type_dict"), SCHEMA_NODE)),
            "Relationship type entry, `schema()['connection_types'][name]`:",
            _table(header, rows(_keys(body, "ct_dict"), SCHEMA_CONN)),
        ]
    )


def describe_tables() -> str:
    corpus = "\n".join(p.read_text(encoding="utf-8") for p in sorted(DESCRIBE_DIR.glob("*.rs")))
    missing = [name for name, _, _ in DESCRIBE_ELEMENTS if not re.search(rf"<{re.escape(name)}[ />\\]", corpus)]
    if missing:
        raise ValueError(f"describe() elements no longer rendered by the engine: {missing}")
    return "\n\n".join(
        [
            _table(["Call", "Returns"], [[a, b] for a, b in DESCRIBE_MODES]),
            _table(
                ["Element", "Appears in", "Attributes and content"],
                [[f"`<{n}>`", w, a] for n, w, a in DESCRIBE_ELEMENTS],
            ),
        ]
    )
