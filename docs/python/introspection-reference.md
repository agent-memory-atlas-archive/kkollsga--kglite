# Introspection reference

`graph_info()`, `schema()` and `describe()` report on a graph. Use the first two for code and `describe()` for an agent.

| Method | Returns | Use it for |
|---|---|---|
| `graph_info()` | `dict` | Storage health, versions, build and load findings. |
| `schema()` | `dict` | Types, counts, property types and indexes. |
| `describe()` | XML `str` | Reading by an agent. Do not parse it. |

The tables below are generated from the source, so they list every key the installed version can return.

(graph-info-reference)=
## `graph_info()` keys

`graph_info()` returns a flat `dict`. Keys marked "always" are present on every graph.

<!-- BEGIN GENERATED: graph-info (scripts/render_docs_facts.py) -->

| Key | Type | Present | Meaning |
|---|---|---|---|
| `node_count` | int | always | Number of live nodes. |
| `node_capacity` | int | always | Upper bound of node indices (includes tombstones). |
| `node_tombstones` | int | always | Number of wasted slots from deletions. |
| `edge_count` | int | always | Number of live edges. |
| `edge_capacity` | int | always | Upper bound of edge indices (includes slots freed by relationship deletes). |
| `edge_tombstones` | int | always | Wasted edge slots. |
| `fragmentation_ratio` | float | always | Ratio of wasted *node* storage (0.0 = clean). |
| `type_count` | int | always | Number of distinct node types. |
| `property_index_count` | int | always | Number of single-property indexes. |
| `composite_index_count` | int | always | Number of composite indexes. |
| `format_version` | int | always | `.kgl` on-disk layout version (engine-owned). |
| `library_version` | str | always | Kglite version that last saved the graph. |
| `user_schema_version` | int | always | Your data-model revision (see `schema_version`); `0` when unversioned. |
| `storage_mode` | str | always | `"memory"`, `"mapped"` or `"disk"` — the backend the graph is actually running on. |
| `columnar_heap_bytes` | int | always | Heap-resident bytes in the property columns. |
| `columnar_is_mapped` | bool | always | Whether any *property column* is file-backed rather than heap-resident (after a spill, or on a graph opened from a file). |
| `edges_mapped` | bool | always | Whether the edge CSR arrays are memory-mapped from files. `True` on a disk graph whose CSR is materialized; always `False` on the memory and mapped backends, which keep edges in the heap graph and have no CSR. |
| `edge_property_overlay_rows` | int | always | Edges whose properties are held in the disk backend's heap mutation overlay rather than the mmap-backed base. `0` on every non-disk backend. |
| `memory_limit` | int or None | always | Configured memory limit (None if unset). |
| `columnar_total_rows` | int | always | Total property-column rows, including rows orphaned by deleted nodes. |
| `columnar_live_rows` | int | always | Rows backed by live nodes. |
| `auto_vacuum_threshold` | float or None | always | The configured auto-vacuum threshold, or `None` when disabled (see `set_auto_vacuum`). |
| `auto_vacuums_run` | int | always | How many times auto-vacuum has fired on this graph object. |
| `valid_time_default` | dict | always | The default an undated statement reads; see the nested keys below. |
| `build` | dict | Graph built by `from_blueprint()` | Advisories of the build, by group; see the nested keys below. Saved with the graph and restored on load. |
| `advisories` | list of dict | Graph loaded from a file whose data shows an older build's defect | Known-defect findings from the load. Each entry also raised one `UserWarning`. |

| Under | Key | Type | Meaning |
|---|---|---|---|
| `valid_time_default` | `effective` | str | Default in force now: `'today'`, `'all'` or a date. |
| `valid_time_default` | `stored` | str | Part of the default saved in the file. |
| `build` | `summary` | dict[str, int] | Count of advisories per group, over all of them. |
| `build` | `diagnostics` | list of dict | At most the 100 most severe advisories. |
| `build.diagnostics[]` | `group` | str | `declarations`, `stubs`, `data_shape`, `data_quality` or `cosmetic`. |
| `build.diagnostics[]` | `kind` | str | Stable advisory kind. |
| `build.diagnostics[]` | `message` | str | Human-readable text. |
| `advisories[]` | `code` | str | Stable advisory code. |
| `advisories[]` | `writer` | str | Oldest kglite version that wrote the data. |
| `advisories[]` | `message` | str | Human-readable text. |
| `advisories[]` | `affected` | list of str | Matched node types. |

<!-- END GENERATED: graph-info -->

Two keys appear only in some situations:

- `build` is absent on a graph that `from_blueprint()` did not build. A clean build has an empty `summary`.
- `advisories` is absent unless the load found data that shows an older build's defect. See {ref}`files-written-by-older-versions`.

(schema-reference)=
## `schema()` shape

`schema()` returns a `dict` with five top-level keys. It scans every edge once, so its cost grows with the edge count.

<!-- BEGIN GENERATED: schema (scripts/render_docs_facts.py) -->

Top level:

| Key | Type | Meaning |
|---|---|---|
| `node_types` | dict | `{type_name: node type entry}`. |
| `connection_types` | dict | `{relationship_type: relationship type entry}`. |
| `indexes` | list of str | One `"Type.property"` string per index. |
| `node_count` | int | Total live nodes. |
| `edge_count` | int | Total live edges. |

Node type entry, `schema()['node_types'][name]`:

