"""A disk directory whose column metadata names a type tag this version does not know is refused.

The loader used to read an unknown tag as a string column, so the column served
another column's bytes as text. The metadata exists in two shapes (a bare array
written by 0.19.0 and earlier, and the format-2 envelope); both must refuse.
"""

from __future__ import annotations

import json
from pathlib import Path

import pandas as pd
import pytest

import kglite


def _build(path: str) -> Path:
    g = kglite.KnowledgeGraph(storage="disk", path=path)
    frame = pd.DataFrame(
        {
            "id": [1, 2, 3],
            "name": ["ann", "bob", "cy"],
            "age": [31, 42, 53],
            "salary": [1.5, 2.5, 3.5],
        }
    )
    g.add_nodes(frame, "Person", "id", "name")
    g.save(path)
    del g
    root = Path(path)
    return root / "generations" / (root / "CURRENT").read_text(encoding="utf-8").strip()


def _rewrite(generation: Path, shape: str, tag: str) -> None:
    meta = generation / "seg_000" / "columns_meta.json"
    body = json.loads(meta.read_text(encoding="utf-8"))
    types = body["types"] if isinstance(body, dict) else body
    changed = 0
    for entry in types:
        for column in entry["col_map"]:
            if column["col_type_str"] == "int64" and not changed:
                column["col_type_str"] = tag
                changed += 1
        for column in entry.get("fixed_cols", []):
            if column["col_type_str"] == "int64" and changed == 1:
                column["col_type_str"] = tag
                changed += 1
    assert changed, "no int64 column to rewrite"
    # The binary twin is preferred whenever it exists, so it must go.
    twin = generation / "seg_000" / "columns_meta.v2.bin.zst"
    twin.unlink()
    if shape == "bare_array":
        # The legacy layout: a bare array and one shared `columns.bin`.
        (generation / "seg_000" / "columns.bin").write_bytes(
            next((generation / "seg_000" / "type_columns").iterdir()).read_bytes()
        )
        meta.write_text(json.dumps(types), encoding="utf-8")
    else:
        meta.write_text(json.dumps({**body, "types": types}), encoding="utf-8")


@pytest.mark.parametrize("shape", ["bare_array", "envelope"])
@pytest.mark.parametrize("tag", ["future_tag", "int128", ""])
def test_unknown_column_type_tag_is_refused(tmp_path, shape, tag):
    path = str(tmp_path / "g")
    generation = _build(path)
    _rewrite(generation, shape, tag)
    with pytest.raises(kglite.FileFormatError, match="column type tag"):
        kglite.load(path)


def test_known_tags_in_the_envelope_still_load(tmp_path):
    shape = "envelope"
    path = str(tmp_path / "g")
    generation = _build(path)
    _rewrite(generation, shape, "int64")
    g = kglite.load(path)
    assert g.cypher("MATCH (p:Person) RETURN p.age AS a ORDER BY a").to_list() == [
        {"a": 31},
        {"a": 42},
        {"a": 53},
    ]


def test_unknown_tag_in_a_real_0_19_bare_array_is_refused(tmp_path):
    """The same refusal against metadata the published 0.19.0 wheel wrote."""
    from tests.fixtures.compat_helpers import copy_fixture

    fixtures = Path(__file__).parent / "fixtures" / "kgl_v6"
    directory = copy_fixture(fixtures / "ntriples_disk", tmp_path)
    seg = directory / "seg_000"
    body = json.loads((seg / "columns_meta.json").read_text(encoding="utf-8"))
    assert isinstance(body, list)
    changed = 0
    for entry in body:
        for column in entry["col_map"] + entry["fixed_cols"]:
            if column["col_type_str"] in ("int64", "float64", "uniqueid", "date", "bool", "timestamp"):
                column["col_type_str"] = "future_tag"
                changed += 1
    assert changed
    (seg / "columns_meta.json").write_text(json.dumps(body), encoding="utf-8")
    (seg / "columns_meta.bin.zst").unlink()
    with pytest.raises(kglite.FileFormatError, match="column type tag"):
        kglite.load(str(directory))
