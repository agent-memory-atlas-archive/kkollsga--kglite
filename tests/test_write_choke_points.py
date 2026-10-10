"""Source guard: every write to a Python ``KnowledgeGraph`` goes through a choke point.

The wheel's mutators reach the graph only through the methods in
``crates/kglite-py/src/graph/write_gate.rs``. The raw ``&mut DirGraph``
accessor and the log drain are private to that module, so a mutator written
against them does not compile. What the type system cannot see is a call that
borrows ``&mut <handle>.inner`` and hands the ``Arc`` to a callee, or one that
forks it with ``Arc::make_mut``; this scan refuses those outside the sites
that are lifecycle by nature.
"""

from __future__ import annotations

from pathlib import Path
import re

import pytest

SRC = Path(__file__).resolve().parent.parent / "crates" / "kglite-py" / "src"
GATE = SRC / "graph" / "write_gate.rs"

# Names that only the gate may use.
FORBIDDEN = re.compile(
    r"get_graph_mut|make_dir_graph_mut(?!_preserving_lineage)|Arc::make_mut|\bmake_mut\(|\bcommit_wal\b|\bflush_wal\b"
)
# `&mut <handle>.inner` hands the Arc to a callee that may mutate it.
INNER_BORROW = re.compile(r"&mut\s+\w+\.inner\b")
LIFECYCLE_ACCESSOR = re.compile(r"\blifecycle_graph_mut\b")

# Lifecycle sites that borrow the Arc by design: opening the log
# (`lib.rs`) and the `save()` checkpoint, which swaps stamps and the log while
# holding disjoint lifecycle borrows. Keyed by path relative to src/.
INNER_BORROW_ALLOWED = {
    "lib.rs": 1,
    "graph/pyapi/kg_core.rs": 5,
}
LIFECYCLE_ACCESSOR_ALLOWED = {"graph/pyapi/kg_core.rs", "graph/write_gate.rs"}


def _code(text: str) -> str:
    """Drop line comments, which may name the forbidden spellings."""
    return "\n".join(re.sub(r"//.*", "", line) for line in text.splitlines())


def violations(rel: str, text: str) -> list[str]:
    code = _code(text)
    found: list[str] = []
    if rel != "graph/write_gate.rs":
        found += [f"{rel}: {m.group(0)}" for m in FORBIDDEN.finditer(code)]
        borrows = len(INNER_BORROW.findall(code))
        if borrows > INNER_BORROW_ALLOWED.get(rel, 0):
            found.append(f"{rel}: {borrows} `&mut <handle>.inner` borrows")
    if rel not in LIFECYCLE_ACCESSOR_ALLOWED and LIFECYCLE_ACCESSOR.search(code):
        found.append(f"{rel}: lifecycle_graph_mut outside save()")
    return found


def _sources():
    return sorted(SRC.rglob("*.rs"))


def test_the_wheel_writes_only_through_the_choke_points():
    bad: list[str] = []
    for path in _sources():
        rel = path.relative_to(SRC).as_posix()
        bad += violations(rel, path.read_text())
    assert not bad, "mutation outside graph/write_gate.rs:\n" + "\n".join(bad)


def test_the_gate_exists_and_owns_the_drain():
    code = _code(GATE.read_text())
    for name in ("fn logged_write", "fn unlogged_write", "fn commit_wal", "fn flush_wal"):
        assert name in code


@pytest.mark.parametrize(
    "snippet",
    [
        "let g = get_graph_mut(&mut self.inner);",
        "Arc::make_mut(&mut self.inner).x = 1;",
        "self.commit_wal()?;",
        "foo(&mut self.inner);",
    ],
)
def test_the_scan_goes_red_on_a_bypass(snippet):
    # Non-vacuity: each spelling a migrated mutator must not use is caught,
    # and a comment naming one is not.
    assert violations("graph/pyapi/new_mutator.rs", snippet)
    assert not violations("graph/pyapi/new_mutator.rs", "// get_graph_mut was here")
