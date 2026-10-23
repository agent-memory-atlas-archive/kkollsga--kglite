"""Bounded child processes detect callback lock cycles without hanging pytest."""

import subprocess
import sys
import textwrap

import pytest

import kglite

CHILD = textwrap.dedent(
    r"""
    import json
    import sys
    import threading

    import kglite
    from tests.test_execution_service_propagation import fixture

    mode, reader_kind = sys.argv[1:]
    graph, model = fixture()
    graph.cypher("MATCH(d:Doc) SET d.marker=0")
    session = graph.session()
    version = session.version()
    entered = threading.Event()
    reader_started = threading.Event()
    reader_done = threading.Event()
    failures = []
    original_embed = model.embed
    def read_committed():
        if reader_kind == 'cypher':
            assert session.cypher('MATCH(d:Doc) RETURN d.marker AS m ORDER BY d.id').to_list() == [{'m':0}, {'m':0}]
        elif reader_kind == 'execute_read':
            assert session.execute('RETURN 1 AS value').to_list() == [{'value':1}]
        elif reader_kind == 'snapshot':
            rows = session.snapshot().cypher(
                'MATCH(d:Doc) RETURN d.marker AS m ORDER BY d.id'
            ).to_list()
            assert rows == [{'m':0}, {'m':0}]
        elif reader_kind == 'cursor':
            rows = session.cursor().cypher(
                'MATCH(d:Doc) RETURN d.marker AS m ORDER BY d.id'
            ).to_list()
            assert rows == [{'m':0}, {'m':0}]
        elif reader_kind == 'version':
            assert session.version() == version
        elif reader_kind == 'node_count':
            assert session.node_count() == 2
        elif reader_kind == 'node_types':
            assert session.node_types == ['Doc']
        else:
            assert f'nodes=2, types=1, version={version}' in repr(session)
    def callback(texts):
        print('callback_entered', flush=True)
        if mode == 'concurrent':
            entered.set()
            assert reader_started.wait(2), 'reader did not enter'
            assert reader_done.wait(2), 'reader did not finish'
        elif mode == 'reentrant_write':
            try:
                session.execute('MATCH(d:Doc) SET d.marker=99')
            except kglite.ArgumentError as error:
                assert error.code == 'InvalidArgument'
                assert 'same Session' in str(error)
            else:
                raise AssertionError('reentrant write was accepted')
        else:
            read_committed()
        return original_embed(texts)
    model.embed = callback
    def reader():
        try:
            assert entered.wait(2), 'callback did not enter'
            reader_started.set()
            print('reader_entered', flush=True)
            read_committed()
        except BaseException as error:
            failures.append(repr(error))
        finally:
            reader_done.set()
    thread = None
    if mode == 'concurrent':
        thread = threading.Thread(target=reader, daemon=True)
        thread.start()
    before = session.snapshot()
    rows = session.execute(
        "MATCH(d:Doc) SET d.marker=1,d.score=text_score(d,'body','query') "
        "RETURN d.id AS id,d.score AS score ORDER BY id"
    ).to_list()
    if thread:
        thread.join(2)
        assert not thread.is_alive()
    assert failures == [], failures
    assert rows == [{'id':1,'score':1.0}, {'id':2,'score':0.0}], rows
    assert session.cypher('MATCH(d:Doc) RETURN d.marker AS m ORDER BY d.id').to_list() == [{'m':1}, {'m':1}]
    assert before.cypher('MATCH(d:Doc) RETURN d.marker AS m ORDER BY d.id').to_list() == [{'m':0}, {'m':0}]
    assert session.version() == version + 1
    # An explained write takes the write route but must not publish its working fork.
    after = session.version()
    explained = session.execute('EXPLAIN CREATE (:NoOp)').to_list()
    assert explained[0]['operation'] == 'Create'
    assert session.version() == after
    # Successful unwinding clears the per-thread write guard.
    session.execute('MATCH(d:Doc) SET d.marker=2')
    assert session.cypher('MATCH(d:Doc) RETURN d.marker AS m ORDER BY d.id').to_list() == [{'m':2}, {'m':2}]
    print(json.dumps({'complete':True,'mode':mode,'reader':reader_kind}), flush=True)
    """
)


@pytest.mark.parametrize("mode", ["self_read", "concurrent"])
@pytest.mark.parametrize(
    "reader", ["cypher", "execute_read", "snapshot", "cursor", "version", "node_count", "node_types", "repr"]
)
def test_callback_reads_committed_session_without_lock_cycle(mode, reader):
    run_child(mode, reader)


def test_callback_reentrant_write_refuses_before_lock_and_leaves_outer_write_usable():
    run_child("reentrant_write", "cypher")


