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
import pandas as pd
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
    assert _retrieval(result)[0]["actual_mode"] == "hnsw_mask"
    assert _retrieval(result)[0]["fallback_reason"] is None
    assert result.diagnostics["temporal"]["retrieval"] == "hnsw_mask"
    # One filtered search, whose ef-wide result set holds every admitted
    # vector at this size: the exact answer, scores and order.
    _close(_pairs(result), _pairs(reference.cypher(non_null.replace("$v)", "$v, {exact: true})"), params=params)))
    # With NULL-scoring versions leading the order, as the reference ranks them.
    _close(
        _pairs(full.cypher(AS_OF + VECTOR_TOP, params=params)), _pairs(reference.cypher(VECTOR_EXACT, params=params))
    )


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


# A store holding a vector for every version, in node order: the store's
# slots are the scan's rows, and the route tests admission per slot it
# touches. Versions are valid at T in a pseudo-random fifth.
COVERED_TOP = "MATCH (d:Doc) RETURN d.vid AS vid, vector_score(d, 'body_emb', $v) AS s ORDER BY s DESC LIMIT 10"


def _covered_rows(n, dim, share, seed=3):
    rng = np.random.default_rng(seed)
    valid = rng.random(n) < share
    return [
        {"vid": vid, "valid": bool(valid[vid]), "vec": rng.standard_normal(dim).astype(np.float32).tolist()}
        for vid in range(n)
    ]


def _covered_graph(graph, rows):
    frame = pd.DataFrame(
        {
            "vid": [r["vid"] for r in rows],
            "name": [f"d{r['vid']}" for r in rows],
            "body": ["x"] * len(rows),
            "vf": pd.to_datetime([PERIODS[VALID][0] if r["valid"] else PERIODS[0][0] for r in rows]),
            "vt": pd.to_datetime([PERIODS[VALID][1] if r["valid"] else PERIODS[0][1] for r in rows]),
        }
    )
    graph.add_nodes(frame, "Doc", "vid", "name")
    graph.set_temporal("Doc", "vf", "vt")
    graph.set_embeddings("Doc", "body", {r["vid"]: r["vec"] for r in rows})
    graph.build_node_vector_index("Doc", "body")
    return graph


def _covered_reference(rows):
    valid = [r for r in rows if r["valid"]]
    graph = kglite.KnowledgeGraph()
    graph.add_nodes(
        pd.DataFrame({"vid": [r["vid"] for r in valid], "name": ["x"] * len(valid), "body": ["x"] * len(valid)}),
        "Doc",
        "vid",
        "name",
    )
    graph.set_embeddings("Doc", "body", {r["vid"]: r["vec"] for r in valid})
    return graph


@pytest.fixture(scope="module")
def covered_rows():
    return _covered_rows(600, 8, 0.2)


@pytest.mark.parametrize("storage", ["memory", "mapped", "disk"])
@pytest.mark.parametrize("forced", [None, "1"])
def test_a_covering_store_ranks_the_valid_vectors_exactly_on_both_routes(
    tmp_path, covered_rows, storage, forced, monkeypatch
):
    if forced:
        monkeypatch.setenv("KGLITE_TEMPORAL_VECTOR_EXACT_MAX", forced)
    full = _covered_graph(_graph(storage, tmp_path), covered_rows)
    reference = _covered_reference(covered_rows)
    for q in (0, 5, 17, 42):
        params = {"t": T, "v": covered_rows[q]["vec"]}
        expected = _pairs(reference.cypher(COVERED_TOP.replace("$v)", "$v, {exact: true})"), params=params))
        result = full.cypher(AS_OF + COVERED_TOP, params=params)
        _close(_pairs(result), expected)
        route = "hnsw_mask" if forced else "exact_mask"
        assert result.diagnostics["temporal"]["retrieval"] == route
        assert _retrieval(result)[0]["fallback_reason"] == (None if forced else "exact_mask")
        _close(_pairs(full.freeze(valid_at=T).cypher(COVERED_TOP, params=params)), expected)


