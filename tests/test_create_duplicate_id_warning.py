"""A Cypher `CREATE` that forks an existing id says so when it writes it.

Ids are unique only by declaration — `define_schema` `primary_key`, or a
durable graph — so a `CREATE` of an id `add_nodes` already loaded makes a
second node, documented. The documented duplicate-id warning, though, came
only from a full index rebuild (in practice a reload): the `CREATE` inserted
into the cached id index and overwrote the original's entry silently, leaving
it unreachable by id. The warning's counter is process-global and
rate-limited, so each case runs in a fresh interpreter.
"""

from __future__ import annotations

import subprocess
import sys
import textwrap

import pytest

import kglite

SCRIPT = textwrap.dedent(
    """
    import kglite, pandas as pd, sys
    g = kglite.KnowledgeGraph()
    g.add_nodes(pd.DataFrame({'code': ['0001', '0003'], 'name': ['A', 'B']}), 'M', 'code', 'name')
    print('BEFORE', file=sys.stderr, flush=True)
    g.cypher(CREATE)
    print('AFTER', file=sys.stderr, flush=True)
    """
)


def _stderr_of_create(create: str) -> str:
    proc = subprocess.run(
        [sys.executable, "-c", f"CREATE = {create!r}\n" + SCRIPT], capture_output=True, text=True, timeout=60
    )
    assert proc.returncode == 0, proc.stderr
    return proc.stderr.split("BEFORE", 1)[1].split("AFTER", 1)[0]


@pytest.mark.parametrize(
    "create",
    ["CREATE (:M {code: '0003', name: 'Dup'})", "CREATE (:M {id: '0003', name: 'Dup'})"],
    ids=["alias", "id"],
)
def test_the_forking_create_warns_as_it_writes(create) -> None:
    assert "duplicate id(s) on type 'M'" in _stderr_of_create(create)


def test_a_fresh_id_does_not_warn() -> None:
    assert "duplicate" not in _stderr_of_create("CREATE (:M {code: '0004', name: 'New'})")


def test_a_primary_key_refuses_the_fork() -> None:
    import pandas as pd

    g = kglite.KnowledgeGraph()
    g.add_nodes(pd.DataFrame({"code": ["0001", "0003"], "name": ["A", "B"]}), "M", "code", "name")
    g.define_schema({"nodes": {"M": {"primary_key": "id"}}})
    with pytest.raises(kglite.KgError):
        g.cypher("CREATE (:M {code: '0003', name: 'Dup'})").to_list()
    assert g.cypher("MATCH (m:M {id: '0003'}) RETURN m.name AS n").to_list() == [{"n": "B"}]


# --- the warning belongs to the statement that forks the id ---

DUPLICATE = "duplicate id(s) on type 'M'"


def _loaded(mode, tmp_path):
    import pandas as pd

    if mode == "memory":
        g = kglite.KnowledgeGraph()
    elif mode == "mapped":
        g = kglite.KnowledgeGraph(storage="mapped")
    else:
        g = kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "disk"))
    g.add_nodes(pd.DataFrame({"code": ["0001", "0003"], "name": ["A", "B"]}), "M", "code", "name")
    return g


def _warned(result):
    return [w for w in result.warnings if DUPLICATE in w]


MODES = pytest.mark.parametrize("mode", ["memory", "mapped", "disk"])


@MODES
@pytest.mark.parametrize(
    "statement",
    [
        "CREATE (:M {id: '0003', name: 'Dup'})",
        "UNWIND ['0003'] AS c CREATE (:M {id: c, name: 'Dup'})",
    ],
    ids=["create", "unwind-create"],
)
def test_the_forking_statement_reports_the_duplicate_in_its_warnings(mode, statement, tmp_path):
    """Regression: the warning went only to the process's stderr, so a caller
    reading `result.warnings` (an agent, MCP, Bolt, the C ABI) saw `[]`."""
    g = _loaded(mode, tmp_path)
    warned = _warned(g.cypher(statement))
    assert len(warned) == 1, warned
    assert "MERGE on the id alone, or dedupe the input" in warned[0], warned


@MODES
def test_a_merge_refuses_to_fork_the_id(mode, tmp_path):
    """`MERGE (n:M:Extra {id})` misses the `M`-only node; it used to create a
    second `M` under the id and warn — `test_merge_duplicate_id.py`."""
    g = _loaded(mode, tmp_path)
    with pytest.raises(kglite.CypherExecutionError, match="lacks label :Extra"):
        g.cypher("MERGE (n:M:Extra {id: '0003'}) RETURN n.id")
    assert g.cypher("MATCH (m:M) RETURN count(m) AS c").to_list() == [{"c": 2}]


@MODES
def test_a_transaction_write_reports_the_duplicate(mode, tmp_path):
    g = _loaded(mode, tmp_path)
    with g.begin() as tx:
        warned = _warned(tx.cypher("CREATE (:M {id: '0003', name: 'Dup'})"))
        tx.commit()
    assert len(warned) == 1, warned


@MODES
def test_a_unique_id_reports_nothing(mode, tmp_path):
    g = _loaded(mode, tmp_path)
    assert _warned(g.cypher("CREATE (:M {id: '0004', name: 'New'})")) == []
    assert _warned(g.cypher("MERGE (n:M {id: '0003'}) RETURN n.id")) == []
