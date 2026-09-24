"""A missing store behind `text_score` is reported as `text_score`'s.

`text_score(d, 's', …)` is rewritten into `vector_score(d, 's_emb', …)` before
execution, so a graph without that store answered "vector_score(): no
embedding 's_emb' found for node type 'D'" — a function the user did not call
and a store name they never wrote.
"""

import pytest

from kglite import KnowledgeGraph


@pytest.fixture
def graph():
    g = KnowledgeGraph()
    g.cypher("CREATE (:D {id:'a', s:'x'})-[:R {s:'y'}]->(:D {id:'b', s:'z'})")
    return g


@pytest.mark.parametrize(
    "query",
    [
        "MATCH (d:D) RETURN text_score(d, 's', [1.0, 0.0]) AS sc",
        "MATCH (d:D) RETURN d.id, text_score(d, 's', [1.0, 0.0]) AS sc ORDER BY sc DESC LIMIT 1",
        "MATCH (d:D) WHERE text_score(d, 's', [1.0, 0.0]) > 0.5 RETURN d.id",
    ],
    ids=["projection", "fused-top-k", "where"],
)
def test_node_message_names_text_score_and_the_property(graph, query):
    with pytest.raises(Exception) as error:
        graph.cypher(query)
    message = str(error.value)
    assert "text_score(): no embedding for property 's' on node type 'D'" in message
    assert "embed_texts('D', 's')" in message
    assert "vector_score" not in message and "s_emb" not in message


@pytest.mark.parametrize(
    "query",
    [
        "MATCH ()-[r:R]->() RETURN text_score(r, 's', [1.0, 0.0]) AS sc",
        "MATCH ()-[r:R]->() RETURN text_score(r, 's', [1.0, 0.0]) AS sc ORDER BY sc DESC LIMIT 1",
        "MATCH ()-[r:R]->() WHERE text_score(r, 's', [1.0, 0.0]) > 0.5 RETURN r",
    ],
    ids=["projection", "fused-top-k", "where"],
)
def test_relationship_message_names_text_score_and_the_property(graph, query):
    with pytest.raises(Exception) as error:
        graph.cypher(query)
    message = str(error.value)
    assert "text_score(): no embedding for property 's' on relationship type 'R'" in message
    assert "db.edge_embeddings.embed" in message
    assert "vector_score" not in message and "s_emb" not in message


def test_vector_score_keeps_its_store_terms(graph):
    with pytest.raises(Exception, match=r"vector_score\(\): no embedding 's_emb' found for node type 'D'"):
        graph.cypher("MATCH (d:D) RETURN vector_score(d, 's_emb', [1.0, 0.0]) AS sc")
