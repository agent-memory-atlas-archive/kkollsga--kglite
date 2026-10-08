"""Write-surface coverage contract for the node ontology gate.

An ontology rule declared at ``error`` must refuse a violating write through
**every** entry point that can write a node. A writer the gate missed would
report success and enforce nothing, so this file classifies the whole public
write surface and fails the moment a method appears that nobody classified.

Every public ``KnowledgeGraph`` method is in exactly one of three sets:

* ``ENFORCED``: has an entry here that attempts an invalid write and must be
  refused with ``OntologyViolationError`` (or, for ``load_ntriples``, which
  cannot be validated per row, refused outright), leaving the graph unchanged.
* ``NO_NODE_RULE``: writes, but cannot break a node rule: it writes nothing a
  node rule judges (secondary labels, side stores, indexes, declarations) or
  removes data.
* ``NOT_A_WRITER``: reads, selection, export or configuration.

Adding a method to ``KnowledgeGraph`` without classifying it fails
``test_every_public_method_is_classified``; classify it by adding an entry to
``ENFORCED`` (preferred for anything that writes node data) or a reasoned
entry to ``NO_NODE_RULE``.
"""

import pandas as pd
import pytest

import kglite

STORAGES = ["memory", "mapped", "disk"]

# Closed set of public KnowledgeGraph methods that never write node data.
NOT_A_WRITER = set(
    """
    all_paths are_connected begin begin_read betweenness_centrality bounds bug_report centroid
    closeness_centrality collect collect_grouped compare composite_index_stats connected_components
    connection_types connections contains_point context copy date degree_centrality degrees describe
    difference embedding embedding_diagnostics embedding_dim embedding_info embeddings exists expand
    explain explain_mcp explore export export_csv export_embeddings export_rdf export_recipes
    export_skills export_string find get_default_max_work_units get_default_row_limit
    get_default_timeout get_properties get_recipe get_skill get_table_property
    get_valid_time_default graph_info has_composite_index has_index has_node_vector_index
    has_relationship_vector_index has_schema has_text_index has_vector_index ids index_stats indexes
    indices intersection intersects_geometry label_pair_counts label_propagation
    last_mutation_stats last_report len limit list_composite_indexes list_embeddings list_indexes
    list_recipes list_skills louvain_communities match_pattern near_point near_point_m
    neighbors_schema node node_embedding node_embedding_dim node_embeddings node_search_text
    node_type_counts node_types node_vector_search offset ontology ontology_diff operation_index
    pagerank properties read_only relationship_embedding relationship_embedding_dim
    relationship_embeddings relationship_search_text relationship_types
    relationship_vector_search relationships report_history sample save_subset schema
    schema_definition schema_locked schema_text schema_version search search_text select selection
    session shape shortest_path shortest_path_ids shortest_path_indices shortest_path_length
    shortest_path_lengths_batch shortest_path_lengths_from show sort source source_dialect
    source_fingerprint source_root spatial statistics subgraph_stats symmetric_difference
    time_index timeseries timeseries_config titles to_bytes to_df to_networkx to_str to_subgraph
    to_text toc traverse union valid_at valid_during validate_schema vector_search
    verify_unique_constraints where where_any where_connected where_orphans within_bounds
    wkt_centroid
    """.split()
)

# Writers that cannot break a node rule, with the reason.
NO_NODE_RULE = {
    # Secondary labels: the rules judge the primary label alone (decision D3),
    # and the primary label is immutable.
    **dict.fromkeys(["add_label", "remove_label"], "secondary labels are never judged"),
    # Side stores keyed by node, not node properties or labels.
    **dict.fromkeys(
        """add_timeseries add_ts_channel set_time_index set_timeseries set_node_embeddings
        set_embeddings add_node_embeddings add_embeddings add_relationship_embeddings
        set_relationship_embeddings remove_embeddings remove_node_embeddings
        remove_relationship_embeddings import_embeddings copy_embeddings_from embed_node_texts
        embed_relationship_texts embed_texts set_embedder""".split(),
        "side store keyed by node",
    ),
    # Indexes and caches over existing data.
    **dict.fromkeys(
        """build_node_vector_index build_relationship_vector_index build_vector_index
        build_text_index drop_node_vector_index drop_relationship_vector_index drop_vector_index
        drop_text_index refresh_node_vector_index refresh_relationship_vector_index
        refresh_vector_index create_index create_global_index create_range_index
        create_composite_index drop_index drop_range_index drop_composite_index rebuild_indexes
        reindex rebuild_caches build_id_indices vacuum compact""".split(),
        "index or cache over existing data",
    ),
    # Declarations and configuration, which verify stored data themselves.
    **dict.fromkeys(
        """set_spatial set_temporal set_valid_time_default define_schema clear_schema lock_schema
        unlock_schema set_schema_version set_instructions set_skill delete_skill import_skills
        set_recipe delete_recipe import_recipes set_parent_type define_ontology clear_ontology
        materialize_ontology dematerialize_ontology set_default_max_work_units
        set_default_row_limit set_default_timeout set_memory_limit set_auto_vacuum
        enable_disk_mode unspill freeze close sync save backup""".split(),
        "declaration, configuration or persistence",
    ),
    # Edges between existing nodes: the node never changes.
    **dict.fromkeys(["create_connections", "create_relationships"], "edges between existing nodes"),
    # Removals cannot violate a presence or type rule.
    **dict.fromkeys(["purge_provisional", "clear"], "removes nodes"),
}

