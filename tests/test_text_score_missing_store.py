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
    # The remedy must run as written: embed() requires `relationships`.
    assert (
        "MATCH ()-[r:R]->() WITH collect(r) AS rs CALL db.relationship_embeddings.embed("
        "{type: 'R', text_column: 's', relationships: rs})" in message
    )
    assert "vector_score" not in message and "s_emb" not in message


def test_vector_score_keeps_its_store_terms(graph):
    with pytest.raises(Exception, match=r"vector_score\(\): no embedding 's_emb' found for node type 'D'"):
        graph.cypher("MATCH (d:D) RETURN vector_score(d, 's_emb', [1.0, 0.0]) AS sc")


# ── A store name written where the text column belongs ──────────────────────
# `text_score(r, 's_emb', …)` with an `R.s` store present used to recommend
# embedding `s_emb`; following that created an empty store and the query then
# returned null for every row.


@pytest.fixture
def embedded(graph):
    graph.set_embeddings("D", "s", {"a": [1.0, 0.0], "b": [0.0, 1.0]})
    graph.cypher(
        "MATCH ()-[r:R]->() CALL db.relationship_embeddings.set({type:'R', text_column:'s', "
        "entries:[{relationship:r, vector:[1.0, 0.0]}]}) YIELD stored RETURN stored"
    )
    return graph


@pytest.mark.parametrize(
    ("query", "entity"),
    [
        ("MATCH ()-[r:R]->() RETURN text_score(r, 's_emb', [1.0, 0.0]) AS sc", "r"),
        ("MATCH ()-[r:R]->() WHERE text_score(r, 's_emb', [1.0, 0.0]) > 0.5 RETURN r", "r"),
        # The hint spells the variable generically, as the node twin's does.
        ("MATCH (d:D) RETURN text_score(d, 's_emb', [1.0, 0.0]) AS sc", "n"),
    ],
    ids=["relationship", "relationship-where", "node"],
)
def test_text_score_given_a_store_name_points_at_the_text_column(embedded, query, entity):
    with pytest.raises(Exception) as error:
        embedded.cypher(query)
    message = str(error.value)
    assert "no embedding for property 's_emb'" in message
    assert "Did you mean 's'?" in message
    assert f"text_score({entity}, 's', <query text>) takes the text column" in message
    # The embed remedy would create an empty `s_emb_emb` store: it must not be offered.
    assert "embed" not in message.replace("embedding", "")
    stores = sorted(row["store_name"] for row in embedded.list_embeddings())
    assert stores == ["s_emb", "s_emb"]


def test_relationship_vector_score_given_the_column_points_at_the_store(embedded):
    with pytest.raises(Exception) as error:
        embedded.cypher("MATCH ()-[r:R]->() RETURN vector_score(r, 's', [1.0, 0.0]) AS sc")
    message = str(error.value)
    assert "no embedding 's' found for relationship type 'R'" in message
    assert "Did you mean 's_emb'?" in message
    assert "text_score(r, 's', <query text>) takes the text column" in message
