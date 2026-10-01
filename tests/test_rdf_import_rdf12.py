"""RDF import: RDF 1.2 reifiers, opt-in language maps, the ``kg:`` manifest.

Red proof (tree before this change): a reifier node became a node of its own
and its literals never reached an edge; language tags were dropped; a
``kg:manifest`` statement became a node, valid-time declarations, secondary
labels and ids were not restored, and ``xsd:duration`` / ``kg:json`` literals
stayed strings.
"""

import datetime
import json

import pytest

import kglite

B = "http://hr.example/"
KG = "https://kglite.readthedocs.io/ns/kg#"
XSD = "http://www.w3.org/2001/XMLSchema#"
RDF = "http://www.w3.org/1999/02/22-rdf-syntax-ns#"
LABEL = "http://www.w3.org/2000/01/rdf-schema#label"

DECLARATIONS = (
    "CALL db.temporal.declarations() "
    "YIELD kind, name, source_type, from, to, convention "
    "RETURN kind, name, from, to, convention ORDER BY kind, name"
)


def iri(path):
    return f"<{B}{path}>"


def lit(text, datatype=None, lang=None):
    quoted = json.dumps(text)
    if datatype:
        return f"{quoted}^^<{datatype}>"
    return f"{quoted}@{lang}" if lang else quoted


def write(tmp_path, name, lines):
    path = tmp_path / name
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return str(path)


def edge_triple(src, pred, dst):
    return f"<<( {iri(src)} {iri(pred)} {iri(dst)} )>>"


# --- language maps ---------------------------------------------------------

LABELS = [
    f"{iri('dept/1')} <{LABEL}> {lit('Sales', lang='en')} .",
    f"{iri('dept/1')} <{LABEL}> {lit('Salg', lang='no')} .",
    f"{iri('dept/1')} {iri('prop/motto')} {lit('Go', lang='en')} .",
    f"{iri('dept/1')} {iri('prop/motto')} {lit('Gå', lang='no')} .",
]


def test_language_tags_are_dropped_by_default(tmp_path):
    g = kglite.load_rdf(write(tmp_path, "labels.nt", LABELS))
    row = g.cypher("MATCH (d) RETURN d.title AS title, d.motto AS motto").to_list()[0]
    assert row["title"] == "Sales"
    # Today's behaviour: both tagged literals fold into one multi-value list.
    assert row["motto"] == ["Go", "Gå"]


def test_language_maps_keep_every_tag(tmp_path):
    g = kglite.load_rdf(write(tmp_path, "labels.nt", LABELS), language_maps=True)
    row = g.cypher("MATCH (d) RETURN d.title AS title, d.motto AS motto, d.rdfs__label AS label").to_list()[0]
    assert row["title"] == "Sales"
    assert row["motto"] == {"en": "Go", "no": "Gå"}
    assert row["label"] == {"en": "Sales", "no": "Salg"}


def test_language_filter_applies_before_the_map(tmp_path):
    g = kglite.load_rdf(write(tmp_path, "labels.nt", LABELS), language_maps=True, languages=["no"])
    row = g.cypher("MATCH (d) RETURN d.motto AS motto").to_list()[0]
    assert row["motto"] == {"no": "Gå"}


# --- reifier edge properties -----------------------------------------------

REIFIED = [
    f"{iri('person/1')} <{RDF}type> {iri('type/Person')} .",
    f"{iri('dept/3')} <{RDF}type> {iri('type/Department')} .",
    f"{iri('person/1')} {iri('rel/WORKS_IN')} {iri('dept/3')} .",
    f"_:r1 <{RDF}reifies> {edge_triple('person/1', 'rel/WORKS_IN', 'dept/3')} .",
    f"_:r1 {iri('prop/since')} {lit('2019', XSD + 'integer')} .",
    f"_:r2 <{RDF}reifies> {edge_triple('person/1', 'rel/WORKS_IN', 'dept/3')} .",
    f"_:r2 {iri('prop/since')} {lit('2023', XSD + 'integer')} .",
]


def test_reifier_properties_land_on_parallel_edges(tmp_path):
    g = kglite.load_rdf(write(tmp_path, "reified.nq", REIFIED))
    assert g.cypher("MATCH (n) RETURN count(n) AS n").to_list()[0]["n"] == 2
    rows = g.cypher("MATCH (:Person)-[r:WORKS_IN]->(:Department) RETURN r.since AS since ORDER BY since").to_list()
    assert [r["since"] for r in rows] == [2019, 2023]


def test_plain_edge_without_a_reifier_is_unchanged(tmp_path):
    lines = [REIFIED[0], REIFIED[1], REIFIED[2]]
    g = kglite.load_rdf(write(tmp_path, "plain.nq", lines))
    assert g.cypher("MATCH ()-[r:WORKS_IN]->() RETURN count(r) AS n").to_list()[0]["n"] == 1


# --- the kg: manifest and typed literals -----------------------------------


