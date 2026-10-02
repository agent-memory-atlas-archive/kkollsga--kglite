"""0.19.0 read-compatibility for two layouts written by the published wheel.

* an ``ntriples_disk`` directory: shared root ``columns.bin``, a bare-array
  ``columns_meta.json`` with its ``.bin.zst`` twin, and no ``CURRENT`` pointer;
* a ``durable_kinds`` directory whose write-ahead log carries timestamps, dates,
  integer ids beyond 32 bits, nulls and a mixed-kind property.

Each is loaded, saved, reopened and compared to the answers 0.19.0 pinned. The
fixtures come from ``tests/fixtures/build_v6_legacy_fixtures.py``; one that stops
loading is a finding, never a prompt to regenerate.
"""

from __future__ import annotations

import json
from pathlib import Path

import kglite
from tests.fixtures.compat_helpers import copy_fixture, expected_answers, generator_queries
from tests.fixtures.disk_generation import current_generation

FIXTURES = Path(__file__).parent / "fixtures" / "kgl_v6"
GENERATOR = FIXTURES.parent / "build_v6_legacy_fixtures.py"


def _capture(graph, queries):
    rows = {name: graph.cypher(query).to_list() for name, query in queries.items()}
    return json.loads(json.dumps(rows, default=str, sort_keys=True))


def test_ntriples_built_directory_loads_resaves_and_reopens(tmp_path):
    queries = generator_queries(GENERATOR, "NTRIPLES_QUERIES")
    expected = expected_answers(FIXTURES, "ntriples_disk")
    directory = copy_fixture(FIXTURES / "ntriples_disk", tmp_path)
    assert not (directory / "CURRENT").exists()
    assert (directory / "seg_000" / "columns.bin").exists()

    graph = kglite.load(str(directory))
    assert _capture(graph, queries) == expected
    graph.save()
    del graph

    resaved = kglite.load(str(directory))
    assert _capture(resaved, queries) == expected
    generation = current_generation(directory)
    assert not list(generation.rglob("columns.bin")), "a save migrates the shared column file away"
    # A write after the migration lands and survives another save.
    resaved.cypher("MATCH (n {title: 'Two'}) SET n.note = 'kept'")
    resaved.save()
    del resaved
    final = kglite.load(str(directory))
    assert final.cypher("MATCH (n {title: 'Two'}) RETURN n.note AS v").scalar() == "kept"
    assert {k: v for k, v in _capture(final, queries).items() if k != "nodes"} == {
        k: v for k, v in expected.items() if k != "nodes"
    }


def test_durable_log_with_timestamps_dates_and_mixed_values_recovers(tmp_path):
    queries = generator_queries(GENERATOR, "DURABLE_QUERIES")
    expected = expected_answers(FIXTURES, "durable_kinds")
    directory = copy_fixture(FIXTURES / "durable_kinds", tmp_path)
    path = str(directory / "app.kgl")

    graph = kglite.open(path, durable=True)
    assert _capture(graph, queries) == expected
    graph.save(path)
    del graph

    reopened = kglite.open(path, durable=True)
    assert _capture(reopened, queries) == expected
