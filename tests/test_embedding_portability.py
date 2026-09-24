"""Relationship embedding stores travel with export/import and copy.

`export_embeddings`, `import_embeddings` and `copy_embeddings_from` used to
walk node stores only and drop relationship stores without a word. A
relationship is addressed by (source type, source id, target type, target id,
relationship type); a parallel group (two or more relationships of the type
between the same endpoints) needs a caller-named key property, and a group
that cannot be told apart is refused by name rather than guessed at.
"""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import sys
import textwrap

import pytest

import kglite
from kglite import KnowledgeGraph

KEYS = {"CLAIMS": "uid"}
PROBES = ([1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.5])


class TinyEmbedder:
    dimension = 4
    model_id = "test/tiny"

    def load(self) -> None:
        pass

    def unload(self) -> None:
        pass

    def embed(self, texts: list[str]) -> list[list[float]]:
        out = []
        for text in texts:
            buckets = [0.0] * self.dimension
            for position, byte in enumerate(text.encode()):
                buckets[position % self.dimension] += float(byte) * (position + 1)
            out.append(buckets)
        return out


def _topology(graph: KnowledgeGraph, *, swapped: bool = False, uids: tuple[str, str] = ("p1", "p2")) -> KnowledgeGraph:
    """Docs 1..3; singleton CLAIMS 1->2 and a parallel pair 1->3. `swapped`
    creates the pair in the other order, so slot order disagrees with the
    source graph's."""
    graph.cypher("CREATE (:Doc {id: 1, title: 'a'}), (:Doc {id: 2, title: 'b'}), (:Doc {id: 3, title: 'c'})")
    graph.cypher("MATCH (a:Doc {id: 1}), (b:Doc {id: 2}) CREATE (a)-[:CLAIMS {uid: 's', text: 'single claim'}]->(b)")
    pair = [(uids[0], "first parallel"), (uids[1], "second parallel")]
    for uid, text in reversed(pair) if swapped else pair:
        graph.cypher(
            "MATCH (a:Doc {id: 1}), (c:Doc {id: 3}) CREATE (a)-[:CLAIMS {uid: $uid, text: $text}]->(c)",
            params={"uid": uid, "text": text},
        )
    return graph


def _embed(graph: KnowledgeGraph) -> KnowledgeGraph:
    graph.set_embedder(TinyEmbedder())
    graph.cypher(
        "MATCH ()-[r:CLAIMS]->() WITH collect(r) AS rs "
        "CALL db.edge_embeddings.embed({type:'CLAIMS', text_property:'text', relationships:rs, mode:'all'}) "
        "YIELD embedded RETURN embedded"
    )
    return graph


def _source() -> KnowledgeGraph:
    return _embed(_topology(KnowledgeGraph()))


def _scores(graph: KnowledgeGraph) -> dict[str, list[float]]:
    """Per-uid scores against fixed probes: equal scores mean the same vector
    sits on the same member."""
    out: dict[str, list[float]] = {}
    for probe in PROBES:
        rows = graph.cypher(
            "MATCH ()-[r:CLAIMS]->() RETURN r.uid AS uid, vector_score(r, 'text_emb', $q) AS s",
            params={"q": probe},
        ).to_list()
        for row in rows:
            out.setdefault(row["uid"], []).append(row["s"])
    return out


def _listed(graph: KnowledgeGraph) -> list[dict]:
    return graph.cypher(
        "CALL db.edge_embeddings.list({type:'CLAIMS', text_property:'text'}) "
        "YIELD count, dimension, model RETURN count, dimension, model"
    ).to_list()


def _graph_in_mode(mode: str, tmp_path: Path) -> KnowledgeGraph:
    if mode == "disk":
        return kglite.open(str(tmp_path / "disk-target"), storage="disk")
    return KnowledgeGraph(storage=mode)


