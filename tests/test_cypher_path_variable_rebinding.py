"""A path variable names one path: reusing a name that is already taken is an error.

`p = (a)-->(b)` must not reuse a name another part of the query already binds
(node, relationship, path or projected value), and a name bound as a path must
not come back as a node or relationship variable.  Re-using a node or
relationship variable stays legal.  Queries are written against a small chain
`A -> B -> C`; the error cases fail at plan time, so the graph content only
matters for the positive cases.
"""

import pytest

import kglite

REFUSED = [
    # path variable also a node/relationship variable of the same MATCH
    "MATCH p = (p)-->(b) RETURN p",
    "MATCH p = (a)-->(p) RETURN p",
    "MATCH p = (a)-[p]->(b) RETURN p",
    "MATCH p = (a)-[p*1..2]->(b) RETURN p",
    "MATCH (a)-[p]->(b), p = (c)-->(d) RETURN p",
    "MATCH (p)-->(b), p = (c)-->(d) RETURN p",
    "MATCH (x), (y)-->(p), p = (c)-->(d) RETURN p",
    # the same path variable twice in one MATCH
    "MATCH p = (a)-->(b), p = (c)-->(d) RETURN p",
    # a path variable that an earlier clause already bound
    "MATCH p = (a)-->(b) MATCH p = (c)-->(d) RETURN p",
    "MATCH (p) MATCH p = (p)-->(d) RETURN p",
    "MATCH ()-[p]->() MATCH p = (c)-->(d) RETURN p",
    "MATCH (a)-->(b) WITH a AS p MATCH p = (c)-->(d) RETURN p",
    "UNWIND [1] AS p MATCH p = (c)-->(d) RETURN p",
    "MATCH p = (a)-->(b) OPTIONAL MATCH p = (c)-->(d) RETURN p",
    # a name bound as a path coming back as a node or relationship
    "MATCH p = (a)-->(b) MATCH (p) RETURN p",
    "MATCH p = (a)-->(b) MATCH ()-[p]->() RETURN p",
    "MATCH p = (a)-->(b) WITH p MATCH (p)-->(c) RETURN c",
    "MATCH p = (a)-->(b), (p) RETURN p",
    "MATCH p = (a)-->(b), (c)-[p]->(d) RETURN p",
    "MATCH (c), p = (a)-->(b), (p) RETURN p",
]


ACCEPTED = {
    "MATCH p = (a)-->(b) RETURN count(p) AS n": 2,
    "MATCH p = (a)-->(b), q = (c)-->(d) RETURN count(*) AS n": 2,
    "MATCH (a)-->(b) MATCH p = (b)-->(c) RETURN count(p) AS n": 1,
    "MATCH (x), p = (x)-[*0..2]->(c) RETURN count(p) AS n": 6,
    "MATCH p = (a)-->(b) WITH 1 AS one MATCH (p) RETURN count(*) AS n": 6,
    "MATCH p = (a)-->(b) WITH a AS p MATCH (p)-->(c) RETURN count(*) AS n": 2,
    "MATCH p = (a)-->(b) WITH p AS q MATCH p = (c)-->(d) RETURN count(*) AS n": 4,
    "UNWIND [1] AS p MATCH q = (c)-->(d) RETURN count(q) AS n": 2,
    "MATCH (a)-[r]->(b) MATCH (a)-[r]->(b) RETURN count(*) AS n": 2,
    "MATCH (a)-[:T*1..2]->(b)-[:T*1..2]->(c) RETURN count(*) AS n": 1,
}


@pytest.fixture(scope="module")
def chain():
    graph = kglite.KnowledgeGraph()
    graph.cypher("CREATE (:N {title:'A'})-[:T]->(:N {title:'B'})-[:T]->(:N {title:'C'})")
    return graph


@pytest.mark.parametrize("query", REFUSED)
def test_a_rebound_path_variable_is_refused(chain, query):
    with pytest.raises(Exception, match=r"Variable `p`"):
        chain.cypher(query)


@pytest.mark.parametrize("query,expected", ACCEPTED.items())
def test_a_distinct_path_variable_still_runs(chain, query, expected):
    assert chain.cypher(query).to_list() == [{"n": expected}]


def test_a_path_variable_is_free_again_after_a_projection_drops_it(chain):
    rows = chain.cypher("MATCH p = (a)-->(b) WITH count(*) AS n MATCH p = (c)-->(d) RETURN n, count(p) AS m").to_list()
    assert rows == [{"n": 2, "m": 2}]
