"""One crash row per public mutator of a durable ``KnowledgeGraph``.

A child opens ``durable="full"``, seeds a fixed graph, calls ONE mutator, records
the state it holds live, and dies with ``os._exit`` (no ``save()``, no
``close()``). The parent reopens the path and requires the recovered state to
equal the live one. Three row kinds:

``logged``
    the mutation reaches the write-ahead log: recovered == live, and live !=
    before (a call that changed nothing would pass vacuously).
``refused``
    selection-based writes (``update``, ``calculate(store_as=)``, ...) raise on
    the derived handle ``select()`` returns; nothing is applied or logged.
``runtime``
    a flag of the live handle (``lock_schema``, ``read_only``): applied live, never persisted, not even by
    ``save()``.
``unlogged``
    schema/config metadata is checkpoint-only: it is applied live, lost by a
    crash, and kept by ``save()`` (a second graph runs the same call then
    ``save()``, then crashes).

``test_every_write_gate_caller_has_a_row`` scans ``crates/kglite-py/src`` for
every function that calls a write-gate method and fails when one has neither a
row nor a reasoned exemption, so a new mutator cannot slip in unexercised.
"""

from __future__ import annotations

from dataclasses import dataclass, field
import json
from pathlib import Path
import re
import subprocess
import sys

import pytest

import kglite

REPO_ROOT = Path(__file__).resolve().parents[1]
GATE_DIR = REPO_ROOT / "crates" / "kglite-py" / "src"

_BASE_SEED = """
g.add_nodes(pd.DataFrame({"id": [1, 2, 3], "name": ["ann", "bob", "cy"], "age": [30, 40, 50]}), "P", "id", "name")
g.add_relationships(pd.DataFrame({"s": [1, 2], "t": [2, 3], "w": [1, 2]}), "K", "P", "s", "P", "t")
g.add_nodes(pd.DataFrame({"id": [1, 2, 3], "name": ["a", "b", "c"],
                          "vf": ["2020-01-01", "2021-01-01", "2019-01-01"],
                          "vt": ["2030-01-01", "2022-01-01", "2020-01-01"],
                          "lat": [1.0, 2.0, 3.0], "lon": [4.0, 5.0, 6.0], "v": [1.5, 2.5, 3.5]}), "E", "id", "name")
g.add_nodes(pd.DataFrame({"id": [10, 11], "name": ["x", "y"]}), "Q", "id", "name")
"""

# The state compared across the crash: everything the base seed can be changed into.
_BASE_PROBE = {
    "nodes": "sorted(json.dumps(r, sort_keys=True, default=str) for r in g.cypher("
    "'MATCH (n) RETURN labels(n) AS l, properties(n) AS p').to_list())",
    "edges": "sorted(json.dumps(r, sort_keys=True, default=str) for r in g.cypher("
    "'MATCH (a)-[r]->(b) RETURN a.id AS a, labels(a) AS al, type(r) AS t, b.id AS b, labels(b) AS bl, "
    "properties(r) AS p').to_list())",
    "indexes": "sorted(json.dumps(i, sort_keys=True, default=str) for i in g.list_indexes())",
    "composite": "sorted(json.dumps(i, sort_keys=True, default=str) for i in g.list_composite_indexes())",
    "show_indexes": "sorted(json.dumps(i, sort_keys=True, default=str) for i in g.cypher('SHOW INDEXES').to_list())",
    "constraints": "json.dumps(g.cypher('SHOW CONSTRAINTS').to_list(), sort_keys=True, default=str)",
    "embeddings": "json.dumps(g.list_embeddings(), sort_keys=True, default=str)",
    "skills": "sorted(s['name'] for s in g.list_skills())",
    "recipes": "sorted(json.dumps(r, sort_keys=True, default=str) for r in g.list_recipes())",
    "ontology": "json.dumps(g.ontology(), sort_keys=True, default=str)",
    "schema_version": "g.schema_version",
}


def _safe(f):
    try:
        return f()
    except Exception as e:  # noqa: BLE001 - an absent channel is a probe value, not a failure
        return f"ERR:{type(e).__name__}"


