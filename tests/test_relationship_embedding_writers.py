"""`set_relationship_embeddings()` and `embed_relationship_texts()`: the Python
write twins of `db.relationship_embeddings.set` / `.embed`, and `types:` on `embed`.

Every vector is checked against what `relationship_embeddings()` reads back,
and the generated path against the Cypher procedure run on a twin graph.
"""

from __future__ import annotations

from pathlib import Path
import struct

import numpy as np
import pytest

import kglite
from kglite import KnowledgeGraph

MODES = ["memory", "mapped", "disk"]
KEYS = {"SUPPORTS": "uid"}

# (source, target, uid, evidence) — uids b and c are a parallel group 1 -> 20.
EDGES = [
    (1, 10, "a", "alpha"),
    (1, 20, "b", "beta"),
    (1, 20, "c", "gamma"),
    (2, 10, "d", "delta"),
]


def _graph(mode: str, tmp_path: Path, name: str = "g") -> KnowledgeGraph:
    graph = kglite.open(str(tmp_path / name), storage="disk") if mode == "disk" else KnowledgeGraph(storage=mode)
    graph.cypher("CREATE (:Claimant {id: 1}), (:Claimant {id: 2}), (:Claim {id: 10}), (:Claim {id: 20})")
    for source, target, uid, evidence in EDGES:
        graph.cypher(
            "MATCH (s:Claimant {id: $s}), (t:Claim {id: $t}) CREATE (s)-[:SUPPORTS {uid: $uid, evidence: $e}]->(t)",
            params={"s": source, "t": target, "uid": uid, "e": evidence},
        )
    return graph


def _f32(values) -> list[float]:
    return [struct.unpack("f", struct.pack("f", float(x)))[0] for x in values]


def _by_uid(graph: KnowledgeGraph) -> dict[str, list[float]]:
    return {
        row["key"]: row["vector"]
        for row in graph.relationship_embeddings("SUPPORTS", "evidence", relationship_keys=KEYS)
    }


VECTORS = {"a": [1.0, 0.0], "b": [0.0, 1.0], "c": [0.6, 0.8], "d": [0.3, 0.1]}


def _keyed_rows() -> dict[tuple, list[float]]:
    return {(s, t, uid): VECTORS[uid] for s, t, uid, _ in EDGES}


class _Stub:
    """Deterministic model: a text's vector is its length and its first letter."""

    dimension = 2
    model_id = "stub/writers"

    def __init__(self) -> None:
        self.calls = 0

    @staticmethod
    def vector(text: str) -> list[float]:
        return [float(len(text)), float(ord(text[0]) - 96)]

    def embed(self, texts: list[str]) -> list[list[float]]:
        self.calls += 1
        return [self.vector(text) for text in texts]


# ── set_relationship_embeddings ──────────────────────────────────────────────


@pytest.mark.parametrize("mode", MODES)
def test_rows_read_back_modified_and_written_round_trip(mode: str, tmp_path: Path) -> None:
    graph = _graph(mode, tmp_path)
    report = graph.set_relationship_embeddings("SUPPORTS", "evidence", _keyed_rows(), relationship_keys=KEYS)
    assert report == {"embeddings_stored": 4, "dimension": 2, "changed": 4, "store_created": True}
    assert _by_uid(graph) == {uid: _f32(v) for uid, v in VECTORS.items()}

    rows = graph.relationship_embeddings("SUPPORTS", "evidence", relationship_keys=KEYS)
    for row in rows:
        row["vector"] = [2 * x for x in row["vector"]]
    report = graph.set_relationship_embeddings("SUPPORTS", "evidence", rows, relationship_keys=KEYS)
    assert report == {"embeddings_stored": 4, "dimension": 2, "changed": 4, "store_created": True}
    assert _by_uid(graph) == {uid: _f32([2 * x for x in v]) for uid, v in VECTORS.items()}

    # Unchanged rows upserted back are not changes.
    unchanged = graph.relationship_embeddings("SUPPORTS", "evidence", relationship_keys=KEYS)
    assert graph.add_relationship_embeddings("SUPPORTS", "evidence", unchanged, relationship_keys=KEYS)["changed"] == 0


