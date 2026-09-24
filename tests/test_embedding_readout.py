"""Reading stored vectors back out: `embedding()` in Cypher, and
`relationship_embeddings()` from Python.

`embedding(x, 'col_emb')` returns the vector a node or relationship holds, so
`vector_score(b, 'col_emb', embedding(a, 'col_emb'))` scores one entity against
another. `relationship_embeddings(type, col)` returns every vector in a
relationship store addressed by its endpoints — the edge list and edge-feature
matrix a graph-learning library wants. Every mode, every answer checked
against a Python oracle.
"""

from __future__ import annotations

import math
from pathlib import Path
import re

import pytest

import kglite
from kglite import KnowledgeGraph

MODES = ["memory", "mapped", "disk"]

# (k, source id, target id, uid, angle) — k=2 and k=3 are a parallel group
# (both 1 -> 20).
EDGES = [
    (1, 1, 10, "a", 0.2),
    (2, 1, 20, "b", 0.9),
    (3, 1, 20, "c", 1.4),
    (4, 2, 10, "d", 2.0),
]
NODE_VECTORS = {10: [1.0, 0.0], 20: [0.6, 0.8]}


def _graph(mode: str, tmp_path: Path) -> KnowledgeGraph:
    graph = (
        kglite.open(str(tmp_path / "disk-graph"), storage="disk") if mode == "disk" else KnowledgeGraph(storage=mode)
    )
    graph.cypher(
        "CREATE (:Claimant {id: 1}), (:Claimant {id: 2}), "
        "(:Claim {id: 10, summary: 'x'}), (:Claim {id: 20, summary: 'y'})"
    )
    for k, source, target, uid, angle in EDGES:
        graph.cypher(
            "MATCH (s:Claimant {id: $s}), (t:Claim {id: $t}) "
            "CREATE (s)-[r:SUPPORTS {k: $k, uid: $uid}]->(t) "
            "WITH r CALL db.edge_embeddings.set({type: 'SUPPORTS', text_property: 'evidence', "
            "entries: [{relationship: r, vector: $v}]}) YIELD stored RETURN stored",
            params={"s": source, "t": target, "k": k, "uid": uid, "v": [math.cos(angle), math.sin(angle)]},
        )
    graph.set_embeddings("Claim", "summary", NODE_VECTORS)
    return graph


def _f32(x: float) -> float:
    """What a stored float32 component reads back as."""
    import struct

    return struct.unpack("f", struct.pack("f", x))[0]


def _stored(angle: float) -> list[float]:
    return [_f32(math.cos(angle)), _f32(math.sin(angle))]


def _cosine(a: list[float], b: list[float]) -> float:
    dot = sum(x * y for x, y in zip(a, b))
    return dot / (math.hypot(*a) * math.hypot(*b))


# ── embedding() in Cypher ─────────────────────────────────────────────────────


@pytest.mark.parametrize("mode", MODES)
def test_embedding_returns_the_stored_relationship_vector(mode: str, tmp_path: Path) -> None:
    graph = _graph(mode, tmp_path)
    rows = graph.cypher(
        "MATCH ()-[r:SUPPORTS]->() RETURN r.k AS k, embedding(r, 'evidence_emb') AS v ORDER BY k"
    ).to_list()
    assert rows == [{"k": k, "v": _stored(angle)} for k, _, _, _, angle in EDGES]
    # A value (from collect) reads the same vector as the binding.
    value = graph.cypher(
        "MATCH ()-[r:SUPPORTS {k: 3}]->() WITH collect(r) AS rs RETURN embedding(rs[0], 'evidence_emb') AS v"
    ).to_list()
    assert value == [{"v": _stored(1.4)}]


@pytest.mark.parametrize("mode", MODES)
def test_edge_to_edge_and_node_to_node_similarity_equal_the_oracle(mode: str, tmp_path: Path) -> None:
    graph = _graph(mode, tmp_path)
    rows = graph.cypher(
        "MATCH ()-[a:SUPPORTS {k: 1}]->() MATCH ()-[b:SUPPORTS]->() "
        "RETURN b.k AS k, vector_score(b, 'evidence_emb', embedding(a, 'evidence_emb')) AS s ORDER BY s DESC"
    ).to_list()
    oracle = sorted(
        ((k, _cosine(_stored(0.2), _stored(angle))) for k, _, _, _, angle in EDGES),
        key=lambda pair: -pair[1],
    )
    assert [row["k"] for row in rows] == [k for k, _ in oracle]
    for row, (_, expected) in zip(rows, oracle):
        assert row["s"] == pytest.approx(expected, abs=1e-6)

    node = graph.cypher(
        "MATCH (a:Claim {id: 10}), (b:Claim {id: 20}) "
        "RETURN vector_score(b, 'summary_emb', embedding(a, 'summary_emb')) AS s"
    ).to_list()
    assert node[0]["s"] == pytest.approx(_cosine(NODE_VECTORS[10], NODE_VECTORS[20]), abs=1e-6)


def test_embedding_is_null_without_a_vector_and_refused_without_a_store(tmp_path: Path) -> None:
    graph = _graph("memory", tmp_path)
    graph.cypher("CREATE (:Claim {id: 30, summary: 'z'})")
    assert graph.cypher("MATCH (c:Claim {id: 30}) RETURN embedding(c, 'summary_emb') AS v").to_list() == [{"v": None}]
    with pytest.raises(
        kglite.CypherExecutionError,
        match=r"embedding\(\): no embedding 'evidence_emb' found for node type 'Claimant'",
    ):
        graph.cypher("MATCH (c:Claimant {id: 1}) RETURN embedding(c, 'evidence_emb') AS v")
    with pytest.raises(kglite.CypherExecutionError, match="Did you mean 'evidence_emb'"):
        graph.cypher("MATCH ()-[r:SUPPORTS]->() RETURN embedding(r, 'evidence') AS v")


