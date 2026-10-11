"""Regenerate the WAL-format-11 golden fixtures (``tests/fixtures/durable_golden_v11/``).

The set is the six scenarios of ``build_durable_golden_fixtures`` plus
``text_index_full`` (the text-index build/drop op that format 11 added), written
by the **published kglite 0.19.7 wheel**, not the tree under test. The point is a
log the pre-swap engine wrote, replayed by whatever engine the tree has now.

Run it from an isolated environment, with the working directory outside the repo
so the repo's ``kglite/`` cannot shadow the wheel::

    uv venv "$S/venv" --python 3.14 && uv pip install --python "$S/venv/bin/python" 'kglite==0.19.7' pandas
    cd "$S" && "$S/venv/bin/python" <repo>/tests/fixtures/build_durable_golden_v11_fixtures.py

``EXPECTED`` is hand-derived from the scenario bodies; the generator reopens a
copy through the same wheel and refuses a scenario whose recovered state differs.
"""

from __future__ import annotations

from pathlib import Path
import sys
import tempfile

try:
    from tests.fixtures import build_durable_golden_fixtures as base
except ImportError:  # run as a script from an isolated interpreter
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    import build_durable_golden_fixtures as base

FIXTURE_DIR = Path(__file__).resolve().parent / "durable_golden_v11"
WHEEL_VERSION = "0.19.7"

SCENARIOS: dict[str, dict] = {
    **base.SCENARIOS,
    "text_index_full": {
        "level": "full",
        "children": [
            """
            g.cypher("CREATE (:Doc {id: 1, body: 'alpha beta', note: 'n one'})")
            g.cypher("CREATE (:Doc {id: 2, body: 'beta gamma', note: 'n two'})")
            g.cypher("CREATE (:Doc {id: 3, body: 'gamma delta', note: 'n three'})")
            g.build_text_index("Doc", "body")
            g.build_text_index("Doc", "note")
            g.drop_text_index("Doc", "note")
            g.cypher("CREATE (:Doc {id: 4, body: 'alpha alpha', note: 'n four'})")
            """
        ],
        "queries": {
            "alpha_hits": "MATCH (n:Doc) WHERE text_bm25(n, 'body', 'alpha') > 0.0 RETURN n.id AS id ORDER BY n.id",
            "gamma_hits": "MATCH (n:Doc) WHERE text_bm25(n, 'body', 'gamma') > 0.0 RETURN n.id AS id ORDER BY n.id",
            "docs": "MATCH (n:Doc) RETURN n.id AS id ORDER BY n.id",
        },
        "api": {
            "body_indexed": "g.has_text_index('Doc', 'body')",
            "note_indexed": "g.has_text_index('Doc', 'note')",
        },
    },
}

EXPECTED: dict[str, dict] = {
    **base.EXPECTED,
    "text_index_full": {
        "queries": {
            "alpha_hits": [{"id": 1}, {"id": 4}],
            "gamma_hits": [{"id": 2}, {"id": 3}],
            "docs": [{"id": 1}, {"id": 2}, {"id": 3}, {"id": 4}],
        },
        "api": {"body_indexed": True, "note_indexed": False},
    },
}


def observe(name: str, g) -> dict:
    return base.observe(name, g, SCENARIOS)


def compare(name: str, got: dict) -> list[str]:
    return base.compare(name, got, EXPECTED)


def main() -> None:
    import kglite

    print("kglite from", kglite.__file__, kglite.__version__)
    if kglite.__version__ != WHEEL_VERSION or "site-packages" not in kglite.__file__:
        raise SystemExit(f"refusing to write: expected the published {WHEEL_VERSION} wheel, got {kglite.__file__}")
    cwd = Path(tempfile.mkdtemp())  # outside the repo: children must import the wheel
    for name in sys.argv[1:] or list(SCENARIOS):
        base._write_scenario(name, scenarios=SCENARIOS, expected=EXPECTED, fixture_dir=FIXTURE_DIR, cwd=cwd)


if __name__ == "__main__":
    main()
