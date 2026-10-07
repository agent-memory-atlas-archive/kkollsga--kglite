"""An open-ended variable-length pattern stops at 10 hops; the result says so."""

import pytest

import kglite

CAP_WARNING = "default ceiling of 10 hops"


@pytest.fixture
def chain():
    graph = kglite.KnowledgeGraph()
    graph.cypher("CREATE (:Hop {id: 0})")
    for i in range(1, 13):
        graph.cypher(f"MATCH (a:Hop {{id: {i - 1}}}) CREATE (a)-[:NEXT]->(:Hop {{id: {i}}})")
    return graph


def _warned(result):
    return any(CAP_WARNING in w for w in result.warnings)


@pytest.mark.parametrize("hops", ["*", "*2..", "*0.."])
def test_open_ended_forms_warn_and_stay_capped(chain, hops):
    result = chain.cypher(f"MATCH (:Hop {{id: 0}})-[:NEXT{hops}]->(n) RETURN max(n.id) AS far")
    assert result.to_list() == [{"far": 10}]
    assert _warned(result)


@pytest.mark.parametrize("hops", ["*1..12", "*..12", "*3", "*0..1"])
def test_written_maximum_does_not_warn(chain, hops):
    assert not _warned(chain.cypher(f"MATCH (:Hop {{id: 0}})-[:NEXT{hops}]->(n) RETURN count(n) AS c"))


def test_explicit_maximum_reaches_past_the_cap(chain):
    far = chain.cypher("MATCH (:Hop {id: 0})-[:NEXT*1..12]->(n) RETURN max(n.id) AS far").to_list()
    assert far == [{"far": 12}]


def test_shortest_path_open_form_is_unbounded_and_silent(chain):
    result = chain.cypher("MATCH p = shortestPath((a:Hop {id: 0})-[:NEXT*]->(b:Hop {id: 12})) RETURN length(p) AS hops")
    assert result.to_list() == [{"hops": 12}]
    assert not _warned(result)


def test_optional_match_and_call_subquery_branches_warn(chain):
    assert _warned(chain.cypher("MATCH (a:Hop {id: 0}) OPTIONAL MATCH (a)-[:NEXT*]->(n) RETURN count(n) AS c"))
    assert _warned(chain.cypher("CALL { MATCH (a:Hop {id: 0})-[:NEXT*]->(n) RETURN n } RETURN count(n) AS c"))
