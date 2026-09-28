"""A column holding values of more than one kind keeps every value's kind
through a save and reload, in every storage mode.

A disk save re-types an untyped (mixed-kind) column only when every value
already has one kind; it never converts a value to fit a column. An exact
integer beside a float stays an integer, as it does in memory and mapped mode.
"""

import datetime as dt

import pytest

import kglite

pytestmark = pytest.mark.parity

# (property, the per-row values of ids 1, 2) — each pair mixes two kinds.
MIXTURES = {
    "int_float": (7, 2.5),
    "bool_int": (True, 3),
    "int_string": (4, "four"),
    "date_datetime": (dt.date(2020, 1, 2), dt.datetime(2020, 1, 2, 3, 4, 5)),
}


def _graph(mode, tmp_path):
    if mode == "memory":
        graph = kglite.KnowledgeGraph()
    elif mode == "mapped":
        graph = kglite.KnowledgeGraph(storage="mapped")
    else:
        graph = kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "g"))
    # Each mixture twice: created mixed (`c_*`), and created uniform with the
    # second node then SET to the other kind (`s_*`) — the SET widens the
    # type's declared kind (int → float) while the column holds both.
    for row, node_id in enumerate((1, 2)):
        params = {name: pair[row] for name, pair in MIXTURES.items()}
        params.update({f"s_{name}": pair[0] for name, pair in MIXTURES.items()})
        props = ", ".join(f"c_{name}: ${name}, s_{name}: $s_{name}" for name in MIXTURES)
        graph.cypher(f"CREATE (:T {{id: {node_id}, {props}}})", params=params)
    sets = ", ".join(f"n.s_{name} = ${name}" for name in MIXTURES)
    graph.cypher(
        f"MATCH (n:T {{id: 2}}) SET {sets}",
        params={name: pair[1] for name, pair in MIXTURES.items()},
    )
    # A type whose values are uniform stays typed; its values must survive too.
    graph.cypher("CREATE (:U {id: 1, v: 7}), (:U {id: 2, v: 8})")
    if mode in ("disk", "disk_reopened"):
        graph.save()
    if mode == "disk_reopened":
        del graph
        graph = kglite.load(str(tmp_path / "g"))
        # A second save of the reopened graph is where a re-typing would run.
        graph.save()
        del graph
        graph = kglite.load(str(tmp_path / "g"))
    return graph


@pytest.mark.parametrize("mode", ["memory", "mapped", "disk", "disk_reopened"])
def test_mixed_kind_column_values_keep_their_kind(mode, tmp_path):
    graph = _graph(mode, tmp_path)
    columns = [f"{how}_{name}" for name in MIXTURES for how in ("c", "s")]
    rows = graph.cypher(
        "MATCH (n:T) RETURN n.id AS id, " + ", ".join(f"n.{c} AS {c}" for c in columns) + " ORDER BY id"
    ).to_list()
    assert [row["id"] for row in rows] == [1, 2]
    for row, node_id in zip(rows, (1, 2)):
        for column in columns:
            expected = MIXTURES[column[2:]][node_id - 1]
            assert row[column] == expected and type(row[column]) is type(expected), (
                f"{mode}: {column} of node {node_id} read back {row[column]!r}, expected {expected!r}"
            )
    uniform = graph.cypher("MATCH (n:U) RETURN n.v AS v ORDER BY v").to_list()
    assert [(r["v"], type(r["v"])) for r in uniform] == [(7, int), (8, int)]
