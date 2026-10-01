"""0.19.0 read-compatibility, and the forward guards of the v7 / disk-format-2 break.

Two formats changed together in the register-scale program:

* the `.kgl` container moved to **v7** (v5 and v6 are still read);
* a disk-graph directory gained a `disk_format` field, a `columns_meta`
  *envelope* (`{"format": 2, "types": [...]}`) and `id_indices.bin` version 3.

Read-compat asserted against files this tree wrote would be circular, so the
fixtures under ``tests/fixtures/kgl_v6/`` were written by the **published 0.19.0
wheel** (``tests/fixtures/build_v6_compat_fixtures.py``) and committed with the
answers that wheel returned. A fixture that stops loading is a finding, never a
prompt to regenerate.

The other direction cannot be tested with this tree's reader — an older binary
is not this binary. What is pinned instead is *the shape of the bytes* an older
reader would meet, which is what its refusal depends on; the actual 0.19.0
refusal texts are quoted in the CHANGELOG, produced by running the published
wheel against a directory this branch wrote.

Every arm copies the fixture into ``tmp_path`` first: loading a durable
directory replays and re-checkpoints it, and opening a disk directory takes its
lock.
"""

from __future__ import annotations

import json
from pathlib import Path
import shutil
import struct

import pytest

import kglite

FIXTURES = Path(__file__).parent / "fixtures" / "kgl_v6"
V6_HEADER = b"RGF\x06\x02"
V7_HEADER = b"RGF\x07\x02"
DISK_FORMAT = 2


def _expected(name: str) -> dict:
    return json.loads((FIXTURES / f"{name}.expected.json").read_text(encoding="utf-8"))


def _queries(name: str) -> dict:
    """The queries the expectation was captured with, read from the generator so
    the two files cannot drift into asserting different things."""
    import ast

    source = (FIXTURES.parent / "build_v6_compat_fixtures.py").read_text(encoding="utf-8")
    for node in ast.parse(source).body:
        if isinstance(node, ast.Assign) and node.targets[0].id == name:  # type: ignore[attr-defined]
            return ast.literal_eval(node.value)
    raise AssertionError(f"{name} is gone from the fixture generator")


def _copy(source: Path, tmp_path: Path, name: str | None = None) -> Path:
    target = tmp_path / (name or source.name)
    if source.is_dir():
        shutil.copytree(source, target)
    else:
        shutil.copy2(source, target)
    return target


def _capture(graph, queries: dict[str, str]) -> dict:
    """The generator's own normalisation: JSON-stable, datetimes as `str`."""
    rows = {name: graph.cypher(query).to_list() for name, query in queries.items()}
    return json.loads(json.dumps(rows, default=str, sort_keys=True))


def _assert_matches(graph, queries: dict[str, str], expected: dict, label: str) -> None:
    got = _capture(graph, queries)
    for name in queries:
        assert got[name] == expected[name], (
            f"{label}: '{name}' differs from what 0.19.0 returned for this fixture. "
            "The fixture has not changed — the reader has."
        )


def _current_generation(directory: Path) -> Path:
    return directory / "generations" / (directory / "CURRENT").read_text(encoding="utf-8").strip()


def _sidecars(directory: Path, name: str) -> list[Path]:
    return sorted(_current_generation(directory).rglob(name))


# ── the fixtures are what they claim to be ───────────────────────────────────