ONTOLOGY = {
    "classes": {
        "Person": {
            "required_properties": ["email"],
            "property_types": {"email": "string", "age": "integer"},
            "enforcement": "error",
        }
    }
}
CLOSED_LABELS = {
    "closed_labels": True,
    "enforcement": "error",
    "classes": {"Person": {}},
}


def people(rows):
    return pd.DataFrame(rows)


def make_graph(storage, tmp_path, ontology=ONTOLOGY):
    opts = {} if storage == "memory" else {"storage": storage}
    if storage == "disk":
        opts["path"] = str(tmp_path / "disk")
    g = kglite.KnowledgeGraph(**opts)
    g.cypher("CREATE (:Person {id: 0, email: 'a', age: 1}), (:Person {id: 1, email: 'b', age: 2})")
    g.cypher("MATCH (a:Person {id: 0}), (b:Person {id: 1}) CREATE (a)-[:KNOWS]->(b)")
    if ontology is not CLOSED_LABELS:
        g.cypher("MATCH (a:Person {id: 0}) CREATE (:Org {id: 5, flag: 'x'})-[:EMPLOYS]->(a)")
    else:
        # A type the graph still knows but no longer holds: the loaders that
        # skip types absent from the graph will vivify a stub of it.
        g.cypher("CREATE (:Ghost {id: 5})")
        g.define_ontology({**ontology, "enforcement": "warn"})
        g.cypher("MATCH (n:Ghost) DELETE n")
    g.define_ontology(ontology)
    return g


def expect_refused(call, rule=None):
    with pytest.raises(kglite.OntologyViolationError) as raised:
        call()
    if rule is not None:
        assert rule in str(raised.value)


# ── Entries: each attempts one invalid write and must be refused ────────────


def _cypher_create(g, tmp):
    expect_refused(lambda: g.cypher("CREATE (:Person {id: 9})"), "required_property")


def _cypher_create_in_unwind(g, tmp):
    expect_refused(
        lambda: g.cypher(
            "UNWIND [1, 2, 3] AS i CREATE (:Person {id: 10 + i, email: CASE WHEN i = 3 THEN null ELSE 'x' END})"
        ),
        "required_property",
    )


def _cypher_merge(g, tmp):
    expect_refused(lambda: g.cypher("MERGE (:Person {id: 9})"), "required_property")


def _cypher_set(g, tmp):
    expect_refused(lambda: g.cypher("MATCH (p:Person {id: 0}) SET p.age = 'old'"), "property_type")


def _cypher_set_map(g, tmp):
    expect_refused(lambda: g.cypher("MATCH (p:Person {id: 0}) SET p += {age: 'old'}"), "property_type")
    expect_refused(lambda: g.cypher("MATCH (p:Person {id: 0}) SET p = {age: 3}"), "required_property")


def _cypher_remove(g, tmp):
    expect_refused(lambda: g.cypher("MATCH (p:Person {id: 0}) REMOVE p.email"), "required_property")


def _cypher_foreach(g, tmp):
    expect_refused(
        lambda: g.cypher("FOREACH (i IN [7] | CREATE (:Person {id: i}))"),
        "required_property",
    )


def _transaction_cypher(g, tmp):
    with g.begin() as tx:
        expect_refused(lambda: tx.cypher("CREATE (:Person {id: 9})"), "required_property")