def probe(g, extra: dict[str, str]) -> dict:
    """The base state plus a row's extra expressions, as plain JSON."""
    ns = {"g": g, "json": json, "safe": _safe}
    out = {k: eval(v, ns) for k, v in _BASE_PROBE.items()}  # noqa: S307 - trusted literals above
    out.update({k: eval(v, ns) for k, v in extra.items()})  # noqa: S307
    return json.loads(json.dumps(out, default=str))


@dataclass
class Row:
    id: str
    covers: tuple[str, ...]
    act: str
    seed: str = ""
    kind: str = "logged"
    probe: dict[str, str] = field(default_factory=dict)
    #: ``False`` for a call whose logical content is unchanged (``vacuum``).
    effective: bool = True
    #: A reproduced defect in today's engine: the row is a strict ``xfail`` so a fix flips it red.
    known_defect: str = ""


_EMBEDDER = """
class Emb:
    dimension = 2
    model_id = "toy"
    def embed(self, texts):
        return [[float(len(t) % 3 + 1), 1.0] for t in texts]
g.set_embedder(Emb())
"""

_ONTOLOGY = '{"classes": {"Thing": {"abstract": True}, "Q": {"is_a": "Thing"}}}'
_VEC = "g.set_node_embeddings('Q', 'name', {10: [1.0, 0.0], 11: [0.0, 1.0]})\n"
_EMB_PROBE = {
    "emb10": "g.embedding('Q', 'name', 10) if g.list_embeddings() else None",
}
_REL_SEED = "g.add_relationships(pd.DataFrame({'s': [10], 't': [11], 'note': ['n']}), 'R', 'Q', 's', 'Q', 't')\n"
_NT = (
    '<http://www.wikidata.org/entity/Q1> <http://www.w3.org/2000/01/rdf-schema#label> "Alpha"@en .\n'
    '<http://www.wikidata.org/entity/Q2> <http://www.w3.org/2000/01/rdf-schema#label> "Beta"@en .\n'
    "<http://www.wikidata.org/entity/Q1> <http://www.wikidata.org/prop/direct/P31> "
    "<http://www.wikidata.org/entity/Q2> .\n"
)

