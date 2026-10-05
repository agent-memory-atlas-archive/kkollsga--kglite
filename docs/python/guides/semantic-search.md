# Semantic Search

Store embedding vectors alongside nodes and query them with fast similarity search. Embeddings are stored separately from node properties. They don't appear in `collect()`, `to_df()`, or regular Cypher property access.

## Text-Level API (Recommended)

Register an embedding model once, then embed and search using text column names. The model runs on the Python side. KGLite only stores the resulting vectors.

```python
from sentence_transformers import SentenceTransformer

class Embedder:
    def __init__(self, model_name="all-MiniLM-L6-v2"):
        self._model_name = model_name
        self._model = None
        self._timer = None
        # Final registration metadata: set_embedder() snapshots this value.
        self.dimension = 384

    def load(self):
        """Called automatically before embedding. Loads model on demand."""
        import threading
        if self._timer:
            self._timer.cancel()
            self._timer = None
        if self._model is None:
            self._model = SentenceTransformer(self._model_name)

    def unload(self, cooldown=60):
        """Called automatically after embedding. Releases after cooldown."""
        import threading
        def _release():
            self._model = None
            self._timer = None
        self._timer = threading.Timer(cooldown, _release)
        self._timer.start()

    def embed(self, texts: list[str]) -> list[list[float]]:
        return self._model.encode(texts).tolist()

# Register once on the graph
graph.set_embedder(Embedder())

# Embed a text column — stores vectors as "summary_emb" automatically
graph.embed_texts("Article", "summary")
# Embedding Article.summary: 100%|████████| 1000/1000 [00:05<00:00]
# → {'embedded': 1000, 'skipped': 3, 'skipped_existing': 0, 'dimension': 384}

# Search with text — resolves "summary" → "summary_emb" internally
results = graph.select("Article").search_text("summary", "machine learning", top_k=10)
# [{'id': 42, 'title': '...', 'type': 'Article', 'score': 0.95, ...}, ...]
```

**Key details:**

- **Auto-naming:** text column `"summary"` → store name `"summary_emb"` (auto-derived).
- **Incremental, three modes:** `embed_texts(mode=…)` takes one of:
  - `'missing'` (default) embeds only nodes without a vector.
  - `'changed'` also re-embeds nodes whose **text changed** since the last pass. A per-node content hash is stored to detect this.
  - `'all'` rebuilds the whole store.
- **Model provenance:** an incremental pass over a store named for model A requires the registered model to name itself A.
  - Model B, or an unnamed model, is refused before model work. Use `mode='all'` to rebuild and assign the current model identity.
  - An unknown/mixed store can be incrementally refreshed, but stays `model=None`.
  - Only a full rebuild restores named aggregate provenance.
- **Progress bar:** a tqdm progress bar shows by default. Disable it with `show_progress=False`.
- **Load/unload lifecycle:** if the model has optional `load()` / `unload()` methods, they are called automatically before and after each embedding operation.
- **Registration metadata:** `set_embedder()` snapshots `dimension` and the optional `model_id` / `model_name`.
  - Set their final values before registration. Register again to adopt changed metadata.
  - The model object itself is **not** saved with `save()`. Call `set_embedder()` again after deserializing.

```python
# Add new articles, then re-embed — only new ones are processed
graph.embed_texts("Article", "summary")
# → {'embedded': 50, 'skipped': 0, 'skipped_existing': 1000, 'reembedded_changed': 0, ...}

# Edit some article summaries, then re-embed ONLY what changed:
graph.embed_texts("Article", "summary", mode="changed")
# → {'embedded': 12, 'reembedded_changed': 12, 'skipped_existing': 1038, ...}

# Inspect provenance (dimension, count, model id, metric, #hashed):
graph.embedding_info("Article", "summary")
# → {'dimension': 384, 'count': 1050, 'model': 'all-MiniLM-L6-v2', 'metric': 'cosine', 'hashed': 1050}
# Or just the dimension (cheap; None if no store) — handy to detect a model swap:
graph.embedding_dim("Article", "summary")   # → 384

# Combine with filters
results = (graph
    .select("Article")
    .where({"category": "politics"})
    .search_text("summary", "foreign policy", top_k=10))
```

### Carrying vectors across a rebuild

A common workflow rebuilds a fresh graph from a source of truth on each load. The vectors then need carrying forward.

`copy_embeddings_from` does it in one call, matched by node id. It carries the dimension, metric, model id and per-node text hashes. A following `mode='changed'` therefore only re-embeds genuinely new text:

```python
new_graph = build_from_source()              # fresh, no vectors yet
new_graph.copy_embeddings_from(old_graph)    # carry every store by node id
new_graph.embed_texts("Article", "summary", mode="changed")  # fill only the new/changed
# → {'stores_copied': 1, 'vectors_copied': 1050, 'vectors_skipped': 0,
#    'relationship_stores_copied': 0, ...}  (from copy_embeddings_from)
```

## Low-Level Vector API

If you manage vectors yourself, use the low-level API.

### Storing Embeddings

```python
# Explicit: pass a dict of {node_id: vector}.
# set_embeddings REPLACES the whole store for ('Article', 'summary_emb').
graph.set_embeddings('Article', 'summary', {
    1: [0.1, 0.2, 0.3, ...],
    2: [0.4, 0.5, 0.6, ...],
})

# Or auto-detect during add_nodes with column_types
df = pd.DataFrame({
    'id': [1, 2, 3],
    'title': ['A', 'B', 'C'],
    'text_emb': [[0.1, 0.2], [0.3, 0.4], [0.5, 0.6]],
})
graph.add_nodes(df, 'Doc', 'id', 'title', column_types={'text_emb': 'embedding'})
```

### Incremental ingest — `add_embeddings`

`set_embeddings` is a **full replace**. Each call discards the existing store for `(node_type, '{text_column}_emb')`.

Ingesting documents in batches breaks under this rule: embed chunks for doc A, then doc B, then doc C. A second `set_embeddings` call would wipe doc A's vectors.

Use `add_embeddings` for batches. It **upserts** into the existing store, creating it on the first call. Batches coexist without a read-merge-write cycle in your own code:

```python
graph.add_embeddings('Chunk', 'text', {  # doc A's chunks
    'a:1': [0.1, 0.2, ...],
    'a:2': [0.3, 0.4, ...],
})
graph.add_embeddings('Chunk', 'text', {  # doc B's chunks — A's survive
    'b:1': [0.5, 0.6, ...],
})
# -> {'embeddings_stored': int, 'dimension': int, 'skipped': int, 'store_created': bool}
```

Reach for `set_embeddings` only when you genuinely want to replace the entire store (e.g. re-embedding everything with a new model).

Both calls share these rules:

- `text_column` must name a property that exists on the node type (`id`, `title` and `type` are always accepted). This guard catches passing the store name `'summary_emb'` where the column name `'summary'` belongs.
- Both resolve every id, check every dimension, and reject NaN or infinite coordinates before writing. A rejected batch leaves the store as it was.
- Both count ids that match no node in `skipped`.

A store built this way records exactly what you supply: the vectors, their dimension, and the metric. `embed_texts()` additionally records the embedder's `model_id` and per-node text hashes.

Provenance changes follow these rules:

- A real `add_embeddings()` upsert into a generated store clears the affected rows' hashes and changes aggregate model provenance to `None`.
- A batch containing only unknown IDs changes neither.
- Incremental generation over unknown/mixed provenance uses text freshness only and keeps `model=None`.
- Use `mode='all'` to restore one named model provenance.

Call `save()` to persist a store. Embeddings ride the checkpoint, so a durable graph writes them at `save()` rather than per-call.

### Vector Search

Query vectors must contain only finite coordinates. NaN and either infinity are rejected before exact or indexed scoring.

Each hit is a dict with `id`, `title`, `type`, `score`, **and all node properties**.

- `score` is always present (every metric).
- Properties are read live from the node, so a hit carries the same fields before and after `save()` + reload.
- You don't need a follow-up `MATCH ... WHERE id IN [...]` to recover properties.

```python
# Basic search — each hit carries id, title, type, score AND every node
# property (read live, so no follow-up MATCH...WHERE id IN [...] hydrate needed).
results = graph.select('Article').vector_search('summary', query_vec, top_k=10)
# [{'id': 5, 'title': '...', 'type': 'Article', 'score': 0.95, ...all props...}, ...]

# Trim the payload with returning= → id + score + only the named fields
# (ranking-heavy or wide-node workloads):
ranked = graph.select('Article').vector_search(
    'summary', query_vec, top_k=50, returning=['title'])   # → {'id', 'score', 'title'}

# Filtered search — only search within a subset
results = (graph
    .select('Article')
    .where({'category': 'politics'})
    .vector_search('summary', query_vec, top_k=10))

# DataFrame output
df = graph.select('Article').vector_search('summary', query_vec, top_k=10, to_df=True)

# Distance metrics: 'cosine' (default), 'dot_product', 'euclidean', 'poincare'
results = graph.select('Article').vector_search(
    'summary', query_vec, top_k=10, metric='dot_product')
```

### Scaling search with an index (HNSW)

By default, vector search is an **exact** brute-force scan: every candidate is scored. That is the right choice for small and medium stores and for filtered searches. On a large corpus, scoring every vector on every query doesn't scale.

Build an **HNSW** approximate-nearest-neighbour index once, and whole-corpus queries become sub-linear:

```python
graph.embed_texts('Article', 'summary')          # produce vectors
graph.build_vector_index('Article', 'summary')    # opt in (like create_index)

# vector_search / search_text now auto-use the index for whole-corpus queries:
hits = graph.select('Article').search_text('summary', 'machine learning', top_k=10)

# ...and for a whole-graph search, even when the graph holds other node types:
hits = graph.search_text('summary', 'machine learning', top_k=10)

# Force an exact scan when you need guaranteed-exact results:
hits = graph.select('Article').vector_search('summary', query_vec, top_k=10, exact=True)
```

The index behaves like `create_index`: it is **opt-in**, and once built it is used automatically. Key points:

- **Auto-use, with an escape hatch.** A query that covers most of a large indexed store (≥400 covered vectors) uses the index. `exact=True` always forces the exact scan. Returned scores are on the exact same scale as the brute-force path, because the index only narrows *which* nodes get scored.
- **The selection doesn't have to be that one type.** While only one node type carries the column, the index is used whatever else the selection spans. That includes a whole-graph `graph.vector_search('summary', q)` on a graph full of other node types. Nodes of other types simply have no vector in the store and are skipped, exactly as the exact scan skips them.
- **Two embedded types are ranked exactly.** Suppose both `Article.summary` and `Note.summary` are embedded. A selection spanning *both* falls back to an exact global ranking, because using one type's index would silently drop the other type's rows. Select a single type to get the index back.
- **Filtered queries stay exact.** A selective `.where(...)` before the search falls back to an exact scan automatically: correctness over speed when a filter is tight. An index helps "search the whole corpus", not "search a small filtered slice".
- **Approximate.** Recall depends on your data and `ef_search`.
  - Well-structured embeddings (sentence-transformers, bge, OpenAI, etc.) typically get ≥0.99 recall@10 at the defaults.
  - Raise `ef_search` for higher recall at some latency cost.
  - Use `exact=True` when you can't tolerate any miss.

  > **Benchmark HNSW on *real* embeddings, not random vectors.** Random unit
  > vectors in high dimensions have no neighbourhood structure — every pair is
  > nearly orthogonal (all cosine sims ≈ 0) — so *any* ANN scores terribly on
  > them (recall can look like ~0.2). That's the curse of dimensionality, not an
  > engine defect: on real embeddings the same index hits ~0.99. If you must
  > sanity-check on synthetic data, query with *stored* vectors (which have a
  > true nearest neighbour) rather than fresh random ones.
