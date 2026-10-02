"""``export_rdf`` -> ``load_rdf`` loses nothing.

Uses the HR / org-chart fixture of the CSV lossless test (people with
valid-time versions, departments, teams, parallel ``WORKS_IN`` edges with
properties) so the two exports are compared on the same graph.
"""

import re

import pytest

import kglite
from kglite import KnowledgeGraph
from tests.test_export_csv_lossless import (
    EXPECTED_EDGES,
    EXPECTED_NODES,
    build_source,
    declarations,
    norm,
    valid_time_answers,
)

PEOPLE_AT = "MATCH (p:Person) RETURN p.id AS id, p.title AS title ORDER BY id, title"


def own(props):
    """Properties minus the importer's ``uri`` and the title-alias ``name``.

    A node without its own ``name`` reads it as its title; only a graph built by
    ``CREATE`` lists that alias in ``properties()``, so it is not data.
    """
    out = {k: v for k, v in props.items() if k != "uri"}
    if out.get("name") == out.get("title"):
        out.pop("name", None)
    return out


def dump(g):
    """Like the CSV test's dump, minus the importer's own ``uri`` property."""
    nodes = sorted(
        repr(
            (
                sorted(r["l"]),
                norm(r["id"]),
                norm(r["t"]),
                norm(own(r["p"])),
            )
        )
        for r in g.cypher("MATCH (n) RETURN labels(n) AS l, n.id AS id, n.title AS t, properties(n) AS p").to_list()
    )
    edges = sorted(
        repr((r["st"], norm(r["sid"]), r["rt"], r["tt"], norm(r["tid"]), norm(r["p"])))
        for r in g.cypher(
            "MATCH (a)-[r]->(b) RETURN labels(a)[0] AS st, a.id AS sid, type(r) AS rt, "
            "labels(b)[0] AS tt, b.id AS tid, properties(r) AS p"
        ).to_list()
    )
    return nodes, edges


def export_and_load(source, tmp_path, fmt="nq", **options):
    path = tmp_path / f"graph.{fmt}"
    summary = source.export_rdf(str(path), **options)
    return summary, kglite.load_rdf(str(path)), path


@pytest.mark.parity
@pytest.mark.parametrize("fmt", ["nq", "trig"])
@pytest.mark.parametrize("storage", ["memory", "mapped", "disk"])
def test_round_trip_loses_nothing(storage, fmt, tmp_path):
    source = build_source(storage, tmp_path)
    summary, back, _ = export_and_load(source, tmp_path, fmt)
    assert sum(summary["nodes"].values()) == EXPECTED_NODES
    assert sum(summary["connections"].values()) == EXPECTED_EDGES

    before_nodes, before_edges = dump(source)
    after_nodes, after_edges = dump(back)
    assert len(after_nodes) == EXPECTED_NODES
    assert len(after_edges) == EXPECTED_EDGES
    assert after_nodes == before_nodes
    assert after_edges == before_edges
    assert declarations(back) == declarations(source)
    assert len(declarations(back)) == 5
    assert valid_time_answers(back) == valid_time_answers(source)


def test_duplicate_id_versions_stay_two_nodes(tmp_path):
    source = build_source("memory", tmp_path)
    _, back, _ = export_and_load(source, tmp_path)
    query = "MATCH (p:Person) RETURN p.id AS id, p.title AS t"
    rows = sorted(map(repr, back.cypher(query).to_list()))
    assert len(rows) == 3
    assert rows == sorted(map(repr, source.cypher(query).to_list()))


def test_parallel_edges_with_properties_keep_their_own_properties(tmp_path):
    source = build_source("memory", tmp_path)
    _, back, path = export_and_load(source, tmp_path)
    q = "MATCH (:Person {id: 2})-[r:WORKS_IN]->(:Department {id: 'D-1'}) RETURN r.role AS role, r.hours AS hours"
    assert sorted(map(repr, back.cypher(q).to_list())) == sorted(map(repr, source.cypher(q).to_list()))
    text = path.read_text(encoding="utf-8")
    assert text.count("rdf-syntax-ns#reifies") == 6  # the six edges that carry properties
    assert "<<(" in text


