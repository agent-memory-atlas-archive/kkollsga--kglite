"""The generic embedding methods route by ``entity=`` to a specific twin.

``set_embeddings`` / ``add_embeddings`` / ``embed_texts`` / ``embeddings`` and
the vector-index lifecycle are routers: ``entity="node"`` (the default) is the
``*_node_*`` twin, ``entity="relationship"`` the ``*_relationship_*`` twin.
Every router call here runs beside its twin on an identical graph and must
return the same thing, fail with the same error, and leave the same store.
"""

from __future__ import annotations

import inspect

import pytest

from kglite import KnowledgeGraph

KEYS = {"SUPPORTS": "uid"}
# (source, target, uid, evidence) — uids b and c are a parallel group 1 -> 20.
EDGES = [(1, 10, "a", "alpha"), (1, 20, "b", "beta"), (1, 20, "c", "gamma"), (2, 10, "d", "delta")]
NODE_VECTORS = {1: [1.0, 0.0], 2: [0.0, 1.0]}
REL_VECTORS = {(1, 10, "a"): [1.0, 0.0], (1, 20, "b"): [0.0, 1.0], (1, 20, "c"): [0.6, 0.8], (2, 10, "d"): [0.3, 0.1]}


class _Stub:
    dimension = 2
    model_id = "stub/routes"

    def embed(self, texts: list[str]) -> list[list[float]]:
        return [[float(len(text)), float(ord(text[0]) - 96)] for text in texts]


def _graph() -> KnowledgeGraph:
    graph = KnowledgeGraph()
    graph.cypher(
        "CREATE (:Claimant {id: 1, note: 'first'}), (:Claimant {id: 2, note: 'second'}), "
        "(:Claim {id: 10}), (:Claim {id: 20})"
    )
    for source, target, uid, evidence in EDGES:
        graph.cypher(
            "MATCH (s:Claimant {id: $s}), (t:Claim {id: $t}) CREATE (s)-[:SUPPORTS {uid: $uid, evidence: $e}]->(t)",
            params={"s": source, "t": target, "uid": uid, "e": evidence},
        )
    graph.set_embedder(_Stub())
    return graph


def _outcome(call):
    try:
        return ("ok", call())
    except Exception as error:  # noqa: BLE001 — the comparison is the point
        return ("error", type(error).__name__, str(error))


def _state(graph: KnowledgeGraph):
    """Everything a write could have changed, read through the specific twins."""
    return (
        _outcome(lambda: graph.node_embeddings("Claimant", "note")),
        _outcome(lambda: graph.relationship_embeddings("SUPPORTS", "evidence", relationship_keys=KEYS)),
        graph.list_embeddings(),
    )


def _same(router, twin, prepare=None):
    """Run ``router`` and ``twin`` on twin graphs; outcome and state must match."""
    left, right = _graph(), _graph()
    if prepare is not None:
        prepare(left)
        prepare(right)
    routed, direct = _outcome(lambda: router(left)), _outcome(lambda: twin(right))
    assert routed == direct
    assert _state(left) == _state(right)
    return routed


# ── writers ──────────────────────────────────────────────────────────────────


@pytest.mark.parametrize("generic", ["set_embeddings", "add_embeddings"])
def test_node_writer_is_the_default_route(generic: str) -> None:
    specific = generic.replace("_embeddings", "_node_embeddings")
    outcome = _same(
        lambda g: getattr(g, generic)("Claimant", "note", NODE_VECTORS, "dot_product"),
        lambda g: getattr(g, specific)("Claimant", "note", NODE_VECTORS, "dot_product"),
    )
    assert outcome[0] == "ok"
    _same(
        lambda g: getattr(g, generic)("Claimant", "note", NODE_VECTORS, entity="node"),
        lambda g: getattr(g, specific)("Claimant", "note", NODE_VECTORS),
    )


@pytest.mark.parametrize("generic", ["set_embeddings", "add_embeddings"])
def test_relationship_writer_route(generic: str) -> None:
    specific = generic.replace("_embeddings", "_relationship_embeddings")
    outcome = _same(
        lambda g: getattr(g, generic)(
            "SUPPORTS", "evidence", REL_VECTORS, entity="relationship", relationship_keys=KEYS, metric="cosine"
        ),
        lambda g: getattr(g, specific)("SUPPORTS", "evidence", REL_VECTORS, relationship_keys=KEYS, metric="cosine"),
    )
    assert outcome[0] == "ok"
    assert outcome[1]["embeddings_stored"] == 4


