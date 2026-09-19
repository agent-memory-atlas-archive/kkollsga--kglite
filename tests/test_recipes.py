"""Graph-carried recipes: the six Python methods and the contract they enforce.

A recipe query is one named, parameterised, read-only Cypher statement stored
as a node under the ``KgliteRecipe`` system label — hidden from every node-type
enumeration, ordinary to Cypher, persisted in the ``.kgl``. These tests pin the
Python surface: what each method returns, which exception class each refusal
raises, and that a JSON catalogue document in the MCP
``extensions.cypher_recipes`` shape round-trips through import/export.
"""

from __future__ import annotations

import json
from pathlib import Path

import pandas as pd
import pytest

import kglite
from kglite import KnowledgeGraph

FIXTURES = Path(__file__).parent / "fixtures" / "recipes"
CODE_REVIEW = FIXTURES / "code_review.json"

# The schema a statement with no `$parameters` carries. `set_recipe` stores
# exactly this when `parameters` is omitted.
EMPTY_SCHEMA = {
    "type": "object",
    "properties": {},
    "required": [],
    "additionalProperties": False,
}

LIMIT_SCHEMA = {
    "type": "object",
    "properties": {"limit": {"type": "integer", "minimum": 1, "maximum": 100}},
    "required": ["limit"],
    "additionalProperties": False,
}

DEEPEST = "MATCH (w:Well) RETURN w.title AS title ORDER BY w.id DESC LIMIT $limit"
COUNT = "MATCH (w:Well) RETURN count(w) AS wells"


@pytest.fixture
def g() -> KnowledgeGraph:
    graph = KnowledgeGraph()
    graph.add_nodes(
        pd.DataFrame({"id": [1, 2], "title": ["a", "b"]}),
        "Well",
        "id",
        node_title_field="title",
    )
    return graph


def _recipe_count(graph: KnowledgeGraph) -> int:
    return graph.cypher("MATCH (r:KgliteRecipe) RETURN count(r) AS c").to_dicts()[0]["c"]


def _store(graph: KnowledgeGraph, name: str = "deepest", **kwargs) -> dict:
    defaults = {
        "recipe": "wells",
        "name": name,
        "description": "Deepest wells first.",
        "cypher": DEEPEST,
        "parameters": LIMIT_SCHEMA,
        "recipe_description": "Asking this graph about wells.",
    }
    defaults.update(kwargs)
    return graph.set_recipe(**defaults)


# ── CRUD ───────────────────────────────────────────────────────────────────


def test_list_is_empty_until_a_recipe_is_set(g):
    assert g.list_recipes() == []

    stored = _store(g)

    listed = g.list_recipes()
    assert len(listed) == 1
    assert listed[0] == {
        "recipe": "wells",
        "name": "deepest",
        "description": "Deepest wells first.",
        "parameters": LIMIT_SCHEMA,
        "cypher": DEEPEST,
        "recipe_description": "Asking this graph about wells.",
        "tool": None,
    }
    assert stored["created"] is True


def test_get_returns_the_stored_record(g):
    _store(g)
    got = g.get_recipe("wells", "deepest")
    assert got["cypher"] == DEEPEST
    assert got["parameters"] == LIMIT_SCHEMA


def test_get_unknown_raises_node_not_found(g):
    with pytest.raises(kglite.NodeNotFoundError):
        g.get_recipe("wells", "absent")


def test_set_again_updates_in_place(g):
    _store(g)
    again = _store(g, description="Deepest wells, newest first.")

    assert again["created"] is False
    assert again["description"] == "Deepest wells, newest first."
    assert _recipe_count(g) == 1, "MERGE must not duplicate the node"


def test_a_second_query_inherits_the_group_description(g):
    _store(g)
    added = g.set_recipe("wells", "count", "How many wells.", COUNT)
    assert added["recipe_description"] == "Asking this graph about wells."
    assert added["parameters"] == EMPTY_SCHEMA, "an omitted schema is the empty closed one"


def test_the_first_query_in_a_group_must_bring_a_group_description(g):
    with pytest.raises(kglite.ArgumentError, match="recipe_description"):
        g.set_recipe("wells", "count", "How many.", COUNT)
    assert _recipe_count(g) == 0


def test_a_tool_name_round_trips_through_the_graph_and_the_catalogue(g, tmp_path):
    """`tool=` is the seventh key: stored, read back, exported and re-imported.

    The name is what an MCP server registers the query under, so losing it on
    a save would silently unpublish a tool the author declared.
    """
    stored = _store(g, tool="deepest_wells")
    assert stored["tool"] == "deepest_wells"
    assert g.get_recipe("wells", "deepest")["tool"] == "deepest_wells"
    assert _store(g, name="count", cypher=COUNT, parameters=None)["tool"] is None

    path = tmp_path / "recipes.json"
    g.export_recipes(str(path))
    document = json.loads(path.read_text(encoding="utf-8"))
    queries = document["wells"]["queries"]
    assert queries["deepest"]["tool"] == "deepest_wells"
    assert "tool" not in queries["count"], "an absent tool is absent, not null"

    fresh = KnowledgeGraph()
    fresh.import_recipes(str(path))
    assert fresh.get_recipe("wells", "deepest")["tool"] == "deepest_wells"
    assert fresh.get_recipe("wells", "count")["tool"] is None

    # And clearing it clears the stored property.
    _store(g, tool=None)
    assert g.get_recipe("wells", "deepest")["tool"] is None


