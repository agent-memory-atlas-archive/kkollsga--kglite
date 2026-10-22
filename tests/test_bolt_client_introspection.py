"""The Neo4j-client connect sequence over the wire, end to end.

The 2026-08-15 compatibility program's net: every introspection call a real
Neo4j client issues on connect — measured from Neo4j Browser's source
(dbMetaDuck.ts) and the original 24-probe audit — driven through
`kglite-bolt-server` with the official `neo4j` driver, on both server
identities. This is the driver-level regression lock the in-process golden
tests can't provide: it exercises the Bolt intercepts (dbms.* /
SHOW DATABASES), packstream struct round-trips, and the CALL column
contract over the wire.
"""

from __future__ import annotations

import pytest

from tests.conftest import (
    _BOLT_SKIP_REASON,
    _bolt_binary_available,
    _build_bolt_fixture_graph,
    _spawn_bolt_server,
    _teardown_bolt_server,
)

pytestmark = [pytest.mark.bolt]

neo4j = pytest.importorskip("neo4j")
from neo4j import GraphDatabase  # noqa: E402


@pytest.fixture
def bolt_server_neo4j_compat(tmp_path):
    """A server started with --neo4j-compat, as a GUI client requires."""
    if not _bolt_binary_available():
        pytest.skip(_BOLT_SKIP_REASON)
    fixture_path = tmp_path / "fixture_compat.kgl"
    _build_bolt_fixture_graph(fixture_path)
    proc, url = _spawn_bolt_server(fixture_path, readonly=True, extra_args=["--neo4j-compat"])
    yield url
    _teardown_bolt_server(proc)


def _run(url, query, **kwargs):
    with GraphDatabase.driver(url, auth=None) as driver:
        with driver.session(**kwargs) as session:
            return list(session.run(query))


# ── Server identity ────────────────────────────────────────────────────────


def test_components_honest_identity(bolt_server):
    rec = _run(bolt_server, "CALL dbms.components()")[0]
    assert rec["name"] == "kglite-bolt-server"
    assert rec["edition"] == "community"
    assert len(rec["versions"]) == 1


def test_components_compat_identity(bolt_server_neo4j_compat):
    rec = _run(
        bolt_server_neo4j_compat,
        "CALL dbms.components() YIELD name, versions, edition",
    )[0]
    assert rec["name"] == "Neo4j Kernel"
    assert rec["versions"] == ["2026.10.0"]
    assert rec["edition"] == "community"


def test_compat_components_carry_the_cypher_dialect_row(bolt_server_neo4j_compat):
    """GraphBridge's preflight needs exactly one 2026.x kernel row and at most
    one Cypher row advertising "25" (Search/GraphSearchPreflight.cs)."""
    rows = _run(
        bolt_server_neo4j_compat,
        "CALL dbms.components() YIELD name, versions, edition RETURN name, versions, edition LIMIT 5",
    )
    by_name = {r["name"]: r for r in rows}
    assert len(rows) == 2 and set(by_name) == {"Neo4j Kernel", "Cypher"}
    assert by_name["Cypher"]["versions"] == ["5", "25"]
    assert by_name["Neo4j Kernel"]["versions"][0].startswith("2026.")


def test_honest_components_stay_a_single_kglite_row(bolt_server):
    rows = _run(bolt_server, "CALL dbms.components() YIELD name, versions, edition")
    assert [r["name"] for r in rows] == ["kglite-bolt-server"]


def test_compat_agent_and_components_agree(bolt_server_neo4j_compat):
    """The handshake agent and the components row come from the same enum —
    a client must never see a Neo4j agent with a kglite components row."""
    with GraphDatabase.driver(bolt_server_neo4j_compat, auth=None) as driver:
        agent = driver.get_server_info().agent
        with driver.session() as session:
            name = list(session.run("CALL dbms.components() YIELD name"))[0]["name"]
    assert agent.startswith("Neo4j/2026.10.0"), agent
    assert name == "Neo4j Kernel"


def test_show_current_user(bolt_server):
    rec = _run(bolt_server, "CALL dbms.showCurrentUser()")[0]
    assert rec["username"] == "neo4j"
    assert rec["roles"] == []


def test_show_databases_row(bolt_server_neo4j_compat):
    rec = _run(bolt_server_neo4j_compat, "SHOW DATABASES")[0]
    assert rec["name"] == "neo4j"
    assert rec["default"] is True and rec["home"] is True
    assert rec["currentStatus"] == "online"
    # This fixture server is --readonly; the row must say so honestly.
    assert rec["access"] == "read-only" and rec["writer"] is False


