"""EXPLAIN names the index state of a fused top-K and never truncates operators."""

import pandas as pd
import pytest

from tests.test_retrieval_options import _graph

VECTOR = "MATCH (d:Doc) RETURN vector_score(d, 'summary_emb', [1.0,0.0]%s) AS s ORDER BY s DESC LIMIT 4"
WITH_VECTOR = (
    "MATCH (d:Doc) WITH d, vector_score(d, 'summary_emb', [1.0,0.0]) AS s ORDER BY s DESC LIMIT 4 "
    "OPTIONAL MATCH (d)-[:R]->(x) RETURN d.id, s"
)
BM25 = "MATCH (d:Doc) RETURN d.id AS id, text_bm25(d, 'body', 'alpha beta') AS s ORDER BY s DESC LIMIT 3"


def _ops(result):
    return [row["operation"] for row in result.to_list()]


@pytest.mark.parametrize(
    "indexed,options,label",
    [(True, "", "[hnsw]"), (False, "", "[exact]"), (True, ", {exact:true}", "[exact]")],
)
def test_vector_top_k_names_index_state(indexed, options, label):
    ops = _ops(_graph(indexed=indexed).cypher("EXPLAIN " + VECTOR % options))
    fused = [op for op in ops if op.startswith("FusedVectorScoreTopK")]
    assert len(fused) == 1 and label in fused[0], ops


def test_with_scope_top_k_names_index_state():
    ops = _ops(_graph().cypher("EXPLAIN " + WITH_VECTOR))
    assert any(op.startswith("FusedVectorScoreTopK [hnsw]") for op in ops), ops


@pytest.mark.parametrize("indexed,label", [(True, "[postings]"), (False, "[scan]")])
def test_bm25_top_k_names_index_state(indexed, label):
    import kglite

    g = kglite.KnowledgeGraph()
    df = pd.DataFrame({"id": [1, 2, 3], "title": ["a", "b", "c"], "body": ["alpha", "beta", "alpha beta"]})
    g.add_nodes(df, "Doc", "id", "title")
    if indexed:
        g.build_text_index("Doc", "body")
    ops = _ops(g.cypher("EXPLAIN " + BM25))
    fused = [op for op in ops if op.startswith("FusedTextBm25TopK")]
    assert len(fused) == 1 and label in fused[0], ops


def test_explain_render_keeps_long_operator_names():
    g = _graph()
    text = str(g.cypher("EXPLAIN " + VECTOR % ""))
    assert "FusedVectorScoreTopK [hnsw] [requested=auto]" in text
    unfused = str(g.cypher("EXPLAIN MATCH (a:Doc)-[:R*2..3]->(b:Doc) RETURN a"))
    assert "Expand (:Doc)-[:R*2..3]->(:Doc)" in unfused
    assert "..." not in text.split("\n", 1)[1]
