"""Build the P10 fixture: gen.py OUT.kgl  (run with each wheel/branch to get its own file format)."""
import sys, time, numpy as np, pandas as pd, kglite
out = sys.argv[1]; N = 50_000; D = 1024
rng = np.random.default_rng(7)
lat = rng.standard_normal((N, 8), dtype=np.float32); proj = rng.standard_normal((8, D), dtype=np.float32)
vec = (lat @ proj + 0.3 * rng.standard_normal((N, D), dtype=np.float32)).astype(np.float32)
g = kglite.KnowledgeGraph()
g.add_nodes(pd.DataFrame({
    "id": np.arange(N, dtype=np.int64), "title": [f"n{i}" for i in range(N)],
    "summary": [f"text {i}" for i in range(N)],
    "val": rng.integers(0, 10**9, N), "score": rng.random(N),
    "cat": [f"category-{i % 97:03d}-{'x' * (i % 11)}" for i in rng.integers(0, 10**6, N)],
}), "Doc", "id", "title")
g.set_embeddings("Doc", "summary", dict(enumerate(vec)), metric="cosine")
src = rng.integers(0, N, 150_000); dst = rng.integers(0, N, 150_000)
g.add_relationships(pd.DataFrame({"s": src, "d": dst}), "REL", "Doc", "s", "Doc", "d")
t = time.time(); g.build_vector_index("Doc", "summary"); print("hnsw build", time.time() - t)
np.save(out + ".q.npy", vec[rng.integers(0, N, 64)] + 0.01)
g.save(out)
print(kglite.__file__, kglite.__version__)
