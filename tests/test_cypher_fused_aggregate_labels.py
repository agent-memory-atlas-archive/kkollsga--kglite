"""Absolute goldens for the fused ``MATCH … WITH <group>, count(…)`` path.

Two silent wrong answers lived behind ``fuse_match_with_aggregate``:

* The peer-count histogram that answers the fused clause counts **every**
  peer of the connection type. Nothing applied the group node's own label,
  so ``MATCH (c)-[:CHILD_OF]->(p:Software) WITH p, count(c)`` returned a row
  for every ``:Api`` and ``:Doc`` parent as well. A ``:A|B`` alternation on
  that node was dropped the same way, and the source-side sweep read the
  pattern's singular ``node_type`` — under alternation that is branch A
  alone, so branch B's sources went uncounted.
* The two-MATCH variant deduplicated the first MATCH's group keys without
  keeping the row multiplicity they stood for, dividing ``count(r)`` by it.

The differential corpus carries every triggering query, which is what pins
the optimiser against the unoptimised path. These goldens are the other
half: absolute expected values, so a defect that both paths shared — or a
"fix" that made both paths equally wrong — cannot pass.

Run across the storage modes because the histogram has a different source
in each: the in-memory backend builds it on demand, while the disk backend
serves it from the persisted index and takes a separate, disk-only sweep
whenever the source node is label-constrained.
"""

from __future__ import annotations

import pytest

import kglite

# s has 2 children (c3:Software, c4:Api); a has 2 (both :Software);
# d has 2 (c5:Api, c6:Doc). Three parent labels over ONE edge type is the
# whole point — with a single label on both ends the histogram's missing
# filter is unobservable.
_BUILD = (
    "CREATE (s:Software{cid:'s'}),(a:Api{cid:'a'}),(d:Doc{cid:'d'}),"
    "(c1:Software{cid:'c1'})-[:CHILD_OF]->(a),"
    "(c2:Software{cid:'c2'})-[:CHILD_OF]->(a),"
    "(c3:Software{cid:'c3'})-[:CHILD_OF]->(s),"
    "(c4:Api{cid:'c4'})-[:CHILD_OF]->(s),"
    "(c5:Api{cid:'c5'})-[:CHILD_OF]->(d),"
    "(c6:Doc{cid:'c6'})-[:CHILD_OF]->(d)"
)


@pytest.fixture(params=["memory", "mapped", "disk"])
def parent_graph(request, tmp_path):
    if request.param == "memory":
        graph = kglite.KnowledgeGraph()
    elif request.param == "mapped":
        graph = kglite.KnowledgeGraph(storage="mapped")
    else:
        graph = kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "parents.kgl"))
    graph.cypher(_BUILD).to_list()
    return graph


def _rows(graph, query):
    return [(r["id"], r["k"]) for r in graph.cypher(query).to_list()]