ROWS: list[Row] = [
    # --- Cypher -----------------------------------------------------------------------------
    Row("cypher_create", ("kg_core.rs::cypher",), "g.cypher(\"CREATE (:P {id: 9, name: 'zed'})\")"),
    Row("cypher_set", ("kg_core.rs::cypher",), "g.cypher('MATCH (p:P {id: 1}) SET p.age = 31, p.city = \"oslo\"')"),
    Row("cypher_remove", ("kg_core.rs::cypher",), "g.cypher('MATCH (p:P {id: 1}) REMOVE p.age')"),
    Row("cypher_detach_delete", ("kg_core.rs::cypher",), "g.cypher('MATCH (p:P {id: 2}) DETACH DELETE p')"),
    Row("cypher_merge", ("kg_core.rs::cypher",), "g.cypher(\"MERGE (:P {id: 7, name: 'mia'})\")"),
    Row(
        "cypher_create_edge",
        ("kg_core.rs::cypher",),
        "g.cypher('MATCH (a:P {id: 1}), (b:P {id: 3}) CREATE (a)-[:LIKES {s: 2}]->(b)')",
    ),
    Row("cypher_label", ("kg_core.rs::cypher",), "g.cypher('MATCH (p:P {id: 2}) SET p:Admin')"),
    Row("cypher_create_index", ("kg_core.rs::cypher",), "g.cypher('CREATE INDEX FOR (n:P) ON (n.name)')"),
    Row(
        "cypher_constraint",
        ("kg_core.rs::cypher",),
        "g.cypher('CREATE CONSTRAINT FOR (n:P) REQUIRE n.name IS UNIQUE')",
    ),
    Row(
        "cypher_temporal_declare",
        ("kg_core.rs::cypher",),
        "g.cypher(\"CALL db.temporal.declare({node: 'Qt', from: 'vf', to: 'vt', convention: 'closed'})\")",
        seed="g.add_nodes(pd.DataFrame({'id': [50], 'name': ['t'], 'vf': ['2020-01-01'], 'vt': ['2030-01-01']}),"
        " 'Qt', 'id', 'name')\n",
        probe={"temporal": "json.dumps(g.cypher('CALL db.temporal.declarations()').to_list(), default=str)"},
    ),
    Row(
        "transaction_commit",
        ("transaction.rs::commit",),
        "tx = g.begin()\ntx.cypher(\"CREATE (:P {id: 8, name: 'tx'})\")\ntx.commit()",
    ),
    # --- fluent / bulk loaders --------------------------------------------------------------
    Row(
        "add_nodes",
        ("kg_mutation.rs::add_nodes",),
        "g.add_nodes(pd.DataFrame({'id': [20, 21], 'name': ['n', 'm'], 'x': [1, 2]}), 'N', 'id', 'name')",
    ),
    Row(
        "add_nodes_update",
        ("kg_mutation.rs::add_nodes",),
        "g.add_nodes(pd.DataFrame({'id': [2, 4], 'name': ['b2', 'd'], 'age': [41, 60]}), 'P', 'id', 'name',"
        " conflict_handling='update')",
    ),
    Row(
        "add_nodes_bulk",
        ("kg_mutation.rs::add_nodes_bulk",),
        "g.add_nodes_bulk([{'node_type': 'N', 'unique_id_field': 'id', 'node_title_field': 'name',"
        " 'data': pd.DataFrame({'id': [30], 'name': ['t30'], 'x': [1]})}])",
    ),
    Row(
        "extend",
        ("kg_mutation.rs::extend",),
        "o = kglite.KnowledgeGraph()\n"
        "o.add_nodes(pd.DataFrame({'id': [40], 'name': ['o']}), 'O', 'id', 'name')\n"
        "g.extend(o)",
    ),
    Row(
        "add_relationships",
        ("kg_mutation.rs::write_connections", "kg_mutation.rs::add_relationships_internal"),
        "g.add_relationships(pd.DataFrame({'s': [1], 't': [3], 'k': [9]}), 'F', 'P', 's', 'P', 't')",
    ),
    Row(
        "add_connections",
        ("kg_mutation.rs::write_connections", "kg_mutation.rs::add_relationships_internal"),
        "g.add_connections(pd.DataFrame({'s': [3], 't': [1]}), 'G', 'P', 's', 'P', 't')",
    ),
    Row(
        "add_label",
        ("kg_mutation.rs::add_label",),
        "g.add_label('P', [1, 2], 'Hot')",
    ),
    Row(
        "remove_label",
        ("kg_mutation.rs::remove_label",),
        "g.remove_label('P', [1], 'Hot')",
        seed="g.add_label('P', [1, 2], 'Hot')\n",
    ),
    Row(
        "purge_provisional",
        ("kg_introspection.rs::purge_provisional",),
        "g.purge_provisional()",
        seed="g.add_relationships(pd.DataFrame({'s': [1], 't': [999]}), 'D', 'P', 's', 'Ghost', 't', "
        "on_invalid='warn')\n",
    ),
    Row(
        "vacuum",
        ("kg_introspection.rs::vacuum",),
        "g.vacuum()",
        seed="g.cypher('MATCH (p:P {id: 2}) DETACH DELETE p')\n",
        effective=False,
    ),
    Row(
        "set_parent_type",
        ("kg_introspection.rs::set_parent_type",),
        "g.set_parent_type('Q', 'P')",
        probe={"describe": "g.describe()"},
    ),
    Row("set_schema_version", ("kg_introspection.rs::set_schema_version",), "g.set_schema_version(7)"),
    Row(
        "set_temporal",
        ("kg_fluent.rs::set_temporal",),
        "g.set_temporal('E', 'vf', 'vt')",
        probe={"temporal": "json.dumps(g.cypher('CALL db.temporal.declarations()').to_list(), default=str)"},
    ),
    Row(
        "set_table_property",
        ("tables.rs::set_table_property",),
        "g.set_table_property('P', 1, 'lines', pd.DataFrame({'a': [1, 2], 'b': ['x', 'y']}))",
    ),
    # --- selection writes: refused on the derived handle ------------------------------------
    Row("update", ("kg_fluent.rs::update",), "g.select('P').where({'id': 1}).update({'age': 99})", kind="refused"),
    Row(
        "calculate_store_as",
        ("kg_introspection.rs::calculate",),
        "g.select('P').calculate('sum(age)', store_as='tot')",
        kind="refused",
    ),
    Row("count_store_as", ("kg_introspection.rs::count",), "g.select('P').count(store_as='cnt')", kind="refused"),
    Row(
        "collect_children_store_as",
        ("kg_introspection.rs::collect_children",),
        "g.select('P').traverse('K').collect_children(property='title', store_as='kids')",
        kind="refused",
    ),
    Row(
        "unique_values_store_as",
        ("kg_introspection.rs::unique_values",),
        "g.select('P').unique_values('age', store_as='uv')",
        kind="refused",
    ),
    Row(
        "create_relationships",
        ("kg_introspection.rs::create_relationships",),
        "g.select('P').traverse('K').create_relationships('LINK')",
        kind="refused",
    ),
    Row(
        "add_properties",
        ("kg_introspection.rs::add_properties",),
        "g.select('P').traverse('K').add_properties({'P': {'title': 'copied'}})",
        kind="refused",
    ),
    # --- indexes ----------------------------------------------------------------------------
    Row("create_index", ("indexes.rs::create_index",), "g.create_index('P', 'age')"),
    Row("drop_index", ("indexes.rs::drop_index",), "g.drop_index('P', 'age')", seed="g.create_index('P', 'age')\n"),
    Row("create_range_index", ("indexes.rs::create_range_index",), "g.create_range_index('P', 'age')"),
    Row(
        "drop_range_index",
        ("indexes.rs::drop_range_index",),
        "g.drop_range_index('P', 'age')",
        seed="g.create_range_index('P', 'age')\n",
    ),
    Row(
        "create_composite_index",
        ("indexes.rs::create_composite_index",),
        "g.create_composite_index('P', ['name', 'age'])",
    ),
    Row(
        "drop_composite_index",
        ("indexes.rs::drop_composite_index",),
        "g.drop_composite_index('P', ['name', 'age'])",
        seed="g.create_composite_index('P', ['name', 'age'])\n",
    ),
    Row(
        "build_text_index",
        ("indexes.rs::build_text_index",),
        "g.build_text_index('P', 'name')",
        probe={
            "text": "[g.has_text_index('P', 'name'), g.cypher(\"MATCH (n:P) WHERE text_bm25(n, 'name', 'ann') > 0.0 "
            "RETURN n.id AS id\").to_list()] if g.has_text_index('P', 'name') else None"
        },
    ),
    Row(
        "drop_text_index",
        ("indexes.rs::drop_text_index",),
        "g.drop_text_index('P', 'name')",
        seed="g.build_text_index('P', 'name')\n",
        probe={"text": "g.has_text_index('P', 'name')"},
    ),
    # --- ontology ---------------------------------------------------------------------------
    Row("define_ontology", ("kg_core.rs::define_ontology",), f"g.define_ontology({_ONTOLOGY})"),
    Row(
        "clear_ontology",
        ("kg_core.rs::clear_ontology",),
        "g.clear_ontology()",
        seed=f"g.define_ontology({_ONTOLOGY})\n",
    ),
    Row(
        "materialize_ontology",
        ("kg_core.rs::materialize_ontology",),
        "g.materialize_ontology()",
        seed=f"g.define_ontology({_ONTOLOGY})\n",
    ),
    Row(
        "dematerialize_ontology",
        ("kg_core.rs::dematerialize_ontology",),
        "g.dematerialize_ontology()",
        seed=f"g.define_ontology({_ONTOLOGY})\ng.materialize_ontology()\n",
    ),
    # --- skills and recipes -----------------------------------------------------------------
    Row("set_skill", ("skills.rs::set_skill",), "g.set_skill('sk', 'when', 'body')"),
    Row(
        "delete_skill", ("skills.rs::delete_skill",), "g.delete_skill('sk')", seed="g.set_skill('sk', 'when', 'body')\n"
    ),
    Row(
        "import_skills",
        ("skills.rs::import_skills",),
        "(work / 'sk.md').write_text('---\\nname: imp\\ndescription: d\\n---\\nbody\\n')\n"
        "g.import_skills(str(work / 'sk.md'))",
    ),
    Row(
        "set_recipe",
        ("recipes.rs::set_recipe",),
        "g.set_recipe('grp', 'q', 'what', 'MATCH (n:Q) RETURN count(n) AS c', recipe_description='a group')",
    ),
    Row(
        "delete_recipe",
        ("recipes.rs::delete_recipe",),
        "g.delete_recipe('grp', 'q')",
        seed="g.set_recipe('grp', 'q', 'what', 'MATCH (n:Q) RETURN count(n) AS c', recipe_description='a group')\n",
    ),
    Row(
        "import_recipes",
        ("recipes.rs::import_recipes",),
        "import json as _j\n(work / 'r.json').write_text(_j.dumps({'grp2': {'description': 'd', 'queries': "
        "{'q2': {'description': 'x', 'cypher': 'MATCH (n:Q) RETURN count(n) AS c',"
        " 'parameters': {'type': 'object', 'properties': {}, 'additionalProperties': False}}}}}))\n"
        "g.import_recipes(str(work / 'r.json'))",
    ),
    # --- spatial, timeseries ----------------------------------------------------------------
    Row(
        "set_spatial",
        ("spatial.rs::set_spatial",),
        "g.set_spatial('E', location=('lat', 'lon'))",
        probe={"spatial": "json.dumps(g.spatial('E'), default=str)"},
    ),
    Row(
        "set_timeseries",
        ("timeseries.rs::set_timeseries",),
        "g.set_timeseries('Q', resolution='month', channels=['out'])",
        probe={"ts_cfg": "json.dumps(g.timeseries_config('Q'), default=str)"},
    ),
    Row(
        "set_time_index",
        ("timeseries.rs::set_time_index",),
        "g.set_time_index(10, ['2020-01', '2020-02'])",
        seed="g.set_timeseries('Q', resolution='month', channels=['out'])\n",
        probe={"ti": "g.time_index(10)"},
    ),
    Row(
        "add_ts_channel",
        ("timeseries.rs::add_ts_channel",),
        "g.add_ts_channel(10, 'out', [1.0, 2.0])",
        seed="g.set_timeseries('Q', resolution='month', channels=['out'])\n"
        "g.set_time_index(10, ['2020-01', '2020-02'])\n",
        probe={"ts": "safe(lambda: g.timeseries(10, 'out'))"},
    ),
    Row(
        "add_timeseries",
        ("timeseries.rs::add_timeseries",),
        "g.add_timeseries('Q', data=pd.DataFrame({'id': [10, 10], 'm': ['2020-01', '2020-02'], 'val': [1.0, 2.0]}),"
        " fk='id', time_key=['m'], channels=['val'], resolution='month')",
        probe={"ts": "safe(lambda: g.timeseries(10, 'val'))"},
    ),
    # --- embeddings and vector indexes ------------------------------------------------------
    Row(
        "set_node_embeddings",
        ("vector.rs::set_node_embeddings",),
        "g.set_node_embeddings('Q', 'name', {10: [1.0, 0.0], 11: [0.0, 1.0]})",
        probe=_EMB_PROBE,
    ),
    Row(
        "add_node_embeddings",
        ("vector.rs::add_node_embeddings",),
        "g.add_node_embeddings('Q', 'name', {11: [0.0, 1.0]})",
        seed="g.set_node_embeddings('Q', 'name', {10: [1.0, 0.0]})\n",
        probe=_EMB_PROBE,
    ),
    Row(
        "remove_node_embeddings",
        ("vector.rs::remove_node_embeddings",),
        "g.remove_node_embeddings('Q', 'name')",
        seed=_VEC,
        probe=_EMB_PROBE,
    ),
    Row(
        "copy_embeddings_from",
        ("vector.rs::copy_embeddings_from",),
        "o = kglite.KnowledgeGraph()\n"
        "o.add_nodes(pd.DataFrame({'id': [10, 11], 'name': ['x', 'y']}), 'Q', 'id', 'name')\n"
        "o.set_node_embeddings('Q', 'name', {10: [1.0, 0.0], 11: [0.0, 1.0]})\n"
        "g.copy_embeddings_from(o)",
        probe=_EMB_PROBE,
    ),
    Row(
        "import_embeddings",
        ("vector.rs::import_embeddings",),
        "o = kglite.KnowledgeGraph()\n"
        "o.add_nodes(pd.DataFrame({'id': [10, 11], 'name': ['x', 'y']}), 'Q', 'id', 'name')\n"
        "o.set_node_embeddings('Q', 'name', {10: [1.0, 0.0], 11: [0.0, 1.0]})\n"
        "o.export_embeddings(str(work / 'e.kgle'))\n"
        "g.import_embeddings(str(work / 'e.kgle'))",
        probe=_EMB_PROBE,
    ),
    Row(
        "embed_node_texts",
        ("vector.rs::embed_node_texts",),
        "g.embed_node_texts('Q', 'name')",
        seed=_EMBEDDER,
        probe=_EMB_PROBE,
    ),
    Row(
        "build_node_vector_index",
        ("vector.rs::build_node_vector_index",),
        "g.build_node_vector_index('Q', 'name')",
        seed=_VEC,
        probe={"vi": "g.has_node_vector_index('Q', 'name')"},
    ),
    Row(
        "quantize_embeddings",
        ("quantization.rs::quantize_embeddings", "quantization.rs::quantize_written"),
        "g.quantize_embeddings('Q', 'name', 'int8')",
        seed=_VEC,
        kind="unlogged",
        probe={"q": "g.embedding_info('Q', 'name')"},
    ),
    Row(
        "drop_node_vector_index",
        ("vector.rs::drop_node_vector_index",),
        "g.drop_node_vector_index('Q', 'name')",
        seed=_VEC + "g.build_node_vector_index('Q', 'name')\n",
        probe={"vi": "g.has_node_vector_index('Q', 'name')"},
    ),
    Row(
        "set_relationship_embeddings",
        ("relationship_vectors.rs::write_relationship_rows",),
        "g.set_embeddings('R', 'note', {(10, 11): [1.0, 0.0]}, entity='relationship')",
        seed=_REL_SEED,
        probe={"rel": "json.dumps(g.list_embeddings(), default=str)"},
    ),
    Row(
        "add_relationship_embeddings",
        ("relationship_vectors.rs::write_relationship_rows",),
        "g.add_embeddings('R', 'note', {(10, 11): [0.0, 1.0]}, entity='relationship')",
        seed=_REL_SEED,
        probe={"rel": "json.dumps(g.list_embeddings(), default=str)"},
    ),
    Row(
        "remove_relationship_embeddings",
        ("relationship_search.rs::remove_relationship_embeddings",),
        "g.remove_embeddings('R', 'note', entity='relationship')",
        seed=_REL_SEED + "g.set_embeddings('R', 'note', {(10, 11): [1.0, 0.0]}, entity='relationship')\n",
    ),
    Row(
        "embed_relationship_texts",
        ("relationship_vectors.rs::embed_relationship_texts",),
        "g.embed_texts('R', 'note', entity='relationship')",
        seed=_EMBEDDER + _REL_SEED,
    ),
    Row(
        "build_relationship_vector_index",
        ("relationship_vectors.rs::build_relationship_vector_index",),
        "g.build_vector_index('R', 'note', entity='relationship')",
        seed=_REL_SEED + "g.set_embeddings('R', 'note', {(10, 11): [1.0, 0.0]}, entity='relationship')\n",
        probe={"vi": "g.has_relationship_vector_index('R', 'note')"},
    ),
    Row(
        "drop_relationship_vector_index",
        ("relationship_vectors.rs::drop_relationship_vector_index",),
        "g.drop_vector_index('R', 'note', entity='relationship')",
        seed=_REL_SEED
        + "g.set_embeddings('R', 'note', {(10, 11): [1.0, 0.0]}, entity='relationship')\n"
        + "g.build_vector_index('R', 'note', entity='relationship')\n",
        probe={"vi": "g.has_relationship_vector_index('R', 'note')"},
    ),
    # --- RDF --------------------------------------------------------------------------------
    Row(
        "load_ntriples",
        ("kg_core.rs::load_ntriples",),
        "(work / 'd.nt').write_text(NT)\ng.load_ntriples(str(work / 'd.nt'), languages=['en'])",
    ),
    # --- checkpoint-only metadata: applied live, lost by a crash, kept by save() -------------
    Row(
        "define_schema",
        ("kg_core.rs::define_schema",),
        "g.define_schema({'nodes': {'P': {'required': ['id']}}})",
        kind="unlogged",
        probe={"schema": "json.dumps(g.schema_definition(), sort_keys=True, default=str)"},
    ),
    Row(
        "clear_schema",
        ("kg_core.rs::clear_schema",),
        "g.clear_schema()",
        seed="g.define_schema({'nodes': {'P': {'required': ['id']}}})\ng.save()\n",
        kind="unlogged",
        probe={"schema": "json.dumps(g.schema_definition(), sort_keys=True, default=str)"},
    ),
    Row(
        "lock_schema",
        ("kg_introspection.rs::lock_schema",),
        "g.lock_schema()",
        kind="runtime",
        probe={"locked": "g.schema_locked"},
    ),
    Row(
        "set_instructions",
        ("kg_core.rs::set_instructions",),
        "g.set_instructions('be careful')",
        kind="unlogged",
        probe={"instr": "'be careful' in g.describe()"},
    ),
    Row(
        "read_only",
        ("kg_introspection.rs::read_only",),
        "g.read_only(True)",
        kind="runtime",
        probe={"ro": "g.read_only()"},
    ),
    Row(
        "set_valid_time_default",
        ("kg_fluent.rs::set_valid_time_default",),
        "g.set_valid_time_default('2021-06-01', persist=True)",
        kind="unlogged",
        probe={"vt": "g.get_valid_time_default()"},
    ),
]