def _session_cypher(g, tmp):
    session = g.session()
    before = session.node_count()
    expect_refused(lambda: session.execute("CREATE (:Person {id: 9})"), "required_property")
    assert session.node_count() == before


def _add_nodes(g, tmp):
    expect_refused(
        lambda: g.add_nodes(people({"id": [9], "age": [3]}), "Person", "id"),
        "required_property",
    )


def _add_nodes_bulk(g, tmp):
    expect_refused(
        lambda: g.add_nodes_bulk(
            [
                {
                    "node_type": "Person",
                    "unique_id_field": "id",
                    "node_title_field": "id",
                    "data": people({"id": [9], "age": [3]}),
                }
            ]
        ),
        "required_property",
    )


def _extend(g, tmp):
    other = kglite.KnowledgeGraph()
    other.cypher("CREATE (:Person {id: 20})")
    expect_refused(lambda: g.extend(other), "required_property")


def _add_properties(g, tmp):
    sel = g.select("Org").traverse("EMPLOYS")
    expect_refused(lambda: sel.add_properties({"Org": {"age": "flag"}}), "property_type")


def _update(g, tmp):
    expect_refused(lambda: g.select("Person").update({"age": "old"}), "property_type")


def _calculate(g, tmp):
    expect_refused(lambda: g.select("Person").calculate("age + 1", store_as="email"), "property_type")


def _count(g, tmp):
    expect_refused(lambda: g.select("Person").count(store_as="email"), "property_type")


def _unique_values(g, tmp):
    expect_refused(
        lambda: g.select("Person").traverse("KNOWS").unique_values("email", store_as="age"),
        "property_type",
    )


def _collect_children(g, tmp):
    expect_refused(
        lambda: g.select("Person").traverse("KNOWS").collect_children("email", store_as="age"),
        "property_type",
    )


def _set_table_property(g, tmp):
    table = pd.DataFrame({"k": ["a"], "v": [1]})
    expect_refused(lambda: g.set_table_property("Person", 0, "age", table), "property_type")


def _attach_rows(g, tmp):
    table = pd.DataFrame({"k": ["a"], "age": [1]})
    expect_refused(
        lambda: kglite.attach_rows(g, "Person", 0, table, row_type="Person", edge_type="HAS_ROW", key="k"),
        "required_property",
    )


def _load_ntriples(g, tmp):
    path = tmp / "t.nt"
    path.write_text("<http://e/a> <http://e/p> <http://e/b> .\n")
    with pytest.raises(Exception, match="load_ntriples cannot run"):
        g.load_ntriples(str(path))


def _stub_closed_labels(call):
    def entry(g, tmp):
        expect_refused(lambda: call(g), "closed_labels")

    return entry


def _edges(**spec):
    return pd.DataFrame({"s": [0], "t": [99]}), spec


def _add_connections(method):
    def call(g):
        getattr(g, method)(pd.DataFrame({"s": [0], "t": [99]}), "KNOWS", "Person", "s", "Ghost", "t")

    return call


def _connection_spec(g):
    return [
        {
            "source_type": "Person",
            "target_type": "Ghost",
            "connection_name": "KNOWS",
            "data": pd.DataFrame({"source_id": [0], "target_id": [99]}),
        }
    ]


def _bulk(method):
    def call(g):
        getattr(g, method)(_connection_spec(g))

    return call


ENFORCED = {
    "cypher": [
        _cypher_create,
        _cypher_create_in_unwind,
        _cypher_merge,
        _cypher_set,
        _cypher_set_map,
        _cypher_remove,
        _cypher_foreach,
        _transaction_cypher,
        _session_cypher,
    ],
    "add_nodes": [_add_nodes],
    "add_nodes_bulk": [_add_nodes_bulk],
    "extend": [_extend],
    "add_properties": [_add_properties],
    "update": [_update],
    "calculate": [_calculate],
    "count": [_count],
    "unique_values": [_unique_values],
    "collect_children": [_collect_children],
    "set_table_property": [_set_table_property],
    "load_ntriples": [_load_ntriples],
}

# Connection loaders write nothing but edges, except the endpoint stubs they
# vivify: those are nodes, and a closed-label ontology refuses an undeclared
# stub type.
CONNECTION_LOADERS = {
    "add_connections": _add_connections("add_connections"),
    "add_relationships": _add_connections("add_relationships"),
    "replace_connections": _add_connections("replace_connections"),
    "replace_relationships": _add_connections("replace_relationships"),
    "add_connections_bulk": _bulk("add_connections_bulk"),
    "add_relationships_bulk": _bulk("add_relationships_bulk"),
    "add_connections_from_source": _bulk("add_connections_from_source"),
    "add_relationships_from_source": _bulk("add_relationships_from_source"),
}
for _name, _call in CONNECTION_LOADERS.items():
    ENFORCED[_name] = [_stub_closed_labels(_call)]