- **Metrics.** cosine / dot_product / euclidean are indexable. `poincare` always uses the exact path.
- **Lifecycle.** Writing vectors (`add_embeddings`, `embed_texts`, `set_embeddings`) does **not** drop the index. The write is recorded, and the next vector query folds it in while the outstanding delta stays at or under `auto_refresh_limit` (default 1000). A larger delta is served by the exact scan (correct, and slower) until you rebuild or call `refresh_vector_index(...)`.
  - **What drops the index:** a change to the slot layout it addresses.
    - Deleting an embedded node (the delete prunes its vector).
    - A `vacuum()` that compacts. Its result reports `tombstones_removed > 0`, whether you called it or auto-vacuum ran it after a large delete. It drops **every** vector index in the graph, node and relationship, including those on types that saw no delete. On disk, `vacuum()` is a no-op.
  - **What leaves it in place:** a delete that a failed statement or a rolled-back transaction undoes.
  - **Rebuilding:** rebuild after either of those. `refresh_vector_index(...)` folds in a delta but never builds. It refuses while no index is built, naming the `build_vector_index(...)` call.
  - **Inspecting:** `SHOW INDEXES` reports `stale` / `delta`, plus `unembedded`: the nodes with no vector at all, which catch-up never embeds. Check with `has_vector_index(...)`, remove with `drop_vector_index(...)`.
- **Persisted.** The index is saved inside the `.kgl` (and `to_bytes()`), so a reloaded graph keeps it. No rebuild on load. A disk-mode graph's directory keeps neither node nor relationship indexes, so rebuild after reopening a disk graph.

```python
graph.build_vector_index(
    'Article', 'summary',
    m=16,                 # neighbours per node (higher → better recall, larger index)
    ef_construction=200,  # build-time search width
    ef_search=64,         # default query-time width (higher → better recall, slower)
)
graph.has_vector_index('Article', 'summary')   # True
graph.save('articles.kgl')                       # index travels with the file
```

#### Recall on hard corpora

Recall is a property of **your vectors**, not only of the index. HNSW walks a neighbourhood graph, so it needs neighbourhoods.

- Vectors with real cluster or low-rank structure, such as a sentence-embedding model produces, are found essentially exactly at the defaults.
- Uniform or independent high-dimensional random vectors have no such structure: every pair is nearly orthogonal. The walk has nothing to follow, and recall degrades with **both** corpus size and dimension.

That is the corpus, not a defect. The same index on the same engine is at ~1.0 on the structured corpus and at 0.4 on the unstructured one.

Measured with `tests/benchmarks/bench_vector_index.py --recall-sweep`. Setup: release build, `m=16`, `ef_construction=200`, `ef_search=64`, cosine, `top_k=10`. Queries are *stored* vectors, and recall@10 is against the exact scan. The range spans two runs:

| corpus | 20k×128 | 50k×128 | 100k×128 | 20k×384 | 50k×384 | 100k×384 |
| --- | --- | --- | --- | --- | --- | --- |
| clustered / low-rank (realistic) | ≥0.99 | ≥0.99 | ≥0.99 | ≥0.99 | ≥0.99 | ≥0.99 |
| independent Gaussian (adversarial) | 0.94 | 0.80 | 0.62–0.71 | 0.77–0.80 | 0.52–0.57 | 0.42 |

**The knobs are `ef_search`, `ef_construction` and `m`**, all arguments of `build_vector_index`. `ef_search` (default 64) is the query-time one. Its effect is very different on the two corpora:

- On the **clustered** corpus there is nothing to buy. Recall is already ≥0.99 at 64, and raising it only costs latency (100k×384: 0.10 ms at 64, 0.13 ms at 128, 0.19 ms at 256).
- On the **adversarial** corpus it helps, but far less than it costs.
  - At 100k×384: recall 0.42 → 0.43–0.45 → 0.46–0.50 for ef_search 64 → 128 → 256, while latency goes 0.17–0.19 → 0.27–0.33 → 0.49 ms.
  - At 100k×128: 0.62–0.71 → 0.70–0.73 → 0.77–0.80.
  - Index construction inserts concurrently. Two builds of the same corpus therefore differ by ±0.04 recall on this corpus (±0.005 on the clustered one). An ef_search step of 64 → 128 is inside that noise.

So `ef_search=64` stays the default. It is exact-grade on the corpora the feature exists for, and no reachable setting rescues a corpus with no neighbourhood structure.

If your vectors are in that regime, **`exact=True` is always available** and, at these sizes, cheap. The exact scan is 0.7 ms at 100k×128 and 1.6 ms at 100k×384 (it parallelises above 10k vectors).

> The Cypher `text_score()` / `vector_score()` whole-corpus top-k
> (`RETURN vector_score(n, prop, q) AS s ORDER BY s DESC LIMIT k`) auto-uses the
> index too — so agent/MCP semantic search via Cypher benefits as well. The
> end-to-end win is smaller than the fluent API's, though: Cypher's fixed
> per-query cost (parse + plan + projection) is a bigger share of the total, so
> the index saving shows through less at small/medium corpus sizes and widens as
> the corpus grows. A heavily-filtered Cypher query (selective `WHERE`) stays
> exact.

### Choosing a Distance Metric

| Metric | Best for | Why |
|--------|----------|-----|
| `cosine` | General-purpose text/semantic embeddings (OpenAI, Sentence-Transformers, Cohere) | Compares direction, ignores magnitude. Works well when embeddings are normalized or you only care about semantic similarity. |
| `dot_product` | Embeddings where magnitude encodes relevance (MIPS) | Like cosine but magnitude matters — a longer vector scores higher. Useful when the model encodes "importance" in the norm. |
| `euclidean` | Spatial/geometric data, clustering, k-means style lookups | Raw geometric distance. Best when absolute position in the space matters, not just angle. |
| `poincare` | Hierarchical/taxonomic data (ontologies, org charts, category trees) | Hyperbolic geometry naturally encodes tree structure. Nodes near the origin are roots; nodes near the boundary are leaves. 5D Poincaré can outperform 200D Euclidean on hierarchy tasks. |

**Rule of thumb:** If you're using off-the-shelf text embeddings, use `cosine`. If your data has inherent hierarchy and you've trained Poincaré embeddings, use `poincare`.

### Stored Metric

When embeddings are trained for a specific geometry, store the intended metric alongside them. It then becomes the default at query time:

```python
# Store Poincaré embeddings with their intended metric
graph.set_embeddings('Concept', 'title', poincare_vectors, metric='poincare')

# Queries now default to poincaré distance — no need to pass metric= each time
results = graph.select('Concept').vector_search('title', query_vec, top_k=10)

# You can still override explicitly
results = graph.select('Concept').vector_search(
    'title', query_vec, top_k=10, metric='cosine')

# list_embeddings() shows the stored metric
graph.list_embeddings()
# [{'entity': 'node', 'node_type': 'Concept', 'text_column': 'title',
#   'store_name': 'title_emb', 'dimension': 5, 'count': 500, 'metric': 'poincare'}]
```

Metric resolution order: explicit `metric=` argument > stored metric > `cosine` default.

### Semantic Search in Cypher

`text_score()` enables semantic search directly in Cypher queries. Its query argument decides how it works:

- A **string** query is embedded with the registered model (via `set_embedder()`) before scoring.
- A **list** is scored directly as your own query vector. It needs only the embedding store.

```python
# Requires: set_embedder() + embed_texts()
graph.cypher("""
    MATCH (n:Article)
    RETURN n.title, text_score(n, 'summary', 'machine learning') AS score
    ORDER BY score DESC LIMIT 10
""")

# With parameters
graph.cypher("""
    MATCH (n:Article)
    WHERE text_score(n, 'summary', $query) > 0.8
    RETURN n.title
""", params={'query': 'artificial intelligence'})

# With explicit metric
graph.cypher("""
    MATCH (n:Article)
    RETURN n.title, text_score(n, 'summary', 'machine learning', 'poincare') AS score
    ORDER BY score DESC LIMIT 10
""")

# Combine with graph filters
graph.cypher("""
    MATCH (n:Article)-[:CITED_BY]->(m:Article)
    WHERE n.category = 'politics'
    RETURN m.title, text_score(m, 'summary', 'foreign policy') AS score
    ORDER BY score DESC LIMIT 5
""")
```

Both scoring functions take a **pre-computed vector**, so scoring works with the embedding store alone:

```python
# Same scores, same ordering — the store is all either one needs.
graph.cypher("MATCH (n:Article) RETURN vector_score(n, 'summary_emb', $q) AS s",
             params={'q': query_vec})    # names the store
graph.cypher("MATCH (n:Article) RETURN text_score(n, 'summary', $q) AS s",
             params={'q': query_vec})    # names the column
```

The query argument's type decides how `text_score` reads it: a list is a vector, a string is text. A stringified vector like `'[1.0, 2.0]'` is therefore embedded as a 10-character query. Pass a list and both spellings agree.

`vector_score` is the Cypher counterpart of the fluent `vector_search()` method. The surfaces differ:

- `text_score()` / `vector_score()` are **Cypher functions**, used in `RETURN` / `WHERE`.
- `search_text()` / `vector_search()` are **fluent methods** on a selection.

#### Filter out unembedded rows before a `DESC` top-k

A node or relationship the store holds no vector for scores `null`. openCypher sorts `null` above every value, so `ORDER BY score DESC LIMIT k` returns the unembedded entities *first*.

Add a filter to the `MATCH`: `WHERE vector_score(n, 'summary_emb', $q) IS NOT NULL`,
or the `text_score` form, or `r` for a relationship.

- In the `MATCH`, it drops those rows. The query is served from the store at the cost of the store procedure, HNSW included.
- Written after the projection as `WITH … WHERE score IS NOT NULL`, it leaves the fused route and scores every row.

Without the filter, the routes behave as follows:

- A node type with unembedded members is still answered from its store: the null-scored nodes first, in the type's order, then the store's ranking. It reports `fallback_reason: 'row_coverage'` only when all `k` rows are null or the store's order differs from the type's.
- A relationship type with unembedded members is answered by row scan (`row_coverage`).

`vector_search()`, `search_text()` and `db.relationship_embeddings.query` rank stored vectors only. They never return an unembedded entity.

#### Relationship embeddings in Cypher

Relationships are queried through the same Cypher path. This claim/evidence example embeds an explicit filtered selection. It then combines exact semantic scoring with the graph pattern:

```python
class DemoEmbedder:
    dimension = 2
    model_id = "demo/v1"
    def load(self): pass
    def unload(self): pass
    def embed(self, texts):
        return [[float(len(text)), 1.0] for text in texts]

graph.set_embedder(DemoEmbedder())
graph.cypher("""
    MATCH (:Claimant)-[r:SUPPORTS]->(c:Claim)
    WHERE c.status = 'open'
    WITH collect(r) AS relationships
    CALL db.relationship_embeddings.embed({
      type:'SUPPORTS', text_column:'evidence',
      relationships:relationships, mode:'changed'
    })
    YIELD embedded RETURN embedded
""")

rows = graph.cypher("""
    MATCH (who:Claimant)-[r:SUPPORTS]->(c:Claim)
    WHERE c.status = 'open'
      AND text_score(r, 'evidence', $question) IS NOT NULL
    RETURN who.name, c.title,
           text_score(r, 'evidence', $question) AS score
    ORDER BY score DESC LIMIT 5
""", params={'question': 'Which evidence supports this claim?'})
```

`embed` always acts on the relationships supplied in its map.

- `mode='all'` rebuilds that selected slice. Missing or non-string selected source text removes an old vector. Unselected vectors remain.
- The aggregate model is null when retained vectors cannot all be attributed to the reported model.
- A dimension change requires selection coverage of every stored vector.
- The write and model callback are atomic.
- Same-statement property updates are read from current graph state.

`embed` is the in-query route: it acts on relationships a query has just matched. The bulk route is a method.

- `graph.embed_relationship_texts('SUPPORTS', 'evidence', mode='changed')` embeds every relationship of the type with the registered model. It is the relationship twin of `embed_texts()`, and stores the same vectors, hashes and model id `embed` stores.
- `types: ['SUPPORTS', 'REFUTES']` in place of `type` makes one `embed` call cover several types. Each listed type gets its own pass.
- For vectors computed outside the graph, see `set_relationship_embeddings()` / `add_relationship_embeddings()` below.

