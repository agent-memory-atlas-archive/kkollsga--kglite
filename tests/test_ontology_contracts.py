"""Ontology contracts: relationship pair uniqueness (`distinct`), property value
contracts (`property_values`) and secondary-label exemptions (`exempt`)."""

import json

import pandas as pd
import pytest

import kglite

STORAGES = ["memory", "mapped", "disk"]


def make_graph(storage, tmp_path):
    opts = {} if storage == "memory" else {"storage": storage}
    if storage == "disk":
        opts["path"] = str(tmp_path / "disk")
    return kglite.KnowledgeGraph(**opts)


def audit(g, by=None):
    param = f"{{by: '{by}'}}" if by else ""
    rows = g.cypher(
        f"CALL ontology_audit({param}) YIELD rule, domain_class, property, violations, exempted, total "
        "RETURN rule, domain_class, property, violations, exempted, total"
    ).to_list()
    return rows


def line(g, rule, prop=None, by=None):
    matches = [r for r in audit(g, by) if r["rule"] == rule and r["property"] == prop]
    assert len(matches) == 1, (rule, prop, matches)
    r = matches[0]
    return r["violations"], r["exempted"], r["total"]


# ---- distinct ------------------------------------------------------------


def distinct_decl(distinct=True, severity="error", exempt=None):
    rel = {"domain": "Law", "range": "Actor", "distinct": distinct, "enforcement": severity}
    if exempt:
        rel["exempt"] = exempt
    return {"classes": {"Law": {}, "Actor": {}}, "relationships": {"HAS": rel}}


def seeded_laws(g):
    g.cypher("CREATE (:Law {id: 1}), (:Law {id: 2}), (:Law:Historical {id: 3}), (:Actor {id: 10}), (:Actor {id: 11})")


def link(g, s, t, props=""):
    return g.cypher(f"MATCH (a {{id: {s}}}), (b {{id: {t}}}) CREATE (a)-[:HAS {props}]->(b)")


def has_count(g):
    return g.cypher("MATCH ()-[r:HAS]->() RETURN count(r) AS c").to_list()[0]["c"]


@pytest.mark.parametrize("storage", STORAGES)
def test_distinct_audit_counts_surplus_and_breaks_down(storage, tmp_path):
    g = make_graph(storage, tmp_path)
    seeded_laws(g)
    g.define_ontology(distinct_decl(True, "advisory"))
    for _ in range(3):
        link(g, 1, 10)
    link(g, 1, 11)
    link(g, 3, 11)
    link(g, 3, 11)
    assert line(g, "HAS.distinct") == (3, 0, 6)
    by_class = [r for r in audit(g, "domain_class") if r["rule"] == "HAS.distinct"]
    assert [(r["domain_class"], r["violations"]) for r in by_class] == [("Law", 3)]


@pytest.mark.parametrize("storage", STORAGES)
def test_distinct_with_properties_groups_by_value(storage, tmp_path):
    g = make_graph(storage, tmp_path)
    seeded_laws(g)
    g.define_ontology(distinct_decl(["side"], "advisory"))
    link(g, 1, 10, "{side: 'a'}")
    link(g, 1, 10, "{side: 'b'}")
    assert line(g, "HAS.distinct") == (0, 0, 2)
    link(g, 1, 10, "{side: 'a'}")
    assert line(g, "HAS.distinct") == (1, 0, 3)


@pytest.mark.parametrize("storage", STORAGES)
def test_distinct_error_refuses_with_a_clear_error_and_rolls_back(storage, tmp_path):
    g = make_graph(storage, tmp_path)
    seeded_laws(g)
    g.define_ontology(distinct_decl())
    link(g, 1, 10)
    with pytest.raises(kglite.OntologyViolationError) as info:
        link(g, 1, 10)
    assert info.value.rule == "distinct"
    assert info.value.entity == "relationship"
    assert info.value.entity_type == "HAS"
    assert "2 'HAS' relationships to the same target" in str(info.value)
    assert has_count(g) == 1
    # A transaction that hits the rule in its second statement rolls back the first.
    with pytest.raises(kglite.OntologyViolationError):
        with g.begin() as tx:
            tx.cypher("MATCH (a {id: 2}), (b {id: 11}) CREATE (a)-[:HAS]->(b)")
            tx.cypher("MATCH (a {id: 1}), (b {id: 10}) CREATE (a)-[:HAS]->(b)")
    assert has_count(g) == 1


