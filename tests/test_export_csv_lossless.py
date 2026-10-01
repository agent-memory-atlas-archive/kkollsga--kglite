"""``export_csv`` -> ``from_blueprint`` loses nothing.

Red proof (on the tree before the lossless export): the round trip dropped the
valid-time declarations and secondary labels, wrote timestamps / lists / maps /
durations as display strings, re-inferred id and edge-property types, merged
null with the empty string, and attached a relationship leaving two source
types to vivified stub nodes (nodes after re-import exceeded nodes exported).
"""

import json
import math
import warnings

import pytest

import kglite
from kglite import KnowledgeGraph

DECLARATIONS = (
    "CALL db.temporal.declarations() "
    "YIELD kind, name, source_type, from, to, convention "
    "RETURN kind, name, source_type, from, to, convention"
)

INSTANTS = [
    "1999-12-31",
    "2000-01-01",
    "2005-12-31",
    "2006-01-01",
    "2009-12-30",
    "2009-12-31",
    "2010-01-01",
    "2030-01-01",
]

FIXTURE = [
    # Person: two versions of id 1 (valid-time), an id-2 row, a secondary
    # label, every Value kind, CSV-hostile text, and null vs empty string.
    "CREATE (:Person:Employee {id: 1, title: 'Ada, \"the\" Countess', "
    "name: 'multi\nline Åse ☃ 雪', nick: '', motto: 'a,b \"c\"\r\n  d,', back: '\\\\e', "
    "hired: date('2000-01-01'), `left`: date('2009-12-31'), "
    "salary: 1.5, active: true, born: date('1990-05-17'), "
    "seen: datetime('2020-01-02T03:04:05.250'), skills: ['a', 'b,c', 'd\"e'], "
    "prefs: {k: 1, z: 'x', n: [1, 2]}, tenure: duration({days: 3}), "
    "home: point({latitude: 60.1, longitude: 5.2})})",
    "CREATE (:Person:Employee {id: 1, title: 'Ada v2', hired: date('2010-01-01')})",
    "CREATE (:Person:Employee {id: 2, title: '', hired: date('2005-01-01'), "
    "`left`: date('2005-12-31'), name: 'Bo'})",
    # Department: string ids, one numeric-looking.
    "CREATE (:Department {id: 'D-1', title: 'Engineering'})",
    "CREATE (:Department {id: '007', title: 'Agents'})",
    # Team: a sub-node of Department, half-open valid time.
    "CREATE (:Team {id: 'T-1', title: 'Core', vf: date('2001-01-01'), vt: date('2009-12-31')})",
    "CREATE (:Team {id: 'T-2', title: 'Edge', vf: date('2010-01-01')})",
    # WORKS_IN from two source types, parallel edges, properties.
    "MATCH (p:Person {id: 2}), (d:Department {id: 'D-1'}) CREATE (p)-[:WORKS_IN "
    "{since: date('2005-02-01'), until: date('2005-06-30'), role: 'lead', hours: 38.5}]->(d)",
    "MATCH (p:Person {id: 2}), (d:Department {id: 'D-1'}) CREATE (p)-[:WORKS_IN "
    "{since: date('2005-07-01'), until: date('2005-12-31'), role: 'mentor', hours: 20.0}]->(d)",
    "MATCH (p:Person {id: 1}), (d:Department {id: '007'}) CREATE (p)-[:WORKS_IN "
    "{since: date('2001-01-01'), until: date('2009-12-31'), role: ''}]->(d)",
    "MATCH (t:Team {id: 'T-1'}), (d:Department {id: 'D-1'}) CREATE (t)-[:WORKS_IN "
    "{a: date('2002-01-01'), b: date('2004-01-01')}]->(d)",
    # MANAGES: several target types, one property named like a CSV column.
    "MATCH (p:Person {id: 2}), (q:Person {id: 1}) CREATE (p)-[:MANAGES {weight: 0.25, "
    "flag: false, at: datetime('2021-05-06T07:08:09'), tags: ['x'], meta: {m: 1}, "
    "span: duration({days: 1}), source_id: 'collides'}]->(q)",
    "MATCH (p:Person {id: 2}), (d:Department {id: 'D-1'}) CREATE (p)-[:MANAGES {weight: 1.0}]->(d)",
]

EXPECTED_NODES = 7
EXPECTED_EDGES = 6


def build_source(storage, tmp_path):
    if storage == "memory":
        g = KnowledgeGraph()
    elif storage == "mapped":
        g = KnowledgeGraph(storage="mapped")
    else:
        g = KnowledgeGraph(storage="disk", path=str(tmp_path / "src.kgl"))
    for query in FIXTURE:
        g.cypher(query)
    g.set_parent_type("Team", "Department")
    g.set_temporal("Person", "hired", "left", "closed")
    g.cypher("CALL db.temporal.declare({node: 'Employee', from: 'hired', to: 'left', convention: 'closed'})").to_list()
    g.set_temporal("Team", "vf", "vt", "half_open")
    g.set_temporal("WORKS_IN", "since", "until", "closed", source_type="Person")
    g.set_temporal("WORKS_IN", "a", "b", "half_open", source_type="Team")
    return g


