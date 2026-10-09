"""Issue #222 acceptance matrix: an enforceable ontology, identical everywhere.

The issue's ontology: `Person` requires `name` (a string) and `WORKS_AT` runs
`Person` -> `Company`, at enforcement level `error`. Each acceptance criterion
(AC1-AC10) runs against the Python API and a real `kglite-bolt-server`
process. The C ABI runs the same criteria in
`crates/kglite-c/tests/ontology_acceptance.rs` (there is no Python-to-C
harness); AC10 pins the shared `(rule, entity, entity_type, property)` tuples
in both places.
"""

import json
import os

import pytest

import kglite

neo4j = pytest.importorskip("neo4j")

from tests.conftest import (  # noqa: E402
    _BOLT_SKIP_REASON,
    _bolt_binary_available,
    _graceful_stop_bolt_server,
    _spawn_bolt_server,
    _teardown_bolt_server,
)

CODE = "Neo.ClientError.Schema.ConstraintValidationFailed"


def issue_ontology(level="error"):
    return {
        "classes": {
            "Person": {"required_properties": ["name"], "property_types": {"name": "string"}, "enforcement": level},
            "Company": {},
        },
        "relationships": {"WORKS_AT": {"domain": "Person", "range": "Company", "enforcement": level}},
    }


# The AC10 table: the violation each invalid write produces, on every surface.
MISSING_NAME = ("required_property", "node", "Person", "name")
WRONG_TYPE = ("property_type", "node", "Person", "name")
REVERSED_EDGE = ("domain", "relationship", "WORKS_AT", None)

Q_NO_NAME = "CREATE (:Person {id: 2})"
Q_NAME_5 = "CREATE (:Person {id: 3, name: 5})"
Q_REVERSED = "CREATE (:Company {id: 11, name: 'Acme'})-[:WORKS_AT]->(:Person {id: 4, name: 'B'})"
Q_VALID = "CREATE (:Person {id: 1, name: 'A'})"
Q_COUNT = "MATCH (n) RETURN count(n) AS c"


class Refusal:
    """One refused write as a surface reports it."""

    def __init__(self, code, text, key=None, report=None):
        self.code = code
        self.text = text
        self.key = key
        self.report = report


class PythonSurface:
    name = "python"

    def __init__(self, tmp_path):
        self.tmp_path = tmp_path
        self.g = kglite.KnowledgeGraph()

    def declare(self, doc):
        self.g.define_ontology(doc)

    def run(self, query):
        return self.g.cypher(query).to_list()

    def refused(self, query):
        with pytest.raises(kglite.OntologyViolationError) as info:
            self.g.cypher(query)
        e = info.value
        assert isinstance(e, kglite.ConstraintViolationError)
        return Refusal("OntologyViolation", str(e), (e.rule, e.entity, e.entity_type, e.property))

    def declare_refused(self, doc):
        with pytest.raises(kglite.OntologyViolationError) as info:
            self.g.define_ontology(doc)
        e = info.value
        return Refusal("OntologyViolation", str(e), (e.rule, e.entity, e.entity_type, e.property), e.report)

    def count(self):
        return self.run(Q_COUNT)[0]["c"]

    def show(self):
        return self.run("SHOW ONTOLOGY")

    def late_violation(self):
        with pytest.raises(kglite.OntologyViolationError):
            with self.g.begin() as tx:
                tx.cypher("CREATE (:Person {id: 1, name: 'A'})")
                tx.cypher("CREATE (:Person {id: 2, name: 'B'})")
                tx.cypher(Q_NO_NAME)

    def warn_write(self, query):
        res = self.g.cypher(query)
        return list(res.diagnostics["warnings"])

    def persist_and_reopen(self):
        path = str(self.tmp_path / "py.kgl")
        self.g.save(path)
        self.g = kglite.open(path)

    def backup_and_open(self):
        dest = self.tmp_path / "py-backup.kgl"
        self.g.backup(str(dest))
        self.g = kglite.open(str(dest))


