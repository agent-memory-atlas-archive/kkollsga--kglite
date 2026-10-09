"""Write-surface coverage contract for the node and relationship ontology gates.

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

The relationship rules (domain, range, required properties, property types)
have the same contract over the same writers: every writer is in
``REL_ENFORCED`` (an entry that attempts an invalid edge write and must be
refused) or ``NO_REL_RULE`` (it cannot write an edge), and
``test_every_writer_is_classified_for_relationship_rules`` fails on one that
is neither.
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
    # Edges between existing nodes: the node never changes. Their relationship
    # contract is REL_ENFORCED below.
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
        lambda: kglite.attach_rows(g, "Person", 0, table, row_type="Row", edge_type="HAS_ROW", key="k"),
        "required_property",
    )


def _load_ntriples(g, tmp):
    path = tmp / "t.nt"
    path.write_text("<http://e/a> <http://e/p> <http://e/b> .\n", encoding="utf-8")
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


# ── Relationship rules ───────────────────────────────────────────────────────

REL_ONTOLOGY = {
    "classes": {"Person": {}, "Company": {}, "Org": {}},
    "relationships": {
        "WORKS_AT": {
            "domain": "Person",
            "range": "Company",
            "required_properties": ["since"],
            "property_types": {"since": "integer"},
            "enforcement": "error",
        },
        "HAS_ROW": {"domain": "Company", "range": "Person", "enforcement": "error"},
    },
}

# Writers that put no edge in the graph.
NO_REL_RULE = {
    **dict.fromkeys(
        """add_nodes add_nodes_bulk add_properties update calculate count unique_values
        collect_children set_table_property""".split(),
        "writes node properties only",
    ),
    **{name: "writes no edge" for name in NO_NODE_RULE if name not in {"create_connections", "create_relationships"}},
}


def make_rel_graph(storage, tmp_path, ontology=REL_ONTOLOGY):
    opts = {} if storage == "memory" else {"storage": storage}
    if storage == "disk":
        opts["path"] = str(tmp_path / "disk")
    g = kglite.KnowledgeGraph(**opts)
    g.cypher("CREATE (:Person {id: 0}), (:Person {id: 1}), (:Company {id: 10})")
    g.cypher("MATCH (c:Company {id: 10}), (p:Person {id: 0}) CREATE (c)-[:OWNS]->(p)")
    g.cypher("MATCH (c:Company {id: 10}), (p:Person {id: 0}) CREATE (p)-[:WORKS_AT {since: 1}]->(c)")
    g.define_ontology(ontology)
    return g


def rel_snapshot(g):
    return (
        g.cypher("MATCH (n) RETURN count(n) AS c").to_list(),
        g.cypher("MATCH ()-[r]->() RETURN type(r) AS t, count(r) AS c ORDER BY t").to_list(),
    )


def _reversed_edge(g):
    return "MATCH (c:Company {id: 10}), (p:Person {id: 1}) "


def _cypher_domain(g, tmp):
    expect_refused(
        lambda: g.cypher(_reversed_edge(g) + "CREATE (c)-[:WORKS_AT {since: 1}]->(p)"),
        "domain",
    )


def _cypher_range(g, tmp):
    expect_refused(
        lambda: g.cypher("MATCH (a:Person {id: 0}), (b:Person {id: 1}) CREATE (a)-[:WORKS_AT {since: 1}]->(b)"),
        "range",
    )


def _cypher_required(g, tmp):
    expect_refused(
        lambda: g.cypher("MATCH (p:Person {id: 0}), (c:Company {id: 10}) CREATE (p)-[:WORKS_AT]->(c)"),
        "required_property",
    )


def _cypher_merge_rel(g, tmp):
    expect_refused(
        lambda: g.cypher(_reversed_edge(g) + "MERGE (c)-[:WORKS_AT {since: 1}]->(p)"),
        "domain",
    )


def _cypher_unwind_rel(g, tmp):
    expect_refused(
        lambda: g.cypher(
            "UNWIND [1, 2] AS i MATCH (p:Person {id: 0}), (c:Company {id: 10}) "
            "CREATE (p)-[:WORKS_AT {since: CASE WHEN i = 2 THEN null ELSE 1 END}]->(c)"
        ),
        "required_property",
    )


def _cypher_set_rel(g, tmp):
    expect_refused(lambda: g.cypher("MATCH ()-[r:WORKS_AT]->() SET r.since = 'old'"), "property_type")
    expect_refused(lambda: g.cypher("MATCH ()-[r:WORKS_AT]->() SET r += {since: 'old'}"), "property_type")
    expect_refused(lambda: g.cypher("MATCH ()-[r:WORKS_AT]->() REMOVE r.since"), "required_property")
    expect_refused(lambda: g.cypher("MATCH ()-[r:WORKS_AT]->() SET r = {}"), "required_property")


def _transaction_rel(g, tmp):
    with g.begin() as tx:
        expect_refused(lambda: tx.cypher(_reversed_edge(g) + "CREATE (c)-[:WORKS_AT {since: 1}]->(p)"), "domain")


def _session_rel(g, tmp):
    session = g.session()
    expect_refused(lambda: session.execute(_reversed_edge(g) + "CREATE (c)-[:WORKS_AT {since: 1}]->(p)"), "domain")


def _frame_call(method, names=("Company", "Person")):
    def call(g):
        data = pd.DataFrame({"s": [10], "t": [1], "since": [1]})
        getattr(g, method)(data, "WORKS_AT", names[0], "s", names[1], "t")

    return call


def _bulk_rel(method):
    def call(g):
        getattr(g, method)(
            [
                {
                    "source_type": "Company",
                    "target_type": "Person",
                    "connection_name": "WORKS_AT",
                    "data": pd.DataFrame({"source_id": [10], "target_id": [1], "since": [1]}),
                }
            ]
        )

    return call


def _entry(call, rule):
    def run(g, tmp):
        expect_refused(lambda: call(g), rule)

    return run


def _create_connections(method):
    def call(g):
        getattr(g.select("Company").traverse("OWNS"), method)("WORKS_AT", properties={"Company": ["id"]})

    return call


def _extend_rel(g, tmp):
    other = kglite.KnowledgeGraph()
    other.cypher("CREATE (:Company {id: 20})-[:WORKS_AT {since: 1}]->(:Person {id: 21})")
    expect_refused(lambda: g.extend(other), "domain")


def _load_ntriples_rel(g, tmp):
    path = tmp / "t.nt"
    path.write_text("<http://e/a> <http://e/p> <http://e/b> .\n", encoding="utf-8")
    with pytest.raises(Exception, match="load_ntriples cannot run"):
        g.load_ntriples(str(path))


def _attach_rows_rel(g, tmp):
    table = pd.DataFrame({"k": ["a"], "age": [1]})
    expect_refused(
        lambda: kglite.attach_rows(g, "Person", 0, table, row_type="Row", edge_type="HAS_ROW", key="k"),
        "domain",
    )


REL_ENFORCED = {
    "cypher": [
        _cypher_domain,
        _cypher_range,
        _cypher_required,
        _cypher_merge_rel,
        _cypher_unwind_rel,
        _cypher_set_rel,
        _transaction_rel,
        _session_rel,
    ],
    "extend": [_extend_rel],
    "load_ntriples": [_load_ntriples_rel],
    **{
        name: [_entry(_frame_call(name), "domain")]
        for name in [
            "add_connections",
            "add_relationships",
            "replace_connections",
            "replace_relationships",
        ]
    },
    **{
        name: [_entry(_bulk_rel(name), "domain")]
        for name in [
            "add_connections_bulk",
            "add_relationships_bulk",
            "add_connections_from_source",
            "add_relationships_from_source",
        ]
    },
    **{name: [_entry(_create_connections(name), "domain")] for name in ["create_connections", "create_relationships"]},
}
# Helper functions (not KnowledgeGraph methods) that write edges.
FUNCTION_ENTRIES = [_attach_rows_rel]

REL_ENTRIES = [
    pytest.param(name, entry, id=f"{name}:{entry.__name__.lstrip('_')}")
    for name, entries in REL_ENFORCED.items()
    for entry in entries
]


def test_every_writer_is_classified_for_relationship_rules():
    """Fails when a node-classified writer has no relationship classification."""
    writers = set(ENFORCED) | set(NO_NODE_RULE)
    unclassified = writers - set(REL_ENFORCED) - set(NO_REL_RULE)
    assert not unclassified, (
        "classify these writers for relationship rules in tests/test_ontology_write_coverage.py "
        f"(REL_ENFORCED if they can write an edge, else NO_REL_RULE): {sorted(unclassified)}"
    )
    assert not set(REL_ENFORCED) & set(NO_REL_RULE), sorted(set(REL_ENFORCED) & set(NO_REL_RULE))
    stale = (set(REL_ENFORCED) | set(NO_REL_RULE)) - writers
    assert not stale, f"classified but not a node-classified writer: {sorted(stale)}"


@pytest.mark.filterwarnings("ignore:.*chained graph view")
@pytest.mark.parametrize("storage", STORAGES)
@pytest.mark.parametrize("name,entry", REL_ENTRIES)
def test_every_edge_writer_refuses_an_invalid_write(storage, name, entry, tmp_path):
    if name in {"extend", "load_ntriples"} and storage != "memory":
        pytest.skip(f"{name} is in-memory only")
    g = make_rel_graph(storage, tmp_path)
    before = rel_snapshot(g)
    entry(g, tmp_path)
    assert rel_snapshot(g) == before, "a refused write left the graph changed"


@pytest.mark.parametrize("storage", STORAGES)
def test_attach_rows_refuses_an_invalid_edge(storage, tmp_path):
    """A refused edge step leaves no row nodes behind either."""
    g = make_rel_graph(storage, tmp_path)
    before = rel_snapshot(g)
    _attach_rows_rel(g, tmp_path)
    assert rel_snapshot(g) == before


@pytest.mark.parametrize("storage", STORAGES)
def test_the_issue_acceptance_case_ac5(storage, tmp_path):
    """A reversed WORKS_AT fails and persists nothing; the right way round works."""
    g = make_rel_graph(storage, tmp_path)
    before = rel_snapshot(g)
    with pytest.raises(kglite.OntologyViolationError) as raised:
        g.cypher("CREATE (c:Company {id: 11})-[:WORKS_AT {since: 1}]->(p:Person {id: 5})")
    assert "domain" in str(raised.value)
    assert rel_snapshot(g) == before
    g.cypher("MATCH (p:Person {id: 1}), (c:Company {id: 10}) CREATE (p)-[:WORKS_AT {since: 1}]->(c)")
    assert g.cypher("MATCH ()-[r:WORKS_AT]->() RETURN count(r) AS c").to_list() == [{"c": 2}]
    with pytest.raises(kglite.OntologyViolationError):
        _frame_call("add_connections")(g)
    good = pd.DataFrame({"s": [1], "t": [10], "since": [2]})
    g.add_connections(good, "WORKS_AT", "Person", "s", "Company", "t", conflict_handling="update")
    assert g.cypher("MATCH ()-[r:WORKS_AT]->() RETURN count(r) AS c").to_list() == [{"c": 2}]


@pytest.mark.parametrize("storage", STORAGES)
def test_warn_level_edges_land_with_a_warning(storage, tmp_path):
    warn = {
        "classes": REL_ONTOLOGY["classes"],
        "relationships": {"WORKS_AT": {**REL_ONTOLOGY["relationships"]["WORKS_AT"], "enforcement": "warn"}},
    }
    g = make_rel_graph(storage, tmp_path, warn)
    result = g.cypher(_reversed_edge(g) + "CREATE (c)-[:WORKS_AT {since: 1}]->(p)")
    assert any("ontology warning (domain)" in w for w in result.diagnostics["warnings"])
    with pytest.warns(UserWarning, match="ontology warning"):
        _frame_call("add_connections")(g)
    # The loader's row meets the pair the Cypher edge made and merges into it.
    assert g.cypher("MATCH ()-[r:WORKS_AT]->() RETURN count(r) AS c").to_list() == [{"c": 2}]


def test_load_ntriples_at_warn_loads_and_says_it_skipped_judgement(tmp_path):
    warn = {
        "classes": REL_ONTOLOGY["classes"],
        "relationships": {"WORKS_AT": {**REL_ONTOLOGY["relationships"]["WORKS_AT"], "enforcement": "warn"}},
    }
    g = make_rel_graph("memory", tmp_path, warn)
    path = tmp_path / "t.nt"
    path.write_text("<http://e/a> <http://e/p> <http://e/b> .\n", encoding="utf-8")
    with pytest.warns(UserWarning, match="not judged"):
        stats = g.load_ntriples(str(path))
    assert any("without per-row validation" in w for w in stats["warnings"])


# ── "Must exist" rules ───────────────────────────────────────────────────────
#
# A rule that demands something be present (a required relationship, a minimum
# degree, an inverse, a symmetric partner, a stored closure) is judged on the
# end state of a transaction. A lone statement or bulk call is its own
# transaction; inside ``begin()`` the verdict is the commit's. Every writer
# that can add or remove a node or a relationship is in ``MUST_ENFORCED`` (an
# entry that leaves a rule unmet and must be refused, the graph unchanged) or
# ``NO_MUST_RULE`` (it changes no topology).

MUST_ONTOLOGY = {
    "classes": {"Person": {}, "Company": {}},
    "relationships": {
        "WORKS_AT": {"domain": "Person", "range": "Company", "required": True, "enforcement": "error"},
        "KNOWS": {"symmetric": True, "enforcement": "error"},
    },
}

# Writers that change no topology: nodes keep their relationships and their
# partners.
NO_MUST_RULE = {
    **dict.fromkeys(
        """add_properties update calculate count unique_values collect_children
        set_table_property""".split(),
        "writes properties only",
    ),
    **{
        name: "changes no topology"
        for name in NO_NODE_RULE
        if name not in {"create_connections", "create_relationships", "purge_provisional", "clear"}
    },
    "clear": "removes every node, so none is left short of a relationship",
}


def make_must_graph(storage, tmp_path, ontology=MUST_ONTOLOGY):
    opts = {} if storage == "memory" else {"storage": storage}
    if storage == "disk":
        opts["path"] = str(tmp_path / "disk")
    g = kglite.KnowledgeGraph(**opts)
    g.cypher("CREATE (:Company {id: 10}), (:Person {id: 0}), (:Person {id: 1}), (:Person {id: 2}), (:Org {id: 5})")
    g.cypher("MATCH (c:Company {id: 10}), (p:Person) CREATE (p)-[:WORKS_AT]->(c)")
    g.cypher("MATCH (a:Person {id: 0}), (b:Person {id: 1}) CREATE (a)-[:KNOWS]->(b), (b)-[:KNOWS]->(a)")
    g.define_ontology(ontology)
    return g


def must_snapshot(g):
    return (
        g.cypher("MATCH (n) RETURN count(n) AS c").to_list(),
        g.cypher("MATCH (a)-[r]->(b) RETURN a.id AS a, type(r) AS t, b.id AS b ORDER BY a, t, b").to_list(),
    )


def _must_create(g, tmp):
    expect_refused(lambda: g.cypher("CREATE (:Person {id: 9})"), "required_relationship")


def _must_unwind(g, tmp):
    expect_refused(
        lambda: g.cypher("UNWIND [8, 9] AS i CREATE (:Person {id: i})"),
        "required_relationship",
    )


def _must_foreach(g, tmp):
    expect_refused(lambda: g.cypher("FOREACH (i IN [7] | CREATE (:Person {id: i}))"), "required_relationship")


def _must_merge(g, tmp):
    expect_refused(lambda: g.cypher("MERGE (:Person {id: 9})"), "required_relationship")


def _must_delete_edge(g, tmp):
    expect_refused(
        lambda: g.cypher("MATCH (:Person {id: 2})-[r:WORKS_AT]->() DELETE r"),
        "required_relationship",
    )


def _must_detach_delete(g, tmp):
    expect_refused(lambda: g.cypher("MATCH (c:Company {id: 10}) DETACH DELETE c"), "required_relationship")


def _must_one_way(g, tmp):
    expect_refused(
        lambda: g.cypher("MATCH (a:Person {id: 0}), (b:Person {id: 2}) CREATE (a)-[:KNOWS]->(b)"),
        "symmetric",
    )


def _must_delete_half(g, tmp):
    expect_refused(
        lambda: g.cypher("MATCH (:Person {id: 0})-[r:KNOWS]->(:Person {id: 1}) DELETE r"),
        "symmetric",
    )


def _must_transaction(g, tmp):
    """The statement is accepted; the commit is the verdict."""
    before = must_snapshot(g)
    tx = g.begin()
    tx.cypher("CREATE (:Person {id: 9})")
    expect_refused(tx.commit, "required_relationship")
    assert must_snapshot(g) == before


def _must_transaction_with_block(g, tmp):
    def run():
        with g.begin() as tx:
            tx.cypher("CREATE (:Person {id: 9})")

    expect_refused(run, "required_relationship")


def _must_session(g, tmp):
    session = g.session()
    before = session.node_count()
    expect_refused(lambda: session.execute("CREATE (:Person {id: 9})"), "required_relationship")
    assert session.node_count() == before


def _must_add_nodes(g, tmp):
    expect_refused(lambda: g.add_nodes(people({"id": [9]}), "Person", "id"), "required_relationship")


def _must_add_nodes_bulk(g, tmp):
    expect_refused(
        lambda: g.add_nodes_bulk(
            [
                {
                    "node_type": "Person",
                    "unique_id_field": "id",
                    "node_title_field": "id",
                    "data": people({"id": [9]}),
                }
            ]
        ),
        "required_relationship",
    )


def _must_extend(g, tmp):
    other = kglite.KnowledgeGraph()
    other.cypher("CREATE (:Person {id: 20})")
    expect_refused(lambda: g.extend(other), "required_relationship")


def _must_one_way_frame(method):
    def call(g):
        getattr(g, method)(pd.DataFrame({"s": [0], "t": [2]}), "KNOWS", "Person", "s", "Person", "t")

    return _entry(call, "symmetric")


def _must_one_way_bulk(method):
    def call(g):
        getattr(g, method)(
            [
                {
                    "source_type": "Person",
                    "target_type": "Person",
                    "connection_name": "KNOWS",
                    "data": pd.DataFrame({"source_id": [0], "target_id": [2]}),
                }
            ]
        )

    return _entry(call, "symmetric")


def _must_create_connections(method):
    def call(g):
        getattr(g.select("Person").traverse("WORKS_AT"), method)("KNOWS")

    return _entry(call, "symmetric")


def _must_purge(g, tmp):
    """A purge that strands a person whose only relationship led to a stub."""
    g.cypher("CREATE (:Person {id: 30})-[:WORKS_AT]->(:Company {id: 99})")
    g.cypher("MATCH (c:Company {id: 99}) SET c._provisional = true")
    before = must_snapshot(g)
    expect_refused(g.purge_provisional, "required_relationship")
    assert must_snapshot(g) == before


def _must_load_ntriples(g, tmp):
    path = tmp / "t.nt"
    path.write_text("<http://e/a> <http://e/p> <http://e/b> .\n", encoding="utf-8")
    with pytest.raises(Exception, match="load_ntriples cannot run"):
        g.load_ntriples(str(path))


MUST_ENFORCED = {
    "cypher": [
        _must_create,
        _must_unwind,
        _must_foreach,
        _must_merge,
        _must_delete_edge,
        _must_detach_delete,
        _must_one_way,
        _must_delete_half,
        _must_transaction,
        _must_transaction_with_block,
        _must_session,
    ],
    "add_nodes": [_must_add_nodes],
    "add_nodes_bulk": [_must_add_nodes_bulk],
    "extend": [_must_extend],
    "purge_provisional": [_must_purge],
    "load_ntriples": [_must_load_ntriples],
    **{
        name: [_must_one_way_frame(name)]
        for name in ["add_connections", "add_relationships", "replace_connections", "replace_relationships"]
    },
    **{
        name: [_must_one_way_bulk(name)]
        for name in [
            "add_connections_bulk",
            "add_relationships_bulk",
            "add_connections_from_source",
            "add_relationships_from_source",
        ]
    },
    **{name: [_must_create_connections(name)] for name in ["create_connections", "create_relationships"]},
}

MUST_ENTRIES = [
    pytest.param(name, entry, id=f"{name}:{entry.__name__.lstrip('_')}")
    for name, entries in MUST_ENFORCED.items()
    for entry in entries
]


def test_every_writer_is_classified_for_must_exist_rules():
    """Fails when a node-classified writer has no must-exist classification."""
    writers = set(ENFORCED) | set(NO_NODE_RULE)
    unclassified = writers - set(MUST_ENFORCED) - set(NO_MUST_RULE)
    assert not unclassified, (
        "classify these writers for must-exist rules in tests/test_ontology_write_coverage.py "
        f"(MUST_ENFORCED if they can add or remove a node or relationship, else NO_MUST_RULE): {sorted(unclassified)}"
    )
    assert not set(MUST_ENFORCED) & set(NO_MUST_RULE), sorted(set(MUST_ENFORCED) & set(NO_MUST_RULE))
    stale = (set(MUST_ENFORCED) | set(NO_MUST_RULE)) - writers
    assert not stale, f"classified but not a node-classified writer: {sorted(stale)}"


@pytest.mark.filterwarnings("ignore:.*chained graph view")
@pytest.mark.parametrize("storage", STORAGES)
@pytest.mark.parametrize("name,entry", MUST_ENTRIES)
def test_every_topology_writer_refuses_an_unmet_must_exist_rule(storage, name, entry, tmp_path):
    if name in {"extend", "load_ntriples"} and storage != "memory":
        pytest.skip(f"{name} is in-memory only")
    g = make_must_graph(storage, tmp_path)
    if name != "purge_provisional":
        before = must_snapshot(g)
    entry(g, tmp_path)
    if name != "purge_provisional":
        assert must_snapshot(g) == before, "a refused write left the graph changed"


@pytest.mark.parametrize("storage", STORAGES)
def test_a_node_and_its_required_edge_commit_across_statements(storage, tmp_path):
    g = make_must_graph(storage, tmp_path)
    with g.begin() as tx:
        tx.cypher("CREATE (:Person {id: 9})")
        tx.cypher("MATCH (p:Person {id: 9}), (c:Company {id: 10}) CREATE (p)-[:WORKS_AT]->(c)")
        tx.cypher("MATCH (p:Person {id: 9}), (q:Person {id: 0}) CREATE (p)-[:KNOWS]->(q)")
        tx.cypher("MATCH (p:Person {id: 9}), (q:Person {id: 0}) CREATE (q)-[:KNOWS]->(p)")
    assert g.cypher("MATCH (p:Person) RETURN count(p) AS c").to_list() == [{"c": 4}]
    g.cypher("CREATE (:Person {id: 11})-[:WORKS_AT]->(:Company {id: 12})")
    assert g.cypher("MATCH (p:Person) RETURN count(p) AS c").to_list() == [{"c": 5}]


@pytest.mark.parametrize("storage", STORAGES)
def test_warn_level_must_exist_rules_commit_with_a_warning(storage, tmp_path):
    warn = {
        "classes": MUST_ONTOLOGY["classes"],
        "relationships": {"WORKS_AT": {**MUST_ONTOLOGY["relationships"]["WORKS_AT"], "enforcement": "warn"}},
    }
    g = make_must_graph(storage, tmp_path, warn)
    result = g.cypher("CREATE (:Person {id: 9})")
    assert any("ontology warning (required_relationship)" in w for w in result.diagnostics["warnings"])
    tx = g.begin()
    tx.cypher("CREATE (:Person {id: 10})")
    with pytest.warns(UserWarning, match="ontology warning"):
        tx.commit()
    with pytest.warns(UserWarning, match="ontology warning"):
        g.add_nodes(people({"id": [11]}), "Person", "id")
    assert g.cypher("MATCH (p:Person) RETURN count(p) AS c").to_list() == [{"c": 6}]
