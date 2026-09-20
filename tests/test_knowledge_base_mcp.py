"""Real MCP contracts for the synthetic knowledge-base recipe."""

from __future__ import annotations

import importlib.util
from pathlib import Path

from tests.test_mcp_server_bundled_wheel import _spawn_wheel
from tests.test_mcp_server_smoke import _is_error, _text_content

SCRIPT = Path(__file__).parents[1] / "examples" / "knowledge_base" / "knowledge_base.py"
SPEC = importlib.util.spec_from_file_location("knowledge_base_mcp_example", SCRIPT)
assert SPEC and SPEC.loader
kb = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(kb)


def test_vault_recipe_uses_native_defaults_and_validation(tmp_path):
    vault = tmp_path / "vault"
    kb.build(vault)
    client = _spawn_wheel(
        [
            "--vault",
            str(vault),
            "--vault-cache",
            "none",
            "--mcp-config",
            str(vault / "mcp.yaml"),
        ],
        cwd=vault,
    )
    try:
        tools = {tool["name"] for tool in client.list_tools()}
        count = client.call_tool(
            "cypher_query",
            {"query": "MATCH (n:Article) RETURN count(n) AS total"},
        )
        omitted = client.call_tool(
            "run_recipe_query",
            {"recipe": "help", "query": "article_index", "variables": {}},
        )
        override = client.call_tool(
            "run_recipe_query",
            {"recipe": "help", "query": "article_index", "variables": {"limit": 1}},
        )
        invalid = client.call_tool(
            "run_recipe_query",
            {"recipe": "help", "query": "article_index", "variables": {"limit": 0}},
        )
        source = client.call_tool("read_source", {"file_path": "Sources/api.html"})
    finally:
        client.shutdown()

    assert {"cypher_query", "list_recipe_queries", "read_source", "run_recipe_query"} <= tools
    assert count["structuredContent"]["rows"] == [[3]]
    assert omitted["structuredContent"]["result"]["row_count"] == 2
    assert override["structuredContent"]["result"]["row_count"] == 1
    assert not _is_error(omitted)
    assert not _is_error(override)
    assert _is_error(invalid)
    assert "$.limit" in _text_content(invalid)
    source_text = _text_content(source)
    assert "Client.connect" in source_text
    assert "timeout: int = 30" in source_text
    assert "options: dict = {&quot;mode&quot;: &quot;safe&quot;}" in source_text