@pytest.mark.parametrize("mode", MODES)
def test_set_replaces_the_store_and_add_keeps_what_it_does_not_name(mode: str, tmp_path: Path) -> None:
    replaced, extended = _graph(mode, tmp_path, "replaced"), _graph(mode, tmp_path, "extended")
    for graph in (replaced, extended):
        graph.set_relationship_embeddings("SUPPORTS", "evidence", _keyed_rows(), relationship_keys=KEYS)
    one = {(2, 10): [0.5, 0.5]}
    assert replaced.set_relationship_embeddings("SUPPORTS", "evidence", one) == {
        "embeddings_stored": 1,
        "dimension": 2,
        "changed": 1,
        "store_created": True,
    }
    assert _by_uid(replaced) == {"d": [0.5, 0.5]}
    assert extended.add_relationship_embeddings("SUPPORTS", "evidence", one) == {
        "embeddings_stored": 4,
        "dimension": 2,
        "changed": 1,
        "store_created": False,
    }
    assert _by_uid(extended) == {**{uid: _f32(v) for uid, v in VECTORS.items()}, "d": [0.5, 0.5]}


def test_set_discards_the_index_and_metric_as_a_replaced_node_store_does(tmp_path: Path) -> None:
    graph = _graph("memory", tmp_path)
    graph.set_relationship_embeddings("SUPPORTS", "evidence", {(2, 10): [0.3, 0.1]}, metric="euclidean")
    graph.cypher("CALL db.relationship_embeddings.build_index({type:'SUPPORTS', text_column:'evidence'}) YIELD indexed")
    graph.set_relationship_embeddings("SUPPORTS", "evidence", {(1, 10): [1.0, 0.0]})
    listed = graph.cypher(
        "CALL db.relationship_embeddings.list({type:'SUPPORTS'}) YIELD count, metric, index_state "
        "RETURN count, metric, index_state"
    ).to_list()
    assert listed == [{"count": 1, "metric": "cosine", "index_state": "none"}]


def test_rows_carry_into_a_graph_that_built_the_group_in_another_order(tmp_path: Path) -> None:
    source = _graph("memory", tmp_path)
    source.set_relationship_embeddings("SUPPORTS", "evidence", _keyed_rows(), relationship_keys=KEYS)
    target = KnowledgeGraph()
    target.cypher("CREATE (:Claimant {id: 1}), (:Claimant {id: 2}), (:Claim {id: 10}), (:Claim {id: 20})")
    for s, t, uid, evidence in reversed(EDGES):
        target.cypher(
            "MATCH (s:Claimant {id: $s}), (t:Claim {id: $t}) CREATE (s)-[:SUPPORTS {uid: $uid, evidence: $e}]->(t)",
            params={"s": s, "t": t, "uid": uid, "e": evidence},
        )
    rows = source.relationship_embeddings("SUPPORTS", "evidence", relationship_keys=KEYS)
    target.set_relationship_embeddings("SUPPORTS", "evidence", rows, relationship_keys=KEYS)
    assert _by_uid(target) == _by_uid(source)


def test_every_accepted_shape_writes_the_same_vectors(tmp_path: Path) -> None:
    shapes = {
        "keyed_lists": _keyed_rows(),
        "keyed_numpy_f32": {k: np.asarray(v, dtype=np.float32) for k, v in _keyed_rows().items()},
        "keyed_numpy_f64": {k: np.asarray(v, dtype=np.float64) for k, v in _keyed_rows().items()},
        "typed_keyed": {("Claimant", s, "Claim", t, uid): VECTORS[uid] for s, t, uid, _ in EDGES},
        "row_dicts": [{"source": s, "target": t, "key": uid, "vector": VECTORS[uid]} for s, t, uid, _ in EDGES],
        "typed_row_dicts": [
            {
                "source": s,
                "target": t,
                "source_type": "Claimant",
                "target_type": "Claim",
                "key": uid,
                "vector": np.asarray(VECTORS[uid]),
            }
            for s, t, uid, _ in EDGES
        ],
    }
    written = {}
    for index, (name, embeddings) in enumerate(shapes.items()):
        graph = _graph("memory", tmp_path, name=f"g{index}")
        graph.set_relationship_embeddings("SUPPORTS", "evidence", embeddings, relationship_keys=KEYS)
        written[name] = _by_uid(graph)
    assert all(result == written["keyed_lists"] for result in written.values()), written

    # A singleton needs no key; endpoint pairs alone address it.
    graph = _graph("memory", tmp_path, name="pairs")
    graph.set_relationship_embeddings("SUPPORTS", "evidence", {(1, 10): [1.0, 0.0], (2, 10): [0.3, 0.1]})
    assert _by_uid(graph) == {"a": [1.0, 0.0], "d": _f32([0.3, 0.1])}


