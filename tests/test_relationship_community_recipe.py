"""The relationship-communities recipe in the semantic-search guide, executed.

Every ``python`` block under "#### Relationship communities" in
``docs/python/guides/semantic-search.md`` runs here, in order, in one
namespace, so the guide cannot drift from what the engine does. The assertions
check what the prose claims each step produces.
"""

from __future__ import annotations

from pathlib import Path

import pytest

GUIDE = Path(__file__).resolve().parents[1] / "docs" / "python" / "guides" / "semantic-search.md"
HEADING = "#### Relationship communities"
PASS = "fuse_vector_score_order_limit"


def _recipe_blocks() -> list[str]:
    """The python fences between the recipe heading and the next heading of
    level four or higher (headings inside fences are code comments, skipped)."""
    lines = GUIDE.read_text(encoding="utf-8").splitlines()
    start = lines.index(HEADING)
    blocks: list[str] = []
    current: list[str] | None = None
    in_fence = False
    for line in lines[start + 1 :]:
        if line.startswith("```"):
            if in_fence:
                if current is not None:
                    blocks.append("\n".join(current))
                current, in_fence = None, False
            else:
                in_fence = True
                current = [] if line.strip() == "```python" else None
            continue
        if in_fence:
            if current is not None:
                current.append(line)
            continue
        if line.startswith("#") and len(line) - len(line.lstrip("#")) <= 4:
            break
    return blocks


@pytest.fixture(scope="module")
def recipe() -> dict:
    blocks = _recipe_blocks()
    assert len(blocks) == 5, f"the recipe has five python snippets, found {len(blocks)}"
    namespace: dict = {}
    for block in blocks:
        exec(compile(block, str(GUIDE), "exec"), namespace)  # noqa: S102 — the guide's own snippets
    return namespace


def test_louvain_splits_the_two_topics(recipe: dict) -> None:
    rows = recipe["graph"].cypher("MATCH (e:Entity) RETURN e.name AS name, e.community AS c").to_list()
    community = {row["name"]: row["c"] for row in rows}
    apple = {community[name] for name in ("Steve Jobs", "Steve Wozniak", "Apple", "Macintosh")}
    voyage = {community[name] for name in ("James Cook", "Endeavour", "Pacific", "Joseph Banks")}
    assert len(apple) == 1 and len(voyage) == 1 and apple != voyage


def test_the_one_cross_topic_relation_is_the_bridge(recipe: dict) -> None:
    assert recipe["bridges"] == [
        {"kind": "bridge", "type": "inspired", "source": "Joseph Banks", "target": "Macintosh"}
    ]
    assert sum(row["kind"] == "intra" for row in recipe["kinds"]) == 9


def test_community_scoped_ranking_stays_in_the_community_and_is_fused(recipe: dict) -> None:
    graph = recipe["graph"]
    rows = recipe["top"].to_list()
    assert [(row["source"], row["type"], row["target"]) for row in rows] == [
        ("Steve Jobs", "founded", "Apple"),
        ("Steve Wozniak", "founded", "Apple"),
    ]
    assert rows[0]["score"] > rows[1]["score"]
    stores = {record["store"] for record in recipe["top"].diagnostics["retrieval"]}
    assert any(store and "relationship:founded.description_emb" in store for store in stores), stores
    # The fused answer is the unfused one.
    query = """
        MATCH (s:Entity)-[r:founded|works_at|created|sailed_on|explored|inspired]->(t:Entity)
        WHERE s.community = $community AND t.community = $community
        RETURN s.name AS source, type(r) AS type, t.name AS target,
               text_score(r, 'description', $question) AS score
        ORDER BY score DESC LIMIT 2
    """
    params = {"community": recipe["apple_community"], "question": "who founded the apple computer company"}
    assert graph.cypher(query, params=params, disabled_passes=[PASS]).to_list() == rows


def test_summaries_pick_the_community_before_its_relations(recipe: dict) -> None:
    rows = recipe["answer"].to_list()
    assert [(row["source"], row["type"], row["target"]) for row in rows] == [
        ("James Cook", "sailed_on", "Endeavour"),
        ("Joseph Banks", "sailed_on", "Endeavour"),
    ]
    summaries = recipe["graph"].cypher("MATCH (c:Community) RETURN c.summary AS s").to_list()
    assert len(summaries) == 2 and all(row["s"] for row in summaries)


def test_relationship_stores_cannot_be_clustered_directly(recipe: dict) -> None:
    """The recipe's closing claim: `CALL cluster()` clusters nodes only."""
    import kglite

    with pytest.raises(kglite.CypherExecutionError, match="preceding MATCH"):
        recipe["graph"].cypher(
            "CALL cluster({method: 'kmeans', k: 2, relationship_type: 'founded', property: 'description_emb'}) "
            "YIELD node, cluster RETURN node, cluster"
        )