def test_session_against_named_database_still_accepted(bolt_server):
    assert _run(bolt_server, "RETURN 1 AS ok", database="neo4j")[0]["ok"] == 1


# ── Neo4j Browser's verbatim connect queries (dbMetaDuck.ts) ───────────────

BROWSER_META_TYPES = """CALL db.labels() YIELD label
RETURN {name:'labels', data:COLLECT(label)[..1000]} AS result
UNION ALL
CALL db.relationshipTypes() YIELD relationshipType
RETURN {name:'relationshipTypes', data:COLLECT(relationshipType)[..1000]} AS result
UNION ALL
CALL db.propertyKeys() YIELD propertyKey
RETURN {name:'propertyKeys', data:COLLECT(propertyKey)[..1000]} AS result"""

BROWSER_META_COUNT = """MATCH () RETURN { name:'nodes', data:count(*) } AS result
UNION ALL
MATCH ()-[]->() RETURN { name:'relationships', data: count(*)} AS result"""


def test_browser_meta_types_query(bolt_server_neo4j_compat):
    rows = _run(bolt_server_neo4j_compat, BROWSER_META_TYPES)
    by_name = {row["result"]["name"]: row["result"]["data"] for row in rows}
    assert by_name["labels"] == ["Person"]
    assert by_name["relationshipTypes"] == ["KNOWS"]
    assert isinstance(by_name["propertyKeys"], list) and by_name["propertyKeys"]
    # Sidebar needs real arrays over the wire, not JSON strings.
    assert all(isinstance(v, list) for v in by_name.values())


def test_browser_meta_count_query(bolt_server_neo4j_compat):
    rows = _run(bolt_server_neo4j_compat, BROWSER_META_COUNT)
    by_name = {row["result"]["name"]: row["result"]["data"] for row in rows}
    assert by_name["nodes"] == 4
    assert by_name["relationships"] == 3


def test_browser_schema_tab_query(bolt_server_neo4j_compat):
    rec = _run(bolt_server_neo4j_compat, "CALL db.schema.visualization()")[0]
    nodes, rels = rec["nodes"], rec["relationships"]
    assert [sorted(n.labels)[0] for n in nodes] == ["Person"]
    assert {(r.type,) for r in rels} == {("KNOWS",)}
    # Virtual endpoints resolve to the virtual label nodes.
    node_ids = {n.element_id for n in nodes}
    for r in rels:
        assert r.start_node.element_id in node_ids
        assert r.end_node.element_id in node_ids


# ── Standalone CALL + column contract over the wire ────────────────────────


def test_bare_call_over_the_wire(bolt_server):
    result_keys = None
    with GraphDatabase.driver(bolt_server, auth=None) as driver:
        with driver.session() as session:
            result = session.run("CALL db.labels()")
            records = list(result)
            result_keys = result.keys()
    assert list(result_keys) == ["label"]
    assert [r["label"] for r in records] == ["Person"]


def test_zero_row_call_reports_columns_over_the_wire(bolt_server):
    """The empty-graph first-connect shape: result.keys() must not be []."""
    with GraphDatabase.driver(bolt_server, auth=None) as driver:
        with driver.session() as session:
            result = session.run("CALL db.indexes() YIELD type, name")
            records = list(result)
            keys = list(result.keys())
    assert records == []
    assert keys == ["type", "name"]


def test_show_procedures_over_the_wire(bolt_server):
    rows = _run(bolt_server, "SHOW PROCEDURES YIELD name")
    names = {r["name"] for r in rows}
    assert {"db.labels", "db.schema.visualization", "pagerank"} <= names


def test_element_id_round_trip_over_the_wire(bolt_server):
    """Read element_id off a packed Node struct, use it in a predicate —
    the function and the packer must agree on the number."""
    with GraphDatabase.driver(bolt_server, auth=None) as driver:
        with driver.session() as session:
            node = list(session.run("MATCH (p:Person) RETURN p LIMIT 1"))[0]["p"]
            eid = node.element_id
            back = list(
                session.run(
                    "MATCH (p:Person) WHERE elementId(p) = $eid RETURN elementId(p) AS e",
                    eid=eid,
                )
            )
    assert [r["e"] for r in back] == [eid]


# ── EXPLAIN/PROFILE Bolt contract (2026-08-15) ─────────────────────────────