# Write-gate callers with no row: every entry says why no crash row can observe it.
EXEMPT: dict[str, str] = {
    "indexes.rs::create_global_index": "unlogged derived index a load rebuilds; no observable difference to compare",
    "indexes.rs::rebuild_indexes": "derived state a load rebuilds; no observable difference to compare",
    "kg_core.rs::rebuild_caches": "derived state a load rebuilds; no observable difference to compare",
    "kg_core.rs::to_bytes": "consolidation for serialisation; logical content unchanged",
    "kg_core.rs::compact": "storage compaction; logical content unchanged",
    "kg_core.rs::verify_unique_constraints": "read-only check routed through the unlogged gate",
    "kg_introspection.rs::reindex": "derived text-index rebuild; logical content unchanged",
    "kg_introspection.rs::enable_disk_mode": "storage-mode switch; logical content unchanged",
    "kg_introspection.rs::unspill": "storage-mode switch; logical content unchanged",
    "kg_introspection.rs::set_auto_vacuum": "runtime policy with no getter and no persisted state",
    "kg_introspection.rs::set_memory_limit": "runtime policy with no getter and no persisted state",
    "kg_introspection.rs::unlock_schema": "inverse of lock_schema, a runtime-only flag whose crash default is unlocked",
    "kg_mutation.rs::_check_relationships": "dry run on a scratch graph; never applies",
}

