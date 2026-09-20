#!/usr/bin/env python3
"""Measure the serialized shape of an offline MCP tools/list catalogue.

This reports UTF-8 bytes in the supplied JSON. It does not estimate tokens or
claim that a client presents every measured field to a model.
"""

from __future__ import annotations

import argparse
from collections import defaultdict
import hashlib
import json
from pathlib import Path
import re
import sys
from typing import Any

TEXT_KEYS = {"description", "instructions", "routing", "body"}
BLOCK_BREAK = re.compile(r"\n[ \t]*\n+")


def json_bytes(value: Any) -> int:
    """Return deterministic compact-JSON UTF-8 bytes for one value."""
    return len(json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode())


def tools_from(payload: Any) -> list[dict[str, Any]]:
    """Accept a tools/list result, its result envelope, or the tools array."""
    tools = payload
    if isinstance(payload, dict):
        tools = payload.get("tools")
        if tools is None and isinstance(payload.get("result"), dict):
            tools = payload["result"].get("tools")
    if not isinstance(tools, list):
        raise ValueError("expected a tools array, {tools: [...]}, or {result: {tools: [...]}}")
    if not all(isinstance(tool, dict) and isinstance(tool.get("name"), str) for tool in tools):
        raise ValueError("every tool must be an object with a string name")
    return tools


def text_blocks(value: Any, path: str = "") -> list[tuple[str, str]]:
    """Find exact paragraph blocks in descriptive fields within a tool."""
    found: list[tuple[str, str]] = []
    if isinstance(value, dict):
        for key, child in value.items():
            child_path = f"{path}/{key}"
            if key in TEXT_KEYS and isinstance(child, str):
                found.extend((child_path, block.strip()) for block in BLOCK_BREAK.split(child) if block.strip())
            else:
                found.extend(text_blocks(child, child_path))
    elif isinstance(value, list):
        for index, child in enumerate(value):
            found.extend(text_blocks(child, f"{path}/{index}"))
    return found


def analyze(payload: Any, source_bytes: int | None = None) -> dict[str, Any]:
    """Build a byte inventory without interpreting client or model behavior."""
    tools = tools_from(payload)
    per_tool: list[dict[str, Any]] = []
    occurrences: dict[str, list[dict[str, str]]] = defaultdict(list)

    for tool in tools:
        description = tool.get("description", "")
        if not isinstance(description, str):
            raise ValueError(f"tool {tool['name']!r} description must be a string")
        record = {
            "name": tool["name"],
            "description_bytes": len(description.encode()),
            "input_schema_bytes": json_bytes(tool["inputSchema"]) if "inputSchema" in tool else 0,
            "output_schema_bytes": json_bytes(tool["outputSchema"]) if "outputSchema" in tool else 0,
            "tool_bytes": json_bytes(tool),
        }
        per_tool.append(record)
        for path, block in text_blocks(tool):
            occurrences[block].append({"tool": tool["name"], "path": path})

    repeated = []
    for block, places in occurrences.items():
        if len(places) < 2:
            continue
        repeated.append(
            {
                "sha256": hashlib.sha256(block.encode()).hexdigest(),
                "block_bytes": len(block.encode()),
                "occurrences": places,
                "repeated_bytes_after_first": len(block.encode()) * (len(places) - 1),
            }
        )
    repeated.sort(key=lambda item: (-item["repeated_bytes_after_first"], item["sha256"]))

    return {
        "tool_count": len(tools),
        "source_json_bytes": source_bytes,
        "canonical_tools_bytes": json_bytes(tools),
        "field_totals": {
            "description_bytes": sum(tool["description_bytes"] for tool in per_tool),
            "input_schema_bytes": sum(tool["input_schema_bytes"] for tool in per_tool),
            "output_schema_bytes": sum(tool["output_schema_bytes"] for tool in per_tool),
            "tool_bytes": sum(tool["tool_bytes"] for tool in per_tool),
        },
        "tools": per_tool,
        "repeated_text_blocks": repeated,
        "assumptions": [
            "Byte counts use UTF-8; schema and tool counts use deterministic compact JSON.",
            "Repeated blocks are exact paragraphs in description, instructions, routing, or body fields; "
            "hashes replace their text.",
            "Dynamically returned skill bodies are invisible unless they occur in the supplied tools/list JSON.",
            "Catalogue bytes are not model tokens and do not show which fields a client projects into model context.",
        ],
    }


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    root.add_argument("catalogue", help="tools/list JSON path, or - for standard input")
    root.add_argument("--pretty", action="store_true", help="indent the JSON report")
    return root


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        raw = sys.stdin.buffer.read() if args.catalogue == "-" else Path(args.catalogue).read_bytes()
        report = analyze(json.loads(raw), len(raw))
    except (OSError, ValueError, TypeError, json.JSONDecodeError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2
    print(json.dumps(report, ensure_ascii=False, indent=2 if args.pretty else None, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
