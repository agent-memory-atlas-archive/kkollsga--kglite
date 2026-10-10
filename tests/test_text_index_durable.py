"""Text indexes on a durable graph are recorded in the write-ahead log.

A child builds (or drops) a node and a relationship text index and dies with
``os._exit`` before any checkpoint, so only WAL replay can bring the index
back. The reopened graph must answer the same ranked queries.
"""

import os
import subprocess
import sys
import textwrap

import pytest

import kglite

SEED = """
g.cypher(
    "CREATE (a:Doc {id: 1, body: 'the quick brown fox'}), (b:Doc {id: 2, body: 'a lazy dog sleeps'}), "
    "(a)-[:CLAIMS {k: 1, text: 'quick fox claim'}]->(b), (b)-[:CLAIMS {k: 2, text: 'lazy dog claim'}]->(a)"
)
"""

NODE_Q = "MATCH (n:Doc) WHERE text_bm25(n, 'body', 'fox') > 0 RETURN n.id AS id ORDER BY id"
EDGE_Q = "MATCH ()-[r:CLAIMS]->() WHERE text_bm25(r, 'text', 'fox') > 0 RETURN r.k AS k ORDER BY k"


def _child(tmp_path, body: str) -> None:
    script = textwrap.dedent(
        f"""
        import kglite, os
        path = {str(tmp_path / "app.kgl")!r}
        g = kglite.open(path, durable=True)
        {textwrap.indent(textwrap.dedent(body), "        ").strip()}
        os._exit(0)
        """
    )
    subprocess.run([sys.executable, "-c", script], check=True, env=dict(os.environ))


def test_text_indexes_built_after_the_last_checkpoint_survive_a_crash(tmp_path):
    _child(
        tmp_path,
        SEED
        + """
g.build_text_index("Doc", "body", auto_refresh_limit=7)
g.cypher("CALL db.relationship_text_index.build({type: 'CLAIMS', text_column: 'text'})")
assert g.cypher("MATCH (n:Doc) WHERE text_bm25(n, 'body', 'fox') > 0 RETURN n.id AS id").to_list()
""",
    )
    assert not (tmp_path / "app.kgl").exists()
    wal = (tmp_path / "app.kgl-wal").read_bytes()
    assert wal[:5] == b"KWAL\x0b", "a log carrying text-index ops is WAL format 11"

    g = kglite.open(str(tmp_path / "app.kgl"), durable=True)
    assert g.has_text_index("Doc", "body")
    assert [r["id"] for r in g.cypher(NODE_Q)] == [1]
    assert [r["k"] for r in g.cypher(EDGE_Q)] == [1]
    listed = g.cypher("CALL db.node_text_index.list({type: 'Doc'})").to_list()
    assert listed[0]["auto_refresh_limit"] == 7


def test_a_drop_after_a_checkpoint_survives_a_crash(tmp_path):
    _child(
        tmp_path,
        SEED
        + """
g.build_text_index("Doc", "body")
g.cypher("CALL db.relationship_text_index.build({type: 'CLAIMS', text_column: 'text'})")
g.save(path)
assert g.drop_text_index("Doc", "body")
g.cypher("CALL db.relationship_text_index.drop({type: 'CLAIMS', text_column: 'text'})")
""",
    )
    g = kglite.open(str(tmp_path / "app.kgl"), durable=True)
    assert not g.has_text_index("Doc", "body")
    with pytest.raises(Exception, match="text index|text_index|No text"):
        g.cypher(NODE_Q).to_list()
    with pytest.raises(Exception, match="text index|text_index|No text"):
        g.cypher(EDGE_Q).to_list()


def test_the_checkpointed_index_is_the_baseline_a_later_build_replays_over(tmp_path):
    _child(
        tmp_path,
        SEED
        + """
g.build_text_index("Doc", "body")
g.save(path)
g.cypher("CREATE (:Doc {id: 3, body: 'another fox appears'})")
g.build_text_index("Doc", "body")
""",
    )
    g = kglite.open(str(tmp_path / "app.kgl"), durable=True)
    assert [r["id"] for r in g.cypher(NODE_Q)] == [1, 3]
