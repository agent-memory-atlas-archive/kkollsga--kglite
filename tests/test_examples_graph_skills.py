"""`examples/code_review_graph_skills.py` is documentation that runs.

The guides point readers at it as the worked example of graph-carried skills
and recipes, so a rename, a signature change or a tightened validation rule
that breaks it breaks the docs. Running it here is the only thing that
notices: nothing else in the suite imports `examples/`.
"""

from __future__ import annotations

from pathlib import Path
import subprocess
import sys

EXAMPLE = Path(__file__).resolve().parent.parent / "examples" / "code_review_graph_skills.py"


def test_the_graph_carried_skills_example_runs_and_shows_both_layers():
    assert EXAMPLE.is_file(), EXAMPLE
    proc = subprocess.run(
        [sys.executable, str(EXAMPLE)],
        capture_output=True,
        text=True,
        timeout=90,
    )
    assert proc.returncode == 0, proc.stderr
    out = proc.stdout
    # The skill, on the default tier, with its referenced tools.
    assert "code_review [lazy]" in out, out
    assert "run_recipe_query" in out, out
    # All three recipe queries, which only store if they compile read-only and
    # their schemas match their `$parameters`.
    for name in ("target_coverage", "callers_page", "bounded_call_path"):
        assert f"code_review/{name}" in out, out
    # The describe sections the guides describe, and the hiding that makes the
    # system labels safe to store.
    assert '<skills count="1"' in out, out
    assert '<recipes count="1"' in out, out
    assert "node_types -> ['Function']" in out, out
    assert "KgliteSkill" in out, "the label is still reachable from Cypher"