Mutating procedures remain top-level pipeline clauses.

- A read-only `CALL {}` subquery may collect native relationship values and return them to an outer top-level `db.relationship_embeddings.embed` call, preserving statement identity.
- Putting the mutating procedure inside `CALL {}` or a `UNION` arm follows the existing Cypher write boundary. It is rejected before invoking the model.

#### Scoring and ranking relationships

Use `vector_score(r, 'evidence_emb', $vector)` for a query vector and `text_score(r, 'evidence', $text)` for source-column text. Scored per row inside a filtered `MATCH`, they are exact.

The top-k shape `ORDER BY vector_score(r, …) DESC LIMIT k` (or `text_score`) is served from the store, as for nodes:

- A plain single-type pattern whose every relationship is embedded goes straight to the store. A `WHERE … IS NOT NULL` filter on the score keeps it there.
- Any other shape scores its matched rows.
- `WITH r, vector_score(r, …) AS s ORDER BY s DESC LIMIT k RETURN startNode(r)…` is served the same way.
- An undirected `(a)-[r:T]-(b)` uses the index too, returning each relationship once per orientation.

Either way, the query runs through HNSW when an index is online. That answer is approximate. Pass `{exact:true}` as the final argument to force exact. Ties at the cut are answered by the ordinary pipeline, and `diagnostics["retrieval"]` reports the route.

Build and query the separate whole-store HNSW index only when the relationship type and source-property store are the intended search corpus:

```python
graph.cypher("""
    CALL db.relationship_embeddings.build_index({
      type:'SUPPORTS', text_column:'evidence',
      m:16, ef_construction:200, ef_search:64
    }) YIELD indexed, metric, m
    RETURN indexed, metric, m
""")

nearest = graph.cypher("""
    CALL db.relationship_embeddings.query({
      type:'SUPPORTS', text_column:'evidence',
      vector:$query_vector, top_k:10
    }) YIELD relationship, score, search_method
    RETURN relationship, score, search_method
""", params={'query_vector': [0.1, 0.2]})
```

`text:'…'` (or `text:$param`) in place of `vector` embeds the query once with the registered embedder before the statement runs. `text` and `vector` together are refused, and so is a text computed from a row.

`search_method` reports `hnsw` only when the index served the query:

- A missing or metric-incompatible index falls back to `exact`.
- A stale writable index may catch up at query entry.
- A stale index that cannot catch up also falls back.
- `exact:true` always bypasses HNSW.

The other index procedures:

- `refresh_index` incorporates pending index changes.
- `drop_index` removes the index while retaining vectors.
- `list` reports `index_state` (`none`, `online`, or `stale`), pending `delta`, and the number of `unembedded` relationships.

A saved `.kgl` keeps a built relationship index. A reloaded graph answers through HNSW immediately and reports the same pending delta it was saved with. A disk-mode graph's directory keeps no index, as for nodes.

#### Relationship index lifecycle

Deletes drop the relationship index, as they drop the node index.

- `SET` and `CREATE` leave it `online`.
- Deleting an embedded relationship takes `index_state` to `none` until `build_index` runs again. This covers `DELETE r`, and `DETACH DELETE` of either endpoint, embedded or not.
- A `vacuum()` that compacts drops every vector index in the graph, node and relationship, including those on types that saw no delete. This is as described for node indexes above. On disk, `vacuum()` is a no-op.
- Deleting a relationship the store holds no vector for leaves the index alone.
- A delete that a failed statement or a rolled-back transaction undoes also leaves it alone.

Until the rebuild, queries answer by the exact scan. `refresh_index` folds pending changes into an index but never builds one. With no index it refuses, naming the `build_index` call, rather than answering `{refreshed: 0}`.

#### Inspecting relationship stores

From Python, `list_embeddings()` and `embedding_diagnostics()` report relationship stores as `entity='relationship'` rows.

- `embedding_info(type, col, entity='relationship')` reads one relationship store. The keyword keeps a node type and a relationship type of the same name apart.
- `describe()` shows each relationship store on its `<conn>` line and in `describe(connections=['T'])`. It marks the store `hnsw` once an index is built, as it marks node stores. A BM25 index shows as `text_index`.
- `describe(cypher=['relationship_semantic'])` gathers the relationship scoring, cross-type ranking and index lifecycle in one topic.

#### Reading and writing relationship vectors

To read the vectors back out, use `embedding(r, 'evidence_emb')` in Cypher. It returns one relationship's stored vector. `embedding(n, 'summary_emb')` does the same for a node.

`vector_score` takes any list as its query. `vector_score(r2, 'evidence_emb', embedding(r1, 'evidence_emb'))` is therefore relationship-to-relationship similarity.

From Python, `relationship_embeddings('SUPPORTS', 'evidence')` returns every vector in the store as rows `{source, target, source_type, target_type, key, vector}`. Rows are ordered by source, then target, then key. That is a graph-learning edge list plus edge features, e.g. for PyTorch Geometric:

```python
import numpy as np

rows = graph.relationship_embeddings("SUPPORTS", "evidence", relationship_keys={"SUPPORTS": "uid"})
src = [(r["source_type"], r["source"]) for r in rows]
dst = [(r["target_type"], r["target"]) for r in rows]
index = {node: i for i, node in enumerate(dict.fromkeys(src + dst))}  # row order
edge_index = np.array([[index[n] for n in src], [index[n] for n in dst]])
edge_attr = np.array([r["vector"] for r in rows], dtype=np.float32)
```

Nodes are keyed on `(type, id)` because ids are unique per type only. They are numbered in row order rather than sorted, because a graph can hold integer and string ids, which Python cannot order against each other.

A parallel group (several relationships of the type between the same two nodes) returns all its members. `relationship_keys` names the property that tells them apart, the same mapping `export_embeddings()` takes. It is optional, but a named key that is missing or repeated within a group is refused.

The write side takes the same address, in the node API's two forms:

- `set_relationship_embeddings()` replaces the store (like `set_embeddings()`).
- `add_relationship_embeddings()` upserts into it (like `add_embeddings()` and `db.relationship_embeddings.set`).

