"""attr_gen.py DIR N: build one fixture graph, save v8 (compact) and mapped-layout files. Run with the branch build."""
import sys, os, numpy as np, pandas as pd, kglite
d, N = sys.argv[1], int(sys.argv[2]); os.makedirs(d, exist_ok=True)
rng = np.random.default_rng(7)
g = kglite.KnowledgeGraph()
g.add_nodes(pd.DataFrame({
    "id": np.arange(N, dtype=np.int64), "title": [f"n{i}" for i in range(N)],
    "val": rng.integers(0, 10**9, N), "score": rng.random(N),
    "cat": [f"category-{i % 97:03d}-{'x' * (i % 11)}" for i in rng.integers(0, 10**6, N)],
}), "Doc", "id", "title")
g.save(f"{d}/compact_{N}.kgl", layout="compact")
g.save(f"{d}/mapped_{N}.kgl", layout="mapped")
print(kglite.__file__)