# ── relationship_embeddings() ─────────────────────────────────────────────────


@pytest.mark.parametrize("mode", MODES)
def test_rows_are_addressed_by_endpoints_in_a_stable_order(mode: str, tmp_path: Path) -> None:
    graph = _graph(mode, tmp_path)
    rows = graph.relationship_embeddings("SUPPORTS", "evidence")
    assert [(r["source"], r["target"], r["key"]) for r in rows] == [
        (1, 10, None),
        (1, 20, None),
        (1, 20, None),
        (2, 10, None),
    ]
    assert all(r["source_type"] == "Claimant" and r["target_type"] == "Claim" for r in rows)
    # Without a key, the parallel group's two members are both present, in
    # the order they were created (relationship slot).
    assert [r["vector"] for r in rows[1:3]] == [_stored(0.9), _stored(1.4)]
    assert rows[0]["vector"] == _stored(0.2)


def test_a_named_key_identifies_parallel_members(tmp_path: Path) -> None:
    graph = _graph("memory", tmp_path)
    rows = graph.relationship_embeddings("SUPPORTS", "evidence", relationship_keys={"SUPPORTS": "uid"})
    assert [(r["source"], r["target"], r["key"], r["vector"]) for r in rows] == [
        (1, 10, "a", _stored(0.2)),
        (1, 20, "b", _stored(0.9)),
        (1, 20, "c", _stored(1.4)),
        (2, 10, "d", _stored(2.0)),
    ]


def test_a_key_that_does_not_tell_members_apart_is_refused(tmp_path: Path) -> None:
    graph = _graph("memory", tmp_path)
    graph.cypher("MATCH ()-[r:SUPPORTS {k: 3}]->() SET r.uid = 'b'")
    with pytest.raises(ValueError, match="2 'SUPPORTS' relationships connect .* repeats the value"):
        graph.relationship_embeddings("SUPPORTS", "evidence", relationship_keys={"SUPPORTS": "uid"})
    with pytest.raises(ValueError, match="No relationship embedding store 'SUPPORTS.nope'"):
        graph.relationship_embeddings("SUPPORTS", "nope")


def test_rows_build_pyg_edge_index_and_edge_attr(tmp_path: Path) -> None:
    np = pytest.importorskip("numpy")
    graph = _graph("memory", tmp_path)
    rows = graph.relationship_embeddings("SUPPORTS", "evidence", relationship_keys={"SUPPORTS": "uid"})
    index = {
        key: i
        for i, key in enumerate(
            sorted({(r["source_type"], r["source"]) for r in rows} | {(r["target_type"], r["target"]) for r in rows})
        )
    }
    edge_index = np.array(
        [[index[(r["source_type"], r["source"])] for r in rows], [index[(r["target_type"], r["target"])] for r in rows]]
    )
    edge_attr = np.array([r["vector"] for r in rows], dtype=np.float32)
    assert edge_index.shape == (2, 4)
    assert edge_attr.shape == (4, 2)
    # The edge-to-edge scores from Cypher are the cosine of these rows.
    first = edge_attr[0]
    cosines = edge_attr @ first / (np.linalg.norm(edge_attr, axis=1) * np.linalg.norm(first))
    scored = graph.cypher(
        "MATCH ()-[a:SUPPORTS {uid: 'a'}]->() MATCH ()-[b:SUPPORTS]->() "
        "RETURN b.uid AS uid, vector_score(b, 'evidence_emb', embedding(a, 'evidence_emb')) AS s ORDER BY uid"
    ).to_list()
    assert [row["s"] for row in scored] == pytest.approx(cosines.tolist(), abs=1e-6)


# ── a missing store raises inside a fused WHERE ──────────────────────────────


@pytest.mark.parametrize("mode", ["memory", "mapped"])
@pytest.mark.parametrize(
    ("query", "message"),
    [
        (
            "MATCH (c:Claimant) WHERE size(embedding(c, 'evidence_emb')) > 0 RETURN count(c) AS n",
            "embedding(): no embedding 'evidence_emb' found for node type 'Claimant'",
        ),
        (
            "MATCH (c:Claimant) WHERE embedding_norm(c, 'evidence_emb') > 0 RETURN count(c) AS n",
            "embedding_norm(): no embedding 'evidence_emb' found for node type 'Claimant'",
        ),
        (
            "MATCH ()-[r:SUPPORTS]->() WHERE size(embedding(r, 'summary_emb')) > 0 RETURN count(r) AS n",
            "embedding(): no embedding 'summary_emb' found for relationship type 'SUPPORTS'",
        ),
        (
            "MATCH ()-[r:SUPPORTS]->() WHERE embedding_norm(r, 'summary_emb') > 0 RETURN count(r) AS n",
            "embedding_norm(): no embedding 'summary_emb' found for relationship type 'SUPPORTS'",
        ),
        (
            "MATCH (c:Claimant) WHERE text_score(c, 'evidence', [1.0, 0.0]) > 0 RETURN count(c) AS n",
            "text_score(): no embedding for property 'evidence' on node type 'Claimant'",
        ),
    ],
)
def test_a_missing_store_raises_in_a_fused_where(mode: str, query: str, message: str, tmp_path: Path) -> None:
    """The fused filters drop a row whose predicate fails to evaluate; a
    missing store is not about the row, and counting 0 hid it."""
    graph = _graph(mode, tmp_path)
    with pytest.raises(kglite.CypherExecutionError, match=re.escape(message)):
        graph.cypher(query)