class BoltSurface:
    name = "bolt"

    def __init__(self, tmp_path):
        self.tmp_path = tmp_path
        self.graph = tmp_path / "served.kgl"
        kglite.KnowledgeGraph().save(str(self.graph))
        self.proc = None
        self.url = None
        self.flags = ["--durability", "off"]

    def start(self, doc=None, *extra):
        args = list(self.flags)
        if doc is not None:
            path = self.tmp_path / "ontology.json"
            path.write_text(json.dumps(doc), encoding="utf-8")
            args += ["--ontology", str(path)]
        self.proc, self.url = _spawn_bolt_server(self.graph, extra_args=[*args, *extra])

    def stop(self):
        if self.proc is not None:
            _teardown_bolt_server(self.proc)
            self.proc = None

    def _driver(self):
        return neo4j.GraphDatabase.driver(self.url, auth=("neo4j", "password"))

    def declare(self, doc):
        if self.proc is None:
            self.start(doc)
        else:
            with self._driver() as d, d.session() as s:
                s.run("CALL db.ontology.declare({ontology: $doc})", doc=doc).consume()

    def run(self, query):
        with self._driver() as d, d.session() as s:
            return [r.data() for r in s.run(query)]

    def refused(self, query):
        with self._driver() as d, d.session() as s:
            with pytest.raises(neo4j.exceptions.Neo4jError) as info:
                s.run(query).consume()
        return Refusal(info.value.code, info.value.message)

    def declare_refused(self, doc):
        with self._driver() as d, d.session() as s:
            with pytest.raises(neo4j.exceptions.Neo4jError) as info:
                s.run("CALL db.ontology.declare({ontology: $doc})", doc=doc).consume()
        return Refusal(info.value.code, info.value.message)

    def count(self):
        return self.run(Q_COUNT)[0]["c"]

    def show(self):
        return self.run("SHOW ONTOLOGY")

    def late_violation(self):
        with self._driver() as d, d.session() as s:
            tx = s.begin_transaction()
            tx.run("CREATE (:Person {id: 1, name: 'A'})").consume()
            tx.run("CREATE (:Person {id: 2, name: 'B'})").consume()
            with pytest.raises(neo4j.exceptions.Neo4jError) as info:
                tx.run(Q_NO_NAME).consume()
            assert info.value.code == CODE
            tx.close()

    def warn_write(self, query):
        with self._driver() as d, d.session() as s:
            meta = s.run(query).consume().metadata
        return meta.get("kglite.ontology", {}).get("warnings", [])

    def persist_and_reopen(self):
        assert _graceful_stop_bolt_server(self.proc) == 0
        self.proc = None
        self.start()

    def backup_and_open(self):
        raise NotImplementedError


@pytest.fixture(params=["python", "bolt"])
def surface(request, tmp_path):
    if request.param == "bolt":
        if os.name != "posix":
            pytest.skip("SIGINT shutdown is POSIX-only")
        if not _bolt_binary_available():
            pytest.skip(_BOLT_SKIP_REASON)
        s = BoltSurface(tmp_path)
        s.flags = ["--durability", "off", "--save-on-exit"]
        yield s
        s.stop()
    else:
        yield PythonSurface(tmp_path)


pytestmark = [pytest.mark.bolt]


def _mentions(refusal, *needles):
    for needle in needles:
        assert needle in refusal.text, (needle, refusal.text)


def _code_ok(refusal):
    assert refusal.code in ("OntologyViolation", CODE), refusal.code


def test_ac1_declared_at_start_visible_and_survives_a_restart(surface):
    surface.declare(issue_ontology())
    rows = surface.show()
    assert {r["name"] for r in rows if r.get("name")} >= {"Person", "WORKS_AT"}, rows
    surface.run(Q_VALID)
    surface.persist_and_reopen()
    again = surface.show()
    assert {r["name"] for r in again if r.get("name")} >= {"Person", "WORKS_AT"}, again
    _code_ok(surface.refused(Q_NO_NAME))


def test_ac2_a_valid_write_succeeds(surface):
    surface.declare(issue_ontology())
    surface.run(Q_VALID)
    surface.run("CREATE (:Company {id: 10, name: 'Acme'})")
    surface.run("MATCH (p:Person {id: 1}), (c:Company {id: 10}) CREATE (p)-[:WORKS_AT]->(c)")
    assert surface.run("MATCH (:Person)-[r:WORKS_AT]->(:Company) RETURN count(r) AS c") == [{"c": 1}]


def test_ac3_a_missing_required_property_fails_and_persists_nothing(surface):
    surface.declare(issue_ontology())
    before = surface.count()
    refusal = surface.refused(Q_NO_NAME)
    _code_ok(refusal)
    _mentions(refusal, "required_propert", "Person", "name")
    assert surface.count() == before


def test_ac4_a_wrong_property_type_fails(surface):
    surface.declare(issue_ontology())
    before = surface.count()
    refusal = surface.refused(Q_NAME_5)
    _code_ok(refusal)
    _mentions(refusal, "property_type", "Person", "name")
    assert surface.count() == before


def test_ac5_domain_and_range_are_enforced(surface):
    surface.declare(issue_ontology())
    before = surface.count()
    refusal = surface.refused(Q_REVERSED)
    _code_ok(refusal)
    _mentions(refusal, "domain", "WORKS_AT")
    assert surface.count() == before


def test_ac6_a_late_violation_rolls_back_the_whole_transaction(surface):
    surface.declare(issue_ontology())
    surface.late_violation()
    assert surface.count() == 0


def test_ac7_the_error_is_specific(surface):
    surface.declare(issue_ontology())
    for query, key in ((Q_NO_NAME, MISSING_NAME), (Q_NAME_5, WRONG_TYPE), (Q_REVERSED, REVERSED_EDGE)):
        refusal = surface.refused(query)
        _code_ok(refusal)
        rule, _entity, entity_type, prop = key
        _mentions(refusal, rule, entity_type)
        if prop:
            _mentions(refusal, prop)