def test_numpy_rows_and_float_lists_store_identical_bits(tmp_path: Path) -> None:
    rng = np.random.default_rng(7)
    matrix = rng.standard_normal((4, 16)).astype(np.float32)
    addresses = [(s, t, uid) for s, t, uid, _ in EDGES]
    arrays, lists = _graph("memory", tmp_path, "arrays"), _graph("memory", tmp_path, "lists")
    arrays.set_relationship_embeddings("SUPPORTS", "evidence", dict(zip(addresses, matrix)), relationship_keys=KEYS)
    lists.set_relationship_embeddings(
        "SUPPORTS", "evidence", dict(zip(addresses, matrix.tolist())), relationship_keys=KEYS
    )
    assert _by_uid(arrays) == _by_uid(lists)
    assert _by_uid(arrays)["c"] == matrix[2].tolist()


def test_add_upserts_and_keeps_the_index_as_a_delta(tmp_path: Path) -> None:
    graph = _graph("memory", tmp_path)
    graph.add_relationship_embeddings("SUPPORTS", "evidence", {(2, 10): [0.3, 0.1]}, metric="euclidean")
    graph.cypher("CALL db.relationship_embeddings.build_index({type:'SUPPORTS', text_column:'evidence'}) YIELD indexed")
    graph.add_relationship_embeddings("SUPPORTS", "evidence", {(1, 10): [1.0, 0.0]})
    listed = graph.cypher(
        "CALL db.relationship_embeddings.list({type:'SUPPORTS'}) YIELD count, metric, model, index_state, delta "
        "RETURN count, metric, model, index_state, delta"
    ).to_list()
    assert listed == [{"count": 2, "metric": "euclidean", "model": None, "index_state": "stale", "delta": 1}]
    with pytest.raises(ValueError, match="Store metric is 'euclidean', but this batch requested 'cosine'"):
        graph.add_relationship_embeddings("SUPPORTS", "evidence", {(1, 10): [1.0, 0.0]}, metric="cosine")


def test_refusals_name_the_row_and_its_relationship(tmp_path: Path) -> None:
    graph = _graph("memory", tmp_path)
    with pytest.raises(ValueError) as parallel:
        graph.set_relationship_embeddings("SUPPORTS", "evidence", {(1, 20): [1.0, 0.0]})
    assert str(parallel.value) == (
        "rows[0] (Claimant id=1)-[:SUPPORTS]->(Claim id=20) is ambiguous: 2 'SUPPORTS' relationships "
        "connect (Claimant id=1) to (Claim id=20), and relationship_keys names no key property for "
        "'SUPPORTS'. A parallel group is written only through a key property whose value is unique "
        "within the group: pass relationship_keys={'SUPPORTS': '<property>'} and give each row its key"
    )
    with pytest.raises(
        ValueError, match=r"^rows\[1\] \(Claimant id=9\)-\[:SUPPORTS\]->\(Claim id=10\): no 'Claimant' node has id 9$"
    ):
        graph.set_relationship_embeddings("SUPPORTS", "evidence", {(1, 10): [1.0, 0.0], (9, 10): [1.0, 0.0]})
    with pytest.raises(ValueError, match="no 'SUPPORTS' relationship connects those nodes"):
        graph.set_relationship_embeddings("SUPPORTS", "evidence", {(2, 20): [1.0, 0.0]})
    with pytest.raises(
        ValueError,
        match=r"^Embedding for relationship \(Claimant id=1\)-\[:SUPPORTS\]->\(Claim id=10\) \(rows\[1\]\) "
        r"has dimension 3, expected 2$",
    ):
        graph.set_relationship_embeddings("SUPPORTS", "evidence", {(2, 10): [1.0, 0.0], (1, 10): [1.0, 0.0, 0.0]})
    with pytest.raises(ValueError, match="rows\\[0\\] and rows\\[1\\] both name relationship"):
        graph.set_relationship_embeddings(
            "SUPPORTS",
            "evidence",
            [{"source": 1, "target": 10, "vector": [1, 0]}, {"source": 1, "target": 10, "vector": [0, 1]}],
        )
    with pytest.raises(ValueError, match="Text column 'evidense' not found on any 'SUPPORTS' relationship"):
        graph.set_relationship_embeddings("SUPPORTS", "evidense", {(1, 10): [1.0, 0.0]})
    assert graph.list_embeddings() == [], "every refusal left the graph untouched"


