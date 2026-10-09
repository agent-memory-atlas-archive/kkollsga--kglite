"""`--ontology FILE` on kglite-bolt-server: the operator-supplied ontology is
declared at startup, enforced on every write, and LOCKED against runtime
`CALL db.ontology.declare/clear` by any client.

Restart behaviour: a stored ontology equal to the file starts clean; a
differing one refuses to start (with a diff) unless `--ontology-replace`.
"""

import json
import os
import subprocess

import pytest

neo4j = pytest.importorskip("neo4j")

from tests.conftest import (  # noqa: E402
    _BOLT_BINARY,
    _BOLT_SKIP_REASON,
    _bolt_binary_available,
    _build_bolt_fixture_graph,
    _find_free_port,
    _graceful_stop_bolt_server,
    _spawn_bolt_server,
    _teardown_bolt_server,
)

pytestmark = [
    pytest.mark.bolt,
    pytest.mark.skipif(os.name != "posix", reason="SIGINT shutdown is POSIX-only"),
]

CITY_REQUIRED = {"classes": {"Person": {"required_properties": ["city"], "enforcement": "error"}}}
EMAIL_REQUIRED = {"classes": {"Person": {"required_properties": ["email"], "enforcement": "error"}}}
CITY_WARN = {"classes": {"Person": {"required_properties": ["city"], "enforcement": "warn"}}}


@pytest.fixture(autouse=True)
def _require_binary():
    if not _bolt_binary_available():
        pytest.skip(_BOLT_SKIP_REASON)


def _driver(url):
    return neo4j.GraphDatabase.driver(url, auth=("neo4j", "password"))


def _write_ontology(path, doc):
    path.write_text(json.dumps(doc), encoding="utf-8")
    return str(path)


def _start_expecting_refusal(graph, *flags):
    """Run the server in the foreground and return (returncode, stderr); a
    startup refusal exits before binding, so no listener is ever awaited."""
    cmd = [str(_BOLT_BINARY), "--graph", str(graph), "--bind", "127.0.0.1", "--port", str(_find_free_port()), *flags]
    try:
        done = subprocess.run(cmd, capture_output=True, timeout=30)
    except subprocess.TimeoutExpired as e:  # it started: the refusal did not happen
        pytest.fail(f"server started instead of refusing: {e.stderr}")
    return done.returncode, done.stderr.decode("utf-8", errors="replace")


def _run_error(url, query):
    with _driver(url) as driver, driver.session() as session:
        with pytest.raises(neo4j.exceptions.Neo4jError) as info:
            session.run(query).consume()
    return info.value


def _setup(tmp_path):
    graph = tmp_path / "g.kgl"
    _build_bolt_fixture_graph(graph)
    return graph


def test_ontology_is_enforced_and_locked_against_runtime_changes(tmp_path):
    graph = _setup(tmp_path)
    ont = _write_ontology(tmp_path / "o.json", CITY_REQUIRED)
    proc, url = _spawn_bolt_server(graph, extra_args=["--ontology", ont, "--durability", "off"])
    try:
        err = _run_error(url, "CREATE (:Person {id: 50, title: 'NoCity'})")
        assert err.code == "Neo.ClientError.Schema.ConstraintValidationFailed", err.code
        # The status is shared with ConstraintViolation, so the refusal is told
        # apart by the documented structured message prefix.
        assert err.message.startswith(
            '[kglite.OntologyViolation rule=required_property entity=node type="Person" property="city"] '
        ), err.message

        redeclare = _run_error(
            url, "CALL db.ontology.declare({classes: {Person: {required_properties: ['title'], enforcement: 'error'}}})"
        )
        assert "--ontology" in redeclare.message, redeclare.message
        clear = _run_error(url, "CALL db.ontology.clear()")
        assert "--ontology" in clear.message, clear.message

        with _driver(url) as driver, driver.session() as session:
            shown = session.run("CALL db.ontology.show() YIELD ontology, locked RETURN ontology, locked").single()
            assert shown["locked"] is True
            assert "Person" in shown["ontology"]["classes"]

        # Still enforced after the refused attempts.
        err = _run_error(url, "CREATE (:Person {id: 51, title: 'StillNoCity'})")
        assert err.code == "Neo.ClientError.Schema.ConstraintValidationFailed", err.code
    finally:
        _teardown_bolt_server(proc)