_COVERED = {c for r in ROWS for c in r.covers} | set(EXEMPT)
_GATE_CALL = re.compile(
    r"\.(logged_write_with_cursor|logged_write_lazy|logged_write|renumbering_write|logged_publish"
    r"|unlogged_write_lazy|unlogged_write)\b"
)
_FN = re.compile(r"\s*(?:pub(?:\([a-z]+\))?\s+)?(?:async\s+)?fn\s+(\w+)")


def gate_callers(source_root: Path) -> set[str]:
    """``file.rs::fn`` for every function (outside the gate itself) that calls a write-gate method."""
    found: set[str] = set()
    for f in sorted(source_root.rglob("*.rs")):
        if f.name == "write_gate.rs":
            continue
        current = None
        for line in f.read_text(encoding="utf-8").splitlines():
            m = _FN.match(line)
            if m:
                current = m.group(1)
            if current and _GATE_CALL.search(line) and not line.lstrip().startswith("//"):
                found.add(f"{f.name}::{current}")
    return found


def test_every_write_gate_caller_has_a_row():
    missing = gate_callers(GATE_DIR) - _COVERED
    assert not missing, f"mutators that call the write gate with no crash row and no exemption: {sorted(missing)}"


def test_every_covered_name_still_exists():
    stale = _COVERED - gate_callers(GATE_DIR)
    assert not stale, f"rows/exemptions naming functions that no longer call the gate: {sorted(stale)}"