They are the bulk route for vectors you computed yourself, without a per-row query. Examples are a numpy matrix from an external model, or rows read above and modified. Address a relationship in one of these ways:

- Key a dict by `(source_id, target_id)` when the relationship type has one source and one target node type.
- Key it by `(source_type, source_id, target_type, target_id)` otherwise.
- Append the key value for a parallel-group member.
- Or pass the rows `relationship_embeddings()` returned.

```python
graph.add_relationship_embeddings(
    "SUPPORTS", "evidence", {(1, 10): vectors[0], (2, 10): vectors[1]}
)

rows = graph.relationship_embeddings("SUPPORTS", "evidence", relationship_keys={"SUPPORTS": "uid"})
for row in rows:
    row["vector"] = [x / 2 for x in row["vector"]]
graph.set_relationship_embeddings("SUPPORTS", "evidence", rows, relationship_keys={"SUPPORTS": "uid"})
```

These cases are refused by row, naming the relationship, and nothing is written:

- A parallel group written without a key.
- An endpoint pair no relationship of the type connects.
- A vector of the wrong width.

`db.relationship_embeddings.set` remains the in-query route, for relationships a `MATCH` binds.

#### Node and relationship routers

Each method in this table has a node spelling, a relationship spelling, and a generic router that picks one with `entity=`. The default is `"node"`, so every existing node call is unchanged:

| Router (`entity="node"` default) | Node route | Relationship route |
|---|---|---|
| `set_embeddings` | `set_node_embeddings` | `set_relationship_embeddings` |
| `add_embeddings` | `add_node_embeddings` | `add_relationship_embeddings` |
| `embed_texts` | `embed_node_texts` | `embed_relationship_texts` |
| `embeddings` | `node_embeddings` | `relationship_embeddings` |
| `embedding` | `node_embedding` | `relationship_embedding` |
| `embedding_dim` | `node_embedding_dim` | `relationship_embedding_dim` |
| `remove_embeddings` | `remove_node_embeddings` | `remove_relationship_embeddings` |
| `vector_search` | `node_vector_search` | `relationship_vector_search` |
| `search_text` | `node_search_text` | `relationship_search_text` |
| `build_vector_index` | `build_node_vector_index` | `build_relationship_vector_index` |
| `refresh_vector_index` / `drop_vector_index` / `has_vector_index` | `…_node_vector_index` | `…_relationship_vector_index` |

The inventory methods need no twin. `embedding_info` takes `entity=` itself. `list_embeddings`, `embedding_diagnostics`, `export_embeddings`, `import_embeddings` and `copy_embeddings_from` cover both entities in one call.

In Cypher the same split holds:

- `db.node_embeddings.*` and `db.relationship_embeddings.*` are the twins.
- `db.embeddings.*` routes to one of them with an `entity:` key (`'node'` by default).

`relationship_vector_search` / `relationship_search_text` rank what `db.relationship_embeddings.query` ranks: every relationship type with a `text_column` store, or the ones `types=` names. Each hit is a `relationship_embeddings()` row without the vector, plus `relationship_type` and `score`. `relationship_embedding` reads one relationship's vector by an address shaped like a writer's dict key:

```python
hits = graph.relationship_search_text("evidence", "water damage", top_k=5)
# [{'source': 1, 'target': 10, 'source_type': 'Claimant', 'target_type': 'Claim',
#   'key': None, 'relationship_type': 'SUPPORTS', 'score': 0.93}, ...]
vector = graph.relationship_embedding("SUPPORTS", "evidence", (1, 10))
```

`remove_embeddings` and `embedding` refuse a store that does not exist. The error names the store you probably meant: the text column when the store name was passed, a near-miss column or type, or the same name on the other entity.

```python
graph.add_embeddings("SUPPORTS", "evidence", rows, entity="relationship")
graph.build_vector_index("SUPPORTS", "evidence", entity="relationship")
```

A router behaves exactly as the method it routes to. It accepts both routes' keywords. It refuses, by name, a keyword the chosen route does not take, such as `relationship_keys` on a node call. The relationship index methods share their code path with `db.relationship_embeddings.build_index` / `.refresh_index` / `.drop_index`.

#### Ranking across relationship types

The `query` procedure ranks the complete declared store before later clauses run. A `WHERE` after `YIELD` filters the returned top-k candidates. It does not constrain HNSW. Use filtered `MATCH` plus `vector_score` / `text_score` when an endpoint or relationship predicate must constrain the ranking corpus.

A graph can spread its relations over many relationship types, one per predicate, as knowledge-graph extractors produce. You can rank all of them as one corpus:

- `types:['created', 'works_at']` in place of `type` ranks those stores together.
- Leaving out both `type` and `types` ranks every relationship store for `text_column`.
- Each store answers on its own route, and the answers merge into one `top_k`. The order is score, then relationship type, then relationship slot.
- Every row yields `type` and its own `search_method`.

```python
rows = graph.cypher("""
    CALL db.relationship_embeddings.query({text_column:'description', text:$q, top_k:5})
    YIELD relationship, score, type, search_method
    RETURN type, relationship.description AS description, score, search_method
""", params={'q': 'who founded the company?'})
```

Error cases:

- A named type without a store is refused by name.
- Stores that declare different metrics refuse the merge, naming both, because their scores are not on one scale. Pass `metric` to score every store under one.

The `MATCH` form takes the same shapes: `MATCH ()-[r:created|works_at]->() … ORDER BY text_score(r, 'description', $q) DESC LIMIT k`, or an untyped `()-[r]->()`. It is served per store and merged when every type in play carries the store. It runs through HNSW when every store's index is online. `diagnostics["retrieval"]` lists the stores it read.

Every form runs **one search per relationship type**, because each type's store has its own HNSW index. The answers are merged, so a ranking across many types costs about the number of types times one search.

Under the long tail of small relationship types an extractor produces, that per-store cost dominates. An index buys little over the exact scan of each small store. The answer is the same either way.

