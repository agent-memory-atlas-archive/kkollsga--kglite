"""Retrieval under a valid-time context: BM25 and vector ranking over only the
elements valid at the instant, in every storage mode.

The reference is a separate graph holding only the valid versions, with its
own text index and embeddings. BM25 is also checked against a brute-force
BM25 computed here from the reference's documents. The prefixed query on the
full graph and the same query on ``freeze(valid_at=…)`` must equal both.
"""

from __future__ import annotations

import datetime as dt
import math
import re

import numpy as np
import pytest

import kglite

T = dt.date(2007, 6, 30)
AS_OF = "FOR VALID_TIME AS OF $t "
PERIODS = [
    (dt.date(2000, 1, 1), dt.date(2004, 12, 31)),
    (dt.date(2005, 1, 1), dt.date(2009, 12, 31)),
    (dt.date(2010, 1, 1), None),
]
VALID = 1  # the period holding T
ENTITIES = 40
DIM = 8
K1, B = 1.2, 0.75
VOCAB = [f"w{i}" for i in range(12)]


def _docs():
    """Version documents: ``vid = 3 * entity + period``. Every entity's
    versions share the word ``shared`` and most of their text, so a term's
    corpus-wide document frequency is up to three times its as-of one; the
    period-0 version of entity 0 repeats ``zeta``, which appears once in a few
    valid versions. Every seventh version has no embedding."""
    rng = np.random.default_rng(7)
    rows = []
    for entity in range(ENTITIES):
        base = [VOCAB[int(i)] for i in rng.integers(0, len(VOCAB), 5)]
        for period in range(len(PERIODS)):
            words = base + [VOCAB[int(rng.integers(0, len(VOCAB)))]] * (period + 1)
            if entity % 4 == 0:
                words.append("shared")
            if entity == 0 and period == 0:
                words += ["zeta"] * 6
            if period == VALID and entity % 9 == 1:
                words.append("zeta")
            vid = 3 * entity + period
            vector = rng.standard_normal(DIM).tolist() if vid % 7 else None
            rows.append({"vid": vid, "eid": entity, "period": period, "body": " ".join(words), "vec": vector})
    return rows


def _write(graph, rows, declare):
    params = {
        "rows": [
            {
                "vid": r["vid"],
                "eid": r["eid"],
                "body": r["body"],
                "vf": PERIODS[r["period"]][0],
                "vt": PERIODS[r["period"]][1],
            }
            for r in rows
        ]
    }
    graph.cypher(
        "UNWIND $rows AS r CREATE (:Doc {id: r.vid, vid: r.vid, eid: r.eid, body: r.body, vf: r.vf, vt: r.vt})",
        params=params,
    ).to_list()
    if declare:
        graph.set_temporal("Doc", "vf", "vt")
    graph.set_embeddings("Doc", "body", {r["vid"]: r["vec"] for r in rows if r["vec"] is not None})
    graph.build_node_vector_index("Doc", "body")
    if graph.graph_info()["storage_mode"] != "disk":  # a BM25 index is heap-resident, refused on Disk
        graph.build_text_index("Doc", "body")
    return graph


def _graph(storage, tmp_path):
    if storage == "disk":
        return kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "graph"))
    if storage == "mapped":
        return kglite.KnowledgeGraph(storage="mapped")
    return kglite.KnowledgeGraph()


@pytest.fixture(scope="module")
def rows():
    return _docs()


@pytest.fixture(scope="module")
def reference(rows):
    return _write(kglite.KnowledgeGraph(), [r for r in rows if r["period"] == VALID], declare=False)


@pytest.fixture(params=["memory", "mapped", "disk"])
def full(request, rows, tmp_path):
    return _write(_graph(request.param, tmp_path), rows, declare=True)


def _tokens(text):
    return re.findall(r"[^\W_]+", text.lower())


def _brute_bm25(docs, query):
    """Okapi BM25 over ``docs`` ({vid: text}): smoothed idf, k1 = 1.2,
    b = 0.75, each distinct query term once, in first-occurrence order."""
    tokens = {vid: _tokens(text) for vid, text in docs.items()}
    n = len(tokens)
    avgdl = sum(len(t) for t in tokens.values()) / n
    terms = list(dict.fromkeys(_tokens(query)))
    df = {term: sum(1 for t in tokens.values() if term in t) for term in terms}
    scores = {}
    for vid, words in tokens.items():
        total = 0.0
        norm = K1 * (1.0 - B + B * (len(words) / avgdl))
        for term in terms:
            tf = words.count(term)
            if tf == 0 or df[term] == 0:
                continue
            idf = math.log(1.0 + (n - df[term] + 0.5) / (df[term] + 0.5))
            total += idf * (tf * (K1 + 1.0)) / (tf + norm)
        scores[vid] = total
    return scores


