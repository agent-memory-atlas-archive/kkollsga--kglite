# Embedding quantization

A quantized copy is a smaller duplicate of an embedding store that search scans in place of the full vectors. The `f32` vectors stay canonical, and every score a search returns is still the exact `f32` score.

## Building a copy

```python
g.quantize_embeddings("Article", "summary", "int8")   # build or replace
g.quantization_recall("Article", "summary")           # measure it on your own vectors
g.quantize_embeddings("Article", "summary", None)     # drop it
```

- `quantize_embeddings(node_type, text_column, precision, oversample=None, *, entity="node")` builds, replaces or (with `precision=None`) drops the copy.
- Pass `entity="relationship"` for a relationship store.
- `precision=` is also a keyword on `set_embeddings`, `embed_texts`, `import_embeddings` and their node and relationship twins. It quantizes after the write.
- `list_embeddings()`, `embedding_info()` and `embedding_diagnostics()` report a `quantization` entry.

## Precisions

| Precision | Stored as | Added size | Default oversample | Distance |
|---|---|---|---|---|
| `f16` | 16-bit floats | 50 % of the store | 4 | Store metric |
| `int8` | 8-bit integers, one scale per vector | 25 % (scales included) | 4 | Store metric |
| `binary` | Sign bits | 3 % | 10 | Hamming, for every metric |

- The Poincare metric never uses a copy, and `quantize_embeddings` refuses a Poincare store.
- A dimension above 100,000 is refused.

## Two-stage search

Search finds candidates on the copy, then ranks them exactly.

1. Scan the copy for `top_k * oversample` candidates.
2. Re-score exactly those candidates against the `f32` vectors.
3. Return the best `top_k`.

Returned scores are exact. Only the candidate set is approximate, as with an HNSW index. If `top_k * oversample` reaches the number of vectors, the copy is skipped and the search is exact.

Order of routes for one query:

1. A built HNSW index serves the query first.
2. Otherwise a quantized copy serves it.
3. `exact=True` (Cypher `{exact: true}`) bypasses both.

## Recall

Run `quantization_recall` on your own data before you rely on a precision. It replays up to `sample` stored vectors as queries, each excluded from its own results.

```python
r = g.quantization_recall("Article", "summary", sample=200, oversample=30)
print(r["recall"], r["recall_rescored"])
```

- `recall` is the overlap of the quantized top-k with the exact top-k, before the re-score.
- `recall_rescored` is the overlap after the re-score of the `top_k * oversample` pool. This is what a search returns.
- `oversample=` measures another pool size without changing the copy. `seed=` fixes the sample.
- The cost is `O(sample * store size * dimension)`.

Measured recall@10 on random Gaussian 1024-d vectors (3,000 vectors, 30 queries, debug build). Independent Gaussian coordinates are the adversarial case for sign bits, so structured embeddings do better.

| Precision | Before re-score | After re-score |
|---|---|---|
| `f16` | 1.000 | 1.000 |
| `int8` | 0.990 | 1.000 at oversample 4 |
| `binary` | 0.137 | 0.557 at oversample 10, 0.790 at 30, 0.960 at 100 |

## Speed and memory

Measured on 60k vectors x 768-d, top-10, release build, Cypher `vector_score` ordering:

| Store | Query time (min) |
|---|---|
| `f32`, no copy | 5.2 ms |
| `f16` | 4.3 ms (1.25x faster) |
| `int8` | 0.59 ms (9x faster) |

- `int8` is an integer dot product, so it is bandwidth-bound and fast.
- `f16` halves the bytes read, but the scan is bound by the float conversion, so it gains about 1.25x. Choose it for recall, not speed.
- `binary` is a popcount. Raise the oversample until `quantization_recall` is acceptable.
- The copy is added to the store, so memory and file size grow by the percentage in the table. Use `embeddings="auto"` so the copy and vectors are mapped from the file.
- Kernels: `f16` uses F16C on x86-64 (detected at run time) and NEON vectorization on aarch64, with a scalar fallback. `binary` uses POPCNT on x86.

## Cypher

Cypher retrieval uses the copy the same way as Python search.

- `MATCH (n:T) RETURN … vector_score(n, 'col_emb', $q) AS s ORDER BY s DESC LIMIT k` scans the copy when no index serves it. Result diagnostics report `actual_mode: "quantized"`.
- `db.node_embeddings.query` and `db.relationship_embeddings.query` report `search_method: "quantized"`.
- Under a `FOR VALID_TIME AS OF` context the route ranks the valid vectors and reports `actual_mode: "quantized_mask"` (`search_method: "quantized_mask"` for the procedures).
- A `MATCH … WHERE` that narrows the rows before ranking scores them exactly, because the rows are a subset of the store.
- `{exact: true}` always bypasses the copy.

See `CYPHER.md` for the full list of `search_method` and `actual_mode` values.

## Lifecycle

- The copy follows every write to the store (set, add, delete, rollback, vacuum) without a rebuild.
- A store written to the v9 data region saves the copy as `qcodes` and `qscales` sections, mapped on load like the vectors.
- A small store or a `layout="compact"` file saves only the declaration. The load re-derives the copy from the `f32` vectors, which costs one pass over the store.
- Replacing a store (`set_embeddings`, `embed_texts(mode="all")`, `import_embeddings`) starts without a copy. Pass `precision=` to keep one.
- A durable graph persists the declaration at the next `save()` or checkpoint, not in the write-ahead log. After a crash before then, search serves exact results from the `f32` vectors.

## What is not carried

- Disk storage mode graphs do not carry copies. Run `quantize_embeddings` after loading one.
- The `.kgle` export (`export_embeddings`) carries `f32` vectors only.
- Poincare stores are never quantized.

See {doc}`storage-and-memory` for where the vectors and copies live and {doc}`semantic-search` for search itself.
