"""Provisioner contracts without starting Docker, Java, or a database."""

import os
from pathlib import Path
import secrets
import sys
from types import SimpleNamespace
from unittest.mock import MagicMock

from benchmarks.competitive.graphsuite import ad_kglite, neo4j_server
import pytest


@pytest.fixture
def launch(monkeypatch, tmp_path):
    previous = {key: secrets.token_urlsafe(24) for key in neo4j_server._EnvScope._KEYS}
    for key, value in previous.items():
        monkeypatch.setenv(key, value)
    monkeypatch.setattr(neo4j_server, "_free_port", lambda: 43123)
    monkeypatch.setattr(neo4j_server.tempfile, "mkdtemp", lambda **kwargs: str(tmp_path))
    run = MagicMock(return_value=SimpleNamespace(returncode=0, stdout="fixture-container", stderr=""))
    wait = MagicMock()
    monkeypatch.setattr(neo4j_server.subprocess, "run", run)
    monkeypatch.setattr(neo4j_server, "_wait_for_bolt", wait)
    return SimpleNamespace(previous=previous, run=run, wait=wait, tmp=tmp_path)


def test_docker_credentials_are_unique_and_passed_to_probe_and_adapter(launch):
    passwords = []
    for _ in range(2):
        server = neo4j_server.DockerNeo4jServer()
        try:
            uri = server.start()
            args = launch.run.call_args.args[0]
            assert args[args.index("-p") + 1] == "127.0.0.1:43123:7687"
            auth = args[args.index("-e") + 1]
            assert auth.startswith("NEO4J_AUTH=neo4j/")
            password = auth.split("/", 1)[1]
            passwords.append(password)
            assert len(password) >= 8
            assert uri == "bolt://127.0.0.1:43123"
            assert launch.wait.call_args.args[:2] == (uri, password)
            assert os.environ["GRAPHSUITE_NEO4J_URI"] == uri
            assert os.environ["GRAPHSUITE_NEO4J_USER"] == "neo4j"
            assert os.environ["GRAPHSUITE_NEO4J_PASSWORD"] == password
        finally:
            server.stop()
        assert launch.run.call_args.args[0] == ["docker", "rm", "-f", "fixture-container"]
        assert {key: os.environ[key] for key in launch.previous} == launch.previous
    assert passwords[0] != passwords[1]


def test_failed_docker_readiness_cleans_up_without_replacing_environment(launch):
    launch.wait.side_effect = neo4j_server.ProvisionError("fixture readiness failure")
    server = neo4j_server.DockerNeo4jServer()
    with pytest.raises(neo4j_server.ProvisionError, match="fixture readiness failure"):
        server.start()
    assert launch.run.call_args.args[0] == ["docker", "rm", "-f", "fixture-container"]
    assert server._container_id is None
    assert {key: os.environ[key] for key in launch.previous} == launch.previous


@pytest.mark.parametrize("legacy_admin", [False, True])
def test_native_launch_binds_loopback_and_uses_generated_credentials(launch, monkeypatch, legacy_admin):
    monkeypatch.setattr(neo4j_server.LocalNeo4jServer, "_launcher", lambda self: "/fixture/neo4j")
    monkeypatch.setattr(neo4j_server.LocalNeo4jServer, "_admin", lambda self, launcher: "/fixture/neo4j-admin")
    launch.run.side_effect = [SimpleNamespace(returncode=int(legacy_admin)), SimpleNamespace(returncode=0)]
    proc = MagicMock()
    proc.poll.return_value = 0  # no OS process exists to kill
    popen = MagicMock(return_value=proc)
    monkeypatch.setattr(neo4j_server.subprocess, "Popen", popen)
    server = neo4j_server.LocalNeo4jServer()
    try:
        uri = server.start()
        conf_path = Path(popen.call_args.kwargs["env"]["NEO4J_CONF"]) / "neo4j.conf"
        conf = conf_path.read_text(encoding="utf-8")
        assert "server.bolt.listen_address=127.0.0.1:43123\n" in conf
        assert "server.http.enabled=false\n" in conf
        assert "server.https.enabled=false\n" in conf
        password = os.environ["GRAPHSUITE_NEO4J_PASSWORD"]
        assert len(password) >= 8
        assert password != neo4j_server.LocalNeo4jServer()._password
        assert launch.run.call_count == (2 if legacy_admin else 1)
        for call in launch.run.call_args_list:
            assert call.args[0][-1] == password
        assert launch.wait.call_args.args[:2] == (uri, password)
        assert launch.wait.call_args.kwargs["watch"] is proc
    finally:
        server.stop()
    assert not launch.tmp.exists()
    assert {key: os.environ[key] for key in launch.previous} == launch.previous


def test_readiness_probe_uses_supplied_credentials_and_closes_failed_drivers(monkeypatch):
    drivers = [MagicMock(), MagicMock()]
    for driver in drivers:
        driver.__enter__.return_value = driver
    drivers[0].verify_connectivity.side_effect = RuntimeError("starting")
    factory = MagicMock(side_effect=drivers)
    monkeypatch.setitem(sys.modules, "neo4j", SimpleNamespace(GraphDatabase=SimpleNamespace(driver=factory)))
    monkeypatch.setattr(neo4j_server.time, "perf_counter", lambda: 0)
    monkeypatch.setattr(neo4j_server.time, "sleep", lambda seconds: None)
    password = secrets.token_urlsafe(24)
    neo4j_server._wait_for_bolt("bolt://127.0.0.1:43123", password, 1)
    assert factory.call_count == 2
    for call in factory.call_args_list:
        assert call.kwargs["auth"] == ("neo4j", password)
    for driver in drivers:
        driver.__exit__.assert_called_once()


def test_kglite_docker_publishes_only_to_loopback(launch, monkeypatch):
    monkeypatch.setattr(ad_kglite, "_free_port", lambda: 43123)
    monkeypatch.setattr(ad_kglite, "build_kglite_graph", lambda ds: MagicMock())
    driver = MagicMock()
    monkeypatch.setitem(
        sys.modules, "neo4j", SimpleNamespace(GraphDatabase=SimpleNamespace(driver=MagicMock(return_value=driver)))
    )
    launch.run.side_effect = lambda args, **kwargs: SimpleNamespace(
        returncode=0, stdout="true" if args[1] == "inspect" else "fixture-container", stderr=""
    )
    ds = SimpleNamespace(nodes={"Person": [{"gid": 1, "embedding": [1.0]}]}, params={"seed_persons": [1]})
    adapter = ad_kglite.KgliteBoltDocker()
    try:
        adapter.build(ds)
        create = launch.run.call_args_list[0].args[0]
        assert create[:3] == ["docker", "create", "-p"]
        assert create[3] == "127.0.0.1:43123:7687"
        driver.verify_connectivity.assert_called_once()
    finally:
        adapter.teardown()
    assert launch.run.call_args.args[0] == ["docker", "rm", "-f", "fixture-container"]