The Python router is the same fan-out: `graph.search_text("description", q, entity="relationship")` ranks every relationship type that has a `description` store. If a type in play has no store, the query raises the same error it raises row by row. Embed that type, or name the types that have stores.

#### Relationship Graph RAG example

The network-free [`examples/relationship_graphrag.py`](https://github.com/kkollsga/kglite/blob/main/examples/relationship_graphrag.py) puts the full workflow together with a deterministic fake embedder:

- selected generation,
- exact endpoint-filtered claim/evidence ranking,
- a changed-text refresh,
- provenance inspection,
- explicit whole-store HNSW retrieval,
- a save/reopen/re-register check that the HNSW index survives the `.kgl`.

Run it with an explicit scratch output:

```bash
python examples/relationship_graphrag.py --output /tmp/claims.kgl
```

#### Carrying relationship vectors across a rebuild

Relationship values should stay bound inside the statement, because physical IDs are graph-local slots.

To carry relationship vectors to an independently rebuilt graph, use `export_embeddings()` / `import_embeddings()` or `copy_embeddings_from()`. Each vector is matched by relationship type and the `(type, id)` of both endpoints.

Where several relationships of one type connect the same two nodes, name a property that is unique within each such group. Each vector then lands on the right member, whatever order the rebuild created them in:

```python
old.export_embeddings("vectors.kgle", relationship_keys={"SUPPORTS": "uid"})
new.import_embeddings("vectors.kgle")  # the file records the key
```

A group with no usable key is refused by name (type, endpoints, member count) rather than guessed at, and nothing is written.

An export that carries relationship stores is `.kgle` version 4, which released versions up to 0.17.12 refuse by version. A node-only export stays version 3.

#### Relationship communities

Graph RAG pipelines such as Microsoft GraphRAG, LightRAG and knwler answer broad questions from *communities*: clusters of the extracted graph, each with a summary. They answer from the community summary and from the relations inside that community.

kglite has no native relationship clustering. The whole recipe is Cypher over node communities. Every snippet below runs as written (`tests/test_relationship_community_recipe.py` executes this section).

Start with a graph of extracted names (nodes labelled `Entity`, as Graph RAG extractors call them). Their relations carry a `description` and a `strength`. Embed the descriptions per relationship type. The embedder here is a network-free stand-in; use a real model in practice:

```python
import kglite


class KeywordEmbedder:
    """Counts a few keywords — a stand-in for a sentence-embedding model."""

    dimension = 6
    model_id = "demo/keywords"
    WORDS = ("apple", "computer", "founded", "ship", "voyage", "pacific")

    def load(self):
        pass

    def unload(self):
        pass

    def embed(self, texts):
        return [[float(word in text.lower()) + 0.01 for word in self.WORDS] for text in texts]


graph = kglite.KnowledgeGraph()
graph.set_embedder(KeywordEmbedder())
graph.cypher("""
    CREATE (jobs:Entity {id: 1, name: 'Steve Jobs'}), (woz:Entity {id: 2, name: 'Steve Wozniak'}),
           (apple:Entity {id: 3, name: 'Apple'}), (mac:Entity {id: 4, name: 'Macintosh'}),
           (cook:Entity {id: 5, name: 'James Cook'}), (ship:Entity {id: 6, name: 'Endeavour'}),
           (pacific:Entity {id: 7, name: 'Pacific'}), (banks:Entity {id: 8, name: 'Joseph Banks'}),
           (jobs)-[:founded {description: 'Jobs founded the Apple computer company', strength: 9.0}]->(apple),
           (woz)-[:founded {description: 'Wozniak co-founded Apple', strength: 8.0}]->(apple),
           (woz)-[:works_at {description: 'Wozniak engineered the first Apple', strength: 7.0}]->(apple),
           (apple)-[:created {description: 'Apple created the Macintosh', strength: 9.0}]->(mac),
           (jobs)-[:works_at {description: 'Jobs led the Macintosh team', strength: 6.0}]->(mac),
           (cook)-[:sailed_on {description: 'Cook sailed the ship Endeavour on his first voyage', strength: 9.0}]->(ship),
           (ship)-[:explored {description: 'The Endeavour charted the Pacific', strength: 8.0}]->(pacific),
           (banks)-[:sailed_on {description: 'Banks joined the Endeavour voyage as naturalist', strength: 7.0}]->(ship),
           (cook)-[:explored {description: 'Cook explored the Pacific by ship', strength: 6.0}]->(pacific),
           (banks)-[:inspired {description: 'A museum model sits beside a Macintosh', strength: 1.0}]->(mac)
""")
RELATION_TYPES = ["founded", "works_at", "created", "sailed_on", "explored", "inspired"]
for rel_type in RELATION_TYPES:
    graph.cypher(
        f"MATCH ()-[r:{rel_type}]->() WITH collect(r) AS rs "
        f"CALL db.relationship_embeddings.embed({{type: '{rel_type}', text_column: 'description', relationships: rs}}) "
        "YIELD embedded RETURN embedded"
    )
```

**1. Detect communities over the `Entity` nodes**, weighted by relation strength. Store each node's community as a property. `CALL leiden` takes the same parameters as `CALL louvain`. The [graph algorithms guide](graph-algorithms.md#community-detection) covers both:

```python
graph.cypher("""
    CALL louvain({node_type: 'Entity', connection_types: $types, weight_property: 'strength'})
    YIELD node, community
    SET node.community = community
""", params={"types": RELATION_TYPES})
```

**2. Classify each relation as intra-community or bridge.** A bridge relation joins two communities. Bridges are the relations a community-scoped answer leaves out. They are also the ones to read when a question spans topics:

```python
kinds = graph.cypher("""
    MATCH (s:Entity)-[r]->(t:Entity)
    RETURN CASE WHEN s.community = t.community THEN 'intra' ELSE 'bridge' END AS kind,
           type(r) AS type, s.name AS source, t.name AS target
    ORDER BY kind, source, target
""").to_list()
bridges = [row for row in kinds if row["kind"] == "bridge"]
# [{'kind': 'bridge', 'type': 'inspired', 'source': 'Joseph Banks', 'target': 'Macintosh'}]
```

**3. Rank one community's relations against a question.** The `WHERE` restricts the ranking to relations with both endpoints in the community.

- The query keeps the `ORDER BY text_score(r, …) DESC LIMIT k` shape. It is served from the relationship stores (merged across the alternation's types) instead of sorting every scored row.
- Name the relation types in the alternation. An untyped `-[r]->` would also put `IN_COMMUNITY` (step 4) in play, and that type has no store.

```python
apple_community = graph.cypher(
    "MATCH (e:Entity {name: 'Apple'}) RETURN e.community AS community"
).to_list()[0]["community"]
top = graph.cypher("""
    MATCH (s:Entity)-[r:founded|works_at|created|sailed_on|explored|inspired]->(t:Entity)
    WHERE s.community = $community AND t.community = $community
    RETURN s.name AS source, type(r) AS type, t.name AS target,
           text_score(r, 'description', $question) AS score
    ORDER BY score DESC LIMIT 2
""", params={"community": apple_community, "question": "who founded the apple computer company"})
# top.to_list() -> Steve Jobs founded Apple, then Steve Wozniak founded Apple
# top.diagnostics["retrieval"] names the stores the ranking read
```

**4. Summaries as embedded nodes, ranked first** (the knwler shape). Materialise each community as a node linked to its members, give it a summary, and embed the summaries. A question then picks the best community first and ranks only that community's relations. Here the summary joins the community's relation descriptions. A real pipeline asks an LLM to summarise them:

```python
graph.cypher("MATCH (e:Entity) WITH DISTINCT e.community AS c CREATE (:Community {id: c})")
graph.cypher("MATCH (e:Entity), (c:Community) WHERE c.id = e.community CREATE (e)-[:IN_COMMUNITY]->(c)")
graph.cypher("""
    MATCH (c:Community)<-[:IN_COMMUNITY]-(s:Entity)-[r]->(t:Entity)-[:IN_COMMUNITY]->(c)
    WITH c, collect(r.description) AS descriptions
    SET c.summary = reduce(acc = '', d IN descriptions | acc + d + '. ')
""")
graph.embed_texts("Community", "summary", show_progress=False)

answer = graph.cypher("""
    MATCH (c:Community)
    WITH c, text_score(c, 'summary', $question) AS community_score
    ORDER BY community_score DESC LIMIT 1
    MATCH (c)<-[:IN_COMMUNITY]-(s:Entity)-[r:founded|works_at|created|sailed_on|explored|inspired]->(t:Entity)
          -[:IN_COMMUNITY]->(c)
    RETURN s.name AS source, type(r) AS type, t.name AS target,
           text_score(r, 'description', $question) AS score
    ORDER BY score DESC LIMIT 2
""", params={"question": "ship voyage"})
# answer.to_list() -> James Cook sailed_on Endeavour, then Joseph Banks sailed_on Endeavour
```

Clustering the relationships themselves, for example k-means over a relationship store, is not supported. `CALL cluster()` clusters the nodes a preceding `MATCH` binds.

### Embedding Norm in Cypher

`embedding_norm()` returns the L2 norm of a node's embedding vector. In Poincaré space, norm indicates hierarchy depth: values near 0 are roots, values near 1 are leaves.

```python
# Find the most "root-like" concepts (lowest norm = highest in hierarchy)
graph.cypher("""
    MATCH (n:Concept)
    RETURN n.name, embedding_norm(n, 'title') AS depth
    ORDER BY depth ASC LIMIT 10
""")

# Find leaf nodes (high norm = deep in hierarchy)
graph.cypher("""
    MATCH (n:Concept)
    WHERE embedding_norm(n, 'title') > 0.8
    RETURN n.name, embedding_norm(n, 'title') AS depth
""")
```

## Embedding Utilities

```python
graph.list_embeddings()
# [{'entity': 'node', 'node_type': 'Article', 'text_column': 'summary',
#   'store_name': 'summary_emb', 'dimension': 384, 'count': 1000, 'metric': 'cosine'}]
# Relationship stores follow as entity='relationship' rows keyed
# 'relationship_type' (never 'node_type'), so check row['entity'] first.

graph.remove_embeddings('Article', 'summary')

# Retrieve all embeddings for a type (no selection needed)
embs = graph.embeddings('Article', 'summary')
# {1: [0.1, 0.2, ...], 2: [0.4, 0.5, ...], ...}

# Retrieve embeddings for current selection only
embs = graph.select('Article').where({'category': 'politics'}).embeddings('summary')

# Get a single node's embedding (indexed lookup; returns None if not found)
vec = graph.embedding('Article', 'summary', node_id)
```

Embeddings persist across `save()`/`load()` cycles automatically.

## Embedding Export / Import

Export embeddings to a standalone `.kgle` file so they survive graph rebuilds:

```python
# Export all embeddings (node and relationship stores)
stats = graph.export_embeddings("embeddings.kgle")
# {'stores': 2, 'embeddings': 5000, 'relationship_stores': 0, 'relationship_embeddings': 0}

# Export only specific node types (no relationship stores)
graph.export_embeddings("embeddings.kgle", ["Article", "Author"])

# Import into a fresh graph — matches by (node_type, node_id)
result = graph.import_embeddings("embeddings.kgle")
# {'stores': 2, 'imported': 4800, 'skipped': 200, 'dropped_stores': 0,
#  'relationship_stores': 0, 'relationship_imported': 0, ...}
```

Relationship stores travel in the same file, matched by relationship type and endpoint ids. Parallel relationships are told apart by the key named in `relationship_keys`. See the relationship section above.

A `.kgle` carries each store's **provenance**: its `metric`, the embedder `model_id`, and per-node text hashes. A rebuild-from-`.kgle` pipeline therefore keeps it.

- After import, `embedding_info()` reports the model/metric.
- `embed_texts(mode='changed')` re-embeds only genuinely-changed text instead of everything.

Current releases import `.kgle` v3 (node stores) and v4 (node and relationship stores), both Postcard. To upgrade from v1/v2 files, convert them with kglite 0.13.4 first: import them into the matching graph and re-export them before upgrading.
