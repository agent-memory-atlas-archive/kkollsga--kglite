"""Per-target ``hidden`` counts and the ``endpoint_invalid`` bucket in
``diagnostics["temporal"]``.

An org chart with known hidden counts: departments ``d1`` (to 2010-12-31),
``d2`` (from 2011-01-01), ``d3`` (from 2020-01-01); employees ``e1`` (from
2005-01-01), ``e2`` (from 2013-01-01), ``e3`` (2012-01-01 to 2012-06-30); six
``ASSIGNED`` edges declared per source type. As of 2012-06-15, ``a4`` has not
started, and ``a2`` (department ended), ``a5`` (employee not started) and
``a6`` (neither endpoint valid) are valid by their own bounds but hidden by an
endpoint.
"""

from __future__ import annotations

import datetime as dt

import pytest

import kglite

QUERY = "MATCH (e:Employee)-[:ASSIGNED]->(d:Department) RETURN e.id AS e, d.id AS d ORDER BY e, d"


def _edge(name, emp, dept, start, end=None):
    end_part = f", to: date('{end}')" if end else ""
    return (
        f"MATCH (e:Employee {{id: '{emp}'}}), (d:Department {{id: '{dept}'}}) "
        f"CREATE (e)-[:ASSIGNED {{name: '{name}', from: date('{start}'){end_part}}}]->(d)"
    )


def _org_chart(convention):
    graph = kglite.KnowledgeGraph()
    statements = [
        "CREATE (:Department {id: 'd1', f: date('2000-01-01'), t: date('2010-12-31')}),"
        " (:Department {id: 'd2', f: date('2011-01-01')}),"
        " (:Department {id: 'd3', f: date('2020-01-01')}),"
        " (:Employee {id: 'e1', f: date('2005-01-01')}),"
        " (:Employee {id: 'e2', f: date('2013-01-01')}),"
        " (:Employee {id: 'e3', f: date('2012-01-01'), t: date('2012-06-30')})",
        _edge("a1", "e1", "d2", "2011-01-01"),
        _edge("a2", "e1", "d1", "2005-01-01"),
        _edge("a3", "e3", "d2", "2012-01-01", "2012-12-31"),
        _edge("a4", "e1", "d2", "2015-01-01"),
        _edge("a5", "e2", "d2", "2012-01-01"),
        _edge("a6", "e2", "d3", "2012-01-01"),
    ]
    for label in ("Department", "Employee"):
        statements.append(
            f"CALL db.temporal.declare({{node: '{label}', from: 'f', to: 't', convention: '{convention}'}})"
        )
    statements.append(
        "CALL db.temporal.declare({relationship: 'ASSIGNED', source_type: 'Employee',"
        f" from: 'from', to: 'to', convention: '{convention}'}})"
    )
    for statement in statements:
        graph.cypher(statement).to_list()
    return graph


def _echo(graph, instant):
    return graph.cypher(QUERY, valid_at=instant).diagnostics["temporal"]


def test_the_echo_counts_what_each_target_hides():
    graph = _org_chart("closed")
    result = graph.cypher(QUERY, valid_at="2012-06-15")
    assert result.to_list() == [{"e": "e1", "d": "d2"}, {"e": "e3", "d": "d2"}]
    echo = result.diagnostics["temporal"]
    assert echo["hidden"] == {
        "(:Department)": 2,
        "(:Employee)": 1,
        "[:ASSIGNED from :Employee]": 1,
    }
    assert list(echo["hidden"]) == echo["targets"]
    assert echo["endpoint_invalid"] == 3


def test_the_counts_follow_the_convention_on_the_boundary_day():
    closed = _echo(_org_chart("closed"), "2010-12-31")
    assert closed["hidden"]["(:Department)"] == 2
    assert closed["endpoint_invalid"] == 0
    half_open = _echo(_org_chart("half_open"), "2010-12-31")
    assert half_open["hidden"]["(:Department)"] == 3
    assert half_open["endpoint_invalid"] == 1


def test_a_timeless_instant_hides_nothing():
    graph = kglite.KnowledgeGraph()
    graph.cypher("CREATE (:Team {id: 1, f: date('2000-01-01'), t: date('2090-01-01')})").to_list()
    graph.cypher("CALL db.temporal.declare({node: 'Team', from: 'f', to: 't', convention: 'closed'})").to_list()
    echo = graph.cypher("MATCH (t:Team) RETURN t.id AS id", valid_at="2024-01-01").diagnostics["temporal"]
    assert echo["route"] == "plain"
    assert echo["hidden"] == {"(:Team)": 0}
    assert echo["endpoint_invalid"] == 0


def test_a_statement_that_reaches_no_declared_label_has_empty_counts():
    graph = _org_chart("closed")
    graph.cypher("CREATE (:Office {id: 'o1'})").to_list()
    echo = graph.cypher("MATCH (o:Office) RETURN o.id AS id", valid_at="2012-06-15").diagnostics["temporal"]
    assert echo["targets"] == []
    assert echo["hidden"] == {}
    assert echo["endpoint_invalid"] == 0


def test_a_graph_without_a_declaration_has_no_echo():
    graph = kglite.KnowledgeGraph()
    graph.cypher("CREATE (:Team {id: 1})").to_list()
    assert graph.cypher("MATCH (t:Team) RETURN t.id AS id").diagnostics["temporal"] is None
    with pytest.raises(Exception, match="validity declaration"):
        graph.cypher("MATCH (t:Team) RETURN t.id AS id", valid_at="2024-01-01")


def test_the_counts_follow_a_write():
    graph = _org_chart("closed")
    assert _echo(graph, "2012-06-15")["endpoint_invalid"] == 3
    graph.cypher("MATCH (d:Department {id: 'd1'}) SET d.t = date('2030-01-01')").to_list()
    echo = _echo(graph, "2012-06-15")
    assert echo["hidden"]["(:Department)"] == 1
    assert echo["endpoint_invalid"] == 2


def test_the_echo_names_where_the_context_came_from():
    graph = _org_chart("closed")
    graph.cypher("CREATE (:Office {id: 'o1'})").to_list()
    default = graph.cypher(QUERY).diagnostics["temporal"]
    assert default["source"] == "default"
    assert default["instant"] == dt.datetime.now(dt.timezone.utc).date().isoformat()
    explicit = graph.cypher(QUERY, valid_at="2012-06-15").diagnostics["temporal"]
    assert explicit["source"] == "explicit"
    every = graph.cypher(f"FOR VALID_TIME ALL {QUERY}").diagnostics["temporal"]
    assert (every["source"], every["instant"], every["route"]) == ("all", "all", "plain")
    assert every["targets"] == [] and every["hidden"] == {} and every["endpoint_invalid"] is None
    own_instant = graph.cypher(
        "MATCH (e:Employee) WHERE valid_at(e, date('2012-06-15')) RETURN e.id AS id"
    ).diagnostics["temporal"]
    assert (own_instant["source"], own_instant["instant"]) == ("skipped:valid_at", "all")
    write = graph.cypher("MATCH (o:Office) SET o.seen = true").diagnostics["temporal"]
    assert write["source"] == "skipped:write"
    # A statement that reaches no declared label filters nothing: plain, not guarded.
    unrelated = graph.cypher("MATCH (o:Office) RETURN o.id AS id").diagnostics["temporal"]
    assert (unrelated["source"], unrelated["route"], unrelated["targets"]) == ("default", "plain", [])