| Key | Type | Meaning |
|---|---|---|
| `count` | int | Nodes of the type. |
| `properties` | dict[str, str] | `{property_name: type_name}`, for example `{"age": "Int64"}`. |

Relationship type entry, `schema()['connection_types'][name]`:

| Key | Type | Meaning |
|---|---|---|
| `count` | int | Relationships of the type. |
| `source_types` | list of str | Node types at the start of the relationship. |
| `target_types` | list of str | Node types at the end of the relationship. |
| `properties` | dict[str, str] | `{property_name: type_name}` of the relationship's own properties; `{}` if none. |

<!-- END GENERATED: schema -->

Type names in `properties` are the engine's names, such as `Int64`, `Float64`, `String`, `Boolean`. A relationship type with no properties has an empty `properties` dict.

For per-property statistics use `properties(node_type)`. For one type's incoming and outgoing relationships use `neighbors_schema(node_type)`.

(describe-reference)=
## `describe()` format

`describe()` returns an XML string. The shape is stable enough for an agent to read and not meant for a parser. Use `schema()` or `graph_info()` when you need structured data.

Each call selects one view:

<!-- BEGIN GENERATED: describe (scripts/render_docs_facts.py) -->

| Call | Returns |
|---|---|
| `describe()` | Overview: inventory of types and connections; large graphs get a summary. |
| `describe(types=[...])` | Detail for named types: properties, example, connections, samples. |
| `describe(type_search='x')` | Types whose name contains `x`, with neighbours. |
| `describe(connections=True)` | Every relationship type with counts, endpoint types and properties. |
| `describe(connections=[...])` | Deep dive per relationship type: endpoint pairs, property stats, samples. |
| `describe(cypher=...)`, `describe(fluent=...)` | Reference text for the Cypher dialect or the fluent API, or one topic of each. |

| Element | Appears in | Attributes and content |
|---|---|---|
| `<graph>` | every call | Root. `kglite_version`; the overview adds `nodes`, `edges`. |
| `<conventions>` | overview | Text: node `.id` and `.title`, and the special property kinds present. |
| `<read-only>` | overview | Text; present when `read_only(True)` is set. |
| `<schema-locked>` | overview | Text; present after `lock_schema()`. |
| `<user-schema-version>` | overview | Text; present when `schema_version` is not 0. |
| `<data-advisory>` | overview | `code`, `writer`; text is the message. One per load advisory. |
| `<valid-time-default>` | overview | `effective`, `stored`; present when the default is not `today` on a graph that declares validity. |
| `<ontology>` | overview, `types=` | `classes`, `relationships`; the declared semantic layer. |
| `<types>` | overview | Wraps one `type` per node type. |
| `<type>` | overview, `types=` | `name`, `count`; children below. |
| `<properties>` | inside `type`, `<conn>` deep-dive | One `prop` per property. |
| `<prop>` | inside `properties` | `name`, `type`, `unique`, `coverage` (only if under 100%), `vals`. |
| `<example>` | inside `type`, `types=` | `query`: a Cypher query anchored on the type's identifier. |
| `<connections>` | overview, `connections=` | Wraps relationship entries; inside `type` it wraps `out` and `in`. |
| `<conn>` | overview, `connections=True` | `type`, `count`, `from`, `to`, `properties` (`name:Type` list). |
| `<out>` | inside `connections` | `type`, `target`, `count`. |
| `<in>` | inside `connections` | `type`, `source`, `count`. |
| `<samples>` | inside `type`, `connections=[...]` | Sample `node` or `edge` elements. |
| `<endpoints>` | `connections=[...]` | One `pair` (`from`, `to`, `count`) per endpoint pair; `<more pairs= edges=/>` marks a hidden tail. |
| `<type_search>` | `type_search=` | `pattern`, `matches`, `depth`; one `match` per type plus a `hint`. |
| `<embeddings>` | inside `type` or a relationship | `text_col`, `dim`, `count`. |
| `<text_index>` | inside `type` | `text_col`: a BM25 text index. |
| `<skills>` | overview | Graph-carried skills; present only when the graph carries some. |
| `<recipes>` | overview | Graph-carried recipe queries; present only when the graph carries some. |
| `<extensions>` | overview | Hint elements: `algorithms`, `rules`, `cypher`, `fluent_api`, `connections`, `temporal`, `bug_report`, `indexing`; `timeseries` and `spatial` when used. |
| `<type_distribution>` | extreme-scale overview | `by_size` buckets and a `top` list. |
| `<connection_summary>` | extreme-scale overview | `count`; a `top` list and a `more` marker. |
| `<exploration_hints>` | overview | Lists `disconnected` types and `join_candidates`. |
| `<more>` | any truncated list | `count` (or `pairs`/`edges`, `out`/`in`) and a `hint` for the next call. |

<!-- END GENERATED: describe -->

Rules for reading the output:

- Attribute `vals=` lists sample values, joined by `|` and truncated to `sample_truncate` characters.
- A property present on only some nodes carries `coverage`. A fully populated one has no `coverage`.
- When `type_search`, `connections`, `cypher` or `fluent` is set, the output holds only those views and no node inventory.
- A graph with nothing to report omits an optional element instead of emitting an empty one.
- The element list above is checked against the engine source. A new element can exist before it has a row here.

See {doc}`guides/valid-time` for the validity information `describe()` shows, and the {doc}`Python API reference <../autoapi/index>` for the full `describe()` signature.
