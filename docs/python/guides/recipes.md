# Common Recipes

This page holds short, copy-paste examples for common tasks.

```{note}
"Recipe" means two different things in KGLite.

- This page is snippets to copy into your own code.
- A **recipe query** is a named, parameterised, read-only Cypher statement. It is stored in a manifest or in the graph and served to an agent as `run_recipe_query`. For those see the `extensions.cypher_recipes` section of {doc}`mcp-servers`.
```

## Upsert with MERGE

```python
graph.cypher("""
    MERGE (p:Person {email: 'alice@example.com'})
    ON CREATE SET p.created = '2024-01-01', p.name = 'Alice'
    ON MATCH SET p.last_seen = '2024-01-15'
""")
```

## Top-K Nodes by Centrality

Two equivalent forms exist. Pick by who is calling.

In agent, manifest and Cypher-tool contexts, use the `CALL` form. Everything reaches KGLite through `cypher()` there:

```python
graph.cypher("""
    CALL pagerank() YIELD node, score
    RETURN node.title AS title, score
    ORDER BY score DESC LIMIT 10
""")
```

For Python-only callers, the inherent method is shorter and skips the parser:

```python
top_nodes = graph.pagerank(top_k=10)
for node in top_nodes:
    print(f"{node['title']}: {node['score']:.3f}")
```

The same `CALL <algo>() YIELD ...` shape works for the other graph algorithms. See {doc}`graph-algorithms` for the full list.

## 2-Hop Neighborhood

```python
graph.cypher("""
    MATCH (me:Person {name: 'Alice'})-[:KNOWS*2]-(fof:Person)
    WHERE fof <> me
    RETURN DISTINCT fof.name
""")
```

## Export Subgraph

```python
subgraph = (
    graph.select('Person')
    .where({'name': 'Alice'})
    .expand(hops=2)
    .to_subgraph()
)
subgraph.export('alice_network.graphml', format='graphml')
```

## Parameterized Queries

```python
graph.cypher(
    "MATCH (p:Person) WHERE p.city = $city AND p.age > $min_age RETURN p.name",
    params={'city': 'Oslo', 'min_age': 25}
)
```

## Delete Subgraph

```python
graph.cypher("""
    MATCH (u:User) WHERE u.status = 'inactive'
    DETACH DELETE u
""")
```

## Aggregation with Relationship Properties

```python
graph.cypher("""
    MATCH (p:Person)-[r:RATED]->(m:Movie)
    RETURN p.name, avg(r.score) AS avg_rating, count(m) AS movies_rated
    ORDER BY avg_rating DESC
""")
```

## Replace Cypher `SET` aggregations with `add_properties()`

The fluent `add_properties()` API can replace imperative Cypher `SET` after an aggregation `MATCH`. If you recompute summary properties on parent nodes that way, `add_properties()` expresses the same operation declaratively in a single chain.

The hub-aggregation pattern is the textbook fit: `count of children`, `sum of a child property`, `most-recent timestamp`.

### Cypher way — imperative `SET` per metric

```python
# Compute three aggregates on Project nodes from their child Wells:
# n_wells, total_production, max_depth.
graph.cypher("""
    MATCH (f:Project)-[:HAS_WELL]->(w:Well)
    WITH f, count(w) AS n_wells,
              sum(w.production) AS total_prod,
              max(w.depth) AS max_depth
    SET f.n_wells = n_wells,
        f.total_production = total_prod,
        f.deepest_well = max_depth
""")
```

This works, with four drawbacks:

- Each metric is a separate string.
- Mistyping a property name fails silently.
- The schema for the new properties isn't visible to `g.describe()` until the query runs.
- The call writes to the graph in place rather than yielding a re-queryable view.

### Fluent way — declarative `add_properties` with `Agg`

```python
from kglite import Agg

graph.select('Project').traverse('HAS_WELL') \
    .add_properties({'Well': {
        'n_wells': Agg.count(),
        'total_production': Agg.sum('production'),
        'deepest_well': Agg.max('depth'),
    }})
```

The effect on the graph is the same. Three things differ:

- The metric definitions are typed expressions (autocompletes in IDEs).
- The traversal direction is explicit.
- You can chain further (`.collect()`, `.to_df()`) without re-querying.

### Spatial hub aggregation

If the parent has geometry, mix `Spatial.*` helpers in:

```python
from kglite import Agg, Spatial

graph.select('Structure').compare('Well', 'contains') \
    .add_properties({'Structure': {
        'wells_inside': Agg.count(),
        'avg_well_depth': Agg.mean('depth'),
        'struct_area_m2': Spatial.area(),
        'mean_dist_to_centroid': Spatial.distance(),
    }})
```

The `compare('Well', 'contains')` step uses the spatial index. It is one query whether you have 1k wells or 1M.

### When to keep the Cypher form

`SET` is still the right tool in three cases:

- The aggregation needs `WHERE` clauses on relationship properties that `add_properties()` doesn't expose (e.g. `WHERE r.score > 0.5` on the edge between parent and child).
- You mutate properties in place that depend on other in-place mutations within the same query (multi-step `WITH` pipelines).
- The aggregation crosses three or more node types in non-hierarchy ways (e.g. both up- and down-walks from the same anchor).

In every other case the fluent form is type-checkable, cheaper to read, and re-runnable without re-querying.

### Helper reference

**`Agg`:** `count()`, `sum(prop)`, `mean(prop)`, `min(prop)`,
`max(prop)`, `std(prop)`, `collect(prop)`

**`Spatial`:** `distance()`, `area()`, `perimeter()`,
`centroid_lat()`, `centroid_lon()`

The raw string forms (`'count(*)'`, `'mean(depth)'`, `'distance'`, etc.) still work. The helpers just return those strings, so you can mix both when adapting older code incrementally.

See [`docs/guides/traversal-hierarchy.md`](traversal-hierarchy.md) for the full traversal-API context.
