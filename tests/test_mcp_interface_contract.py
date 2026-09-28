"""Exact MCP tools/list contracts for each public server mode."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from tests.test_mcp_server_smoke import _SKIP_REASON, _build_fixture_graph, _spawn

ROOT = Path(__file__).resolve().parent.parent
BASELINE = ROOT / "tests" / "api-baselines" / "mcp-tools.json"

pytestmark = pytest.mark.skipif(_SKIP_REASON is not None, reason=_SKIP_REASON or "")


def _tool_contract(client) -> list[dict]:
    tools = client.list_tools()
    return sorted(
        (
            {
                "name": tool["name"],
                "description": tool.get("description") or "",
                "inputSchema": tool.get("inputSchema") or {},
            }
            for tool in tools
        ),
        key=lambda tool: tool["name"],
    )


def _capture(args: list[str]) -> list[dict]:
    client = _spawn(args, env_remove=["GITHUB_TOKEN", "GH_TOKEN"])
    try:
        return _tool_contract(client)
    finally:
        client.shutdown()


def capture_mcp_contract(base: Path) -> dict[str, list[dict]]:
    base.mkdir(parents=True, exist_ok=True)
    graph = base / "fixture.kgl"
    _build_fixture_graph(graph)

    local_root = base / "local-root"
    local_root.mkdir()
    (local_root / "demo.py").write_text("print('hello')\n", encoding="utf-8")
    local_manifest = base / "local_mcp.yaml"
    local_manifest.write_text(
        f"name: Local Contract\nworkspace:\n  kind: local\n  root: {local_root}\n", encoding="utf-8"
    )

    custom_manifest = base / "custom_mcp.yaml"
    custom_manifest.write_text(
        "name: Custom Contract\n"
        "tools:\n"
        "  - name: people_in_city\n"
        "    description: Find people in one city.\n"
        '    cypher: "MATCH (p:Person {city: $city}) RETURN p.title AS name"\n'
        "    parameters:\n"
        "      city:\n"
        "        type: string\n"
        "        description: Exact city name.\n"
        "        required: true\n",
        encoding="utf-8",
    )

    # A recipe catalogue publishes the fixed pair and a named tool, each
    # taking `valid_at` beside the query's own variables.
    recipe_manifest = base / "recipe_mcp.yaml"
    recipe_manifest.write_text(
        "name: Recipe Contract\n"
        "extensions:\n"
        "  cypher_recipes:\n"
        "    people:\n"
        "      description: Person lookups.\n"
        "      queries:\n"
        "        in_city:\n"
        "          description: People in one city.\n"
        "          tool: people_by_city\n"
        "          parameters:\n"
        "            type: object\n"
        "            properties:\n"
        "              city: {type: string}\n"
        "            required: [city]\n"
        "            additionalProperties: false\n"
        '          cypher: "MATCH (p:Person {city: $city}) RETURN p.title AS name ORDER BY name"\n',
        encoding="utf-8",
    )

    return {
        "graph_readonly": _capture(["--graph", str(graph)]),
        "graph_writable": _capture(["--graph", str(graph), "--writable"]),
        # An operator-pinned write scope names its types in the cypher_query
        # description — an agent plans inside the ceiling instead of finding it
        # one refusal at a time, so the pinned wording is part of the contract.
        "graph_writable_scoped": _capture(["--graph", str(graph), "--writable", "--write-scope", "Person,City"]),
        "local_workspace": _capture(["--mcp-config", str(local_manifest)]),
        "manifest_tool": _capture(["--graph", str(graph), "--mcp-config", str(custom_manifest)]),
        "manifest_recipes": _capture(["--graph", str(graph), "--mcp-config", str(recipe_manifest)]),
    }


@pytest.fixture(scope="module")
def mcp_contract(tmp_path_factory):
    return capture_mcp_contract(tmp_path_factory.mktemp("mcp-interface"))


def test_mcp_tools_list_matches_reviewed_mode_schemas(mcp_contract):
    expected = json.loads(BASELINE.read_text(encoding="utf-8"))
    assert mcp_contract == expected, (
        "MCP tool names/descriptions/input schemas drifted; review and refresh mcp-tools.json"
    )


def test_recipe_tools_take_valid_at(mcp_contract):
    tools = {tool["name"]: tool for tool in mcp_contract["manifest_recipes"]}
    for name in ("run_recipe_query", "people_by_city"):
        assert tools[name]["inputSchema"]["properties"]["valid_at"]["type"] in ("string", ["string", "null"]), name
    assert "valid_at" not in tools["people_by_city"]["inputSchema"]["required"]


def test_operator_pinned_write_scope_is_stated_in_the_tool_description(mcp_contract):
    """The pin is access control the agent cannot read off its own arguments."""
    pinned = next(tool for tool in mcp_contract["graph_writable_scoped"] if tool["name"] == "cypher_query")
    unpinned = next(tool for tool in mcp_contract["graph_writable"] if tool["name"] == "cypher_query")
    assert "pinned write_scope to [Person, City]" in pinned["description"]
    assert "pinned" not in unpinned["description"], (
        "an unpinned server must not advertise a ceiling it does not enforce"
    )


def test_local_workspace_has_one_activation_tool(mcp_contract):
    tools = mcp_contract["local_workspace"]
    names = {tool["name"] for tool in tools}
    assert "set_root_dir" in names
    assert "repo_management" not in names


def test_cypher_query_declares_its_deadline_and_the_default(mcp_contract):
    """The MCP server adopts the shared 180 s default because a tool call has
    no cancel channel and a runaway read stalls the reload gate every later
    call goes through. A default nothing declares is one an agent cannot plan
    around, so the number is part of the published schema, not lore."""
    for mode, tools in mcp_contract.items():
        cypher = next((tool for tool in tools if tool["name"] == "cypher_query"), None)
        if cypher is None:
            continue
        properties = cypher["inputSchema"].get("properties", {})
        assert "timeout_ms" in properties, f"{mode}: cypher_query must expose timeout_ms"
        described = properties["timeout_ms"].get("description", "")
        assert "180000" in described, f"{mode}: the default must be named, got {described!r}"
        assert "0" in described, f"{mode}: the disable spelling must be named"


def test_a_recipe_whose_every_parameter_has_a_default_needs_no_required(tmp_path):
    """`required` may be left out of a recipe's parameters: absent means none.

    Red proof (user test 3, B4): boot refused the catalogue with "root
    required is required" even though every property had a default."""
    from tests.test_mcp_server_smoke import _text_content

    graph = tmp_path / "fixture.kgl"
    _build_fixture_graph(graph)
    manifest = tmp_path / "recipe_mcp.yaml"
    manifest.write_text(
        "name: Defaults Contract\n"
        "extensions:\n"
        "  cypher_recipes:\n"
        "    people:\n"
        "      description: Person lookups.\n"
        "      queries:\n"
        "        greeting:\n"
        "          description: Echo a word.\n"
        "          tool: echo_word\n"
        "          parameters:\n"
        "            type: object\n"
        "            properties:\n"
        "              x: {type: string, default: hello}\n"
        "            additionalProperties: false\n"
        '          cypher: "RETURN $x AS x"\n',
        encoding="utf-8",
    )
    client = _spawn(["--graph", str(graph), "--mcp-config", str(manifest)], env_remove=["GITHUB_TOKEN", "GH_TOKEN"])
    try:
        tools = {tool["name"]: tool for tool in client.list_tools()}
        assert "echo_word" in tools
        assert tools["echo_word"]["inputSchema"].get("required", []) == []
        assert "hello" in _text_content(client.call_tool("echo_word", {}))
    finally:
        client.shutdown()
