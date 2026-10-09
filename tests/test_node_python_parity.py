"""Cross-binding file-format parity: a graph written by the Node binding must
read identically through Python and through ``kglite-bolt-server``, and a graph
written by Python must read identically through Node. One engine file format,
one write-ahead-log format, four consumers.

Both directions are proven, each for a *checkpointed* graph and a *WAL-ahead*
one (the writer exits without a final checkpoint, so its last batch exists only
in the ``<path>-wal`` sidecar and the reader has to recover it).

Vacuity guard: with ``KGLITE_NODE_PARITY=1`` (the CI leg) a missing ``node``,
unbuilt addon, absent ``neo4j`` driver or unbuilt bolt server is a failure,
never a skip. Without it a plain local ``pytest`` run skips with the command
that builds what is missing.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

import pytest

import kglite
from tests.conftest import _BOLT_BINARY, _bolt_binary_available, _spawn_bolt_server, _teardown_bolt_server

REPO_ROOT = Path(__file__).resolve().parent.parent
NODE_DIR = REPO_ROOT / "crates" / "kglite-node"
HARNESS = NODE_DIR / "__test__" / "parity_harness.mjs"
JSON_PREFIX = "PARITY_JSON:"

BATCH_ONE = {
    "people": [
        {
            "id": 1,
            "name": "Ada",
            "score": 1.5,
            "active": True,
            "born": "1815-12-10",
            "big": {"$bigint": str(2**53 + 1)},
        },
        {
            "id": 2,
            "name": "Alan",
            "score": 2.25,
            "active": False,
            "born": "1912-06-23",
            "big": {"$bigint": str(-(2**60))},
        },
    ],
    "knows": [{"from": 1, "to": 2, "since": 1843}],
}
# The batch that is WAL-only in the "ahead" scenarios.
BATCH_TWO = {
    "people": [
        {"id": 3, "name": "Grace", "score": -0.5, "active": True, "born": "1906-12-09", "big": 0},
        {"id": 4, "name": "Édsger", "score": 3.5, "active": True, "born": "1930-05-11", "big": 7},
    ],
    "knows": [{"from": 2, "to": 3, "since": 1950}, {"from": 3, "to": 4, "since": 1968}],
}

PEOPLE_Q = (
    "MATCH (p:Person) RETURN p.id AS id, p.name AS name, p.score AS score, p.active AS active, "
    "toString(p.born) AS born, p.big AS big ORDER BY id"
)
KNOWS_Q = "MATCH (a:Person)-[r:KNOWS]->(b:Person) RETURN a.id AS from, b.id AS to, r.since AS since ORDER BY from"


def _hard_required() -> bool:
    return os.environ.get("KGLITE_NODE_PARITY") == "1"


def _unavailable(reason: str) -> None:
    message = f"node parity prerequisite missing: {reason}"
    if _hard_required():
        pytest.fail(message)
    pytest.skip(message)


@pytest.fixture(scope="module")
def node() -> str:
    exe = shutil.which("node")
    if exe is None:
        _unavailable("no `node` on PATH")
    probe = subprocess.run(
        [exe, "-e", "process.stdout.write(require(process.argv[1]).version())", str(NODE_DIR / "index.js")],
        capture_output=True,
        text=True,
        timeout=60,
    )
    if probe.returncode != 0:
        _unavailable(
            "the kglite-node addon does not load (build it with `make node-build`): " + probe.stderr.strip()[-300:]
        )
    return exe


def _run_node(node: str, *args: str) -> str:
    done = subprocess.run([node, str(HARNESS), *args], capture_output=True, text=True, timeout=120, encoding="utf-8")
    assert done.returncode == 0, f"node harness {args[:2]} failed:\n{done.stdout}\n{done.stderr}"
    return done.stdout


def _node_write(node: str, path: Path, durability: str, batches: list[dict], ending: str) -> None:
    _run_node(node, "write", str(path), durability, json.dumps({"batches": batches, "ending": ending}))


def _node_read(node: str, path: Path, how: str) -> dict:
    out = _run_node(node, "read", str(path), how)
    lines = [line for line in out.splitlines() if line.startswith(JSON_PREFIX)]
    assert len(lines) == 1, out
    return _decode(json.loads(lines[0][len(JSON_PREFIX) :]))


def _decode(value):
    """Revive the harness' `{"$bigint": "..."}` integers."""
    if isinstance(value, dict):
        if set(value) == {"$bigint"}:
            return int(value["$bigint"])
        return {k: _decode(v) for k, v in value.items()}
    if isinstance(value, list):
        return [_decode(v) for v in value]
    return value


def _expected(batches: list[dict]) -> dict:
    return {
        "people": [_decode(p) for b in batches for p in b["people"]],
        "knows": [k for b in batches for k in b["knows"]],
    }


def _python_read(graph) -> dict:
    return {"people": [dict(r) for r in graph.cypher(PEOPLE_Q)], "knows": [dict(r) for r in graph.cypher(KNOWS_Q)]}


def _normalise(snapshot: dict) -> dict:
    """Both sides' rows, with the dataset's own key order and float/int kept exact."""
    people = [{k: p[k] for k in ("id", "name", "score", "active", "born", "big")} for p in snapshot["people"]]
    return {"people": people, "knows": [{k: r[k] for k in ("from", "to", "since")} for r in snapshot["knows"]]}


def _assert_same(actual: dict, expected: dict, context: str) -> None:
    got, want = _normalise(actual), _normalise(expected)
    assert got == want, f"{context}: {got} != {want}"
    # `==` treats 1 and 1.0 and True alike; the types are part of the contract.
    for g, w in zip(got["people"], want["people"], strict=True):
        for key in g:
            assert type(g[key]) is type(w[key]), (
                f"{context}: {key} is {type(g[key]).__name__}, want {type(w[key]).__name__}"
            )


