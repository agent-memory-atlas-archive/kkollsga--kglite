"""A title field that names the type's id column is the id column, not a title alias.

``node_title_field`` defaults to ``unique_id_field``, so naming the id column
explicitly — ``add_nodes(df, 'A', 'id', 'id')`` — must mean exactly what
omitting it means. It used to register ``id`` as the type's *title* spelling,
and from then on the universal ``id`` resolved to the title slot on every route
that consults the spelling maps: ``MERGE (a:A {id: 'n1'})`` probed titles and
created a twin on every run, the twin kept its identity but read back
``a.id = 'A_1'`` (its fabricated title), and ``WHERE a.id = 'n1'`` disagreed
with ``MATCH (a:A {id: 'n1'})``. A ``GROUP BY a.id`` duplicate audit saw only
the fabricated titles and reported no duplicates.
"""

import pandas as pd
import pytest

import kglite

MODES = ["memory", "mapped", "disk"]

# (unique_id_field, node_title_field) — every combination must behave alike.
COMBOS = [("id", "id"), ("id", None), ("id", "name"), ("code", "code"), ("code", None)]


def _graph(mode, tmp_path):
    if mode == "disk":
        return kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "g"))
    return kglite.KnowledgeGraph(storage=mode)


def _load(mode, tmp_path, id_field, title_field):
    g = _graph(mode, tmp_path)
    cols = {id_field: ["x1", "x2"]}
    if title_field == "name":
        cols["name"] = ["X1", "X2"]
    g.add_nodes(pd.DataFrame(cols), "A", id_field, title_field)
    return g


def _reload(g, mode, tmp_path):
    path = str(tmp_path / ("g" if mode == "disk" else "g.kgl"))
    g.save(path)
    return kglite.load(path)


def _key(id_field):
    return id_field


def _assert_agree(g, id_field, n1_expected):
    """Every route to a node's identity answers the same thing."""
    key = _key(id_field)
    ids = sorted(r["i"] for r in g.cypher(f"MATCH (a:A) RETURN a.{key} AS i").to_list())
    assert ids == sorted(["x1", "x2"] + (["n1"] if n1_expected else []))
    assert sorted(r["i"] for r in g.cypher("MATCH (a:A) RETURN a.id AS i").to_list()) == ids
    by_pattern = g.cypher(f"MATCH (a:A {{{key}: 'n1'}}) RETURN count(a) AS c").to_list()[0]["c"]
    by_where = g.cypher(f"MATCH (a:A) WHERE a.{key} = 'n1' RETURN count(a) AS c").to_list()[0]["c"]
    by_id_where = g.cypher("MATCH (a:A) WHERE a.id = 'n1' RETURN count(a) AS c").to_list()[0]["c"]
    assert by_pattern == by_where == by_id_where == (1 if n1_expected else 0)
    audit = g.cypher(f"MATCH (a:A) WITH a.{key} AS k, count(*) AS c WHERE c > 1 RETURN k").to_list()
    assert audit == []
    props = g.cypher("MATCH (a:A) RETURN properties(a) AS p").to_list()
    assert sorted(r["p"].get(key, r["p"].get("id")) for r in props) == ids


@pytest.mark.parametrize("mode", MODES)
@pytest.mark.parametrize("id_field,title_field", COMBOS)
def test_repeated_merge_hits_the_node_its_first_run_made(mode, tmp_path, id_field, title_field):
    g = _load(mode, tmp_path, id_field, title_field)
    key = _key(id_field)
    for _ in range(3):
        g.cypher(f"MERGE (a:A {{{key}: 'n1'}})")
    assert g.cypher("MATCH (a:A) RETURN count(a) AS c").to_list()[0]["c"] == 3
    _assert_agree(g, id_field, n1_expected=True)
    reloaded = _reload(g, mode, tmp_path)
    _assert_agree(reloaded, id_field, n1_expected=True)
    reloaded.cypher(f"MERGE (a:A {{{key}: 'n1'}})")
    assert reloaded.cypher("MATCH (a:A) RETURN count(a) AS c").to_list()[0]["c"] == 3


@pytest.mark.parametrize("mode", MODES)
@pytest.mark.parametrize("id_field,title_field", COMBOS)
def test_merge_hits_a_loaded_node(mode, tmp_path, id_field, title_field):
    g = _load(mode, tmp_path, id_field, title_field)
    key = _key(id_field)
    g.cypher(f"MERGE (a:A {{{key}: 'x1'}}) SET a.hit = true")
    assert g.cypher("MATCH (a:A) RETURN count(a) AS c").to_list()[0]["c"] == 2
    rows = g.cypher(f"MATCH (a:A) WHERE a.hit RETURN a.{key} AS k").to_list()
    assert rows == [{"k": "x1"}]
    _assert_agree(g, id_field, n1_expected=False)


@pytest.mark.parametrize("mode", MODES)
def test_the_id_column_as_title_keeps_the_loaded_title(mode, tmp_path):
    """The loaded rows still take their title from the id column."""
    g = _load(mode, tmp_path, "id", "id")
    rows = g.cypher("MATCH (a:A) RETURN a.id AS i, a.title AS t ORDER BY i").to_list()
    assert rows == [{"i": "x1", "t": "x1"}, {"i": "x2", "t": "x2"}]
    assert 'title_alias="id"' not in g.describe()


def test_chunked_reload_with_the_id_column_as_title_is_accepted():
    """Re-declaring the same spelling (every chunked load) stays allowed."""
    for id_field in ("id", "code"):
        g = kglite.KnowledgeGraph()
        g.add_nodes(pd.DataFrame({id_field: ["x1"]}), "A", id_field, id_field)
        g.add_nodes(pd.DataFrame({id_field: ["x2"]}), "A", id_field, id_field)
        ids = sorted(r["i"] for r in g.cypher(f"MATCH (a:A) RETURN a.{id_field} AS i").to_list())
        assert ids == ["x1", "x2"]


def test_a_title_column_literally_named_id_never_shadows_the_identity():
    """`id` always names the identity, even when another column is the id."""
    g = kglite.KnowledgeGraph()
    g.add_nodes(pd.DataFrame({"code": ["c1"], "id": ["t1"]}), "A", "code", "id")
    g.cypher("MERGE (a:A {id: 'c1'})")
    assert g.cypher("MATCH (a:A) RETURN a.id AS i, a.code AS c, a.title AS t").to_list() == [
        {"i": "c1", "c": "c1", "t": "t1"}
    ]


def test_from_records_with_the_id_field_as_title():
    g = kglite.from_records(
        {"nodes": [{"type": "A", "id_field": "id", "title_field": "id", "records": [{"id": "x1"}, {"id": "x2"}]}]}
    )
    for _ in range(2):
        g.cypher("MERGE (a:A {id: 'n1'})")
    _assert_agree(g, "id", n1_expected=True)


def test_extend_carries_the_normalised_spelling():
    src = kglite.KnowledgeGraph()
    src.add_nodes(pd.DataFrame({"id": ["x1", "x2"]}), "A", "id", "id")
    dst = kglite.KnowledgeGraph()
    dst.extend(src)
    for _ in range(2):
        dst.cypher("MERGE (a:A {id: 'n1'})")
    _assert_agree(dst, "id", n1_expected=True)