BM25_TOP = "MATCH (d:Doc) RETURN d.vid AS vid, text_bm25(d, 'body', $q) AS s ORDER BY s DESC LIMIT 10"
BM25_ROWS = "MATCH (d:Doc) WHERE d.eid < 12 RETURN d.vid AS vid, text_bm25(d, 'body', $q) AS s"


def _pairs(result):
    return [(row["vid"], row["s"]) for row in result.to_list()]


def _close(left, right):
    assert [v for v, _ in left] == [v for v, _ in right], (left, right)
    for (_, a), (_, b) in zip(left, right):
        assert (a is None and b is None) or math.isclose(a, b, rel_tol=0, abs_tol=1e-12), (left, right)


@pytest.fixture
def text_full(full):
    if full.graph_info()["storage_mode"] == "disk":
        pytest.skip("Disk mode refuses a BM25 index")
    return full


@pytest.mark.parametrize("q", ["w3 w7", "shared w1", "zeta", "w0 w5 w11 shared"])
def test_bm25_ranks_every_tier_as_the_reference_slice(text_full, reference, rows, q):
    full = text_full
    params = {"t": T, "q": q}
    expected = _pairs(reference.cypher(BM25_TOP, params=params))
    brute = _brute_bm25({r["vid"]: r["body"] for r in rows if r["period"] == VALID}, q)
    for vid, score in expected:
        assert math.isclose(score, brute[vid], abs_tol=1e-12), (vid, score, brute[vid])
    _close(_pairs(full.cypher(AS_OF + BM25_TOP, params=params)), expected)
    _close(_pairs(full.freeze(valid_at=T).cypher(BM25_TOP, params=params)), expected)
    # The per-row scalar scores with the as-of statistics too.
    expected_rows = sorted(_pairs(reference.cypher(BM25_ROWS, params=params)))
    _close(sorted(_pairs(full.cypher(AS_OF + BM25_ROWS, params=params))), expected_rows)


def test_an_invisible_top_hit_gives_way_to_the_next_visible_one_under_as_of_statistics(text_full, reference):
    full = text_full
    params = {"t": T, "q": "zeta"}
    unguarded = _pairs(full.cypher(BM25_TOP, params=params))
    assert unguarded[0][0] == 0, "the period-0 version repeating zeta leads the whole corpus"
    guarded = _pairs(full.cypher(AS_OF + BM25_TOP, params=params))
    assert 0 not in [vid for vid, _ in guarded]
    top_vid, top_score = guarded[0]
    assert top_vid % 3 == VALID
    # The same document scores differently under the corpus-wide statistics.
    whole = full.cypher("MATCH (d:Doc {vid: $v}) RETURN text_bm25(d, 'body', $q) AS s", params={**params, "v": top_vid})
    assert whole.to_list()[0]["s"] != pytest.approx(top_score, abs=1e-9)
    _close(guarded, _pairs(reference.cypher(BM25_TOP, params=params)))


VECTOR_TOP = "MATCH (d:Doc) RETURN d.vid AS vid, vector_score(d, 'body_emb', $v) AS s ORDER BY s DESC LIMIT 10"
VECTOR_EXACT = (
    "MATCH (d:Doc) RETURN d.vid AS vid, vector_score(d, 'body_emb', $v, {exact: true}) AS s ORDER BY s DESC LIMIT 10"
)


def _query_vector(rows, vid):
    return next(r["vec"] for r in rows if r["vid"] == vid)


def _retrieval(result):
    return (result.diagnostics or {}).get("retrieval", [])


def test_vector_top_k_is_exact_over_the_mask(full, reference, rows):
    params = {"t": T, "v": _query_vector(rows, 5)}
    expected = _pairs(reference.cypher(VECTOR_EXACT, params=params))
    # Versions without an embedding score NULL and lead the descending order,
    # as the reference ranks them.
    assert expected[0][1] is None
    result = full.cypher(AS_OF + VECTOR_TOP, params=params)
    _close(_pairs(result), expected)
    assert {"actual_mode": "exact", "fallback_reason": "exact_mask"}.items() <= _retrieval(result)[0].items()
    assert result.diagnostics["temporal"]["retrieval"] == "exact_mask"
    _close(_pairs(full.freeze(valid_at=T).cypher(VECTOR_TOP, params=params)), expected)
    non_null = (
        "MATCH (d:Doc) WHERE vector_score(d, 'body_emb', $v) IS NOT NULL "
        "RETURN d.vid AS vid, vector_score(d, 'body_emb', $v) AS s ORDER BY s DESC LIMIT 10"
    )
    _close(_pairs(full.cypher(AS_OF + non_null, params=params)), _pairs(reference.cypher(non_null, params=params)))