ENTRIES = [
    pytest.param(name, entry, id=f"{name}:{entry.__name__.lstrip('_')}")
    for name, entries in ENFORCED.items()
    for entry in entries
]
CLOSED_ENTRY_NAMES = set(CONNECTION_LOADERS)


def public_methods():
    return {n for n in dir(kglite.KnowledgeGraph) if not n.startswith("_")}


def test_every_public_method_is_classified():
    """Fails when a KnowledgeGraph method is added (or renamed) unclassified."""
    classified = set(ENFORCED) | set(NO_NODE_RULE) | NOT_A_WRITER
    unclassified = public_methods() - classified
    assert not unclassified, (
        f"classify these KnowledgeGraph methods in tests/test_ontology_write_coverage.py "
        f"(an ENFORCED entry if they can write node data): {sorted(unclassified)}"
    )
    stale = classified - public_methods()
    assert not stale, f"classified but no longer on KnowledgeGraph: {sorted(stale)}"


def test_the_classes_do_not_overlap():
    sets = [set(ENFORCED), set(NO_NODE_RULE), NOT_A_WRITER]
    for i, a in enumerate(sets):
        for b in sets[i + 1 :]:
            assert not a & b, sorted(a & b)


def test_the_other_write_classes_are_covered_too():
    """Transaction and Session expose only Cypher, which has entries above."""
    assert {n for n in dir(kglite.Transaction) if not n.startswith("_")} == {
        "commit",
        "cypher",
        "is_read_only",
        "rollback",
    }
    assert {n for n in dir(kglite.Session) if not n.startswith("_")} == {
        "backup",
        "cursor",
        "cypher",
        "execute",
        "node_count",
        "node_types",
        "snapshot",
        "version",
    }


def snapshot(g):
    return g.cypher("MATCH (n) RETURN count(n) AS c").to_list(), g.cypher(
        "MATCH (p:Person) RETURN p.id AS id, p.email AS email, p.age AS age ORDER BY id"
    ).to_list()


@pytest.mark.parametrize("storage", STORAGES)
@pytest.mark.parametrize("name,entry", ENTRIES)
def test_every_writer_refuses_an_invalid_write(storage, name, entry, tmp_path):
    ontology = CLOSED_LABELS if name in CLOSED_ENTRY_NAMES else ONTOLOGY
    if name in {"extend", "load_ntriples"} and storage != "memory":
        pytest.skip(f"{name} is in-memory only")
    g = make_graph(storage, tmp_path, ontology)
    before = snapshot(g)
    entry(g, tmp_path)
    assert snapshot(g) == before, "a refused write left the graph changed"


def test_the_gate_is_not_a_blanket_refusal(tmp_path):
    """The same writers succeed when the data is valid."""
    g = make_graph("memory", tmp_path)
    g.cypher("CREATE (:Person {id: 5, email: 'ok', age: 3})")
    g.cypher("CREATE (p:Person {id: 6}) SET p.email = 'repaired'")
    g.cypher("MATCH (p:Person {id: 0}) SET p.age = 7 REMOVE p.age SET p.age = 8")
    g.add_nodes(people({"id": [7], "email": ["e"], "age": [4]}), "Person", "id")
    g.select("Person").update({"age": 9})
    assert g.cypher("MATCH (p:Person) RETURN count(p) AS c").to_list() == [{"c": 5}]


@pytest.mark.parametrize("storage", STORAGES)
def test_warn_level_writes_land_with_a_warning(storage, tmp_path):
    warn = {"classes": {"Person": {**ONTOLOGY["classes"]["Person"], "enforcement": "warn"}}}
    g = make_graph(storage, tmp_path, warn)
    result = g.cypher("CREATE (:Person {id: 9})")
    assert any("ontology warning" in w for w in result.diagnostics["warnings"])
    with pytest.warns(UserWarning, match="ontology warning"):
        g.add_nodes(people({"id": [10], "age": [3]}), "Person", "id")
    assert g.cypher("MATCH (p:Person) RETURN count(p) AS c").to_list() == [{"c": 4}]