@pytest.mark.parametrize("mode", ["memory", "mapped", "disk"])
def test_export_import_round_trip_lands_each_vector_on_its_twin(mode: str, tmp_path: Path) -> None:
    source = _source()
    path = tmp_path / "claims.kgle"
    report = source.export_embeddings(str(path), relationship_keys=KEYS)
    assert report == {"stores": 0, "embeddings": 0, "relationship_stores": 1, "relationship_embeddings": 3}
    assert path.read_bytes()[4:8] == (4).to_bytes(4, "little")

    target = _topology(_graph_in_mode(mode, tmp_path), swapped=True)
    # The file records the key, so the import needs none.
    imported = target.import_embeddings(str(path))
    assert imported["relationship_stores"] == 1
    assert imported["relationship_imported"] == 3
    assert imported["relationship_skipped"] == 0
    assert _scores(target) == pytest.approx(_scores(source))
    assert _listed(target) == [{"count": 3, "dimension": 4, "model": "test/tiny"}]

    # Source-text hashes travelled too: nothing changed, so nothing re-embeds.
    target.set_embedder(TinyEmbedder())
    again = target.cypher(
        "MATCH ()-[r:CLAIMS]->() WITH collect(r) AS rs "
        "CALL db.edge_embeddings.embed({type:'CLAIMS', text_property:'text', relationships:rs, mode:'changed'}) "
        "YIELD embedded RETURN embedded"
    ).to_list()
    assert again == [{"embedded": 0}], "a lost text hash would re-embed every relationship"


def test_copy_carries_relationship_stores_with_a_key() -> None:
    source = _source()
    target = _topology(KnowledgeGraph(), swapped=True)
    report = target.copy_embeddings_from(source, relationship_keys=KEYS)
    assert report["relationship_stores_copied"] == 1
    assert report["relationship_vectors_copied"] == 3
    assert report["relationship_vectors_skipped"] == 0
    assert _scores(target) == pytest.approx(_scores(source))


def test_a_parallel_group_without_a_key_is_refused_by_name(tmp_path: Path) -> None:
    source = _source()
    target = _topology(KnowledgeGraph())
    message = "2 'CLAIMS' relationships connect \\(Doc id=1\\) to \\(Doc id=3\\)"
    with pytest.raises(kglite.ArgumentError, match=message):
        target.copy_embeddings_from(source)
    assert target.list_embeddings() == []

    path = tmp_path / "refused.kgle"
    with pytest.raises(kglite.ArgumentError, match=message):
        source.export_embeddings(str(path))
    assert not path.exists()


def test_an_ambiguous_target_group_refuses_the_whole_import(tmp_path: Path) -> None:
    source = _source()
    source.set_embeddings("Doc", "title", {1: [1.0, 0.0], 2: [0.0, 1.0]})
    path = tmp_path / "claims.kgle"
    source.export_embeddings(str(path), relationship_keys=KEYS)

    target = _topology(KnowledgeGraph(), uids=("p1", "p1"))
    with pytest.raises(kglite.ArgumentError, match="'uid' repeats the value \"p1\" within the group"):
        target.import_embeddings(str(path))
    assert target.list_embeddings() == [], "neither the node nor the relationship store may land"


def test_missing_relationships_count_as_skipped(tmp_path: Path) -> None:
    path = tmp_path / "claims.kgle"
    _source().export_embeddings(str(path), relationship_keys=KEYS)

    partial = KnowledgeGraph()
    partial.cypher("CREATE (:Doc {id: 1, title: 'a'}), (:Doc {id: 2, title: 'b'})")
    partial.cypher("MATCH (a:Doc {id: 1}), (b:Doc {id: 2}) CREATE (a)-[:CLAIMS {uid: 's', text: 'single claim'}]->(b)")
    imported = partial.import_embeddings(str(path))
    assert (imported["relationship_imported"], imported["relationship_skipped"]) == (1, 2)

    unrelated = KnowledgeGraph()
    unrelated.cypher("CREATE (:Doc {id: 99, title: 'z'})")
    with pytest.warns(UserWarning, match="imported 0 relationship embeddings, skipped 3"):
        report = unrelated.import_embeddings(str(path))
    assert report["relationship_stores"] == 0
    assert report["relationship_dropped_stores"] == 1