@pytest.mark.parametrize(
    ("name", "query", "expected"),
    [
        (
            # The reported shape. Pre-fix this also returned ('a', 2) and
            # ('d', 2) — parents that are not :Software at all.
            "group_label",
            "MATCH (c)-[:CHILD_OF]->(p:Software) WITH p, count(c) AS k RETURN p.cid AS id, k ORDER BY id",
            [("s", 2)],
        ),
        (
            # `optimize_pattern_start_node` rewrites one spelling into the
            # other, so both reach the fused clause and both are pinned.
            "group_label_reversed",
            "MATCH (p:Software)<-[:CHILD_OF]-(c) WITH p, count(c) AS k RETURN p.cid AS id, k ORDER BY id",
            [("s", 2)],
        ),
        (
            "group_label_edge_var_count",
            "MATCH (c)-[r:CHILD_OF]->(p:Software) WITH p, count(r) AS k RETURN p.cid AS id, k ORDER BY id",
            [("s", 2)],
        ),
        (
            "group_label_count_star",
            "MATCH (c)-[:CHILD_OF]->(p:Software) WITH p, count(*) AS k RETURN p.cid AS id, k ORDER BY id",
            [("s", 2)],
        ),
        (
            # ORDER BY + LIMIT is absorbed into the fused clause as its
            # top-K hint, a separate re-entry into the same operator.
            "group_label_top_k",
            "MATCH (c)-[:CHILD_OF]->(p:Software) WITH p, count(c) AS k "
            "RETURN p.cid AS id, k ORDER BY k DESC, id LIMIT 3",
            [("s", 2)],
        ),
        (
            "group_label_with_limit",
            "MATCH (c)-[:CHILD_OF]->(p:Software) WITH p, count(c) AS k LIMIT 3 RETURN p.cid AS id, k ORDER BY id",
            [("s", 2)],
        ),
        (
            "group_label_with_where",
            "MATCH (c)-[:CHILD_OF]->(p:Software) WITH p, count(c) AS k WHERE k > 0 RETURN p.cid AS id, k ORDER BY id",
            [("s", 2)],
        ),
        (
            # Alternation keeps both branches and still drops :Doc. Filtering
            # on the pattern's singular node_type would have returned only
            # ('s', 2); filtering on nothing returned ('d', 2) as well.
            "group_label_alternation",
            "MATCH (c)-[:CHILD_OF]->(p:Software|Api) WITH p, count(c) AS k RETURN p.cid AS id, k ORDER BY id",
            [("a", 2), ("s", 2)],
        ),
        (
            # Source-side alternation. Only :Doc child c6 is excluded, so d
            # drops to 1 — the case a first-branch-only read would have
            # reported as a=2, d=1, s=1.
            "source_label_alternation",
            "MATCH (c:Software|Api)-[:CHILD_OF]->(p) WITH p, count(c) AS k RETURN p.cid AS id, k ORDER BY id",
            [("a", 2), ("d", 1), ("s", 2)],
        ),
        (
            # The two clauses join on p: the first binds p=s twice (once per
            # child), the second finds 2 CHILD_OF edges into s, so count(r)
            # is over 2x2 = 4 rows. The dropped multiplicity reported 2.
            "two_match_multiplicity",
            "MATCH (c)-[:CHILD_OF]->(p:Software) MATCH (p)<-[r:CHILD_OF]-() "
            "WITH p, count(r) AS k RETURN p.cid AS id, k ORDER BY id",
            [("s", 4)],
        ),
        (
            "two_match_multiplicity_labelled_peer",
            "MATCH (c)-[:CHILD_OF]->(p) MATCH (p)<-[r:CHILD_OF]-(:Api) "
            "WITH p, count(r) AS k RETURN p.cid AS id, k ORDER BY id",
            [("d", 2), ("s", 2)],
        ),
        (
            # Guard rail: a property map on the group node bails the fusion
            # outright and was always right — it must stay right.
            "group_property_map",
            "MATCH (c)-[:CHILD_OF]->(p {cid:'s'}) WITH p, count(c) AS k RETURN p.cid AS id, k ORDER BY id",
            [("s", 2)],
        ),
        (
            # Guard rail: a label on the source only. The source sweep was
            # already correct for a single label and stays so.
            "source_label_only",
            "MATCH (c:Software)-[:CHILD_OF]->(p) WITH p, count(c) AS k RETURN p.cid AS id, k ORDER BY id",
            [("a", 2), ("s", 1)],
        ),
        (
            # Guard rail: the RETURN-side aggregate never had the defect —
            # it already ran the group node's label through the type index.
            "return_side_group_label",
            "MATCH (c)-[:CHILD_OF]->(p:Software) RETURN p.cid AS id, count(c) AS k ORDER BY id",
            [("s", 2)],
        ),
    ],
)
def test_fused_aggregate_honours_pattern_labels(parent_graph, name, query, expected):
    assert _rows(parent_graph, query) == expected


def test_disabling_the_fusion_pass_gives_the_same_answers(parent_graph):
    """The fused and unfused plans agree on the reported shape.

    Not a substitute for the goldens above — it would stay green if both
    paths broke together — but it names the pass, so a bisect lands on
    `fuse_match_with_aggregate` rather than on the whole pipeline.
    """
    query = "MATCH (c)-[:CHILD_OF]->(p:Software) WITH p, count(c) AS k RETURN p.cid AS id, k ORDER BY id"
    fused = parent_graph.cypher(query).to_list()
    unfused = parent_graph.cypher(query, disabled_passes=["fuse_match_with_aggregate"]).to_list()
    assert fused == unfused == [{"id": "s", "k": 2}]