def test_the_manifest_travels_as_one_kg_json_statement(tmp_path):
    source = build_source("memory", tmp_path)
    _, _, path = export_and_load(source, tmp_path)
    text = path.read_text(encoding="utf-8")
    manifest_lines = [ln for ln in text.splitlines() if "kg#manifest" in ln]
    assert len(manifest_lines) == 1
    assert "kg#json" in manifest_lines[0] and "/meta>" in manifest_lines[0]
    assert "@prefix" not in text


def test_literals_are_typed(tmp_path):
    source = build_source("memory", tmp_path)
    _, _, path = export_and_load(source, tmp_path)
    text = path.read_text(encoding="utf-8")
    for datatype in (
        "XMLSchema#double",
        "XMLSchema#boolean",
        "XMLSchema#date>",
        "XMLSchema#dateTime>",
        "XMLSchema#duration>",
        "geosparql#wktLiteral",
        "kg#json",
    ):
        assert datatype in text, datatype
    assert re.search(r'"P3D"\^\^<http://www.w3.org/2001/XMLSchema#duration>', text)


def test_cross_format_csv_and_rdf_agree(tmp_path):
    source = build_source("memory", tmp_path)
    out = tmp_path / "csv"
    source.export_csv(str(out))
    from_csv = kglite.from_blueprint(str(out / "blueprint.json"), save=False)
    _, from_rdf, _ = export_and_load(source, tmp_path)
    assert declarations(from_csv) == declarations(from_rdf) == declarations(source)
    assert valid_time_answers(from_csv) == valid_time_answers(from_rdf)
    for day in ("2005-12-31", "2006-01-01", "2009-12-31", "2010-01-01"):
        boundary = f"FOR VALID_TIME AS OF date('{day}') {PEOPLE_AT}"
        assert from_csv.cypher(boundary).to_list() == from_rdf.cypher(boundary).to_list()


def test_schema_org_alias_is_opt_in(tmp_path):
    source = build_source("memory", tmp_path)
    plain = tmp_path / "plain.nq"
    aliased = tmp_path / "aliased.nq"
    source.export_rdf(str(plain))
    source.export_rdf(str(aliased), schema_org=True)
    assert "schema.org/validFrom" not in plain.read_text(encoding="utf-8")
    text = aliased.read_text(encoding="utf-8")
    assert "http://schema.org/validFrom" in text and "http://schema.org/validThrough" in text
    # The manifest still carries the declarations; the aliases are extra properties.
    back = kglite.load_rdf(str(aliased))
    assert declarations(back) == declarations(source)
    assert back.cypher("MATCH (p:Person {id: 2}) RETURN p.schema__validFrom AS f").to_list()[0]["f"] is not None


def test_mixed_id_kinds_do_not_merge(tmp_path):
    g = KnowledgeGraph()
    g.cypher("CREATE (:Badge {id: 1, title: 'int one'})")
    g.cypher("CREATE (:Badge {id: '1', title: 'string one'})")
    _, back, _ = export_and_load(g, tmp_path)
    assert sorted(r["t"] for r in back.cypher("MATCH (b:Badge) RETURN b.title AS t").to_list()) == [
        "int one",
        "string one",
    ]


def test_names_needing_escapes_round_trip(tmp_path):
    g = KnowledgeGraph()
    g.cypher("CREATE (:`Org Unit` {id: 'a/b c', title: 'x', `first name`: 'Ada'})")
    g.cypher("CREATE (:`Org Unit` {id: 'z', title: 'y'})")
    g.cypher(
        "MATCH (a:`Org Unit` {id: 'z'}), (b:`Org Unit` {id: 'a/b c'}) CREATE (a)-[:`REPORTS TO` {since: 2020}]->(b)"
    )
    _, back, _ = export_and_load(g, tmp_path)
    row = back.cypher("MATCH (n:`Org Unit` {id: 'a/b c'}) RETURN n.`first name` AS f").to_list()
    assert row == [{"f": "Ada"}]
    assert back.cypher("MATCH ()-[r:`REPORTS TO`]->() RETURN r.since AS s").to_list() == [{"s": 2020}]