def norm(v):
    if isinstance(v, float):
        return ("float", "nan" if math.isnan(v) else repr(v))
    if isinstance(v, dict):
        return ("dict", tuple(sorted((k, norm(x)) for k, x in v.items())))
    if isinstance(v, (list, tuple)):
        return ("list", tuple(norm(x) for x in v))
    return (type(v).__name__, repr(v))


def dump(g):
    nodes = sorted(
        repr(
            (
                sorted(r["l"]),
                norm(r["id"]),
                norm(r["t"]),
                norm(r["p"]),
            )
        )
        for r in g.cypher("MATCH (n) RETURN labels(n) AS l, n.id AS id, n.title AS t, properties(n) AS p").to_list()
    )
    edges = sorted(
        repr(
            (
                r["st"],
                norm(r["sid"]),
                r["rt"],
                r["tt"],
                norm(r["tid"]),
                norm(r["p"]),
            )
        )
        for r in g.cypher(
            "MATCH (a)-[r]->(b) RETURN labels(a)[0] AS st, a.id AS sid, type(r) AS rt, "
            "labels(b)[0] AS tt, b.id AS tid, properties(r) AS p"
        ).to_list()
    )
    return nodes, edges


def declarations(g):
    rows = g.cypher(DECLARATIONS).to_list()
    return sorted((r["kind"], r["name"], r["source_type"], r["from"], r["to"], r["convention"]) for r in rows)


def valid_time_answers(g):
    out = []
    for day in INSTANTS:
        ctx = f"FOR VALID_TIME AS OF date('{day}') "
        for q in (
            "MATCH (p:Person) RETURN p.id AS a, p.title AS b",
            "MATCH (e:Employee) RETURN e.id AS a, e.title AS b",
            "MATCH (t:Team) RETURN t.id AS a, t.title AS b",
            "MATCH (p)-[r:WORKS_IN]->(d) RETURN p.id AS a, d.id AS b",
        ):
            rows = g.cypher(ctx + q).to_list()
            out.append((day, q, sorted(repr((norm(r["a"]), norm(r["b"]))) for r in rows)))
        out.append((day, "valid_at", sorted(r["title"] for r in g.select("Person").valid_at(day).collect())))
    return out


def reimport(tmp_path, source):
    out = tmp_path / "out"
    summary = source.export_csv(str(out))
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        back = kglite.from_blueprint(str(out / "blueprint.json"), save=False)
    return summary, back, [str(w.message) for w in caught], out


@pytest.mark.parity
@pytest.mark.parametrize("storage", ["memory", "mapped", "disk"])
def test_round_trip_loses_nothing(storage, tmp_path):
    source = build_source(storage, tmp_path)
    assert dump(source)[0].__len__() == EXPECTED_NODES
    assert dump(source)[1].__len__() == EXPECTED_EDGES

    summary, back, messages, _ = reimport(tmp_path, source)
    assert sum(summary["nodes"].values()) == EXPECTED_NODES
    assert sum(summary["connections"].values()) == EXPECTED_EDGES

    before_nodes, before_edges = dump(source)
    after_nodes, after_edges = dump(back)
    assert len(after_nodes) == EXPECTED_NODES, "stub nodes were vivified"
    assert len(after_edges) == EXPECTED_EDGES
    assert after_nodes == before_nodes
    assert after_edges == before_edges
    assert declarations(back) == declarations(source)
    assert len(declarations(back)) == 5
    assert valid_time_answers(back) == valid_time_answers(source)
    assert not [m for m in messages if "stub" in m.lower() or "unknown key" in m], messages


def test_null_and_empty_string_stay_distinct(tmp_path):
    source = build_source("memory", tmp_path)
    _, back, _, _ = reimport(tmp_path, source)
    rows = back.cypher("MATCH (p:Person) WHERE p.name STARTS WITH 'multi' RETURN p.nick AS nick, p.back AS back").to_list()
    assert rows == [{"nick": "", "back": "\\e"}]
    row = back.cypher("MATCH (p:Person {id: 2}) RETURN p.nick AS nick").to_list()
    assert row == [{"nick": None}]


def test_a_relationship_from_two_sources_gets_a_csv_per_source(tmp_path):
    source = build_source("memory", tmp_path)
    _, _, _, out = reimport(tmp_path, source)
    blueprint = json.loads((out / "blueprint.json").read_text())
    csvs = sorted(p.name for p in (out / "connections").iterdir())
    assert csvs == ["MANAGES.csv", "WORKS_IN.Person.csv", "WORKS_IN.Team.csv"]
    manages = blueprint["nodes"]["Person"]["connections"]["junction_edges"]["MANAGES"]
    assert manages["target"] == ["Department", "Person"]
    assert manages["target_type_column"] == "_kg_target_type"  # `source_id` is a property name
    manifest = json.loads((out / "manifest.json").read_text())
    assert manifest["format"] == "kglite-export/1"
    assert blueprint["settings"]["manifest"] == "manifest.json"