class TrackingModel:
    def __init__(self):
        self.dimension_reads = 0
        self.loads = 0
        self.embeds = 0
        self.unloads = 0

    @property
    def dimension(self):
        self.dimension_reads += 1
        return 2

    def load(self):
        self.loads += 1

    def embed(self, texts):
        self.embeds += 1
        return [[0.0, 1.0] if text == "beta" else [1.0, 0.0] for text in texts]

    def unload(self):
        self.unloads += 1

    def reset(self):
        self.dimension_reads = 0
        self.loads = 0
        self.embeds = 0
        self.unloads = 0

    def counts(self):
        return (self.dimension_reads, self.loads, self.embeds, self.unloads)


def test_callback_routing_matches_canonical_text_score_rewrite():
    graph = kglite.KnowledgeGraph()
    graph.cypher("CREATE (:Doc {id:1,body:'alpha',marker:0}),(:Note {id:2,body:'beta'})")
    model = TrackingModel()
    graph.set_embedder(model)
    graph.embed_texts("Doc", "body", show_progress=False)
    graph.embed_texts("Note", "body", show_progress=False)
    model.reset()
    session = graph.session()

    def execute(query, params=None):
        before = model.counts()
        options = {} if params is None else {"params": params}
        result = session.execute(query, **options)
        after = model.counts()
        return result, tuple(end - start for start, end in zip(before, after, strict=True))

    ordinary, callbacks = execute("MATCH(d:Doc) SET d.marker=1 RETURN d.id AS id")
    assert ordinary.scalar() == 1
    assert callbacks == (0, 0, 0, 0)

    literal, callbacks = execute("MATCH(d:Doc) SET d.marker=text_score(d,'body','query') RETURN d.marker AS score")
    assert literal.scalar() == 1.0
    assert callbacks == (0, 1, 1, 1)

    param_query = "MATCH(d:Doc) SET d.marker=text_score(d,'body',$query) RETURN d.marker AS score"
    for _ in range(2):
        repeated, callbacks = execute(param_query, {"query": "query"})
        assert repeated.scalar() == 1.0
        assert callbacks == (0, 1, 1, 1)

    for query, params in [
        ("MATCH(d:Doc) SET d.marker=text_score(d,'body',[1.0,0.0]) RETURN d.marker", None),
        (param_query, {"query": [1.0, 0.0]}),
    ]:
        _, callbacks = execute(query, params)
        assert callbacks == (0, 0, 0, 0)

    explained, callbacks = execute("EXPLAIN MATCH(d:Doc) SET d.marker=text_score(d,'body','query') RETURN d.marker")
    assert explained.to_list()[1]["operation"] == "Set"
    assert callbacks == (0, 0, 0, 0)

    before_version = session.version()
    before_callbacks = model.counts()
    with pytest.raises(kglite.CypherExecutionError, match="must be a string or a list"):
        session.execute(param_query, params={"query": 3})
    assert session.version() == before_version
    assert model.counts() == before_callbacks

    nested = (
        "CALL { MATCH(d:Doc) RETURN text_score(d,'body','query') AS score } "
        "WITH score AS doc_score MATCH(n:Note) "
        "WITH doc_score,text_score(n,'body','query') AS score "
        "CREATE (:Log {doc_score:doc_score,note_score:score}) RETURN doc_score,score"
    )
    nested_result, callbacks = execute(nested)
    assert nested_result.to_list() == [{"doc_score": 1.0, "score": 0.0}]
    assert callbacks == (0, 1, 1, 1)
    assert session.version() == before_version + 1
    assert session.node_count() == 3

    after_nested_version = session.version()
    after_nested_callbacks = model.counts()
    unsupported = (
        "CALL { MATCH(d:Doc) SET d.marker=text_score(d,'body','query') RETURN d.marker AS score } RETURN score"
    )
    with pytest.raises(kglite.CypherSyntaxError, match="write clauses.*inside a CALL"):
        session.execute(unsupported)
    assert session.version() == after_nested_version
    assert session.node_count() == 3
    assert model.counts() == after_nested_callbacks