def test_distinct_bulk_loader_refuses_repeated_rows_and_merging_loads_fold(tmp_path):
    g = make_graph("memory", tmp_path)
    seeded_laws(g)
    g.define_ontology(distinct_decl())

    def frame(rows):
        return pd.DataFrame(rows, columns=["s", "t"])

    with pytest.raises(kglite.OntologyViolationError) as info:
        g.add_connections(frame([(1, 10), (1, 11), (1, 10)]), "HAS", "Law", "s", "Actor", "t")
    assert info.value.rule == "distinct"
    assert has_count(g) == 0
    g.add_connections(frame([(1, 10), (1, 11)]), "HAS", "Law", "s", "Actor", "t")
    g.add_connections(frame([(1, 10), (1, 10)]), "HAS", "Law", "s", "Actor", "t")
    assert has_count(g) == 2


def test_distinct_warn_reports_and_lands(tmp_path):
    g = make_graph("memory", tmp_path)
    seeded_laws(g)
    g.define_ontology(distinct_decl(True, "warn"))
    link(g, 1, 10)
    result = link(g, 1, 10)
    assert any("ontology warning (distinct)" in w for w in result.diagnostics["warnings"])
    assert has_count(g) == 2


def test_distinct_declare_over_copies_is_refused_at_error(tmp_path):
    g = make_graph("memory", tmp_path)
    seeded_laws(g)
    for _ in range(3):
        link(g, 1, 10)
    with pytest.raises(kglite.OntologyViolationError) as info:
        g.define_ontology(distinct_decl())
    assert info.value.report == [
        {"rule": "distinct", "entity": "relationship", "entity_type": "HAS", "property": None, "count": 2}
    ]
    g.define_ontology(distinct_decl(True, "warn"))


def test_distinct_and_exempt_survive_save_and_load(tmp_path):
    g = make_graph("memory", tmp_path)
    seeded_laws(g)
    g.define_ontology(distinct_decl(["side"], "advisory", {"distinct": ["Historical"]}))
    path = str(tmp_path / "g.kgl")
    g.save(path)
    loaded = kglite.load(path)
    rel = loaded.ontology()["relationships"]["HAS"]
    assert rel["distinct"] == ["side"]
    assert rel["exempt"] == {"distinct": ["Historical"]}
    # The read-back document declares again unchanged.
    loaded.define_ontology(loaded.ontology())
    bare = make_graph("memory", tmp_path)
    bare.define_ontology(distinct_decl(True, "advisory"))
    assert bare.ontology()["relationships"]["HAS"]["distinct"] is True


@pytest.mark.parametrize("bad", [[], ["a", "a"], [1], "yes"])
def test_distinct_malformed_is_refused(bad):
    g = kglite.KnowledgeGraph()
    with pytest.raises(ValueError, match="distinct"):
        g.define_ontology(distinct_decl(bad))


def blueprint_with_duplicates(tmp_path, ontology):
    pd.DataFrame({"law_id": [1, 2], "name": ["A", "B"]}).to_csv(tmp_path / "laws.csv", index=False)
    pd.DataFrame({"actor_id": [10], "aname": ["X"]}).to_csv(tmp_path / "actors.csv", index=False)
    pd.DataFrame({"s": [1, 1, 2], "t": [10, 10, 10]}).to_csv(tmp_path / "has.csv", index=False)
    (tmp_path / "o.json").write_text(json.dumps(ontology), encoding="utf-8")
    bp = {
        "settings": {"root": str(tmp_path), "output": "out.kgl"},
        "ontology": "o.json",
        "nodes": {
            "Law": {
                "csv": "laws.csv",
                "pk": "law_id",
                "title": "name",
                "connections": {
                    "junction_edges": {"HAS": {"csv": "has.csv", "source_fk": "s", "target": "Actor", "target_fk": "t"}}
                },
            },
            "Actor": {"csv": "actors.csv", "pk": "actor_id", "title": "aname"},
        },
    }
    path = tmp_path / "blueprint.json"
    path.write_text(json.dumps(bp), encoding="utf-8")
    return path


