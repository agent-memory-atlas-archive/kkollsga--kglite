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

# `inner` is private to write_gate.rs, so the compiler already refuses any other
# module naming it on a `KnowledgeGraph`; the scan keeps the spelling out of the
# tree for the handles it cannot type. The lifecycle sites that need the graph
# and the lifecycle state together (opening the log, the `save()` checkpoint,
# the background checkpoint) take them through `lifecycle_parts()` instead.
INNER_BORROW_ALLOWED: dict[str, int] = {}
LIFECYCLE_PARTS = re.compile(r"\blifecycle_parts\b")
LIFECYCLE_PARTS_ALLOWED = {
    "lib.rs",
    "graph/bg_checkpoint.rs",
    "graph/pyapi/kg_core.rs",
    "graph/write_gate.rs",
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
    if rel not in LIFECYCLE_PARTS_ALLOWED and LIFECYCLE_PARTS.search(code):
        found.append(f"{rel}: lifecycle_parts outside the lifecycle sites")
    return found


def _sources():
    return sorted(SRC.rglob("*.rs"))


def test_the_wheel_writes_only_through_the_choke_points():
    bad: list[str] = []
    for path in _sources():
        rel = path.relative_to(SRC).as_posix()
        bad += violations(rel, path.read_text(encoding="utf-8"))
    assert not bad, "mutation outside graph/write_gate.rs:\n" + "\n".join(bad)


def test_the_gate_exists_and_owns_the_drain():
    code = _code(GATE.read_text(encoding="utf-8"))
    for name in ("fn logged_write", "fn unlogged_write", "fn commit_wal", "fn flush_wal"):
        assert name in code


@pytest.mark.parametrize(
    "snippet",
    [
        "let g = get_graph_mut(&mut self.inner);",
        "Arc::make_mut(&mut self.inner).x = 1;",
        "self.commit_wal()?;",
        "foo(&mut self.inner);",
        "let (g, l) = self.lifecycle_parts();",
    ],
)
def test_the_scan_goes_red_on_a_bypass(snippet):
    # Non-vacuity: each spelling a migrated mutator must not use is caught,
    # and a comment naming one is not.
    assert violations("graph/pyapi/new_mutator.rs", snippet)
    assert not violations("graph/pyapi/new_mutator.rs", "// get_graph_mut was here")