def manifest():
    kinds = {
        "count": 1,
        "id_kind": "Int64",
        "title_kind": "String",
        "parent": None,
        "partial_labels": [],
        "properties": {},
    }
    return {
        "format": "kglite-export/1",
        "node_types": {
            "Person": {**kinds, "labels": ["Employee"]},
            "Department": {**kinds, "labels": []},
        },
        "relationship_types": {},
        "temporal": [
            {
                "target": "node",
                "name": "Person",
                "source_type": None,
                "from": "hired",
                "to": "left",
                "convention": "closed",
            },
            {
                "target": "relationship",
                "name": "WORKS_IN",
                "source_type": None,
                "from": "since",
                "to": "until",
                "convention": "half_open",
            },
        ],
    }


def kg_lines():
    meta = iri("meta")
    return [
        f"{meta} <{KG}manifest> {lit(json.dumps(manifest()), KG + 'json')} {meta} .",
        f"{iri('node/Person/7')} <{RDF}type> {iri('type/Person')} .",
        f"{iri('node/Person/7')} <{LABEL}> {lit('Ada')} .",
        f"{iri('node/Person/7')} {iri('prop/hired')} {lit('2019-03-01', XSD + 'date')} .",
        f"{iri('node/Person/7')} {iri('prop/left')} {lit('2023-01-01', XSD + 'date')} .",
        f"{iri('node/Person/7')} {iri('prop/skills')} {lit('["sql", "rust"]', KG + 'json')} .",
        f"{iri('node/Person/7')} {iri('prop/profile')} {lit('{"level": 3}', KG + 'json')} .",
        f"{iri('node/Person/7')} {iri('prop/office')} {lit('POINT(10.7 59.9)', 'http://www.opengis.net/ont/geosparql#wktLiteral')} .",
        f"{iri('node/Person/7')} {iri('prop/tenure')} {lit('P1Y6M2DT3H', XSD + 'duration')} .",
        f"{iri('node/Person/7')} {iri('prop/seen')} {lit('2024-05-06T07:08:09', XSD + 'dateTime')} .",
        f"{iri('node/Person/8')} <{RDF}type> {iri('type/Person')} .",
        f"{iri('node/Person/8')} {iri('prop/hired')} {lit('2021-01-01', XSD + 'date')} .",
        f"{iri('node/Department/3')} <{RDF}type> {iri('type/Department')} .",
        f"{iri('node/Department/3')} <{LABEL}> {lit('Sales')} .",
        f"{iri('node/Person/7')} {iri('rel/WORKS_IN')} {iri('node/Department/3')} .",
        f"_:r1 <{RDF}reifies> {edge_triple('node/Person/7', 'rel/WORKS_IN', 'node/Department/3')} .",
        f"_:r1 {iri('prop/since')} {lit('2019-03-01', XSD + 'date')} .",
        f"_:r1 {iri('prop/until')} {lit('2023-01-01', XSD + 'date')} .",
    ]


@pytest.fixture
def kg_graph(tmp_path):
    return kglite.load_rdf(write(tmp_path, "export.nq", kg_lines()))


def test_manifest_restores_declarations_and_valid_at(kg_graph):
    rows = kg_graph.cypher(DECLARATIONS).to_list()
    assert [(r["kind"], r["name"], r["from"], r["to"], r["convention"]) for r in rows] == [
        ("node", "Person", "hired", "left", "closed"),
        ("relationship", "WORKS_IN", "since", "until", "half_open"),
    ]
    q = "MATCH (p:Person) RETURN p.id AS id ORDER BY id"
    assert [r["id"] for r in kg_graph.cypher(q, valid_at="2019-06-01").to_list()] == [7]
    assert [r["id"] for r in kg_graph.cypher(q, valid_at="2022-06-01").to_list()] == [7, 8]
    assert [r["id"] for r in kg_graph.cypher(q, valid_at="2024-01-01").to_list()] == [8]


def test_manifest_restores_ids_labels_and_does_not_become_a_node(kg_graph):
    assert kg_graph.cypher("MATCH (n) RETURN count(n) AS n").to_list()[0]["n"] == 3
    rows = kg_graph.cypher("MATCH (p:Person) RETURN p.id AS id ORDER BY id").to_list()
    assert [r["id"] for r in rows] == [7, 8]
    assert kg_graph.cypher("MATCH (p:Employee) RETURN count(p) AS n").to_list()[0]["n"] == 2
    since = kg_graph.cypher("MATCH ()-[r:WORKS_IN]->() RETURN r.since AS s").to_list()
    assert since == [{"s": datetime.date(2019, 3, 1)}]


def test_typed_literals_map_back_per_kind(kg_graph):
    row = kg_graph.cypher(
        "MATCH (p:Person {id: 7}) RETURN p.skills AS skills, p.profile AS profile, "
        "p.office AS office, p.tenure AS tenure, p.seen AS seen, p.hired AS hired"
    ).to_list()[0]
    assert row["skills"] == ["sql", "rust"]
    assert row["profile"] == {"level": 3}
    assert row["office"] == {"latitude": 59.9, "longitude": 10.7}
    assert row["seen"] == datetime.datetime(2024, 5, 6, 7, 8, 9)
    assert row["hired"] == datetime.date(2019, 3, 1)
    assert row["tenure"] is not None and not isinstance(row["tenure"], str)
