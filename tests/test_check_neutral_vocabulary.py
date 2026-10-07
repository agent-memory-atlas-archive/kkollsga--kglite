"""Red proofs for scripts/check_neutral_vocabulary.py.

The gate is only worth its place in `make gate` if each forbidden form can
turn it red and the ordinary-English uses cannot.
"""

from __future__ import annotations

import importlib.util
from pathlib import Path
import sys

import pytest

_SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "check_neutral_vocabulary.py"
_SPEC = importlib.util.spec_from_file_location("_check_neutral_vocabulary", _SCRIPT)
assert _SPEC is not None and _SPEC.loader is not None
_MODULE = importlib.util.module_from_spec(_SPEC)
sys.modules[_SPEC.name] = _MODULE
_SPEC.loader.exec_module(_MODULE)


def _flagged(text: str, path: str = "docs/guide.md") -> bool:
    return bool(_MODULE.violations({path: text}))


@pytest.mark.parametrize(
    "text",
    [
        "MATCH (w:Wellbore) RETURN w",
        "the Sodir dataset",
        "CREATE (a)-[:HAS_OPERATOR]->(b)",
        "HAS_LICENSEE edges",
        "MATCH (l:Licence) RETURN l",
        "graph.select('Licence')",
        "edge: 'IN_LICENCE'",
        "(:Prospect)-[:BECAME_DISCOVERY]->(d)",
        "graph.select('Discovery')",
        "a petroleum graph",
    ],
)
def test_registry_vocabulary_is_flagged(text):
    assert _flagged(text)


@pytest.mark.parametrize(
    "text",
    [
        "Tool discovery: graph_overview is always registered.",
        'pub(crate) const DISCOVERY_STEER: &str = "x";',
        "discovery_summary() returns the hint",
        "A software licence line above the header.",
        "Licence verification is wheel-only.",
        "Refuse the licence rather than assume the gate.",
        "def test_tier2_has_operators_and_functions(self):",
        "MATCH (p:Project)-[:MANAGED_BY]->(o:Company)",
    ],
)
def test_ordinary_english_and_neutral_names_pass(text):
    assert not _flagged(text)


def test_allowlisted_paths_may_carry_the_vocabulary():
    assert not _flagged("kglite.datasets.sodir.wrapper", "stubtest_allowlist.txt")
    assert not _flagged("Wellbore", "CHANGELOG.md")
    assert _flagged("Wellbore", "docs/python/guides/other.md")


def test_every_allowlist_entry_names_a_reason():
    assert all(reason.strip() for _, reason in _MODULE.ALLOWLIST)