def test_the_invisible_nearest_vector_is_not_returned(full, rows):
    invisible = 3  # entity 1, period 0
    params = {"t": T, "v": _query_vector(rows, invisible)}
    assert _pairs(full.cypher(VECTOR_TOP, params=params))[0][1] is None
    nearest = (
        "MATCH (d:Doc) WHERE vector_score(d, 'body_emb', $v) IS NOT NULL "
        "RETURN d.vid AS vid ORDER BY vector_score(d, 'body_emb', $v) DESC LIMIT 1"
    )
    assert full.cypher(nearest, params=params).to_list()[0]["vid"] == invisible
    guarded = [vid for vid, _ in _pairs(full.cypher(AS_OF + VECTOR_TOP, params=params))]
    assert invisible not in guarded
    assert all(vid % 3 == VALID for vid in guarded)


def test_above_the_threshold_the_index_serves_the_admitted_candidates(full, reference, rows, monkeypatch):
    monkeypatch.setenv("KGLITE_TEMPORAL_VECTOR_EXACT_MAX", "1")
    params = {"t": T, "v": _query_vector(rows, 5)}
    non_null = (
        "MATCH (d:Doc) WHERE vector_score(d, 'body_emb', $v) IS NOT NULL "
        "RETURN d.vid AS vid, vector_score(d, 'body_emb', $v) AS s ORDER BY s DESC LIMIT 10"
    )
    result = full.cypher(AS_OF + non_null, params=params)
    got = [vid for vid, _ in _pairs(result)]
    assert _retrieval(result)[0]["actual_mode"] == "hnsw_mask"
    assert result.diagnostics["temporal"]["retrieval"] == "hnsw_mask"
    assert all(vid % 3 == VALID for vid in got) and len(got) == 10
    exact = [vid for vid, _ in _pairs(reference.cypher(non_null.replace("$v)", "$v, {exact: true})"), params=params))]
    assert len(set(got) & set(exact)) >= 9, (got, exact)


def test_embedding_query_procedures_rank_only_admitted_nodes(full, reference, rows, monkeypatch):
    params = {"t": T, "v": _query_vector(rows, 5)}
    call = (
        "CALL db.node_embeddings.query({text_column: 'body', vector: $v, top_k: 6}) "
        "YIELD node, score, search_method RETURN node.vid AS vid, score AS s, search_method AS m"
    )
    expected = [
        (r["vid"], r["s"])
        for r in reference.cypher(call.replace("top_k: 6", "top_k: 6, exact: true"), params=params).to_list()
    ]
    result = full.cypher(AS_OF + call, params=params).to_list()
    _close([(r["vid"], r["s"]) for r in result], expected)
    assert {r["m"] for r in result} == {"exact_mask"}
    routed = full.cypher(AS_OF + call.replace("db.node_embeddings.query", "db.embeddings.query"), params=params)
    _close([(r["vid"], r["s"]) for r in routed.to_list()], expected)
    monkeypatch.setenv("KGLITE_TEMPORAL_VECTOR_EXACT_MAX", "1")
    indexed = full.cypher(AS_OF + call, params=params).to_list()
    assert {r["m"] for r in indexed} == {"hnsw_mask"}
    assert all(r["vid"] % 3 == VALID for r in indexed)


def test_a_limit_past_the_store_size_returns_every_admitted_vector(full, reference, rows, monkeypatch):
    # k (200, 500) exceeds the store (~103 vectors): the index route must size
    # its fetch by what the store can give, not panic, and return every
    # admitted vector.
    monkeypatch.setenv("KGLITE_TEMPORAL_VECTOR_EXACT_MAX", "1")
    params = {"t": T, "v": _query_vector(rows, 5)}
    non_null = (
        "MATCH (d:Doc) WHERE vector_score(d, 'body_emb', $v) IS NOT NULL "
        "RETURN d.vid AS vid, vector_score(d, 'body_emb', $v) AS s ORDER BY s DESC LIMIT 200"
    )
    expected = _pairs(reference.cypher(non_null.replace("$v)", "$v, {exact: true})"), params=params))
    assert 0 < len(expected) < 200
    _close(_pairs(full.cypher(AS_OF + non_null, params=params)), expected)
    call = (
        "CALL db.node_embeddings.query({text_column: 'body', vector: $v, top_k: 500}) "
        "YIELD node, score RETURN node.vid AS vid, score AS s"
    )
    got = full.cypher(AS_OF + call, params=params).to_list()
    assert sorted(r["vid"] for r in got) == sorted(vid for vid, _ in expected)


# Relationship BM25: CITES (undeclared) joins Papers to Papers and to Books,
# both declared. A statement that names only Paper must still score with the
# statistics of the CITES documents valid at T — an edge whose Book endpoint
# is invalid is not one — whatever statement computed them first.
CITE_WORDS = ["graph", "valid", "time", "index", "rank", "query", "edge", "node"]