@pytest.mark.parametrize("generic", ["set_embeddings", "add_embeddings"])
@pytest.mark.parametrize(
    "args",
    [
        ("Claimant", "nope", NODE_VECTORS),  # missing column
        ("Claimant", "note_emb", NODE_VECTORS),  # the store name
        ("Nobody", "note", NODE_VECTORS),  # unknown type
        ("Claimant", "note", [[1.0, 0.0]]),  # not a dict
        ("Claimant", "note", {1: [1.0, float("nan")]}),  # non-finite
    ],
)
def test_node_writer_refusals_are_the_twins(generic: str, args) -> None:
    specific = generic.replace("_embeddings", "_node_embeddings")
    outcome = _same(lambda g: getattr(g, generic)(*args), lambda g: getattr(g, specific)(*args))
    assert outcome[0] == "error"


@pytest.mark.parametrize("generic", ["set_embeddings", "add_embeddings"])
@pytest.mark.parametrize(
    "args",
    [
        ("NOPE", "evidence", REL_VECTORS),  # unknown relationship type
        ("SUPPORTS", "missing", REL_VECTORS),  # a property no relationship carries
        ("SUPPORTS", "evidence", {(1, 20): [1.0, 0.0]}),  # ambiguous parallel group
        ("SUPPORTS", "evidence", "rows"),  # wrong shape
    ],
)
def test_relationship_writer_refusals_are_the_twins(generic: str, args) -> None:
    specific = generic.replace("_embeddings", "_relationship_embeddings")
    outcome = _same(
        lambda g: getattr(g, generic)(*args, entity="relationship"),
        lambda g: getattr(g, specific)(*args),
    )
    assert outcome[0] == "error"


# ── embed_texts ──────────────────────────────────────────────────────────────


def test_embed_texts_routes() -> None:
    node = _same(
        lambda g: g.embed_texts("Claimant", "note", show_progress=False),
        lambda g: g.embed_node_texts("Claimant", "note", show_progress=False),
    )
    assert node[0] == "ok" and node[1]["embedded"] == 2
    rel = _same(
        lambda g: g.embed_texts("SUPPORTS", "evidence", 2, False, "all", entity="relationship", metric="dot_product"),
        lambda g: g.embed_relationship_texts(
            "SUPPORTS", "evidence", batch_size=2, show_progress=False, mode="all", metric="dot_product"
        ),
    )
    assert rel[0] == "ok" and rel[1]["embedded"] == 4
    # Refusals travel unchanged too.
    node_refusal = _same(
        lambda g: g.embed_texts("Claimant", "note", mode="bogus"),
        lambda g: g.embed_node_texts("Claimant", "note", mode="bogus"),
    )
    assert node_refusal[0] == "error"
    rel_refusal = _same(
        lambda g: g.embed_texts("SUPPORTS", "evidence", mode="bogus", entity="relationship"),
        lambda g: g.embed_relationship_texts("SUPPORTS", "evidence", mode="bogus"),
    )
    assert rel_refusal[0] == "error"


# ── readers ──────────────────────────────────────────────────────────────────


def _embedded(graph: KnowledgeGraph) -> None:
    graph.set_node_embeddings("Claimant", "note", NODE_VECTORS)
    graph.set_relationship_embeddings("SUPPORTS", "evidence", REL_VECTORS, relationship_keys=KEYS)


def test_embeddings_reader_routes() -> None:
    graph = _graph()
    _embedded(graph)
    assert graph.embeddings("Claimant", "note") == graph.node_embeddings("Claimant", "note") == NODE_VECTORS
    assert graph.embeddings("note") == graph.node_embeddings("note")
    assert graph.embeddings("Claimant", "note", entity="node") == NODE_VECTORS
    routed = graph.embeddings("SUPPORTS", "evidence", entity="relationship", relationship_keys=KEYS)
    assert routed == graph.relationship_embeddings("SUPPORTS", "evidence", relationship_keys=KEYS)
    assert len(routed) == 4
    assert _outcome(lambda: graph.embeddings("NOPE", "evidence", entity="relationship")) == _outcome(
        lambda: graph.relationship_embeddings("NOPE", "evidence")
    )


# ── vector-index lifecycle ───────────────────────────────────────────────────