def test_blueprint_gate_fails_on_distinct_at_error(tmp_path):
    path = blueprint_with_duplicates(tmp_path, distinct_decl(True, "error"))
    with pytest.raises(ValueError, match=r"HAS\.distinct: 1/3"):
        kglite.from_blueprint(str(path))
    assert not (tmp_path / "out.kgl").exists()


def test_blueprint_gate_reports_distinct_at_warn(tmp_path, recwarn):
    path = blueprint_with_duplicates(tmp_path, distinct_decl(True, "warn"))
    g = kglite.from_blueprint(str(path), save=False, verbose=True)
    assert any("HAS.distinct: 1/3" in str(w.message) for w in recwarn)
    assert line(g, "HAS.distinct") == (1, 0, 3)


# ---- property_values -----------------------------------------------------


def values_decl(severity="error", exempt=None):
    rel = {
        "domain": "Matter",
        "range": "Judge",
        "property_values": {"side": {"enum": ["plaintiff", "defendant"]}, "year": {"min": 1900, "max": 2100}},
        "enforcement": severity,
    }
    if exempt:
        rel["exempt"] = exempt
    return {
        "classes": {
            "Judge": {
                "property_values": {
                    "name": {"not_pattern": r" og |, "},
                    "level": {"enum": ["supreme", "appeal", "district"]},
                    "ref": {"pattern": r"^[0-9]+/[0-9]+$"},
                },
                "enforcement": severity,
            },
            "Matter": {},
        },
        "relationships": {"JUDGED_BY": rel},
    }


@pytest.mark.parametrize("storage", STORAGES)
def test_property_values_audit_with_property_breakdown(storage, tmp_path):
    g = make_graph(storage, tmp_path)
    g.cypher(
        "CREATE (:Judge {id: 1, name: 'A. Judge', level: 'district', ref: '1/2'}), "
        "(:Judge {id: 2, name: 'A. Judge og B. Judge', level: 'district'}), "
        "(:Judge {id: 3, name: 'C', level: 'county', ref: 'x'}), "
        "(:Judge {id: 4, name: null, level: null}), "
        "(:Judge {id: 5, level: 7})"
    )
    g.define_ontology(values_decl("advisory"))
    # #2 name; #3 level + ref; #5 level (a number is outside the enum).
    assert line(g, "Judge.property_values") == (3, 0, 5)
    for prop, violations in [("name", 1), ("level", 2), ("ref", 1)]:
        assert line(g, "Judge.property_values", prop, by="property") == (violations, 0, 5)


@pytest.mark.parametrize("storage", STORAGES)
def test_property_values_edge_audit_enforcement_and_drilldown(storage, tmp_path):
    g = make_graph(storage, tmp_path)
    g.cypher("CREATE (:Matter {id: 10}), (:Matter:Historical {id: 11}), (:Judge {id: 1})")
    g.define_ontology(values_decl("advisory", {"property_values": ["Historical"]}))
    for m, props in [
        (10, "{side: 'plaintiff', year: 2020}"),
        (10, "{side: 'claimant', year: 1850}"),
        (10, "{year: 2300}"),
        (11, "{side: 'claimant'}"),
    ]:
        g.cypher(f"MATCH (m {{id: {m}}}), (j {{id: 1}}) CREATE (m)-[:JUDGED_BY {props}]->(j)")
    assert line(g, "JUDGED_BY.property_values") == (2, 1, 4)
    assert line(g, "JUDGED_BY.property_values", "side", by="property") == (1, 1, 4)
    assert line(g, "JUDGED_BY.property_values", "year", by="property") == (2, 1, 4)
    rows = g.cypher(
        "CALL edge_property_violation() YIELD check, properties, exempt "
        "WHERE check = 'property_values' RETURN properties, exempt"
    ).to_list()
    assert len(rows) == 3 and sum(r["exempt"] for r in rows) == 1


