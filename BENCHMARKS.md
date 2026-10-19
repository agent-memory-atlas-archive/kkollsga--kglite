# KGLite benchmarks

Minimum observed wall time for the workloads each adapter exercised on one seed-deterministic synthetic graph. Lower is better within a row; `not exercised` is not a claim about the underlying product's capabilities.

**Dataset:** 25,333 nodes · 280,774 edges (Person/Company/Project/Skill/City), scale `medium`.

| Workload topic | kglite | DuckDB | igraph | LadybugDB | Neo4j (native) | NetworkX | rustworkx |
|---|---|---|---|---|---|---|---|
| Node/edge scans and ID lookup | 2.6ms | 1.7ms | **16µs** | 4.3ms | 4.7ms | 2.8ms | 347µs |
| Property filters and aggregation | 3.8ms | **1.3ms** | 4.1ms | 3.3ms | 62.1ms | 5.7ms | 4.1ms |
| Reachability and traversal | 16.1ms | 13.2ms | **2.9ms** | 30.47s (measured >10s) | 273.5ms | 8.6ms | 8.2ms |
| Shortest-path queries | **275µs** | not exercised | 23.1ms | 210.8ms | 14.4ms | 510µs | 90.1ms |
| Typed graph joins and aggregation | 6.1ms | **1.9ms** | not exercised | 3.7ms | 27.1ms | 119.5ms | not exercised |
| Degree and connected-components operations | 16.7ms | 3.3ms (2/3 exercised) | **2.0ms** | 10.5ms (2/3 exercised) | 41.4ms (2/3 exercised) | 9.7ms | 8.3ms |
| Louvain community detection | **141.9ms** | not exercised | 1.23s | not exercised | not exercised | 11.71s (measured >10s) | not exercised |
| Updates and create/delete batch | 3.8ms | 2.4ms | 6.8ms | 121.6ms | 61.8ms | 716µs | **128µs** |
| Exact vector scoring | **4.4ms** | not exercised | not exercised | not exercised | not exercised | not exercised | not exercised |
| Latitude/longitude bounding-box filter | **12µs** | 72µs | not exercised | 284µs | 698µs | not exercised | not exercised |

Bold marks the fastest complete, non-slow measurement in that row. Partial cells sum only the named executed groups; missing work is not estimated. Values above 10 seconds remain in the measurement and are labelled, not treated as timeouts. There is deliberately no grand total across unequal workload coverage.

Adapters receive the same generated input records and shared query parameters, but use different data models and idiomatic APIs. Their operations are corresponding workloads, not necessarily identical query text or identical execution semantics. The detailed parity report records result differences instead of treating every cross-library difference as a failure.

### Workloads exercised by these adapters

This is coverage of the recorded adapter implementations, not a product capability matrix.

| Workload topic | kglite | DuckDB | igraph | LadybugDB | Neo4j (native) | NetworkX | rustworkx |
|---|---|---|---|---|---|---|---|
| Node/edge scans and ID lookup | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Property filters and aggregation | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Reachability and traversal | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Shortest-path queries | ✓ | not exercised | ✓ | ✓ | ✓ | ✓ | ✓ |
| Typed graph joins and aggregation | ✓ | ✓ | not exercised | ✓ | ✓ | ✓ | not exercised |
| Degree and connected-components operations | ✓ | 2/3 exercised | ✓ | 2/3 exercised | 2/3 exercised | ✓ | ✓ |
| Louvain community detection | ✓ | not exercised | ✓ | not exercised | not exercised | ✓ | not exercised |
| Updates and create/delete batch | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Exact vector scoring | ✓ | not exercised | not exercised | not exercised | not exercised | not exercised | not exercised |
| Latitude/longitude bounding-box filter | ✓ | ✓ | not exercised | ✓ | ✓ | not exercised | not exercised |

### What's being compared