def test_a_base_inside_a_well_known_namespace_is_refused(tmp_path):
    g = KnowledgeGraph()
    g.cypher("CREATE (:A {id: 1, title: 't'})")
    with pytest.raises(ValueError, match="well-known"):
        g.export_rdf(str(tmp_path / "x.nq"), base="http://schema.org/")
    with pytest.raises(ValueError, match="must end"):
        g.export_rdf(str(tmp_path / "x.nq"), base="https://e.org/ns")


def test_export_entry_point_infers_rdf_formats(tmp_path):
    source = build_source("memory", tmp_path)
    source.export(str(tmp_path / "a.nq"))
    source.export(str(tmp_path / "b.trig"))
    assert dump(kglite.load_rdf(str(tmp_path / "a.nq"))) == dump(kglite.load_rdf(str(tmp_path / "b.trig")))
    with pytest.raises(ValueError, match="Unknown RDF format"):
        source.export_rdf(str(tmp_path / "c.nq"), format="turtle")


def test_integers_and_negative_durations(tmp_path):
    g = KnowledgeGraph()
    g.cypher("CREATE (:Role {id: 1, title: 'r', level: 7, delta: duration({days: -2}), neg: -3.5})")
    _, back, path = export_and_load(g, tmp_path)
    assert "XMLSchema#integer" in path.read_text(encoding="utf-8")
    query = "MATCH (r:Role) RETURN r.level AS level, r.delta AS delta, r.neg AS neg"
    assert back.cypher(query).to_list() == g.cypher(query).to_list()


def test_a_property_named_uri_survives_the_round_trip(tmp_path):
    g = KnowledgeGraph()
    g.cypher("CREATE (:Person {id: 1, title: 'Ada', uri: 'https://hr.example/people/ada'})")
    g.cypher("CREATE (:Person {id: 2, title: 'Bo'})")
    _, back, _ = export_and_load(g, tmp_path)
    rows = back.cypher("MATCH (p:Person) RETURN p.id AS id, properties(p) AS p ORDER BY id").to_list()
    assert rows[0]["p"]["uri"] == "https://hr.example/people/ada"
    assert "uri" not in rows[1]["p"]


def test_plain_rdf_still_gets_its_uri_property(tmp_path):
    path = tmp_path / "plain.nt"
    path.write_text(
        '<http://ex.org/a> <http://www.w3.org/2000/01/rdf-schema#label> "A" .\n',
        encoding="utf-8",
    )
    back = kglite.load_rdf(str(path))
    assert back.cypher("MATCH (n) RETURN n.uri AS u").to_list() == [{"u": "http://ex.org/a"}]


def test_mixed_id_kinds_keep_their_ids(tmp_path):
    g = KnowledgeGraph()
    g.cypher("CREATE (:Badge {id: 1, title: 'int one'})")
    g.cypher("CREATE (:Badge {id: '1', title: 'string one'})")
    _, back, _ = export_and_load(g, tmp_path)
    query = "MATCH (b:Badge) RETURN b.id AS id, b.title AS t ORDER BY t"
    assert back.cypher(query).to_list() == g.cypher(query).to_list()


def test_mixed_sign_durations_stay_durations(tmp_path):
    g = KnowledgeGraph()
    g.cypher("CREATE (:Role {id: 1, title: 'r', span: duration({months: 1, days: -2})})")
    _, back, _ = export_and_load(g, tmp_path)
    # Cypher shows a duration as a map, so compare its string form: a map
    # (the old kg:json spelling) would print differently.
    query = "MATCH (r:Role) RETURN r.span AS span, toString(r.span) AS text"
    assert back.cypher(query).to_list() == g.cypher(query).to_list()
    assert back.cypher(query).to_list()[0]["text"].startswith("duration(")


def test_duplicate_id_versions_stay_two_nodes_without_a_prior_lookup(tmp_path):
    # No query has touched the id index, so the exporter must build it itself
    # to tell the versions apart; otherwise both share one IRI and merge.
    g = KnowledgeGraph()
    g.cypher("CREATE (:Person {id: 1, title: 'Ada', level: 7}), (:Person {id: 1, title: 'Ada', level: 8})")
    _, back, _ = export_and_load(g, tmp_path)
    query = "MATCH (p:Person) RETURN p.level AS level ORDER BY level"
    assert back.cypher(query).to_list() == [{"level": 7}, {"level": 8}]