# ── Node writes -> Python reads ─────────────────────────────────────────────


@pytest.mark.parametrize("durability", ["full", "normal", "off"])
def test_node_checkpointed_graph_loads_in_python(node, tmp_path, durability):
    path = tmp_path / "g.kgl"
    _node_write(node, path, durability, [BATCH_ONE, BATCH_TWO], "close")
    _assert_same(_python_read(kglite.load(str(path))), _expected([BATCH_ONE, BATCH_TWO]), "node->python load")


@pytest.mark.parametrize("durability", ["full", "normal"])
def test_node_wal_ahead_graph_recovers_in_python(node, tmp_path, durability):
    path = tmp_path / "g.kgl"
    _node_write(node, path, durability, [BATCH_ONE, BATCH_TWO], "abandon")
    wal = Path(f"{path}-wal")
    assert wal.is_file() and wal.stat().st_size > 0, "the writer left no write-ahead log to recover"
    with kglite.open(str(path), durable="full") as graph:
        _assert_same(_python_read(graph), _expected([BATCH_ONE, BATCH_TWO]), "node->python WAL recovery")


# ── Node writes -> bolt-server serves ───────────────────────────────────────


def _bolt_snapshot(url: str) -> dict:
    neo4j = pytest.importorskip("neo4j") if not _hard_required() else __import__("neo4j")
    with neo4j.GraphDatabase.driver(url, auth=("neo4j", "password")) as driver, driver.session() as session:
        people = [dict(r) for r in session.run(PEOPLE_Q)]
        knows = [dict(r) for r in session.run(KNOWS_Q)]
    return {"people": people, "knows": knows}


@pytest.fixture
def bolt_binary():
    if not _bolt_binary_available():
        _unavailable(f"kglite-bolt-server not built ({_BOLT_BINARY}); run `cargo build -p kglite-bolt-server`")


@pytest.mark.parametrize("ending", ["close", "abandon"])
def test_bolt_server_serves_a_node_written_graph(node, bolt_binary, tmp_path, ending):
    path = tmp_path / "g.kgl"
    _node_write(node, path, "normal", [BATCH_ONE, BATCH_TWO], ending)
    proc, url = _spawn_bolt_server(path, extra_args=["--durability", "normal"] if ending == "abandon" else [])
    try:
        _assert_same(_bolt_snapshot(url), _expected([BATCH_ONE, BATCH_TWO]), f"node({ending})->bolt")
    finally:
        _teardown_bolt_server(proc)


# ── Python writes -> Node reads ─────────────────────────────────────────────

_PY_WRITER = """
import json, os, sys
import kglite
path, plan = sys.argv[1], json.loads(sys.argv[2])
batches, ending = plan["batches"], plan["ending"]
create_people = {people!r}
create_knows = {knows!r}
def dec(v):
    if isinstance(v, dict) and set(v) == {{"$bigint"}}:
        return int(v["$bigint"])
    if isinstance(v, dict):
        return {{k: dec(x) for k, x in v.items()}}
    if isinstance(v, list):
        return [dec(x) for x in v]
    return v
g = kglite.open(path, durable="full")
for i, batch in enumerate(batches):
    batch = dec(batch)
    g.cypher(create_people, params={{"people": batch["people"]}})
    g.cypher(create_knows, params={{"knows": batch["knows"]}})
    if i < len(batches) - 1:
        g.save()
if ending == "close":
    g.save()
    g.close()
else:
    # Leave without a final checkpoint or a clean close: the last batch is WAL-only.
    os._exit(0)
"""
CREATE_PEOPLE = (
    "UNWIND $people AS p CREATE (:Person {id: p.id, name: p.name, score: p.score, active: p.active, "
    "born: date(p.born), big: p.big})"
)
CREATE_KNOWS = (
    "UNWIND $knows AS k MATCH (a:Person {id: k.from}), (b:Person {id: k.to}) CREATE (a)-[:KNOWS {since: k.since}]->(b)"
)


def _python_write(path: Path, batches: list[dict], ending: str) -> None:
    script = _PY_WRITER.format(people=CREATE_PEOPLE, knows=CREATE_KNOWS)
    done = subprocess.run(
        [sys.executable, "-c", script, str(path), json.dumps({"batches": batches, "ending": ending})],
        capture_output=True,
        text=True,
        timeout=120,
        encoding="utf-8",
        cwd=Path(sys.executable).parent,  # not the repo root: the installed wheel, not ./kglite, must load
    )
    assert done.returncode == 0, f"python writer failed:\n{done.stdout}\n{done.stderr}"


@pytest.mark.parametrize("how", ["readonly", "writer"])
def test_python_checkpointed_graph_reads_in_node(node, tmp_path, how):
    path = tmp_path / "g.kgl"
    _python_write(path, [BATCH_ONE, BATCH_TWO], "close")
    _assert_same(_node_read(node, path, how), _expected([BATCH_ONE, BATCH_TWO]), f"python->node {how}")


def test_python_wal_ahead_graph_recovers_in_node(node, tmp_path):
    path = tmp_path / "g.kgl"
    _python_write(path, [BATCH_ONE, BATCH_TWO], "abandon")
    # A lease-less reader sees only the last checkpoint (batch one) ...
    _assert_same(_node_read(node, path, "readonly"), _expected([BATCH_ONE]), "python WAL-ahead -> node readOnly")
    # ... and the writer open replays the log and sees both.
    _assert_same(
        _node_read(node, path, "writer"), _expected([BATCH_ONE, BATCH_TWO]), "python WAL-ahead -> node recovery"
    )