@pytest.mark.parametrize("storage", STORAGES)
def test_property_values_error_refuses_node_and_edge_writes(storage, tmp_path):
    g = make_graph(storage, tmp_path)
    g.define_ontology(values_decl("error"))
    g.cypher("CREATE (:Judge {id: 1, level: 'appeal', ref: '1/2', name: 'A'}), (:Matter {id: 10})")
    with pytest.raises(kglite.OntologyViolationError) as info:
        g.cypher("CREATE (:Judge {id: 2, level: 'county'})")
    assert (info.value.rule, info.value.entity, info.value.property) == ("property_value", "node", "level")
    assert "enum [supreme, appeal, district]" in str(info.value)
    with pytest.raises(kglite.OntologyViolationError) as info:
        g.cypher("MATCH (j:Judge {id: 1}) SET j.ref = 'bad'")
    assert info.value.property == "ref"
    with pytest.raises(kglite.OntologyViolationError) as info:
        g.cypher("MATCH (m {id: 10}), (j {id: 1}) CREATE (m)-[:JUDGED_BY {year: 1700}]->(j)")
    assert (info.value.rule, info.value.entity, info.value.property) == ("property_value", "relationship", "year")
    # Null, absent and a value of another type are not value violations.
    g.cypher("CREATE (:Judge {id: 3, name: null, ref: 12})")
    assert g.cypher("MATCH (j:Judge) RETURN count(j) AS c").to_list()[0]["c"] == 2


def test_property_values_bulk_loaders_refuse_the_whole_frame(tmp_path):
    g = make_graph("memory", tmp_path)
    g.define_ontology(values_decl("error"))
    bad = pd.DataFrame({"id": [1, 2], "level": ["appeal", "county"]})
    with pytest.raises(kglite.OntologyViolationError) as info:
        g.add_nodes(bad, "Judge", "id")
    assert info.value.rule == "property_value"
    assert g.cypher("MATCH (j:Judge) RETURN count(j) AS c").to_list()[0]["c"] == 0
    g.add_nodes(pd.DataFrame({"id": [1], "level": ["appeal"]}), "Judge", "id")
    g.add_nodes(pd.DataFrame({"id": [10]}), "Matter", "id")
    edges = pd.DataFrame({"m": [10], "j": [1], "side": ["claimant"]})
    with pytest.raises(kglite.OntologyViolationError) as info:
        g.add_connections(edges, "JUDGED_BY", "Matter", "m", "Judge", "j", columns=["side"])
    assert (info.value.rule, info.value.property) == ("property_value", "side")


def test_property_values_warn_reports_and_lands(tmp_path):
    g = make_graph("memory", tmp_path)
    g.define_ontology(values_decl("warn"))
    result = g.cypher("CREATE (:Judge {id: 1, level: 'county'})")
    assert any("ontology warning (property_value)" in w for w in result.diagnostics["warnings"])


@pytest.mark.parametrize(
    ("contract", "needle"),
    [
        ({"pattern": "("}, "not a valid regex"),
        ({"not_pattern": "[a"}, "not_pattern"),
        ({}, "declares no constraint"),
        ({"enum": []}, "at least one value"),
        ({"min": 5, "max": 1}, "above max"),
        ({"min": "1"}, "must be a number"),
        ({"pattrn": "x"}, "Did you mean"),
    ],
)
def test_property_values_malformed_is_refused(contract, needle):
    g = kglite.KnowledgeGraph()
    with pytest.raises(ValueError, match=needle):
        g.define_ontology({"classes": {"A": {"property_values": {"p": contract}}}})


def test_property_values_declare_over_data_and_save_load(tmp_path):
    g = make_graph("memory", tmp_path)
    g.cypher("CREATE (:Judge {id: 1, level: 'county'}), (:Judge {id: 2, level: 'county'})")
    with pytest.raises(kglite.OntologyViolationError) as info:
        g.define_ontology(values_decl("error"))
    assert {"rule": "property_value", "entity": "node", "entity_type": "Judge", "property": "level", "count": 2} in (
        info.value.report
    )
    g.define_ontology(values_decl("warn"))
    path = str(tmp_path / "g.kgl")
    g.save(path)
    loaded = kglite.load(path)
    contracts = loaded.ontology()["classes"]["Judge"]["property_values"]
    assert contracts["level"] == {"enum": ["supreme", "appeal", "district"]}
    assert contracts["name"] == {"not_pattern": " og |, "}
    assert line(loaded, "Judge.property_values") == (2, 0, 2)
    loaded.define_ontology(loaded.ontology())


