"""A store small enough to scan exactly is scanned exactly, even with an HNSW index.

The suite-wide conftest lowers the exact-scan size to 0 so index-path fixtures
stay small; these tests run in a child process that keeps the real size.
"""

import json
import os
import subprocess
import sys
import textwrap

import pytest

PROBE = textwrap.dedent(
    """
    import json, random
    import pandas as pd
    import kglite

    N, D = {n}, {d}
    rng = random.Random(11)
    g = kglite.KnowledgeGraph()
    frame = pd.DataFrame({{"id": list(range(N)), "title": ["t"] * N, "summary": ["t"] * N}})
    g.add_nodes(frame, "Doc", "id", "title")
    emb = {{i: [rng.gauss(0, 1) for _ in range(D)] for i in range(N)}}
    g.set_embeddings("Doc", "summary", emb, metric="cosine")
    # Plant a tight cluster around vector 0 so the top 10 has structure.
    crng = random.Random(5)
    cluster = {{i: [v + crng.gauss(0, 0.01) for v in emb[0]] for i in range(1, 40)}}
    g.add_embeddings("Doc", "summary", cluster)
    emb.update(cluster)
    g.build_vector_index("Doc", "summary")
    q = emb[0]
    query = "MATCH (d:Doc) RETURN d.id AS id, vector_score(d, 'summary_emb', $q) AS s ORDER BY s DESC LIMIT 10"
    explain = [r["operation"] for r in g.cypher("EXPLAIN " + query, params={{"q": q}}).to_list()]
    profile = g.cypher("PROFILE " + query, params={{"q": q}})
    info = profile.diagnostics["retrieval"][0]
    exact = [r["id"] for r in g.select("Doc").vector_search("summary", q, top_k=10, exact=True)]
    default = [r["id"] for r in g.select("Doc").vector_search("summary", q, top_k=10)]
    print(json.dumps({{
        "explain": [op for op in explain if op.startswith("FusedVectorScoreTopK")],
        "mode": info["actual_mode"],
        "reason": info.get("fallback_reason"),
        "default_equals_exact": default == exact,
    }}))
    """
)


def _probe(n: int, d: int, floats: str | None) -> dict:
    env = {k: v for k, v in os.environ.items() if k != "KGLITE_EXACT_SCAN_FLOATS"}
    if floats is not None:
        env["KGLITE_EXACT_SCAN_FLOATS"] = floats
    done = subprocess.run(
        [sys.executable, "-c", PROBE.format(n=n, d=d)],
        capture_output=True,
        text=True,
        env=env,
        timeout=100,
        check=False,
    )
    assert done.returncode == 0, done.stderr
    return json.loads(done.stdout.strip().splitlines()[-1])


def test_a_small_indexed_store_is_answered_exactly():
    # 1,500 x 64 = 96,000 floats, under the exact-scan size.
    result = _probe(1500, 64, None)
    assert result["mode"] == "exact"
    assert result["reason"] == "small_store"
    assert any("[exact]" in op for op in result["explain"]), result["explain"]
    assert result["default_equals_exact"]


def test_a_large_indexed_store_is_answered_by_the_index():
    # 1,500 x 256 = 384,000 floats, over the exact-scan size.
    result = _probe(1500, 256, None)
    assert result["mode"] == "hnsw"
    assert result["reason"] is None
    assert any("[hnsw]" in op for op in result["explain"]), result["explain"]


@pytest.mark.parametrize("floats", ["0"])
def test_lowering_the_size_restores_the_index_path_for_a_small_store(floats):
    result = _probe(1500, 64, floats)
    assert result["mode"] == "hnsw"
    assert any("[hnsw]" in op for op in result["explain"]), result["explain"]