def test_shape_errors_are_type_errors(tmp_path: Path) -> None:
    graph = _graph("memory", tmp_path)
    with pytest.raises(TypeError, match="a dict key must be"):
        graph.set_relationship_embeddings("SUPPORTS", "evidence", {1: [1.0, 0.0]})
    with pytest.raises(TypeError, match="a dict key must be"):
        graph.set_relationship_embeddings("SUPPORTS", "evidence", {(1, 2, 3, 4, 5, 6): [1.0, 0.0]})
    with pytest.raises(TypeError, match=r"embeddings\[0\] has unknown key 'source_id'"):
        graph.set_relationship_embeddings("SUPPORTS", "evidence", [{"source_id": 1, "target": 10, "vector": [1, 0]}])
    with pytest.raises(TypeError, match=r"embeddings\[0\] is missing 'vector'"):
        graph.set_relationship_embeddings("SUPPORTS", "evidence", [{"source": 1, "target": 10}])
    with pytest.raises(TypeError, match="must be a dict keyed by endpoint tuples or a list of row dicts"):
        graph.set_relationship_embeddings("SUPPORTS", "evidence", "nope")


def test_endpoint_types_are_required_once_the_type_connects_several(tmp_path: Path) -> None:
    graph = _graph("memory", tmp_path)
    graph.cypher(
        "CREATE (:Bot {id: 5}) WITH 1 AS x MATCH (b:Bot), (c:Claim {id: 10}) "
        "CREATE (b)-[:SUPPORTS {uid: 'z', evidence: 'zeta'}]->(c)"
    )
    with pytest.raises(ValueError, match="source nodes of types Bot, Claimant; address it by"):
        graph.set_relationship_embeddings("SUPPORTS", "evidence", {(1, 10): [1.0, 0.0]})
    graph.set_relationship_embeddings("SUPPORTS", "evidence", {("Bot", 5, "Claim", 10): [1.0, 0.0]})
    assert _by_uid(graph) == {"z": [1.0, 0.0]}


# ── embed_relationship_texts and db.relationship_embeddings.embed ────────────────────


def _cypher_embed(graph: KnowledgeGraph, mode: str = "missing") -> list[dict]:
    return graph.cypher(
        "MATCH ()-[r:SUPPORTS]->() WITH collect(r) AS rs "
        "CALL db.relationship_embeddings.embed({type:'SUPPORTS', text_column:'evidence', relationships: rs, mode: "
        "$mode}) "
        "YIELD embedded, skipped, dimension, model RETURN embedded, skipped, dimension, model",
        params={"mode": mode},
    ).to_list()


@pytest.mark.parametrize("mode", MODES)
def test_embed_relationship_texts_equals_the_procedure(mode: str, tmp_path: Path) -> None:
    direct, procedure = _graph(mode, tmp_path, "direct"), _graph(mode, tmp_path, "procedure")
    for graph in (direct, procedure):
        graph.set_embedder(_Stub())
    outcome = direct.embed_relationship_texts("SUPPORTS", "evidence", show_progress=False)
    assert outcome == {"embedded": 4, "skipped": 0, "skipped_existing": 0, "reembedded_changed": 0, "dimension": 2}
    assert _cypher_embed(procedure) == [{"embedded": 4, "skipped": 0, "dimension": 2, "model": "stub/writers"}]
    assert _by_uid(direct) == _by_uid(procedure) == {uid: _Stub.vector(e) for _, _, uid, e in EDGES}
    for graph in (direct, procedure):
        info = graph.embedding_info("SUPPORTS", "evidence", entity="relationship")
        assert (info["model"], info["hashed"], info["count"]) == ("stub/writers", 4, 4)


def test_modes_follow_embed_texts(tmp_path: Path) -> None:
    graph = _graph("memory", tmp_path)
    stub = _Stub()
    graph.set_embedder(stub)
    embed = lambda mode: graph.embed_relationship_texts("SUPPORTS", "evidence", mode=mode, show_progress=False)  # noqa: E731
    assert embed(None)["embedded"] == 4
    assert embed("missing") == {
        "embedded": 0,
        "skipped": 0,
        "skipped_existing": 4,
        "reembedded_changed": 0,
        "dimension": 2,
    }
    graph.cypher("MATCH ()-[r:SUPPORTS {uid: 'a'}]->() SET r.evidence = 'aardvark'")
    changed = embed("changed")
    assert (changed["embedded"], changed["reembedded_changed"], changed["skipped_existing"]) == (1, 1, 3)
    assert _by_uid(graph)["a"] == _Stub.vector("aardvark")
    graph.cypher("MATCH ()-[r:SUPPORTS {uid: 'b'}]->() REMOVE r.evidence")
    everything = embed("all")
    assert (everything["embedded"], everything["skipped"]) == (3, 1)
    assert sorted(_by_uid(graph)) == ["a", "c", "d"], "mode='all' drops the vector whose text is gone"


