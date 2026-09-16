#!/usr/bin/env python3
"""A code graph that carries its own review methodology and the queries it names.

Demonstrates: ``set_recipe`` / ``set_skill``, the ``KgliteSkill`` and
``KgliteRecipe`` system labels, and the ``<skills>`` / ``<recipes>`` sections
``describe()`` renders from them.

A skill that says "find a function's callers" is worth much more when the
exact, parameter-checked query is in the same file. Both travel inside the
``.kgl``, so serving this graph over MCP with ``skills: true`` gives an agent
the methodology *and* ``run_recipe_query("code_review", "callers_page", ...)``
with nothing else to ship.

Run: ``python examples/code_review_graph_skills.py``
"""

from pathlib import Path
import tempfile

import pandas as pd

import kglite

# -- A tiny code graph (inline DataFrames) ---------------------------------

functions = pd.DataFrame(
    {
        "id": [1, 2, 3, 4, 5],
        "title": ["parse", "tokenize", "normalize", "run", "test_parse"],
        "qualified_name": [
            "parser::parse",
            "lexer::tokenize",
            "lexer::normalize",
            "cli::run",
            "tests::test_parse",
        ],
        "file_path": [
            "src/parser.rs",
            "src/lexer.rs",
            "src/lexer.rs",
            "src/cli.rs",
            "tests/parser.rs",
        ],
        "line_number": [42, 17, 63, 8, 11],
        "is_test": [False, False, False, False, True],
    }
)

calls = pd.DataFrame(
    {
        "source": [1, 2, 4, 5],
        "target": [2, 3, 1, 1],
    }
)

graph = kglite.KnowledgeGraph()
graph.add_nodes(functions, "Function", "id", "title")
graph.add_connections(calls, "CALLS", "Function", "source", "Function", "target")

# -- Recipe queries: the exact operations the methodology names -------------
#
# Every stored query must parse, must be read-only, and its `parameters`
# schema must match its `$parameters` exactly — `set_recipe` refuses anything
# an MCP host would have skipped at boot, so a bad query fails here rather
# than silently disappearing at serve time.

GROUP = "code_review"
GROUP_DESCRIPTION = "Exact Function-scoped operations for an initial code review."


def schema(properties):
    """A closed root schema requiring every property it declares."""
    return {
        "type": "object",
        "properties": properties,
        "required": sorted(properties),
        "additionalProperties": False,
    }


graph.set_recipe(
    GROUP,
    "target_coverage",
    "Report, for each requested qualified name, whether it exists and how many tests reach it.",
    "UNWIND $requested AS wanted "
    "MATCH (f:Function) WHERE f.qualified_name = wanted "
    "OPTIONAL MATCH (t:Function)-[:CALLS]->(f) WHERE t.is_test = true "
    "RETURN f.qualified_name AS qualified_name, f.file_path AS file_path, "
    "count(t) AS covering_tests ORDER BY qualified_name",
    schema({"requested": {"type": "array", "items": {"type": "string"}}}),
    GROUP_DESCRIPTION,
)

graph.set_recipe(
    GROUP,
    "callers_page",
    "One page of direct callers of a function, filtered to test or non-test callers.",
    "MATCH (caller:Function)-[:CALLS]->(target:Function) "
    "WHERE target.qualified_name = $qualified_name AND caller.is_test = $is_test "
    "RETURN DISTINCT caller.qualified_name AS qualified_name, "
    "caller.file_path AS file_path, caller.line_number AS line_number "
    "ORDER BY qualified_name LIMIT 25",
    schema({"qualified_name": {"type": "string"}, "is_test": {"type": "boolean"}}),
)

graph.set_recipe(
    GROUP,
    "bounded_call_path",
    "Up to ten call paths of at most four hops between two functions.",
    "MATCH path = (start:Function)-[:CALLS*1..4]->(finish:Function) "
    "WHERE start.qualified_name = $start AND finish.qualified_name = $finish "
    "RETURN [n IN nodes(path) | n.qualified_name] AS hops, length(path) AS hop_count "
    "ORDER BY hop_count LIMIT 10",
    schema({"start": {"type": "string"}, "finish": {"type": "string"}}),
)

