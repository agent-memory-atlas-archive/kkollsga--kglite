"""Contracts for the offline knowledge-base interface diagnostic."""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path

SCRIPT = Path(__file__).parents[1] / "examples" / "knowledge_base" / "interface_budget.py"
SPEC = importlib.util.spec_from_file_location("knowledge_base_interface", SCRIPT)
assert SPEC and SPEC.loader
budget = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(budget)


def fixture():
    routing = "Choose this route for a focused lookup.\n\nFetch the decisive context once, then stop."
    return {
        "jsonrpc": "2.0",
        "result": {
            "tools": [
                {
                    "name": "find_ø",
                    "description": routing,
                    "inputSchema": {
                        "type": "object",
                        "properties": {"query": {"type": "string", "description": "Søk etter café"}},
                    },
                    "outputSchema": {"type": "object", "properties": {"title": {"type": "string"}}},
                },
                {
                    "name": "expand",
                    "description": "Expand exact source.\n\nFetch the decisive context once, then stop.",
                    "inputSchema": {"type": "object", "properties": {"source": {"type": "string"}}},
                },
            ]
        },
    }


def test_counts_utf8_and_each_serialized_field():
    payload = fixture()
    tools = payload["result"]["tools"]
    report = budget.analyze(payload, source_bytes=999)

    assert report["tool_count"] == 2
    assert report["source_json_bytes"] == 999
    assert report["canonical_tools_bytes"] == budget.json_bytes(tools)
    assert report["tools"][0] == {
        "name": "find_ø",
        "description_bytes": len(tools[0]["description"].encode()),
        "input_schema_bytes": budget.json_bytes(tools[0]["inputSchema"]),
        "output_schema_bytes": budget.json_bytes(tools[0]["outputSchema"]),
        "tool_bytes": budget.json_bytes(tools[0]),
    }
    assert report["tools"][1]["output_schema_bytes"] == 0
    assert report["field_totals"]["tool_bytes"] == sum(budget.json_bytes(tool) for tool in tools)


def test_finds_repeated_routing_body_without_copying_its_text():
    repeated = budget.analyze(fixture())["repeated_text_blocks"]

    assert len(repeated) == 1
    assert repeated[0]["block_bytes"] == len("Fetch the decisive context once, then stop.".encode())
    assert repeated[0]["repeated_bytes_after_first"] == repeated[0]["block_bytes"]
    assert [place["tool"] for place in repeated[0]["occurrences"]] == ["find_ø", "expand"]
    assert "block" not in repeated[0]


def test_cli_accepts_bare_tools_array_and_records_file_bytes(tmp_path, capsys):
    path = tmp_path / "tools.json"
    path.write_text(json.dumps(fixture()["result"]["tools"], ensure_ascii=False))

    assert budget.main([str(path)]) == 0
    report = json.loads(capsys.readouterr().out)
    assert report["source_json_bytes"] == len(path.read_bytes())
    assert report["tool_count"] == 2
