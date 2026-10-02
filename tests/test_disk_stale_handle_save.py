"""A disk handle whose graph was saved by someone else since it loaded refuses to save.

Handle A loads generation N; another handle (here in the same process, or a
second process) publishes N+1. A's save used to publish a generation built from
N, silently dropping the other writer's work. It now refuses before writing
anything and names the two generations.
"""

from __future__ import annotations

import subprocess
import sys
import textwrap

import pandas as pd
import pytest

import kglite

STALE = "saved by another process since this handle loaded it"


def _seed(path):
    g = kglite.KnowledgeGraph(storage="disk", path=path)
    frame = pd.DataFrame({"id": [1, 2, 3], "name": ["a", "b", "c"]})
    g.add_nodes(frame, "Person", "id", "name")
    g.save(path)
    del g


def _name(graph, ident):
    return graph.cypher("MATCH (n:Person {id: $i}) RETURN n.name AS v", params={"i": ident}).scalar()


def _generations(path):
    return sorted(p.name for p in (path / "generations").iterdir())


def test_stale_handle_save_is_refused_and_writes_nothing(tmp_path):
    path = tmp_path / "g"
    _seed(str(path))
    stale = kglite.load(str(path))
    other = kglite.load(str(path))
    other.cypher("MATCH (n:Person {id: 1}) SET n.name = 'other'")
    other.save()
    before = (path / "CURRENT").read_text(), _generations(path)

    stale.cypher("MATCH (n:Person {id: 2}) SET n.name = 'stale'")
    with pytest.raises(ValueError, match=STALE) as raised:
        stale.save()
    assert "reload and reapply" in str(raised.value)
    assert ((path / "CURRENT").read_text(), _generations(path)) == before

    fresh = kglite.load(str(path))
    assert _name(fresh, 1) == "other"
    assert _name(fresh, 2) == "b"


def test_stale_refusal_names_both_generations(tmp_path):
    path = tmp_path / "g"
    _seed(str(path))
    stale = kglite.load(str(path))
    other = kglite.load(str(path))
    other.cypher("MATCH (n:Person {id: 1}) SET n.name = 'x'")
    other.save()
    stale.cypher("MATCH (n:Person {id: 2}) SET n.name = 'y'")
    with pytest.raises(ValueError) as raised:
        stale.save()
    assert "generation 2, now 3" in str(raised.value) or "generation 1, now 2" in str(raised.value)


def test_own_successive_saves_and_reload_keep_working(tmp_path):
    path = tmp_path / "g"
    _seed(str(path))
    g = kglite.load(str(path))
    for n in range(3):
        g.cypher("MATCH (n:Person {id: 1}) SET n.name = $v", params={"v": f"v{n}"})
        g.save()
        # A view held across the save reads the generation it was taken from.
        assert _name(g, 1) == f"v{n}"
    reloaded = kglite.load(str(path))
    reloaded.cypher("MATCH (n:Person {id: 3}) SET n.name = 'again'")
    reloaded.save()
    assert _name(kglite.load(str(path)), 3) == "again"


def test_a_new_handle_refuses_after_another_process_saved_over_it(tmp_path):
    path = tmp_path / "g"
    g = kglite.KnowledgeGraph(storage="disk", path=str(path))
    seed = kglite.load(str(path))
    seed.add_nodes(pd.DataFrame({"id": [1], "name": ["a"]}), "Person", "id", "name")
    seed.save()
    del seed
    writer = textwrap.dedent(
        """
        import sys, kglite
        h = kglite.load(sys.argv[1])
        h.cypher("MATCH (n:Person {id: 1}) SET n.name = 'writer'")
        h.save()
        """
    )
    subprocess.run([sys.executable, "-c", writer, str(path)], check=True, timeout=120)
    g.add_nodes(pd.DataFrame({"id": [2], "name": ["late"]}), "Person", "id", "name")
    with pytest.raises(ValueError, match=STALE):
        g.save()
    assert _name(kglite.load(str(path)), 1) == "writer"


def test_a_refused_handle_can_be_replaced_by_a_reload(tmp_path):
    path = tmp_path / "g"
    _seed(str(path))
    stale = kglite.load(str(path))
    other = kglite.load(str(path))
    other.cypher("MATCH (n:Person {id: 1}) SET n.name = 'other'")
    other.save()
    stale.cypher("MATCH (n:Person {id: 2}) SET n.name = 'stale'")
    with pytest.raises(ValueError, match=STALE):
        stale.save()
    del stale
    again = kglite.load(str(path))
    again.cypher("MATCH (n:Person {id: 2}) SET n.name = 'reapplied'")
    again.save()
    final = kglite.load(str(path))
    assert (_name(final, 1), _name(final, 2)) == ("other", "reapplied")