def test_fixtures_carry_the_0_19_0_signature():
    """A guard on the *premise*: were these ever rewritten by this tree, every
    arm below would still pass and prove nothing."""
    assert (FIXTURES / "graph.kgl").read_bytes()[:5] == V6_HEADER
    assert (FIXTURES / "durable" / "app.kgl").read_bytes()[:5] == V6_HEADER
    assert (FIXTURES / "durable" / "app.kgl-wal").stat().st_size > 64

    metas = sorted((FIXTURES / "disk").rglob("disk_graph_meta.json"))
    assert len(metas) >= 2, "the disk fixture is supposed to hold more than one generation"
    for meta in metas:
        assert "disk_format" not in json.loads(meta.read_text(encoding="utf-8")), meta
    columns_meta = sorted((FIXTURES / "disk").rglob("columns_meta.json"))
    assert columns_meta, "the disk fixture has no columns_meta.json to pin the bare-array shape"
    for path in columns_meta:
        assert isinstance(json.loads(path.read_text(encoding="utf-8")), list), path
    for path in (FIXTURES / "disk").rglob("id_indices.bin"):
        assert path.read_bytes()[8:12] == struct.pack("<I", 2), path

    int_title = FIXTURES / "disk_int_title"
    for meta in int_title.rglob("disk_graph_meta.json"):
        assert "disk_format" not in json.loads(meta.read_text(encoding="utf-8")), meta
    assert not list(int_title.rglob("columns_meta.json")), "0.19.0 kept an integer-title type off the mmap path"
    assert list(int_title.rglob("columns.zst")), "the integer-title type must sit in a per-type sidecar"


# ── .kgl: v6 read-compat, v7 written ─────────────────────────────────────────


def test_this_build_writes_v7(tmp_path):
    graph = kglite.KnowledgeGraph()
    graph.cypher("CREATE (:Item {id: 1, name: 'x'})")
    path = tmp_path / "written.kgl"
    graph.save(str(path))
    assert path.read_bytes()[:5] == V7_HEADER
    assert graph.graph_info()["format_version"] == 7


def test_v6_file_loads_with_pinned_answers(tmp_path):
    path = _copy(FIXTURES / "graph.kgl", tmp_path)
    _assert_matches(kglite.load(str(path)), _queries("QUERIES"), _expected("graph"), "v6 load")


def test_v6_file_loads_mapped(tmp_path):
    path = _copy(FIXTURES / "graph.kgl", tmp_path)
    graph = kglite.open(str(path), storage="mapped")
    assert graph.graph_info()["storage_mode"] == "mapped"
    _assert_matches(graph, _queries("QUERIES"), _expected("graph"), "v6 mapped load")


def test_v6_resaves_as_v7_with_identical_answers(tmp_path):
    path = _copy(FIXTURES / "graph.kgl", tmp_path)
    graph = kglite.load(str(path))
    out = tmp_path / "migrated.kgl"
    graph.save(str(out))
    assert out.read_bytes()[:5] == V7_HEADER
    _assert_matches(kglite.load(str(out)), _queries("QUERIES"), _expected("graph"), "v6->v7 resave")


def test_v6_declared_validity_survives_the_migration(tmp_path):
    """The as-of answers depend on the declaration, which rides in the metadata."""
    path = _copy(FIXTURES / "graph.kgl", tmp_path)
    out = tmp_path / "migrated.kgl"
    kglite.load(str(path)).save(str(out))
    reloaded = kglite.load(str(out))
    counts = [
        reloaded.cypher(_queries("QUERIES")[q]).to_list()[0]["c"] for q in ("as_of_2006", "as_of_micros", "as_of_2021")
    ]
    assert counts == [2, 1, 4]


def test_v6_durable_directory_recovers_and_checkpoints_as_v7(tmp_path):
    directory = _copy(FIXTURES / "durable", tmp_path)
    graph = kglite.open(str(directory / "app.kgl"), durable=True)
    logged = graph.cypher("MATCH (e:Event {kind: 'logged'}) RETURN count(e) AS c").to_list()
    assert logged == [{"c": 5}], (
        f"replay produced {logged}: the five post-checkpoint frames are the point of this fixture"
    )
    _assert_matches(graph, _queries("DURABLE_QUERIES"), _expected("durable"), "v6 recovery")
    graph.save(str(directory / "app.kgl"))
    assert (directory / "app.kgl").read_bytes()[:5] == V7_HEADER


# ── disk directories: 0.19.0 layout read, format 2 written ───────────────────


def test_disk_directory_from_0_19_0_opens_with_pinned_answers(tmp_path):
    directory = _copy(FIXTURES / "disk", tmp_path)
    _assert_matches(kglite.load(str(directory)), _queries("QUERIES"), _expected("disk"), "0.19.0 disk load")