@pytest.mark.parametrize("bad", ["", "9lives", "two words", "a.b", "a" * 65])
def test_an_illegal_tool_name_is_refused(g, bad):
    with pytest.raises(kglite.ArgumentError, match="tool name"):
        _store(g, tool=bad)
    assert _recipe_count(g) == 0


def test_delete_reports_whether_there_was_anything_to_delete(g):
    _store(g)
    assert g.delete_recipe("wells", "deepest") is True
    assert g.delete_recipe("wells", "deepest") is False
    assert g.list_recipes() == []


# ── Validation refusals ────────────────────────────────────────────────────


@pytest.mark.parametrize(
    ("label", "kwargs", "message"),
    [
        (
            "a mutation",
            {"cypher": "CREATE (:Well {id: 9})", "parameters": EMPTY_SCHEMA},
            "read-only",
        ),
        (
            "EXPLAIN",
            {"cypher": "EXPLAIN MATCH (w:Well) RETURN w", "parameters": EMPTY_SCHEMA},
            "EXPLAIN",
        ),
        (
            "a $param absent from the schema",
            {"cypher": "MATCH (w:Well) WHERE w.id = $wanted RETURN w", "parameters": EMPTY_SCHEMA},
            "parameter properties",
        ),
        (
            "an unsupported schema keyword",
            {
                "cypher": "MATCH (w:Well) WHERE w.title = $title RETURN w",
                "parameters": {
                    "type": "object",
                    "properties": {"title": {"type": "string", "pattern": "^a"}},
                    "required": ["title"],
                    "additionalProperties": False,
                },
            },
            "unsupported JSON Schema keywords",
        ),
        ("an empty description", {"description": "   "}, "description"),
        ("a non-identifier recipe", {"recipe": "two words"}, "identifier"),
        ("a non-identifier name", {"name": "a/b"}, "identifier"),
        ("empty cypher", {"cypher": "   ", "parameters": EMPTY_SCHEMA}, "cypher"),
    ],
)
def test_invalid_arguments_raise_argument_error(g, label, kwargs, message):
    with pytest.raises(kglite.ArgumentError, match=message):
        _store(g, **kwargs)
    assert _recipe_count(g) == 0, f"{label}: a refused write must leave the graph alone"


def test_read_only_refuses_writes_and_changes_nothing(g):
    _store(g)
    g.read_only(True)

    with pytest.raises(kglite.ArgumentError, match="read-only"):
        _store(g, name="count", cypher=COUNT, parameters=EMPTY_SCHEMA)
    with pytest.raises(kglite.ArgumentError, match="read-only"):
        g.delete_recipe("wells", "deepest")

    assert [r["name"] for r in g.list_recipes()] == ["deepest"]
    g.read_only(False)
    assert _store(g, name="count", cypher=COUNT, parameters=EMPTY_SCHEMA)["created"] is True


def test_a_schema_locked_graph_refuses_an_undeclared_recipe_type(g):
    """A locked schema stops recipes too, and says which type it does not know.

    The class is the Cypher path's own — `set_recipe` writes through `MERGE`,
    so the schema-lock refusal arrives wrapped as a Cypher execution failure
    rather than a `SchemaError`, exactly as a hand-written `CREATE` would.
    """
    g.lock_schema()
    with pytest.raises(kglite.CypherExecutionError, match="Unknown node type 'KgliteRecipe'"):
        _store(g)
    assert g.list_recipes() == []


# ── The hidden label ───────────────────────────────────────────────────────


def test_the_label_is_hidden_from_enumerations_but_not_from_cypher(g):
    _store(g)

    assert "KgliteRecipe" not in g.node_types
    assert "KgliteRecipe" not in g.describe()
    assert _recipe_count(g) == 1


# ── Persistence ────────────────────────────────────────────────────────────


def test_a_nested_schema_survives_a_save_load_round_trip(g, tmp_path):
    nested = {
        "type": "object",
        "properties": {
            "names": {"type": "array", "items": {"type": "string"}, "minItems": 1},
            "kind": {"type": "string", "enum": ["well", "field"]},
            "depth": {"type": "number", "minimum": 1, "maximum": 2.5},
        },
        "required": ["names", "kind", "depth"],
        "additionalProperties": False,
    }
    _store(
        g,
        cypher="MATCH (w:Well) WHERE w.title IN $names AND $kind IS NOT NULL AND w.id < $depth RETURN w.title AS title",
        parameters=nested,
    )
    path = str(tmp_path / "graph.kgl")
    g.save(path)

    reloaded = kglite.load(path)
    assert reloaded.get_recipe("wells", "deepest")["parameters"] == nested


# ── Import / export ────────────────────────────────────────────────────────