def test_without_the_flag_a_client_may_declare_and_clear(tmp_path):
    graph = _setup(tmp_path)
    proc, url = _spawn_bolt_server(graph, extra_args=["--durability", "off"])
    try:
        with _driver(url) as driver, driver.session() as session:
            row = session.run(
                "CALL db.ontology.declare({ontology: $doc}) YIELD declared, warnings RETURN declared, warnings",
                doc=CITY_REQUIRED,
            ).single()
            assert row["declared"] is True
            shown = session.run("CALL db.ontology.show() YIELD ontology, locked RETURN ontology, locked").single()
            assert shown["locked"] is False
            assert "Person" in shown["ontology"]["classes"]
            session.run("CALL db.ontology.clear()").consume()
            session.run("CREATE (:Person {id: 60, title: 'Free'})").consume()
    finally:
        _teardown_bolt_server(proc)


@pytest.mark.parametrize("durability", ["off", "normal"])
def test_restart_with_the_same_file_starts_and_a_differing_one_is_refused(tmp_path, durability):
    graph = _setup(tmp_path)
    ont = _write_ontology(tmp_path / "o.json", CITY_REQUIRED)
    other = _write_ontology(
        tmp_path / "other.json", {"classes": {"Person": {"required_properties": ["title"], "enforcement": "error"}}}
    )
    flags = ["--durability", durability, "--save-on-exit"]

    proc, _ = _spawn_bolt_server(graph, extra_args=["--ontology", ont, *flags])
    assert _graceful_stop_bolt_server(proc) == 0

    # Same file: starts, still enforcing.
    proc, url = _spawn_bolt_server(graph, extra_args=["--ontology", ont, *flags])
    try:
        err = _run_error(url, "CREATE (:Person {id: 70, title: 'NoCity'})")
        assert err.code == "Neo.ClientError.Schema.ConstraintValidationFailed"
    finally:
        assert _graceful_stop_bolt_server(proc) == 0

    # Differing file: refused with a diff, graph untouched.
    code, stderr = _start_expecting_refusal(graph, "--ontology", other, *flags)
    assert code != 0
    assert "differs" in stderr and "Person" in stderr and "--ontology-replace" in stderr, stderr

    # --ontology-replace takes the file's declaration.
    proc, url = _spawn_bolt_server(graph, extra_args=["--ontology", other, "--ontology-replace", *flags])
    try:
        with _driver(url) as driver, driver.session() as session:
            session.run("CREATE (:Person {id: 71, title: 'NoCityIsFineNow'})").consume()
        err = _run_error(url, "CREATE (:Person {id: 72})")
        assert err.code == "Neo.ClientError.Schema.ConstraintValidationFailed"
    finally:
        assert _graceful_stop_bolt_server(proc) == 0


def test_declaring_over_violating_data_refuses_to_start_with_the_report(tmp_path):
    graph = _setup(tmp_path)
    ont = _write_ontology(tmp_path / "o.json", EMAIL_REQUIRED)
    code, stderr = _start_expecting_refusal(graph, "--ontology", ont, "--durability", "off")
    assert code != 0
    assert "Person.required_properties" in stderr and "email" in stderr, stderr


def test_a_missing_or_malformed_file_refuses_to_start(tmp_path):
    graph = _setup(tmp_path)
    code, stderr = _start_expecting_refusal(graph, "--ontology", str(tmp_path / "nope.json"))
    assert code != 0 and "--ontology" in stderr, stderr
    bad = tmp_path / "bad.json"
    bad.write_text("{not json", encoding="utf-8")
    code, stderr = _start_expecting_refusal(graph, "--ontology", str(bad))
    assert code != 0 and "--ontology" in stderr, stderr


def test_ontology_replace_requires_ontology(tmp_path):
    graph = _setup(tmp_path)
    code, stderr = _start_expecting_refusal(graph, "--ontology-replace")
    assert code != 0 and "--ontology" in stderr, stderr


def test_warn_level_findings_ride_the_kglite_ontology_summary_key(tmp_path):
    graph = _setup(tmp_path)
    ont = _write_ontology(tmp_path / "o.json", CITY_WARN)
    proc, url = _spawn_bolt_server(graph, extra_args=["--ontology", ont, "--durability", "off"])
    try:
        with _driver(url) as driver, driver.session() as session:
            summary = session.run("CREATE (:Person {id: 80, title: 'NoCity'})").consume()
            meta = summary.metadata
            assert "kglite.ontology" in meta, meta
            warnings = meta["kglite.ontology"]["warnings"]
            assert len(warnings) == 1 and "city" in warnings[0], warnings
            # A conforming write carries no key.
            clean = session.run("CREATE (:Person {id: 81, title: 'Ok', city: 'Oslo'})").consume()
            assert "kglite.ontology" not in clean.metadata
    finally:
        _teardown_bolt_server(proc)
