"""A relationship UNIQUE violation reaches a Bolt client as
``Neo.ClientError.Schema.ConstraintValidationFailed``, a relationship index is
listed for the GraphBridge-style inspector, and the identified-edge read is
served from the index."""

import os

import pytest

neo4j = pytest.importorskip("neo4j")

pytestmark = [
    pytest.mark.bolt,
    pytest.mark.skipif(os.name != "posix", reason="Bolt server tests are POSIX-only"),
]

AUTH = ("neo4j", "password")
CODE = "Neo.ClientError.Schema.ConstraintValidationFailed"


def test_a_duplicate_relationship_value_is_a_constraint_validation_failure(bolt_server):
    with neo4j.GraphDatabase.driver(bolt_server, auth=AUTH) as driver:
        with driver.session() as session:
            session.run("CREATE (:Rp {id: 1}), (:Rp {id: 2}), (:Rm {id: 10})").consume()
            session.run("CREATE CONSTRAINT rel_rid FOR ()-[r:ACTED]-() REQUIRE (r.rid) IS UNIQUE").consume()
            create = "MATCH (p:Rp {id: $p}), (m:Rm {id: 10}) CREATE (p)-[:ACTED {rid: $rid}]->(m)"
            session.run(create, p=1, rid="a").consume()
            with pytest.raises(neo4j.exceptions.ClientError) as raised:
                session.run(create, p=2, rid="a").consume()
            assert raised.value.code == CODE, raised.value.code
            session.run(create, p=2, rid="b").consume()
            with pytest.raises(neo4j.exceptions.ClientError) as raised:
                session.run("MATCH (:Rp {id: 2})-[r:ACTED]->() SET r.rid = 'a'").consume()
            assert raised.value.code == CODE, raised.value.code
            rows = list(session.run("MATCH ()-[r:ACTED]->() RETURN count(r) AS c"))
            assert rows[0]["c"] == 2


def test_the_inspector_reads_the_constraint_and_the_owned_index(bolt_server):
    with neo4j.GraphDatabase.driver(bolt_server, auth=AUTH) as driver:
        with driver.session() as session:
            session.run("CREATE (:Rp {id: 1}), (:Rm {id: 10})").consume()
            session.run("CREATE CONSTRAINT rel_rid FOR ()-[r:ACTED]-() REQUIRE (r.rid) IS UNIQUE").consume()
            session.run("CREATE RANGE INDEX acted_w FOR ()-[r:ACTED]-() ON (r.w)").consume()
            constraints = list(session.run("SHOW CONSTRAINTS YIELD name, type, entityType, labelsOrTypes, properties"))
            row = next(c for c in constraints if c["name"] == "rel_rid")
            assert (row["type"], row["entityType"], row["labelsOrTypes"], row["properties"]) == (
                "RELATIONSHIP_UNIQUENESS",
                "RELATIONSHIP",
                ["ACTED"],
                ["rid"],
            )
            indexes = {
                r["name"]: r["owningConstraint"]
                for r in session.run(
                    "SHOW INDEXES YIELD name, entityType, owningConstraint WHERE entityType = 'RELATIONSHIP'"
                )
            }
            assert indexes == {"relationship:ACTED.rid": "rel_rid", "acted_w": None}


def test_the_identified_edge_read_answers_through_the_index(bolt_server):
    with neo4j.GraphDatabase.driver(bolt_server, auth=AUTH) as driver:
        with driver.session() as session:
            session.run("CREATE (:Rp {id: 1}), (:Rm {id: 10})").consume()
            session.run("CREATE INDEX FOR ()-[r:ACTED]-() ON (r.rid)").consume()
            session.run("MATCH (p:Rp {id: 1}), (m:Rm {id: 10}) CREATE (p)-[:ACTED {rid: 'a'}]->(m)").consume()
            rows = list(
                session.run("MATCH ()-[candidate:ACTED]->() WHERE candidate.rid = $k RETURN count(*) AS c", k="a")
            )
            assert rows[0]["c"] == 1