- **kglite** — Cypher graph engine, in-memory
- **DuckDB** — SQL/relational database
- **igraph** — C-backed graph library
- **LadybugDB** — Cypher graph database, embedded
- **Neo4j (native)** — Cypher graph database, native server
- **NetworkX** — pure-Python graph library
- **rustworkx** — Rust-backed graph library

Graph-construction time is omitted from the cross-kind headline because these adapters materialise different internal representations and indices. Compare workload rows individually and consult the raw per-group report for detail.

### kglite storage modes and protocols

| Workload topic | kglite | kglite (mapped) | kglite (disk) | kglite (fluent) | kglite (Bolt) |
|---|---|---|---|---|---|
| Graph construction | 47.6ms | 53.5ms | 102.4ms | **46.0ms** | 254.9ms |
| Node/edge scans and ID lookup | 2.6ms | **2.6ms** | 4.4ms | 500µs (2/3 exercised) | 176.3ms |
| Property filters and aggregation | 3.8ms | **3.6ms** | 7.3ms | 5.4ms | 29.1ms |
| Reachability and traversal | **16.1ms** | 16.8ms | 20.1ms | 49.1ms | 210.2ms |
| Shortest-path queries | **275µs** | 282µs | 311µs | 22.4ms | 3.1ms |
| Typed graph joins and aggregation | **6.1ms** | 6.1ms | 8.1ms | 45.5ms (2/3 exercised) | 7.7ms |
| Degree and connected-components operations | **16.7ms** | 26.3ms | 18.0ms | 6.1ms (2/3 exercised) | 24.4ms |
| Louvain community detection | **141.9ms** | 142.4ms | 143.3ms | not exercised | 147.2ms |
| Updates and create/delete batch | **3.8ms** | 3.9ms | 9.6ms | 95.0ms | 19.4ms |
| Exact vector scoring | 4.4ms | 4.7ms | 12.9ms | **127µs** | 5.7ms |
| Latitude/longitude bounding-box filter | **12µs** | 13µs | 21µs | 15µs | 267µs |

### Scaling

The headline uses the `medium` graph selected above. For the separate historical load-and-first-query study of disk-backed modes, see [`benchmarks/competitive/largescale/`](benchmarks/competitive/largescale/README.md).

## Reproduce

```bash
uv pip install --python .venv/bin/python ladybug networkx rustworkx python-igraph duckdb neo4j
uv run --no-sync maturin develop --release
make build-bolt-server
.venv/bin/python benchmarks/benchmark.py
```

The public command publishes only a clean, complete, error-free invocation containing every requested adapter. See [`graphsuite/README.md`](benchmarks/competitive/graphsuite/README.md) for opt-in native and Docker server profiles.

### Measurement protocol

Each query group records the minimum observed wall time from an adaptive 1–5 repetitions; mutation groups use at most two. Graph construction uses one measurement, or two for inexpensive non-Bolt builds. There is no warm-up phase. These labels describe this harness exactly; they are not confidence intervals or enforced timeouts.

Versions used for the selected rows:

_kglite 0.20.1, DuckDB 1.5.5, igraph 1.0.0, LadybugDB 0.17.0, Neo4j (native) 2026.02.3, NetworkX 3.6.1, rustworkx 0.18.1_

Run on macOS-27.0.1-arm64-arm-64bit-Mach-O · Python 3.14.3.

### Capture provenance

- Publication qualification: `qualified`
- Results schema: `2`
- Results file writer: `graphsuite` v3
- Dataset signature: `medium-s1234-n25333-e280774`
- Selected run timestamps: `2026-10-19T21:51:49+02:00` through `2026-10-19T21:53:06+02:00`
- Selected capture harness: `v3`
- Capture id: `2a2274bc45f1435cbd491095843af0bf`
- Source commit: `4de6f262c5e59db9d4f049c483998f6c9fb7851f` (dirty: `false`)
- Base repeat policy: `5`
- Dataset seed: `1234`
- Raw metadata authority: `benchmarks/competitive/graphsuite/results.json`.