def test_the_scan_goes_red_on_an_unlisted_mutator(tmp_path):
    """Non-vacuity: a synthetic new mutator calling the gate is reported as missing."""
    (tmp_path / "newthing.rs").write_text(
        "fn brand_new_mutator(&mut self) {\n    self.logged_write(|g| Ok(()))?;\n}\n", encoding="utf-8"
    )
    assert "newthing.rs::brand_new_mutator" in gate_callers(tmp_path) - _COVERED
    (tmp_path / "comment.rs").write_text("fn quiet(&self) {\n    // self.logged_write(x)\n}\n", encoding="utf-8")
    assert "comment.rs::quiet" not in gate_callers(tmp_path)


def test_row_ids_are_unique():
    ids = [r.id for r in ROWS]
    assert len(ids) == len(set(ids))


_CHILD = """
import json, os, sys, traceback
from pathlib import Path
import pandas as pd
import kglite
from tests.test_durable_mutator_crash_table import ROWS, BASE_SEED, NT, probe

root = Path(sys.argv[1])

results = {}
for row in ROWS:
    work = root / row.id
    work.mkdir()
    variants = ["live"] + (["saved"] if row.kind in ("unlogged", "runtime") else [])
    results[row.id] = {}
    for variant in variants:
        path = work / variant / "app.kgl"
        path.parent.mkdir()
        rec = {}
        try:
            g = kglite.open(str(path), durable="full")
            ns = {"g": g, "pd": pd, "kglite": kglite, "work": work, "NT": NT, "json": json}
            exec(BASE_SEED, ns)
            exec(row.seed, ns)
            rec["before"] = probe(g, row.probe)
            try:
                exec(row.act, ns)
                rec["raised"] = None
            except Exception as e:
                rec["raised"] = f"{type(e).__name__}: {e}"
            if variant == "saved":
                g.save()
            rec["after"] = probe(g, row.probe)
        except Exception:
            rec["error"] = traceback.format_exc()
        results[row.id][variant] = rec
(root / "results.json").write_text(json.dumps(results))
os._exit(0)
"""