def test_embed_relationship_texts_refusals(tmp_path: Path) -> None:
    graph = _graph("memory", tmp_path)
    with pytest.raises(RuntimeError):
        graph.embed_relationship_texts("SUPPORTS", "evidence", show_progress=False)
    graph.set_embedder(_Stub())
    with pytest.raises(ValueError, match="unknown mode"):
        graph.embed_relationship_texts("SUPPORTS", "evidence", mode="some", show_progress=False)
    with pytest.raises(ValueError, match="Text column 'evidense' not found on any 'SUPPORTS' relationship"):
        graph.embed_relationship_texts("SUPPORTS", "evidense", show_progress=False)
    with pytest.raises(ValueError, match="not found on any 'CITES' relationship"):
        graph.embed_relationship_texts("CITES", "evidence", show_progress=False)
    graph.set_relationship_embeddings("SUPPORTS", "evidence", {(1, 10): [1.0, 0.0, 0.0]})
    with pytest.raises(ValueError, match="relationship store is 3-d"):
        graph.embed_relationship_texts("SUPPORTS", "evidence", show_progress=False)
    assert graph.embed_relationship_texts("SUPPORTS", "evidence", mode="all", show_progress=False)["dimension"] == 2


def _typed_graph() -> KnowledgeGraph:
    graph = KnowledgeGraph()
    graph.cypher("CREATE (:Doc {id: 1}), (:Doc {id: 2}), (:Doc {id: 3})")
    graph.cypher(
        "MATCH (a:Doc {id: 1}), (b:Doc {id: 2}), (c:Doc {id: 3}) "
        "CREATE (a)-[:CITES {ctx: 'cites b'}]->(b), (a)-[:CITES {ctx: 'cites c'}]->(c), "
        "(b)-[:REFUTES {ctx: 'refutes c'}]->(c), (c)-[:MENTIONS {ctx: 'mentions a'}]->(a)"
    )
    graph.set_embedder(_Stub())
    return graph


def _vectors_by_type(graph: KnowledgeGraph) -> dict:
    return {
        rel: {(r["source"], r["target"]): r["vector"] for r in graph.relationship_embeddings(rel, "ctx")}
        for rel in ("CITES", "REFUTES")
    }


def test_types_embeds_each_listed_type_as_its_own_call_would() -> None:
    together, apart = _typed_graph(), _typed_graph()
    row = together.cypher(
        "MATCH ()-[r:CITES|REFUTES]->() WITH collect(r) AS rs "
        "CALL db.relationship_embeddings.embed({types:['REFUTES','CITES'], text_column:'ctx', relationships: rs}) "
        "YIELD embedded, skipped, dimension, model RETURN embedded, skipped, dimension, model"
    ).to_list()
    assert row == [{"embedded": 3, "skipped": 0, "dimension": 2, "model": "stub/writers"}]
    per_type = [
        apart.cypher(
            f"MATCH ()-[r:{rel}]->() WITH collect(r) AS rs "
            f"CALL db.relationship_embeddings.embed({{type:'{rel}', text_column:'ctx', relationships: rs}}) "
            "YIELD embedded RETURN embedded"
        ).to_list()[0]["embedded"]
        for rel in ("CITES", "REFUTES")
    ]
    assert per_type == [2, 1]
    assert _vectors_by_type(together) == _vectors_by_type(apart)
    assert together.embedding_info("MENTIONS", "ctx", entity="relationship") is None


def test_types_refusals() -> None:
    graph = _typed_graph()
    call = (
        "MATCH ()-[r:CITES|MENTIONS]->() WITH collect(r) AS rs "
        "CALL db.relationship_embeddings.embed({{{params}, text_column:'ctx', relationships: rs}}) "
        "YIELD embedded RETURN embedded"
    )
    with pytest.raises(kglite.CypherExecutionError, match="has type 'MENTIONS', expected one of 'CITES', 'REFUTES'"):
        graph.cypher(call.format(params="types:['CITES','REFUTES']"))
    with pytest.raises(kglite.CypherExecutionError, match="'type' and 'types' are mutually exclusive"):
        graph.cypher(call.format(params="type:'CITES', types:['CITES']"))
    with pytest.raises(
        kglite.CypherExecutionError, match="'types' is empty; name at least one relationship type, or pass type"
    ):
        graph.cypher(call.format(params="types:[]"))
    assert graph.list_embeddings() == [], "a refused call embeds nothing"