def test_ac8_warn_accepts_and_reports(surface):
    surface.declare(issue_ontology("warn"))
    before = surface.count()
    warnings = surface.warn_write(Q_NO_NAME)
    assert surface.count() == before + 1
    assert any("name" in w and "Person" in w for w in warnings), warnings
    assert surface.warn_write(Q_VALID) == []


def test_ac9_declaring_over_violating_data_is_refused_with_the_report(surface):
    if surface.name == "bolt":
        surface.start()
    surface.run("CREATE (:Person {id: 1})")
    refusal = surface.declare_refused(issue_ontology())
    _code_ok(refusal)
    _mentions(refusal, "Person", "name", "1/1")
    if refusal.key is not None:
        assert refusal.key == MISSING_NAME
        assert refusal.report and refusal.report[0]["count"] == 1
    # Nothing was installed: the same write is still accepted.
    assert surface.show() == []
    surface.run("CREATE (:Person {id: 2})")


def test_ac9_the_same_declaration_at_warn_installs_and_reports(surface):
    if surface.name == "bolt":
        surface.start()
    surface.run("CREATE (:Person {id: 1})")
    surface.declare(issue_ontology("warn"))
    assert surface.show()


def test_ac10_python_attributes_carry_the_shared_tuple(tmp_path):
    s = PythonSurface(tmp_path)
    s.declare(issue_ontology())
    assert s.refused(Q_NO_NAME).key == MISSING_NAME
    assert s.refused(Q_NAME_5).key == WRONG_TYPE
    assert s.refused(Q_REVERSED).key == REVERSED_EDGE
    s.persist_and_reopen()
    assert s.refused(Q_NO_NAME).key == MISSING_NAME


def test_backup_of_an_error_graph_keeps_enforcement_python(tmp_path):
    s = PythonSurface(tmp_path)
    s.declare(issue_ontology())
    s.run(Q_VALID)
    s.backup_and_open()
    assert {r["name"] for r in s.show() if r.get("name")} >= {"Person", "WORKS_AT"}
    assert s.refused(Q_NO_NAME).key == MISSING_NAME
    assert s.count() == 1


@pytest.mark.skipif(os.name != "posix", reason="SIGINT shutdown is POSIX-only")
def test_backup_of_an_error_graph_keeps_enforcement_bolt(tmp_path):
    if not _bolt_binary_available():
        pytest.skip(_BOLT_SKIP_REASON)
    s = BoltSurface(tmp_path)
    bdir = tmp_path / "backups"
    try:
        s.start(issue_ontology(), "--backup-dir", str(bdir))
        s.run(Q_VALID)
        with s._driver() as d, d.session() as sess:
            sess.run("CALL db.backup($n)", n="b.kgl").consume()
    finally:
        s.stop()
    g = kglite.open(str(bdir / "b.kgl"))
    assert {r["name"] for r in g.cypher("SHOW ONTOLOGY").to_list() if r.get("name")} >= {"Person", "WORKS_AT"}
    with pytest.raises(kglite.OntologyViolationError):
        g.cypher(Q_NO_NAME)
    assert g.cypher(Q_COUNT).to_list() == [{"c": 1}]


def test_bolt_declare_refusal_carries_the_typed_code_and_report(tmp_path):
    """The Cypher procedure path is typed like the write gates (was an untyped
    ArgumentError string)."""
    if os.name != "posix" or not _bolt_binary_available():
        pytest.skip(_BOLT_SKIP_REASON)
    s = BoltSurface(tmp_path)
    try:
        s.start()
        s.run("CREATE (:Person {id: 1})")
        refusal = s.declare_refused(issue_ontology())
        assert refusal.code == CODE, (refusal.code, refusal.text)
        assert "Person.required_properties [name]" in refusal.text
    finally:
        s.stop()


@pytest.mark.skipif(os.name != "posix", reason="SIGINT shutdown is POSIX-only")
def test_bolt_warn_level_logs_a_line(tmp_path):
    if not _bolt_binary_available():
        pytest.skip(_BOLT_SKIP_REASON)
    s = BoltSurface(tmp_path)
    s.start(issue_ontology("warn"))
    s.run(Q_NO_NAME)
    proc = s.proc
    assert _graceful_stop_bolt_server(proc) == 0
    s.proc = None
    log = proc.stderr.read().decode("utf-8", errors="replace")
    assert "ontology" in log and "name" in log, log


def test_python_declare_refusal_through_the_procedure_is_typed_with_the_report(tmp_path):
    s = PythonSurface(tmp_path)
    s.run("CREATE (:Person {id: 1})")
    with pytest.raises(kglite.OntologyViolationError) as info:
        s.g.cypher("CALL db.ontology.declare({ontology: $doc})", params={"doc": issue_ontology()})
    e = info.value
    assert (e.rule, e.entity, e.entity_type, e.property) == MISSING_NAME
    assert e.report and e.report[0]["count"] == 1