@pytest.mark.parametrize(
    ("entity", "type_name", "column"), [("node", "Claimant", "note"), ("relationship", "SUPPORTS", "evidence")]
)
def test_vector_index_lifecycle_routes(entity: str, type_name: str, column: str) -> None:
    def call(graph: KnowledgeGraph, verb: str, generic: bool, *args, **kwargs):
        if generic:
            return getattr(graph, f"{verb}_vector_index")(type_name, column, *args, entity=entity, **kwargs)
        return getattr(graph, f"{verb}_{entity}_vector_index")(type_name, column, *args, **kwargs)

    for generic_first in (True, False):
        left, right = _graph(), _graph()
        _embedded(left)
        _embedded(right)
        trail = []
        for graph, generic in ((left, generic_first), (right, not generic_first)):
            steps = [
                _outcome(lambda: call(graph, "refresh", generic)),  # no index yet: refused
                _outcome(lambda: call(graph, "has", generic)),
                _outcome(lambda: call(graph, "build", generic, 8, metric="cosine")),
                _outcome(lambda: call(graph, "has", generic)),
                _outcome(lambda: call(graph, "refresh", generic)),
                _outcome(lambda: call(graph, "drop", generic)),
                _outcome(lambda: call(graph, "drop", generic)),
                _outcome(lambda: call(graph, "has", generic)),
            ]
            trail.append(steps)
        assert trail[0] == trail[1]
        assert [step[0] for step in trail[0]] == ["error", "ok", "ok", "ok", "ok", "ok", "ok", "ok"]
        assert [step[1] for step in trail[0][1:]] == [
            False,
            {"indexed": 2 if entity == "node" else 4, "metric": "cosine", "m": 8},
            True,
            0,
            True,
            False,
            False,
        ]


def test_the_default_route_is_node_for_every_router() -> None:
    graph = _graph()
    _embedded(graph)
    assert graph.build_vector_index("Claimant", "note")["indexed"] == 2
    assert graph.has_vector_index("Claimant", "note") is True
    assert graph.has_relationship_vector_index("SUPPORTS", "evidence") is False
    # A relationship type on the default route is a node lookup, not a relationship one.
    assert _outcome(lambda: graph.build_vector_index("SUPPORTS", "evidence")) == _outcome(
        lambda: graph.build_node_vector_index("SUPPORTS", "evidence")
    )
    for name in ("set_embeddings", "add_embeddings", "embed_texts", "embeddings"):
        parameter = inspect.signature(getattr(graph, name)).parameters["entity"]
        assert parameter.default == "node"
        assert parameter.kind is inspect.Parameter.KEYWORD_ONLY


# ── keywords that belong to the other route ──────────────────────────────────


@pytest.mark.parametrize(
    ("call", "message"),
    [
        (
            lambda g: g.set_embeddings("Claimant", "note", NODE_VECTORS, relationship_keys=KEYS),
            "set_embeddings(): `relationship_keys` belongs to entity='relationship'; this call routes to "
            "set_node_embeddings()",
        ),
        (
            lambda g: g.add_embeddings("Claimant", "note", NODE_VECTORS, relationship_keys=KEYS),
            "add_embeddings(): `relationship_keys` belongs to entity='relationship'; this call routes to "
            "add_node_embeddings()",
        ),
        (
            lambda g: g.embed_texts("Claimant", "note", metric="cosine"),
            "embed_texts(): `metric` belongs to entity='relationship'; this call routes to embed_node_texts()",
        ),
        (
            lambda g: g.embeddings("Claimant", "note", relationship_keys=KEYS),
            "embeddings(): `relationship_keys` belongs to entity='relationship'; this call routes to node_embeddings()",
        ),
    ],
)
def test_a_keyword_the_route_does_not_take_is_refused_by_name(call, message: str) -> None:
    graph = _graph()
    before = _state(graph)
    with pytest.raises(TypeError) as raised:
        call(graph)
    assert str(raised.value) == message
    assert _state(graph) == before


def test_an_unknown_entity_and_a_missing_relationship_column_are_refused() -> None:
    graph = _graph()
    with pytest.raises(ValueError, match=r"^set_embeddings\(entity='edge'\): entity must be 'node'"):
        graph.set_embeddings("SUPPORTS", "evidence", REL_VECTORS, entity="edge")
    with pytest.raises(ValueError, match=r"^has_vector_index\(entity='Node'\)"):
        graph.has_vector_index("Claimant", "note", entity="Node")
    with pytest.raises(TypeError, match=r"text_column is missing"):
        graph.embeddings("SUPPORTS", entity="relationship")