def test_disk_directory_from_0_19_0_resaves_into_format_2_and_reopens_identically(tmp_path):
    directory = _copy(FIXTURES / "disk", tmp_path)
    expected = _expected("disk")
    queries = _queries("QUERIES")

    graph = kglite.load(str(directory))
    graph.save()
    del graph

    meta = json.loads((_current_generation(directory) / "disk_graph_meta.json").read_text(encoding="utf-8"))
    assert meta["disk_format"] == DISK_FORMAT

    envelopes = _sidecars(directory, "columns_meta.json")
    assert envelopes, "the re-saved generation lost its mmap columns metadata"
    for path in envelopes:
        body = json.loads(path.read_text(encoding="utf-8"))
        assert isinstance(body, dict), f"{path} is still a bare array — an older reader would not be stopped"
        assert body["format"] == DISK_FORMAT
        assert {t["type_name"] for t in body["types"]} >= {"Tag", "Unit"}
    assert not _sidecars(directory, "columns_meta.bin.zst"), "the legacy binary sidecar name must not be reused"

    for path in _sidecars(directory, "id_indices.bin"):
        assert path.read_bytes()[8:12] == struct.pack("<I", 3), path

    _assert_matches(kglite.load(str(directory)), queries, expected, "0.19.0 disk -> resave -> reopen")

    # A second cycle from the format-2 directory itself.
    again = kglite.load(str(directory))
    again.save()
    del again
    _assert_matches(kglite.load(str(directory)), queries, expected, "format 2 -> resave -> reopen")


def test_int_title_directory_from_0_19_0_opens_with_pinned_answers(tmp_path):
    directory = _copy(FIXTURES / "disk_int_title", tmp_path)
    _assert_matches(
        kglite.load(str(directory)), _queries("INT_TITLE_QUERIES"), _expected("disk_int_title"), "0.19.0 int-title load"
    )


def test_int_title_type_moves_from_its_sidecar_into_a_column_file_and_reopens_identically(tmp_path):
    """0.19.0 sent a type with an integer title to a per-type zstd sidecar (a
    non-string title forced one). The first save here gives it its own column
    file with a typed integer title, leaves no sidecar, and answers the same."""
    directory = _copy(FIXTURES / "disk_int_title", tmp_path)
    expected = _expected("disk_int_title")
    queries = _queries("INT_TITLE_QUERIES")

    graph = kglite.load(str(directory))
    graph.save()
    del graph

    (envelope,) = _sidecars(directory, "columns_meta.json")
    body = json.loads(envelope.read_text(encoding="utf-8"))
    assert body["format"] == DISK_FORMAT
    (entry,) = [t for t in body["types"] if t["type_name"] == "Badge"]
    assert entry["title_offsets"]["len"] == 0 and entry["title_data"]["len"] == 5 * 8, "the title is a bare i64 region"
    column_file = envelope.parent / body["files"]["Badge"]
    assert column_file.is_file() and column_file.stat().st_size >= 5 * 8
    assert not (_current_generation(directory) / "columns").exists(), "the type is still on a sidecar"

    _assert_matches(kglite.load(str(directory)), queries, expected, "0.19.0 int-title -> resave -> reopen")

    # A write after the migration lands in the next generation and survives it.
    graph = kglite.load(str(directory))
    graph.cypher("MATCH (b:Badge {id: 3400000000003}) SET b.grade = 40")
    graph.save()
    del graph
    reopened = kglite.load(str(directory))
    assert reopened.cypher("MATCH (b:Badge {id: 3400000000003}) RETURN b.grade AS g, b.title AS t").to_list() == [
        {"g": 40, "t": 7100000000003}
    ]
    assert reopened.cypher("MATCH (b:Badge) WHERE b.title = 7100000000004 RETURN b.id AS id").to_list() == [
        {"id": 3400000000004}
    ]