def test_import_reads_a_manifest_shaped_json_catalogue(g):
    written = g.import_recipes(str(CODE_REVIEW))

    assert written == [
        "code_review/affected_tests",
        "code_review/direct_callers",
        "code_review/resolve_function",
    ]
    listed = g.list_recipes()
    assert [r["name"] for r in listed] == [
        "affected_tests",
        "direct_callers",
        "resolve_function",
    ]
    assert all(
        r["recipe_description"] == "Exact Function-scoped operations for an initial code review." for r in listed
    )
    assert listed[2]["parameters"]["required"] == ["qualified_name"]


def test_import_reads_a_bare_catalogue_mapping(g, tmp_path):
    bare = json.loads(CODE_REVIEW.read_text(encoding="utf-8"))["extensions"]["cypher_recipes"]
    path = tmp_path / "bare.json"
    path.write_text(json.dumps(bare), encoding="utf-8")

    assert len(g.import_recipes(str(path))) == 3


MARKDOWN_RECIPE = """---
recipe: local
name: by_name
description: One function by name.
parameters:
  {type: object, properties: {name: {type: string}},
   required: [name], additionalProperties: false}
recipe_description: Reading a local code graph.
---

```cypher
MATCH (f:Function) WHERE f.name = $name RETURN f.qualified_name AS qn LIMIT 10
```
"""


def test_import_reads_a_markdown_recipe_file_and_a_directory(g, tmp_path):
    # The dialect a vault's `.kglite/recipes/` uses (VAULT.md §8), available
    # to any graph through the same method a JSON catalogue goes through.
    one = tmp_path / "by_name.md"
    one.write_text(MARKDOWN_RECIPE, encoding="utf-8")
    assert g.import_recipes(str(one)) == ["local/by_name"]

    stored = g.get_recipe("local", "by_name")
    assert stored["cypher"].startswith("MATCH (f:Function) WHERE f.name = $name")
    assert stored["recipe_description"] == "Reading a local code graph."
    # Nested, not flattened to a `properties.name.type` key.
    assert stored["parameters"]["properties"]["name"] == {"type": "string"}

    # A whole directory, with the group description inherited by the sibling
    # that omits it.
    folder = tmp_path / "recipes"
    folder.mkdir()
    (folder / "a_by_name.md").write_text(MARKDOWN_RECIPE, encoding="utf-8")
    (folder / "b_all.md").write_text(
        "---\nrecipe: local\nname: all_functions\ndescription: Every function.\n---\n\n"
        "```cypher\nMATCH (f:Function) RETURN f.qualified_name AS qn LIMIT 10\n```\n",
        encoding="utf-8",
    )
    assert sorted(g.import_recipes(str(folder))) == ["local/all_functions", "local/by_name"]
    assert g.get_recipe("local", "all_functions")["recipe_description"] == "Reading a local code graph."


def test_a_markdown_recipe_that_fails_validation_writes_nothing(g, tmp_path):
    folder = tmp_path / "recipes"
    folder.mkdir()
    (folder / "good.md").write_text(MARKDOWN_RECIPE, encoding="utf-8")
    (folder / "writes.md").write_text(
        "---\nrecipe: local\nname: bad\ndescription: A write.\n"
        "recipe_description: Reading a local code graph.\n---\n\n"
        "```cypher\nCREATE (:Function {name: 'x'})\n```\n",
        encoding="utf-8",
    )
    with pytest.raises(kglite.FileFormatError):
        g.import_recipes(str(folder))
    assert g.list_recipes() == [], "the batch is all-or-nothing"


def test_import_of_a_missing_path_raises_file_error(g, tmp_path):
    with pytest.raises(kglite.FileError):
        g.import_recipes(str(tmp_path / "nowhere.json"))


def test_import_of_yaml_says_convert_it_first(g):
    # A `.yaml` *catalogue* is still refused by name: the wheel links no
    # general YAML reader for that shape. (Markdown recipe files are read —
    # their schema goes through the non-flattening frontmatter parser.)
    with pytest.raises(kglite.FileFormatError, match="convert a YAML catalogue first"):
        g.import_recipes(str(Path("examples") / "local_code_review_mcp.yaml"))


def test_an_invalid_document_writes_nothing_and_names_the_query(g, tmp_path):
    broken = json.loads(CODE_REVIEW.read_text(encoding="utf-8"))
    queries = broken["extensions"]["cypher_recipes"]["code_review"]["queries"]
    queries["resolve_function"]["cypher"] = "CREATE (:Function {qualified_name: $qualified_name})"
    path = tmp_path / "broken.json"
    path.write_text(json.dumps(broken), encoding="utf-8")

    with pytest.raises(kglite.ArgumentError, match="resolve_function"):
        g.import_recipes(str(path))
    assert g.list_recipes() == [], "one bad query must leave the whole import unwritten"


def test_export_then_import_into_a_fresh_graph_is_identical(g, tmp_path):
    g.import_recipes(str(CODE_REVIEW))
    out = tmp_path / "recipes.json"

    assert g.export_recipes(str(out)) is None
    assert set(json.loads(out.read_text(encoding="utf-8"))) == {"code_review"}

    fresh = KnowledgeGraph()
    fresh.import_recipes(str(out))
    assert fresh.list_recipes() == g.list_recipes()
