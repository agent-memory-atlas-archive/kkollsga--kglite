"""Reading the committed read-compatibility fixtures and the answers pinned for them."""

from __future__ import annotations

import ast
import json
from pathlib import Path
import shutil

V7_HEADER = b"RGF\x07\x02"


def expected_answers(fixtures: Path, name: str) -> dict:
    """The answers the published wheel returned for fixture ``name``."""
    return json.loads((fixtures / f"{name}.expected.json").read_text(encoding="utf-8"))


def generator_queries(generator: Path, name: str) -> dict:
    """The queries an expectation was captured with, read from the fixture generator
    so the two files cannot drift into asserting different things.

    The 0.19.0 wheel answered them over every version; an undated statement on a
    graph that declares validity now means "valid today", so each without a context of its own
    is sent under ``FOR VALID_TIME ALL`` (a no-op on a graph with no declaration)."""
    for node in ast.parse(generator.read_text(encoding="utf-8")).body:
        if isinstance(node, ast.Assign) and node.targets[0].id == name:  # type: ignore[attr-defined]
            queries = ast.literal_eval(node.value)
            return {
                key: query if query.startswith("FOR VALID_TIME") else f"FOR VALID_TIME ALL {query}"
                for key, query in queries.items()
            }
    raise AssertionError(f"{name} is gone from the fixture generator")


def copy_fixture(source: Path, tmp_path: Path, name: str | None = None) -> Path:
    """Copy a fixture into ``tmp_path``: loading a durable or disk directory rewrites it."""
    target = tmp_path / (name or source.name)
    if source.is_dir():
        shutil.copytree(source, target)
    else:
        shutil.copy2(source, target)
    return target