def test_property_values_blueprint_gate(tmp_path):
    pd.DataFrame({"jid": [1, 2], "name": ["A", "B og C"]}).to_csv(tmp_path / "judges.csv", index=False)
    (tmp_path / "o.json").write_text(
        json.dumps(
            {"classes": {"Judge": {"property_values": {"name": {"not_pattern": " og "}}, "enforcement": "error"}}}
        ),
        encoding="utf-8",
    )
    bp = {
        "settings": {"root": str(tmp_path), "output": "out.kgl"},
        "ontology": "o.json",
        "nodes": {"Judge": {"csv": "judges.csv", "pk": "jid", "title": "name"}},
    }
    path = tmp_path / "blueprint.json"
    path.write_text(json.dumps(bp), encoding="utf-8")
    with pytest.raises(ValueError, match=r"Judge\.property_values: 1/2"):
        kglite.from_blueprint(str(path))
    assert not (tmp_path / "out.kgl").exists()


def test_show_ontology_and_describe_render_the_new_keys(tmp_path):
    g = make_graph("memory", tmp_path)
    decl = values_decl("advisory")
    decl["relationships"]["JUDGED_BY"]["distinct"] = ["side"]
    g.define_ontology(decl)
    rows = g.cypher("SHOW ONTOLOGY").to_list()
    rel = next(r for r in rows if r["kind"] == "relationship")
    assert rel["distinct"] == "[side]"
    assert rel["property_values"]["side"] == "enum [plaintiff, defendant]"
    cls = next(r for r in rows if r["name"] == "Judge")
    assert "pattern" in cls["property_values"]["ref"]
    text = g.describe()
    assert 'distinct="[side]"' in text
    assert "property_values=" in text


# ---- exempt by secondary label --------------------------------------------


def required_decl(severity="advisory", exempt=None):
    rel = {"domain": "Section", "range": "Law", "required": True, "enforcement": severity}
    if exempt is not None:
        rel["exempt"] = exempt
    return {"classes": {"Section": {}, "Law": {}}, "relationships": {"SECTION_OF": rel}}


def test_exempt_secondary_label_covers_required_and_counts_apart(tmp_path):
    g = make_graph("memory", tmp_path)
    g.cypher(
        "CREATE (:Law {id: 100}), (:Section {id: 1}), (:Section:Historical {id: 2}), (:Section:Historical {id: 3})"
    )
    g.define_ontology(required_decl(exempt={"required": ["Historical"]}))
    assert line(g, "SECTION_OF.required") == (1, 2, 3)
    g.define_ontology(required_decl())
    assert line(g, "SECTION_OF.required") == (3, 0, 3)


def test_exempt_secondary_label_excuses_the_enforced_write(tmp_path):
    g = make_graph("memory", tmp_path)
    g.cypher("CREATE (:Law {id: 100})")
    g.define_ontology(required_decl("error", {"required": ["Historical"]}))
    g.cypher("CREATE (:Section:Historical {id: 2})")
    with pytest.raises(kglite.OntologyViolationError) as info:
        g.cypher("CREATE (:Section {id: 1})")
    assert info.value.rule == "required_relationship"


def test_exempt_secondary_label_covers_edge_property_checks(tmp_path):
    g = make_graph("memory", tmp_path)
    g.cypher("CREATE (:Law {id: 100}), (:Section {id: 1}), (:Section:Historical {id: 2})")
    decl = required_decl("advisory", {"required_properties": ["Historical"]})
    decl["relationships"]["SECTION_OF"]["required_properties"] = ["since"]
    g.define_ontology(decl)
    for sid in (1, 2):
        g.cypher(f"MATCH (s {{id: {sid}}}), (l:Law) CREATE (s)-[:SECTION_OF]->(l)")
    assert line(g, "SECTION_OF.required_properties") == (1, 1, 2)
    # `{by: 'property'}` leaves the excused edge out of the census.
    assert line(g, "SECTION_OF.required_properties", "since", by="property") == (1, 1, 2)


def test_exempt_name_matching_nothing_warns_and_cardinality_stays_refused(tmp_path):
    g = make_graph("memory", tmp_path)
    warnings = g.define_ontology(required_decl(exempt={"required": ["Histroical"]}))
    assert any("'Histroical'" in w and "exempts nothing" in w for w in warnings), warnings
    with pytest.raises(ValueError, match="does not accept check 'cardinality'"):
        g.define_ontology(required_decl(exempt={"cardinality": ["Historical"]}))