@pytest.mark.parametrize("storage", ["memory", "mapped", "disk"])
@pytest.mark.parametrize("forced", [None, "1"])
def test_an_invisible_nearest_vector_never_comes_back_from_a_covering_store(
    tmp_path, covered_rows, storage, forced, monkeypatch
):
    if forced:
        monkeypatch.setenv("KGLITE_TEMPORAL_VECTOR_EXACT_MAX", forced)
    full = _covered_graph(_graph(storage, tmp_path), covered_rows)
    invisible = [r for r in covered_rows if not r["valid"]][:5]
    for row in invisible:
        params = {"t": T, "v": row["vec"]}
        assert _pairs(full.cypher(COVERED_TOP, params=params))[0][0] == row["vid"], "the query is its own nearest"
        guarded = _pairs(full.cypher(AS_OF + COVERED_TOP, params=params))
        assert len(guarded) == 10
        assert row["vid"] not in [vid for vid, _ in guarded]
        assert all(covered_rows[vid]["valid"] for vid, _ in guarded)


def test_the_route_rule_weighs_the_admitted_count_by_the_admitted_share():
    """`admitted² / store` against 1,500: 2,000 of 4,000 valid (1,000)
    scores exactly; 3,000 of 4,000 (2,250) searches the index."""
    for share, route in ((0.5, "exact_mask"), (0.75, "hnsw_mask")):
        rows = _covered_rows(4000, 8, share, seed=11)
        admitted = sum(r["valid"] for r in rows)
        assert (admitted * admitted // 4000 >= 1500) == (route == "hnsw_mask"), admitted
        full = _covered_graph(kglite.KnowledgeGraph(), rows)
        result = full.cypher(AS_OF + COVERED_TOP, params={"t": T, "v": rows[1]["vec"]})
        assert result.diagnostics["temporal"]["retrieval"] == route, (share, admitted)


def test_a_filtered_search_past_its_budget_gives_way_to_the_exact_pass(monkeypatch):
    # About 40 valid versions in 20,000: fewer than the index's ef (64), so
    # the search can never fill its result set, never stops early, and walks
    # until its step budget runs out; the exact pass answers, saying why.
    monkeypatch.setenv("KGLITE_TEMPORAL_VECTOR_EXACT_MAX", "1")
    rows = _covered_rows(20_000, 8, 0.002, seed=5)
    full = _covered_graph(kglite.KnowledgeGraph(), rows)
    reference = _covered_reference(rows)
    params = {"t": T, "v": next(r["vec"] for r in rows if not r["valid"])}
    result = full.cypher(AS_OF + COVERED_TOP, params=params)
    assert _retrieval(result)[0]["actual_mode"] == "exact"
    assert _retrieval(result)[0]["fallback_reason"] == "exact_mask_visit_limit"
    assert result.diagnostics["temporal"]["retrieval"] == "exact_mask"
    _close(_pairs(result), _pairs(reference.cypher(COVERED_TOP.replace("$v)", "$v, {exact: true})"), params=params)))


def test_the_filtered_search_keeps_the_recall_contract_against_the_exact_valid_top_k():
    """Recall@10 of the filtered search against the exact top-10 of the valid
    vectors, over 40 stored-vector queries (valid and invalid), on 8,000
    random 32-d vectors half of which are valid — where the route rule
    itself picks the search: at least the 0.8 the unfiltered index is held
    to (`test_vector_index.py`)."""
    rows = _covered_rows(8000, 32, 0.5, seed=7)
    full = _covered_graph(kglite.KnowledgeGraph(), rows)
    hits = 0
    for row in rows[:40]:
        params = {"t": T, "v": row["vec"]}
        got = full.cypher(AS_OF + COVERED_TOP, params=params)
        assert got.diagnostics["temporal"]["retrieval"] == "hnsw_mask"
        exact = full.cypher(AS_OF + COVERED_TOP.replace("$v)", "$v, {exact: true})"), params=params)
        hits += len({vid for vid, _ in _pairs(got)} & {vid for vid, _ in _pairs(exact)})
    assert hits / 400 >= 0.8, hits / 400


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
