"""Heap tails under a held view: rows a write appends to a large type while a
``freeze()`` holds the graph live in a tail beside the shared columns, and fold
back into the type once the view drops. Every observable answer — type records,
reads, recovery, spills — must equal the same writes without a held view.
"""

import gc
import os
import re
import subprocess
import sys
import textwrap

import pandas as pd
import pytest

import kglite

#: Rows past the heap-tail floor (16,384), so a held view's appends take a tail.
ROWS = 16_384 + 1_000
SEED = f"UNWIND range(0, {ROWS - 1}) AS i CREATE (:Item {{id: i, title: 'n' + toString(i), score: i, tag: 'base'}})"


def _graph():
    g = kglite.KnowledgeGraph()
    g.cypher(SEED)
    return g


def _w_type(g):
    return re.findall(r'<prop name="w" type="(\w+)"', g.describe(types=["Item"]))


def _reads(g):
    def rows(query):
        return [dict(r) for r in g.cypher(query)]

    return (
        g.cypher("MATCH (n:Item) RETURN count(n) AS c").scalar(),
        rows("MATCH (n:Item) WHERE n.tag = 'tx' RETURN n.id AS id, n.title AS t, n.w AS w ORDER BY id"),
        g.cypher(f"MATCH (n:Item) WHERE n.score >= {ROWS} RETURN sum(n.score) AS s").scalar(),
        rows("MATCH (n:Item) WHERE n.fresh IS NOT NULL RETURN n.id AS id, n.fresh AS f ORDER BY id"),
    )


def _tail_writes(g):
    g.cypher(f"CREATE (:Item {{id: {ROWS}, title: 'a', score: {ROWS}, tag: 'tx'}})")
    g.cypher(f"CREATE (:Item {{id: {ROWS + 1}, title: 'b', score: {ROWS + 1}, tag: 'tx'}})")
    g.cypher(f"MATCH (n:Item {{id: {ROWS}}}) SET n.w = 1.5")
    g.cypher(f"MATCH (n:Item {{id: {ROWS + 1}}}) SET n.w = 2")
    ids = [ROWS + 2, ROWS + 3]
    g.add_nodes(
        pd.DataFrame({"id": ids, "title": ["c", "d"], "tag": ["tx", "tx"], "fresh": [7, 8]}),
        "Item",
        "id",
        "title",
    )


@pytest.mark.parametrize("hold", [False, True])
def test_a_tail_only_float_key_keeps_its_float_record(tmp_path, hold):
    g = _graph()
    view = g.freeze() if hold else None
    _tail_writes(g)
    assert kglite._backend_is_forked(g) is hold
    live = _w_type(g)
    path = str(tmp_path / "g.kgl")
    g.save(path)
    loaded = _w_type(kglite.load(path))
    del view
    gc.collect()
    g.cypher("CREATE (:Item {id: 999999})")
    assert kglite._backend_is_forked(g) is False
    assert (live, loaded, _w_type(g)) == (["Float64"], ["Float64"], ["Float64"])


@pytest.mark.parametrize("hold", [False, True])
def test_add_nodes_into_a_tail_keeps_the_float_record(hold):
    g = _graph()
    view = g.freeze() if hold else None
    g.add_nodes(pd.DataFrame({"id": [ROWS], "title": ["a"], "w": [1.5]}), "Item", "id", "title")
    with pytest.warns(UserWarning, match="stays the recorded type"):
        g.add_nodes(pd.DataFrame({"id": [ROWS + 1], "title": ["b"], "w": [2]}), "Item", "id", "title")
    assert kglite._backend_is_forked(g) is hold
    assert _w_type(g) == ["Float64"]
    del view
    gc.collect()
    g.cypher("CREATE (:Item {id: 999999})")
    assert _w_type(g) == ["Float64"]


def test_add_nodes_under_freeze_reads_back_and_folds():
    control = _graph()
    _tail_writes(control)
    g = _graph()
    view = g.freeze()
    _tail_writes(g)
    assert kglite._backend_is_forked(g) is True
    assert _reads(g) == _reads(control)
    assert view.cypher("MATCH (n:Item) RETURN count(n) AS c").scalar() == ROWS
    del view
    gc.collect()
    g.cypher("MATCH (n:Item {id: 0}) SET n.score = 0")
    assert kglite._backend_is_forked(g) is False
    assert _reads(g) == _reads(control)


@pytest.mark.parametrize("level", ["normal", "full"])
def test_tail_rows_survive_a_crash(tmp_path, level):
    path = str(tmp_path / "app.kgl")
    script = textwrap.dedent(
        f"""
        import os, kglite, pandas as pd
        import sys
        sys.path.insert(0, {os.path.dirname(__file__)!r})
        from test_heap_tail import SEED, _tail_writes, _reads
        g = kglite.open({path!r}, durable={level!r})
        g.cypher(SEED)
        g.save()
        view = g.freeze()
        _tail_writes(g)
        assert kglite._backend_is_forked(g)
        with open({path + ".expected"!r}, "w", encoding="utf-8") as f:
            f.write(repr(_reads(g)))
        os._exit(0)
        """
    )
    subprocess.run([sys.executable, "-c", script], check=True, env=dict(os.environ))
    with open(path + ".expected", encoding="utf-8") as f:
        expected = f.read()
    reopened = kglite.open(path, durable=level)
    assert repr(_reads(reopened)) == expected
    assert _w_type(reopened) == ["Float64"]


def test_tail_rows_then_a_spill_and_a_fold_read_as_the_control():
    def run(hold):
        g = _graph()
        view = g.freeze() if hold else None
        _tail_writes(g)
        assert kglite._backend_is_forked(g) is hold
        g.set_memory_limit(0)
        g.cypher(f"MATCH (n:Item {{id: {ROWS}}}) SET n.tag = 'tx'")
        spilled = _reads(g)
        del view
        gc.collect()
        g.cypher("MATCH (n:Item {id: 0}) SET n.score = 0")
        assert kglite._backend_is_forked(g) is False
        folded = _reads(g)
        g.set_memory_limit(None)
        g.unspill()
        return spilled, folded, _reads(g)

    assert run(True) == run(False)