@pytest.fixture(scope="module")
def crashed(tmp_path_factory):
    """Run every row in one child, then kill it with ``os._exit``: no save, no close, no finalizers."""
    root = tmp_path_factory.mktemp("crash_table")
    done = subprocess.run(
        [sys.executable, "-c", _CHILD, str(root)], cwd=REPO_ROOT, capture_output=True, text=True, timeout=100
    )
    assert done.returncode == 0, done.stderr[-2000:]
    return root, json.loads((root / "results.json").read_text(encoding="utf-8"))


def _reopen_probe(root: Path, row: Row, variant: str) -> dict:
    g = kglite.open(str(root / row.id / variant / "app.kgl"), durable="full")
    try:
        return probe(g, row.probe)
    finally:
        del g


def _diff(a: dict, b: dict) -> list[str]:
    return [k for k in sorted(set(a) | set(b)) if a.get(k) != b.get(k)]


@pytest.mark.parametrize(
    "row",
    [
        pytest.param(r, id=r.id, marks=pytest.mark.xfail(strict=True, reason=r.known_defect) if r.known_defect else ())
        for r in ROWS
    ],
)
def test_mutator_survives_a_hard_exit(crashed, row):
    root, results = crashed
    live = results[row.id]["live"]
    assert "error" not in live, live.get("error")
    recovered = _reopen_probe(root, row, "live")

    if row.kind == "logged":
        assert live["raised"] is None, live["raised"]
        if row.effective:
            assert _diff(live["before"], live["after"]), "the call changed nothing observable; the row proves nothing"
        assert _diff(live["after"], recovered) == [], (
            f"recovered state differs from live in {_diff(live['after'], recovered)}"
        )
    elif row.kind == "refused":
        assert live["raised"] and "derived from a durable graph" in live["raised"], live["raised"]
        assert live["after"] == live["before"] == recovered
    elif row.kind == "runtime":
        assert live["raised"] is None, live["raised"]
        assert _diff(live["before"], live["after"]), "the call changed nothing observable; the row proves nothing"
        assert recovered == live["before"], "a runtime-only flag was persisted by a crash"
        saved = results[row.id]["saved"]
        assert _reopen_probe(root, row, "saved") == saved["before"], "a runtime-only flag was kept by save()"
    else:
        assert live["raised"] is None, live["raised"]
        assert _diff(live["before"], live["after"]), "the call changed nothing observable; the row proves nothing"
        assert recovered == live["before"], (
            f"checkpoint-only metadata survived a crash without save(): {_diff(live['before'], recovered)}"
        )
        saved = results[row.id]["saved"]
        assert "error" not in saved, saved.get("error")
        assert _diff(saved["before"], saved["after"])
        assert _reopen_probe(root, row, "saved") == saved["after"], "save() did not keep the metadata"


# The names the child imports.
BASE_SEED = _BASE_SEED
NT = _NT