def test_a_node_only_export_is_unchanged(tmp_path: Path) -> None:
    graph = KnowledgeGraph()
    graph.cypher("CREATE (:Doc {id: 1, title: 'a'}), (:Doc {id: 2, title: 'b'})")
    graph.set_embeddings("Doc", "title", {1: [1.0, 0.0], 2: [0.0, 1.0]})
    path = tmp_path / "nodes.kgle"

    report = graph.export_embeddings(str(path))
    assert report == {"stores": 1, "embeddings": 2, "relationship_stores": 0, "relationship_embeddings": 0}
    assert path.read_bytes()[:9] == b"KGLE\x03\x00\x00\x00\x02"

    target = KnowledgeGraph()
    target.cypher("CREATE (:Doc {id: 1, title: 'a'}), (:Doc {id: 2, title: 'b'})")
    imported = target.import_embeddings(str(path))
    assert {key: imported[key] for key in ("stores", "imported", "skipped", "dropped_stores")} == {
        "stores": 1,
        "imported": 2,
        "skipped": 0,
        "dropped_stores": 0,
    }
    copied = KnowledgeGraph()
    copied.cypher("CREATE (:Doc {id: 1, title: 'a'})")
    report = copied.copy_embeddings_from(graph)
    assert {key: report[key] for key in ("stores_copied", "vectors_copied", "vectors_skipped")} == {
        "stores_copied": 1,
        "vectors_copied": 1,
        "vectors_skipped": 1,
    }


def test_a_durable_import_survives_a_killed_process(tmp_path: Path) -> None:
    kgle = tmp_path / "claims.kgle"
    source = _source()
    source.export_embeddings(str(kgle), relationship_keys=KEYS)
    expected = _scores(source)

    path = tmp_path / "durable.kgl"
    script = textwrap.dedent(
        f"""
        import kglite, os
        g = kglite.open({str(path)!r}, durable='normal')
        g.cypher("CREATE (:Doc {{id: 1, title: 'a'}}), (:Doc {{id: 2, title: 'b'}}), (:Doc {{id: 3, title: 'c'}})")
        link = "MATCH (a:Doc {{id: 1}}), (b:Doc {{id: $to}}) CREATE (a)-[:CLAIMS {{uid: $uid, text: $text}}]->(b)"
        for to, uid, text in [(2, 's', 'single claim'), (3, 'p2', 'second parallel'), (3, 'p1', 'first parallel')]:
            g.cypher(link, params={{"to": to, "uid": uid, "text": text}})
        g.save()
        report = g.import_embeddings({str(kgle)!r})
        assert report["relationship_imported"] == 3, report
        os._exit(0)
        """
    )
    done = subprocess.run([sys.executable, "-c", script], capture_output=True, text=True, timeout=120)
    assert done.returncode == 0, done.stderr

    reopened = kglite.open(str(path), durable="normal")
    assert _scores(reopened) == pytest.approx(expected)
    assert _listed(reopened) == [{"count": 3, "dimension": 4, "model": "test/tiny"}]


REFERENCE_PYTHON = os.environ.get("KGLITE_REFERENCE_PYTHON")


@pytest.mark.skipif(
    not REFERENCE_PYTHON or not Path(REFERENCE_PYTHON).exists(),
    reason="set KGLITE_REFERENCE_PYTHON to a python with the published kglite 0.17.12 wheel installed",
)
def test_the_published_0_17_12_wheel_refuses_a_v4_file_by_version(tmp_path: Path) -> None:
    path = tmp_path / "claims.kgle"
    _source().export_embeddings(str(path), relationship_keys=KEYS)
    probe = textwrap.dedent(
        f"""
        import kglite
        assert kglite.__version__ == "0.17.12", kglite.__version__
        g = kglite.KnowledgeGraph()
        try:
            g.import_embeddings({str(path)!r})
        except OSError as error:
            print(error)
        """
    )
    # Run outside the repository so the local package cannot shadow the wheel.
    done = subprocess.run([REFERENCE_PYTHON, "-c", probe], capture_output=True, text=True, cwd=tmp_path, timeout=120)
    assert done.returncode == 0, done.stderr
    assert done.stdout.strip() == "Embedding file version 4 is newer than supported version 3. Please upgrade kglite."