def test_explain_returns_plan_metadata_and_zero_records(bolt_server):
    """Neo4j's contract: EXPLAIN yields no records; the plan rides in the
    SUCCESS summary. Pre-fix the engine's step rows were forwarded as records
    with no plan metadata, so plan tabs (Browser, G.V()) rendered blank."""
    with GraphDatabase.driver(bolt_server, auth=None) as driver:
        with driver.session() as session:
            result = session.run("EXPLAIN MATCH (p:Person)-[:KNOWS]->(q) RETURN p.name ORDER BY p.name LIMIT 2")
            records = list(result)
            summary = result.consume()
    assert records == []
    plan = summary.plan
    assert plan is not None
    assert plan["args"]["runtime"] == "kglite"
    assert "optimizer-passes" in plan["args"]
    # Root is the final operator; the chain bottoms out at the Match.
    node = plan
    while node.get("children"):
        assert len(node["children"]) == 1
        node = node["children"][0]
    assert node["operatorType"].startswith("Match")


def test_profile_still_executes_without_fabricated_stats(bolt_server):
    """No per-operator statistics exist in the engine, so PROFILE executes
    normally and reports no profile — the Memgraph shape (plan without
    profiling), never fabricated dbHits."""
    with GraphDatabase.driver(bolt_server, auth=None) as driver:
        with driver.session() as session:
            result = session.run("PROFILE MATCH (p:Person) RETURN count(p) AS c")
            records = list(result)
            summary = result.consume()
    assert records[0]["c"] == 4
    assert summary.profile is None


def test_explain_plan_carries_string_representation(bolt_server):
    """G.V()'s plan tab reads args['string-representation'] unconditionally
    (NPE without it, verified by decompilation) — the root carries a rendered
    text plan, Neo4j's convention."""
    with GraphDatabase.driver(bolt_server, auth=None) as driver:
        with driver.session() as session:
            result = session.run("EXPLAIN MATCH (p:Person) RETURN p.name LIMIT 2")
            list(result)
            plan = result.consume().plan
    text = plan["args"]["string-representation"]
    assert "Match" in text and "\n" in text


# ── Neo4j-named index search over the wire ────────────────────────────────


@pytest.fixture
def bolt_search_server(tmp_path):
    """--neo4j-compat over a graph carrying a BM25 and a vector index."""
    if not _bolt_binary_available():
        pytest.skip(_BOLT_SKIP_REASON)
    from kglite import KnowledgeGraph

    graph = KnowledgeGraph()
    for doc_id, text in [(1, "alpha quick fox"), (2, "beta quick quick"), (3, "gamma ray burst")]:
        graph.cypher("CREATE (:Doc {id: $id, text: $text})", params={"id": doc_id, "text": text})
    graph.build_text_index("Doc", "text")
    graph.add_embeddings("Doc", "text", {1: [1.0, 0.0], 2: [0.0, 1.0], 3: [0.6, 0.8]})
    graph.build_vector_index("Doc", "text", m=8)
    path = tmp_path / "search.kgl"
    graph.save(str(path))
    proc, url = _spawn_bolt_server(path, readonly=True, extra_args=["--neo4j-compat"])
    yield url
    _teardown_bolt_server(proc)


def test_graphbridge_fulltext_statement_runs_verbatim(bolt_search_server):
    with GraphDatabase.driver(bolt_search_server, auth=None) as driver:
        with driver.session() as session:
            rows = list(
                session.run(
                    "CYPHER 25 CALL db.index.fulltext.queryNodes($index,$query,{limit:$limit}) "
                    "YIELD node AS entity,score RETURN entity,score ORDER BY score DESC LIMIT $limit",
                    {"index": "Doc.text", "query": "quick", "limit": 5},
                )
            )
    assert [r["entity"]["id"] for r in rows] == [2, 1]
    assert all(r["score"] > 0 for r in rows)


def test_vector_procedure_runs_over_the_wire_and_the_search_clause_is_named(bolt_search_server):
    with GraphDatabase.driver(bolt_search_server, auth=None) as driver:
        with driver.session() as session:
            rows = list(
                session.run(
                    "CALL db.index.vector.queryNodes($index, $k, $v) YIELD node, score "
                    "RETURN node.id AS id, score ORDER BY score DESC",
                    index="Doc.text",
                    k=2,
                    v=[1.0, 0.1],
                )
            )
            assert [r["id"] for r in rows] == [1, 3]
            with pytest.raises(neo4j.exceptions.ClientError, match="SEARCH clause is not supported"):
                list(
                    session.run(
                        "CYPHER 25 MATCH (entity) SEARCH entity IN (VECTOR INDEX `Doc.text` FOR $q LIMIT 2) "
                        "SCORE AS score RETURN entity, score",
                        q=[1.0, 0.1],
                    )
                )