def _citations(graph, keep=None):
    """Papers p0..p5 and Books b0..b3; odd ones are invalid at T. ``keep``
    limits the graph to the elements valid at T (the reference)."""
    valid = PERIODS[VALID]
    stale = PERIODS[0]

    def period(i):
        return valid if i % 2 == 0 else stale

    nodes = [("Paper", f"p{i}", period(i)) for i in range(6)] + [("Book", f"b{i}", period(i)) for i in range(4)]
    alive = {name for _, name, (vf, vt) in nodes if vf <= T and (vt is None or T <= vt)}
    for label, name, (vf, vt) in nodes:
        if keep and name not in alive:
            continue
        graph.cypher(
            f"CREATE (:{label} {{id: $id, abstract: $a, vf: $vf, vt: $vt}})",
            params={"id": name, "a": " ".join(CITE_WORDS[: 2 + len(name) % 5]) + f" {name}", "vf": vf, "vt": vt},
        )
    rng = np.random.default_rng(11)
    targets = [f"p{i}" for i in range(6)] + [f"b{i}" for i in range(4)]
    k = 0
    for source in [f"p{i}" for i in range(6)]:
        for target in targets:
            if source == target:
                continue
            words = [CITE_WORDS[int(i)] for i in rng.integers(0, len(CITE_WORDS), 3 + k % 4)]
            if target.startswith("b"):
                words += ["book"] * 3
            if not keep or (source in alive and target in alive):
                label = "Book" if target.startswith("b") else "Paper"
                graph.cypher(
                    f"MATCH (s:Paper {{id: $s}}), (t:{label} {{id: $t}}) CREATE (s)-[:CITES {{k: $k, note: $n}}]->(t)",
                    params={"s": source, "t": target, "k": k, "n": " ".join(words)},
                )
            k += 1
    if not keep:
        graph.set_temporal("Paper", "vf", "vt")
        graph.set_temporal("Book", "vf", "vt")
    graph.cypher("CALL db.relationship_text_index.build({type: 'CITES', text_column: 'note'}) YIELD indexed RETURN 1")
    graph.build_text_index("Paper", "abstract")
    return graph


CITE_QUERIES = [
    "MATCH (:Paper)-[r:CITES]->(:Paper) RETURN r.k AS vid, text_bm25(r, 'note', $q) AS s ORDER BY vid",
    "MATCH ()-[r:CITES]->() RETURN r.k AS vid, text_bm25(r, 'note', $q) AS s ORDER BY vid",
    "MATCH (:Paper)-[r:CITES]->() RETURN r.k AS vid, text_bm25(r, 'note', $q) AS s ORDER BY s DESC, vid LIMIT 5",
    "MATCH (p:Paper) RETURN p.id AS vid, text_bm25(p, 'abstract', $q) AS s ORDER BY vid",
]


@pytest.mark.parametrize("storage", ["memory", "mapped"])
@pytest.mark.parametrize("q", ["book graph", "valid rank edge"])
def test_relationship_bm25_scores_with_the_instant_statistics_whichever_query_asks(tmp_path, storage, q):
    reference = _citations(kglite.KnowledgeGraph(), keep=True)
    params = {"t": T, "q": q}
    expected = [_pairs(reference.cypher(query, params=params)) for query in CITE_QUERIES]
    assert expected[1] != expected[0], "the Paper-only pattern binds fewer edges"
    # A fresh graph per order and per tier: a view pins the instant's masks,
    # which would hide what a prefixed statement alone computes first.
    for order in [CITE_QUERIES, CITE_QUERIES[::-1]]:
        full = _citations(_graph(storage, tmp_path / f"p{len(order[0])}"))
        for query in order:
            _close(_pairs(full.cypher(AS_OF + query, params=params)), expected[CITE_QUERIES.index(query)])
        view = _citations(_graph(storage, tmp_path / f"v{len(order[0])}")).freeze(valid_at=T)
        for query in order:
            _close(_pairs(view.cypher(query, params=params)), expected[CITE_QUERIES.index(query)])


def test_a_disk_mask_over_its_cap_declines_the_vector_entry_to_the_guarded_matcher(full, reference, rows, monkeypatch):
    # Over the Disk mask cap the vector top-k entry cannot mask the store; it
    # declines, and the guarded matcher answers as every other tier does.
    monkeypatch.setenv("KGLITE_TEMPORAL_DISK_MASK_MAX_BYTES", "1")
    params = {"t": T, "v": _query_vector(rows, 5)}
    expected = _pairs(reference.cypher(VECTOR_EXACT, params=params))
    _close(_pairs(full.cypher(AS_OF + VECTOR_TOP, params=params)), expected)
    _close(_pairs(full.cypher(AS_OF + VECTOR_EXACT, params=params)), expected)
