"""Within-statement read-after-write goldens across storage modes.

Disk mode stages a node write and flushes it into the column store before the
next read of the same statement: a later row, a later SET item, a MERGE of the
following row, a FOREACH iteration, REMOVE, and later clauses all read the
flushed state. Each shape below has an absolute expected value, so a flush
that moves (or is batched past a reader) fails here in every mode that has it,
not only as a divergence between modes.
"""

import pandas as pd
import pytest

import kglite

pytestmark = pytest.mark.parity

# (statement, follow-up read or None). Order matters: later cases read values
# earlier ones wrote (case 12 reads node 1's `x` after case 2 incremented it).
CASES = [
    (
        "UNWIND [1,2,3] AS i MATCH (n:P {id: 1}) SET n.c = coalesce(n.c, 0) + 1",
        "MATCH (n:P {id: 1}) RETURN n.c AS v",
    ),
    ("UNWIND [1,2,3] AS i MATCH (n:P {id: 1}) SET n.x = n.x + 1 WITH n RETURN max(n.x) AS v", None),
    ("MATCH (n:P {id: 2}) SET n.a = 5, n.b = n.a + 1 RETURN n.b AS v", None),
    ("MATCH (n:P {id: 3}) SET n += {a: 7}, n.b = n.a * 2 RETURN n.b AS v", None),
    (
        "UNWIND [4, 4, 4] AS k MERGE (n:P {id: k}) ON MATCH SET n.hits = coalesce(n.hits, 0) + 1",
        "MATCH (n:P {id: 4}) RETURN n.hits AS v",
    ),
    (
        "UNWIND [99, 99] AS k MERGE (n:P {id: k}) ON CREATE SET n.v = 1 ON MATCH SET n.v = n.v + 1",
        "MATCH (n:P {id: 99}) RETURN n.v AS v",
    ),
    ("MATCH (n:P {id: 5}) FOREACH (i IN [1,2,3] | SET n.x = n.x + i) RETURN n.x AS v", None),
    ("MATCH (n:P {id: 6}) SET n.flag = true WITH n MATCH (m:P) WHERE m.flag = true RETURN count(m) AS v", None),
    (
        "MATCH (n:P {id: 7}) SET n.title = 'renamed' WITH n MATCH (m:P {title: 'renamed'}) RETURN count(m) AS v",
        None,
    ),
    ("MATCH (n:P {id: 8}) SET n.tmp = 1 REMOVE n.tmp RETURN n.tmp AS v", None),
    (
        "UNWIND [9, 10] AS k MATCH (n:P {id: k}) SET n.x = 777 WITH count(*) AS c "
        "MATCH (m:P) WHERE m.x = 777 RETURN count(m) AS v",
        None,
    ),
    (
        "UNWIND [[1,2],[2,1]] AS p MATCH (a:P {id: p[0]}), (b:P {id: p[1]}) SET a.s = b.x",
        "MATCH (n:P) WHERE n.id IN [1,2] RETURN n.id AS id, n.s AS s ORDER BY id",
    ),
]

EXPECTED = [
    [{"v": 3}],
    [{"v": 13}],
    [{"v": 6}],
    [{"v": 14}],
    [{"v": 3}],
    [{"v": 2}],
    [{"v": 56}],
    [{"v": 1}],
    [{"v": 1}],
    [{"v": None}],
    [{"v": 2}],
    [{"id": 1, "s": 20}, {"id": 2, "s": 13}],
]


def _graph(mode, tmp_path):
    frame = pd.DataFrame(
        {"id": list(range(1, 11)), "name": [f"n{i}" for i in range(1, 11)], "x": list(range(10, 110, 10))}
    )
    if mode == "memory":
        graph = kglite.KnowledgeGraph()
    elif mode == "mapped":
        graph = kglite.KnowledgeGraph(storage="mapped")
    else:
        graph = kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "g"))
    graph.add_nodes(frame, "P", "id", "name")
    if mode == "disk_reopened":
        graph.save()
        del graph
        graph = kglite.load(str(tmp_path / "g"))
    return graph


@pytest.mark.parametrize("mode", ["memory", "mapped", "disk", "disk_reopened"])
def test_statement_reads_its_own_writes(mode, tmp_path):
    graph = _graph(mode, tmp_path)
    results = []
    for statement, follow_up in CASES:
        rows = graph.cypher(statement).to_list()
        results.append(graph.cypher(follow_up).to_list() if follow_up else rows)
    assert results == EXPECTED
