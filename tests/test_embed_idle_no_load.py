"""A bulk embedding pass with nothing to embed must not load the model.

Operator report: a second `embed_texts` over 6,848 unchanged texts reported
`embedded: 0` yet took 9.5 s, because the embedder's `load()` ran anyway.
"""

from __future__ import annotations

import pytest

from kglite import KnowledgeGraph


class _Counting:
    dimension = 2

    def __init__(self, model_id: str | None = "stub/idle") -> None:
        if model_id is not None:
            self.model_id = model_id
        self.loads = 0
        self.unloads = 0
        self.embedded = 0

    def load(self) -> None:
        self.loads += 1

    def unload(self) -> None:
        self.unloads += 1

    def embed(self, texts: list[str]) -> list[list[float]]:
        self.embedded += len(texts)
        return [[float(len(text)), 1.0] for text in texts]


def _graph(model: _Counting) -> KnowledgeGraph:
    graph = KnowledgeGraph()
    graph.cypher("CREATE (:Doc {id: 1, note: 'one'}), (:Doc {id: 2, note: 'two'}), (:Doc {id: 3, note: ''})")
    graph.cypher("MATCH (a:Doc {id: 1}), (b:Doc {id: 2}) CREATE (a)-[:CITES {uid: 'x', why: 'because'}]->(b)")
    graph.cypher("MATCH (a:Doc {id: 2}), (b:Doc {id: 1}) CREATE (a)-[:CITES {uid: 'y', why: 'since'}]->(b)")
    graph.set_embedder(model)
    return graph


def _node(graph: KnowledgeGraph, mode: str | None = None) -> dict:
    return graph.embed_texts("Doc", "note", mode=mode, show_progress=False)


def _rel(graph: KnowledgeGraph, mode: str | None = None) -> dict:
    return graph.embed_relationship_texts("CITES", "why", mode=mode, show_progress=False)


@pytest.mark.parametrize("model_id", ["stub/idle", None], ids=["named", "unnamed"])
@pytest.mark.parametrize("route", [_node, _rel], ids=["node", "relationship"])
def test_idle_pass_never_loads_and_a_changed_text_does(route, model_id: str | None) -> None:
    model = _Counting(model_id)
    graph = _graph(model)
    first = route(graph)
    assert first["embedded"] == 2
    assert (model.loads, model.unloads) == (1, 1) or model_id is None
    loads_after_first = model.loads

    again = route(graph)
    assert again["embedded"] == 0
    assert again["skipped_existing"] == 2
    assert again["reembedded_changed"] == 0
    assert again["dimension"] == 2
    assert model.loads == loads_after_first, "an idle pass loaded the embedder"
    assert model.unloads == model.loads

    changed = route(graph, "changed")
    assert changed["embedded"] == 0
    assert model.loads == loads_after_first, "an idle 'changed' pass loaded the embedder"

    if route is _node:
        graph.cypher("MATCH (d:Doc {id: 1}) SET d.note = 'one, edited'")
    else:
        graph.cypher("MATCH ()-[r:CITES {uid: 'x'}]->() SET r.why = 'because, edited'")
    refreshed = route(graph, "changed")
    assert refreshed["embedded"] == 1
    assert refreshed["reembedded_changed"] == 1
    # An unnamed embedder pays one extra load/unload pair to be asked its id.
    assert model.loads > loads_after_first
    assert model.unloads == model.loads


def test_query_time_embedding_still_loads() -> None:
    model = _Counting()
    graph = _graph(model)
    _node(graph)
    before = model.loads
    graph.select("Doc").search_text("note", "one", top_k=1)
    assert model.loads == before + 1


class _Wider(_Counting):
    dimension = 3

    def embed(self, texts: list[str]) -> list[list[float]]:
        self.embedded += len(texts)
        return [[float(len(text)), 1.0, 0.0] for text in texts]


@pytest.mark.parametrize("route", [_node, _rel], ids=["node", "relationship"])
def test_idle_pass_still_refuses_a_model_of_another_width(route) -> None:
    graph = _graph(_Counting())
    route(graph)
    wider = _Wider()
    graph.set_embedder(wider)
    with pytest.raises(ValueError, match="mode='all'"):
        route(graph)
    assert wider.loads == 0