def test_fresh_disk_build_writes_the_forward_guards(tmp_path):
    """A directory built by this tree, not migrated: the same three guards."""
    import pandas as pd

    directory = tmp_path / "fresh"
    graph = kglite.KnowledgeGraph(storage="disk", path=str(directory))
    graph.add_nodes(
        pd.DataFrame({"id": [1, 2, 3], "name": ["a", "b", "c"], "v": [1.5, 2.5, 3.5]}), "Thing", "id", "name"
    )
    graph.save()
    del graph

    meta = json.loads((_current_generation(directory) / "disk_graph_meta.json").read_text(encoding="utf-8"))
    assert meta["disk_format"] == DISK_FORMAT
    bodies = [json.loads(p.read_text(encoding="utf-8")) for p in _sidecars(directory, "columns_meta.json")]
    assert bodies and all(isinstance(b, dict) and b["format"] == DISK_FORMAT for b in bodies)
    for path in _sidecars(directory, "id_indices.bin"):
        assert path.read_bytes()[8:12] == struct.pack("<I", 3)


def test_a_directory_with_no_mmap_columns_is_still_stopped_by_the_id_index_version(tmp_path):
    """A type holding a ``Mixed`` column (here: an integer beside a string in one
    property) is served from a per-type sidecar. Its directory carries an envelope
    with no column types, only the ``sidecars`` map that names the sidecar's
    directory; an older reader reads ``id_indices.bin`` before the envelope, so
    the version there is what stops it first."""
    import pandas as pd

    directory = tmp_path / "sidecar_only"
    graph = kglite.KnowledgeGraph(storage="disk", path=str(directory))
    frame = pd.DataFrame({"id": [3100000000001, 3100000000002], "ident": [3100000000001, 3100000000002], "v": [1, 2]})
    graph.add_nodes(frame, "Employment", "id", "ident")
    graph.cypher("MATCH (p:Employment {id: 3100000000001}) SET p.note = 7")
    graph.cypher("MATCH (p:Employment {id: 3100000000002}) SET p.note = 'seven'")
    graph.save()
    del graph
    assert not _sidecars(directory, "columns.bin") and not list(directory.rglob("type_columns")), (
        "this arm is only meaningful while the type is served from a sidecar"
    )
    (envelope_path,) = _sidecars(directory, "columns_meta.json")
    envelope = json.loads(envelope_path.read_text(encoding="utf-8"))
    assert envelope["types"] == [] and list(envelope["sidecars"]) == ["Employment"]
    assert envelope["sidecars"]["Employment"].startswith("columns/") and ".." not in envelope["sidecars"]["Employment"]
    (path,) = _sidecars(directory, "id_indices.bin")
    assert path.read_bytes()[8:12] == struct.pack("<I", 3)
    assert kglite.load(str(directory)).cypher("MATCH (p:Employment) RETURN count(p) AS c").to_list() == [{"c": 2}]


def _resaved_directory(tmp_path: Path) -> Path:
    directory = _copy(FIXTURES / "disk", tmp_path, "future")
    graph = kglite.load(str(directory))
    graph.save()
    del graph
    return directory


def test_a_newer_disk_format_is_refused_by_name(tmp_path):
    directory = _resaved_directory(tmp_path)
    meta_path = _current_generation(directory) / "disk_graph_meta.json"
    meta = json.loads(meta_path.read_text(encoding="utf-8"))
    meta["disk_format"] = DISK_FORMAT + 1
    meta_path.write_text(json.dumps(meta), encoding="utf-8")
    with pytest.raises(kglite.FileFormatError) as excinfo:
        kglite.load(str(directory))
    message = str(excinfo.value)
    assert f"on-disk format {DISK_FORMAT + 1}" in message
    assert f"up to format {DISK_FORMAT}" in message
    assert "Please upgrade kglite" in message


def test_a_newer_columns_metadata_format_is_refused_by_name(tmp_path):
    directory = _resaved_directory(tmp_path)
    for path in _sidecars(directory, "columns_meta.json") + _sidecars(directory, "columns_meta.v2.bin.zst"):
        if path.suffix == ".json":
            body = json.loads(path.read_text(encoding="utf-8"))
            body["format"] = DISK_FORMAT + 1
            path.write_text(json.dumps(body), encoding="utf-8")
        else:
            path.unlink()  # the JSON is then the only sidecar a reader can find
    with pytest.raises(kglite.FileFormatError) as excinfo:
        kglite.load(str(directory))
    assert "Please upgrade kglite" in str(excinfo.value)
