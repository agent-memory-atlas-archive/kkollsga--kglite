"""`create_index` and Cypher `CREATE INDEX` share one checked build.

On a disk graph a persistent index covers string columns only. Cypher refused
an index that indexed nothing over a populated type, while the Python route
reported it as created and serving with zero entries. Both now refuse. The
report also says whether the node type exists at all, since an index on a
misspelled type is real but empty.
"""

from __future__ import annotations

import pandas as pd
import pytest

import kglite


def _disk_graph(tmp_path):
    graph = kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "g"))
    graph.add_nodes(pd.DataFrame({"id": [1, 2], "n": [10, 20], "s": ["a", "b"]}), "Item", "id")
    return graph


def test_disk_create_index_refuses_a_column_it_cannot_index(tmp_path) -> None:
    graph = _disk_graph(tmp_path)
    with pytest.raises(ValueError, match="indexed no values for 'Item.n'"):
        graph.create_index("Item", "n")
    assert not [row for row in graph.list_indexes() if row.get("property") == "n"]
    with pytest.raises(kglite.CypherExecutionError, match="indexed no values for 'Item.n'"):
        graph.cypher("CREATE INDEX FOR (i:Item) ON (i.n)")
    info = graph.create_index("Item", "s")
    assert info["persistent"] and info["serves_lookups"] and info["unique_values"] == 2


@pytest.mark.parametrize("storage", ["default", "disk"])
def test_create_index_reports_whether_the_node_type_is_known(storage, tmp_path) -> None:
    if storage == "disk":
        graph = _disk_graph(tmp_path)
    else:
        graph = kglite.KnowledgeGraph()
        graph.add_nodes(pd.DataFrame({"id": [1, 2], "s": ["a", "b"]}), "Item", "id")
    assert graph.create_index("Item", "s")["node_type_known"] is True
    unknown = graph.create_index("Itme", "s")
    assert unknown["node_type_known"] is False
    assert unknown["created"] is True
    graph.define_schema({"nodes": {"Planned": {}}})
    assert graph.create_index("Planned", "s")["node_type_known"] is True