# -- The skill: methodology that routes to those queries -------------------
#
# `delivery` defaults to "lazy": an agent sees the description in the tool
# descriptions this skill references, and fetches the body below by calling
# `skill("code_review")`.

SKILL_BODY = """\
# Reviewing this code graph

Nodes are `Function`, carrying `qualified_name`, `file_path`, `line_number`
and `is_test`. Edges are `CALLS`, from caller to callee.

## Sequence

1. Resolve the names you were given with `target_coverage` before anything
   else. An empty caller list means "no callers" only once you know the
   function exists — otherwise it may mean you spelled it wrong.
2. Read callers a page at a time with `callers_page`, once with
   `is_test: false` for production callers and once with `is_test: true` for
   the tests that would notice a change.
3. For "can A reach B?", use `bounded_call_path` rather than an open-ended
   traversal. Four hops is the useful depth here; deeper answers stop being
   about the change under review.
4. Drop to `cypher_query` only for questions these three do not shape, and to
   `read_code_source` only once the graph has told you which lines to read.

## Recipes shipped in this graph

- `code_review/target_coverage` — existence plus test coverage, for a list of names.
- `code_review/callers_page` — direct callers, test or non-test, 25 at a time.
- `code_review/bounded_call_path` — up to ten paths of at most four hops.

For a diff-scoped review, compose the walk yourself: start from the functions
whose `file_path` the diff touched, then run step 2 on each.
"""

graph.set_skill(
    "code_review",
    "TRIGGER for reviewing a change to this codebase — who calls what, what a "
    "change would break, which tests cover it. Resolve names with "
    "target_coverage before reading an empty result as an absence. SKIP for "
    "whole-file reads (read_code_source) and for questions no stored recipe "
    "shapes (plain cypher_query).",
    body=SKILL_BODY,
    references_tools=["cypher_query", "run_recipe_query", "read_code_source"],
)

# -- Save: both layers travel inside the .kgl ------------------------------

with tempfile.TemporaryDirectory() as tmp:
    path = Path(tmp) / "code_review.kgl"
    graph.save(str(path))
    reopened = kglite.open(str(path))

    print("== skills carried in the graph ==")
    for skill in reopened.list_skills():
        print(f"  {skill['name']} [{skill['delivery']}] -> {skill['references_tools']}")
        print(f"    {skill['description'][:72]}...")

    print()
    print("== recipe queries carried in the graph ==")
    for query in reopened.list_recipes():
        params = ", ".join(sorted(query["parameters"]["properties"]))
        print(f"  {query['recipe']}/{query['name']}({params})")
        print(f"    {query['description']}")

    print()
    print("== running one of them, the way an agent would ==")
    rows = reopened.cypher(
        reopened.get_recipe(GROUP, "callers_page")["cypher"],
        params={"qualified_name": "parser::parse", "is_test": False},
    )
    print(f"  callers_page(parser::parse, is_test=False) -> {rows}")

    print()
    print("== describe() indexes both, and names neither label ==")
    document = reopened.describe()
    for line in document.splitlines():
        stripped = line.strip()
        if stripped.startswith(("<skills", "<skill ", "</skills", "<recipes", "<recipe ", "</recipes")):
            print(f"  {stripped}")
    assert "KgliteSkill" not in document, "the system labels stay out of type enumerations"
    assert "KgliteRecipe" not in document, "the system labels stay out of type enumerations"
    print()
    print(f"  node_types -> {reopened.node_types}")
    print("  ...but MATCH (s:KgliteSkill) still finds it:")
    print(f"  {reopened.cypher('MATCH (s:KgliteSkill) RETURN s.name AS name')}")