def run_child(mode, reader):
    process = subprocess.Popen(
        [sys.executable, "-c", CHILD, mode, reader],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        stdout, stderr = process.communicate(timeout=8)
    except subprocess.TimeoutExpired:
        process.kill()
        stdout, stderr = process.communicate(timeout=2)
        pytest.fail(f"Session callback lock cycle: {mode}/{reader}\n{stdout}\n{stderr}")
    assert process.returncode == 0, (stdout, stderr)
    assert "callback_entered" in stdout
    if mode == "concurrent":
        assert "reader_entered" in stdout
    assert '"complete": true' in stdout


# --- durable graphs -----------------------------------------------------------------------------
#
# A write on a durable handle holds the handle's mutable borrow while the embedder callback runs. A
# second thread touching the same handle WAITS for the write to finish (it neither raises nor reads
# a half-applied graph); the callback itself re-entering the handle raises "accessed concurrently",
# since it would wait on a release only it can trigger. A Session taken earlier still reads the
# committed snapshot and refuses writes.
# These cases pin that contract in a bounded child so a lock cycle fails instead of hanging pytest.

DURABLE_CHILD = textwrap.dedent(
    r"""
    import json, os, sys, threading
    import kglite

    mode, path = sys.argv[1:]

    class Model:
        dimension = 2
        hook = None
        def embed(self, texts):
            if self.hook:
                self.hook()
            return [[0.0, 1.0] if t == "beta" else [1.0, 0.0] for t in texts]

    g = kglite.open(path, durable="full")
    g.cypher("CREATE (:Doc {id:1,body:'alpha',m:0}),(:Doc {id:2,body:'beta',m:0})")
    model = Model()
    g.set_embedder(model)
    g.embed_texts("Doc", "body", show_progress=False)
    session = g.session()
    seen = {}

    def in_thread(fn):
        box = {}
        def run():
            try:
                box["ok"] = fn()
            except BaseException as e:
                box["err"] = f"{type(e).__name__}: {e}"
        t = threading.Thread(target=run, daemon=True)
        t.start()
        t.join(1)
        box["hung"] = t.is_alive()
        return box

    def hook():
        if mode == "reader_thread":
            seen["r"] = in_thread(lambda: g.cypher("MATCH (d:Doc) RETURN count(d) AS c").to_list())
        elif mode == "writer_thread":
            seen["r"] = in_thread(lambda: g.cypher("CREATE (:Late {id:5})").to_list())
        elif mode == "reentrant_write":
            try:
                g.cypher("CREATE (:Re {id:6})")
                seen["r"] = {"ok": True}
            except BaseException as e:
                seen["r"] = {"err": f"{type(e).__name__}: {e}"}
        elif mode == "session_reader":
            seen["r"] = in_thread(lambda: session.cypher("MATCH (d:Doc) RETURN d.m AS m ORDER BY d.id").to_list())
        elif mode == "session_writer":
            seen["r"] = in_thread(lambda: session.execute("CREATE (:S {id:7})").to_list())

    model.hook = hook
    rows = g.cypher(
        "MATCH (d:Doc) SET d.m = 1, d.s = text_score(d, 'body', 'query') RETURN d.id AS id, d.s AS s ORDER BY id"
    ).to_list()
    print(json.dumps({"rows": rows, "seen": seen}), flush=True)
    os._exit(0)  # no save, no close: the parent reopens what the log carries
    """
)


@pytest.mark.parametrize(
    ("mode", "check"),
    [
        ("reader_thread", lambda r: r["hung"] is True and "err" not in r),
        ("writer_thread", lambda r: r["hung"] is True and "err" not in r),
        ("reentrant_write", lambda r: "accessed concurrently" in r["err"]),
        ("session_reader", lambda r: r["ok"] == [{"m": 0}, {"m": 0}]),
        ("session_writer", lambda r: "durable" in r["err"]),
    ],
)
def test_durable_write_with_a_callback_embedder_keeps_todays_concurrency_contract(mode, check, tmp_path):
    path = tmp_path / "app.kgl"
    process = subprocess.Popen(
        [sys.executable, "-c", DURABLE_CHILD, mode, str(path)],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        stdout, stderr = process.communicate(timeout=30)
    except subprocess.TimeoutExpired:
        process.kill()
        stdout, stderr = process.communicate(timeout=2)
        pytest.fail(f"durable callback lock cycle: {mode}\n{stdout}\n{stderr}")
    assert process.returncode == 0, (stdout, stderr)
    result = __import__("json").loads(stdout.strip().splitlines()[-1])
    assert result["rows"] == [{"id": 1, "s": 1.0}, {"id": 2, "s": 0.0}]
    outcome = result["seen"]["r"]
    if mode not in ("reader_thread", "writer_thread"):
        assert outcome.get("hung") is not True, outcome
    assert check(outcome), outcome

    # The outer write survived the hard exit; nothing the refused callers tried reached the log. A
    # writer that waited may have landed in the instant between the outer commit and the exit.
    reopened = kglite.open(str(path), durable="full")
    assert reopened.cypher("MATCH (d:Doc) RETURN d.m AS m ORDER BY d.id").to_list() == [{"m": 1}, {"m": 1}]
    stray = "n:Re OR n:S" if mode == "writer_thread" else "n:Late OR n:Re OR n:S"
    assert reopened.cypher(f"MATCH (n) WHERE {stray} RETURN count(n) AS c").scalar() == 0
