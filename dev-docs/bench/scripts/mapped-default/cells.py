"""cells.py gen|run FILE (FILE=BUILT builds in-process instead of loading): the P10b in-process cells on a 300k-row graph. `gen` writes FILE with whichever kglite imports; `run` loads FILE and prints min of 60 (control min of 20) in ms."""
import sys, time
import numpy as np, pandas as pd, kglite
mode, path = sys.argv[1], sys.argv[2]
N = 300_000
def build():
    rng = np.random.default_rng(7)
    g = kglite.KnowledgeGraph()
    g.add_nodes(pd.DataFrame({
        "id": np.arange(N, dtype=np.int64), "title": [f"n{i}" for i in range(N)],
        "val": rng.integers(0, 10**9, N), "score": rng.random(N),
        "cat": [f"category-{i % 97:03d}-{'x' * (i % 11)}" for i in rng.integers(0, 10**6, N)],
    }), "Doc", "id", "title")
    g.add_relationships(pd.DataFrame({"s": rng.integers(0, N, 600_000), "d": rng.integers(0, N, 600_000)}), "REL", "Doc", "s", "Doc", "d")
    return g

if mode == "gen":
    g = build()
    g.save(path); print(kglite.__file__, kglite.__version__)
else:
    g = build() if path == "BUILT" else kglite.load(path, **({'columns': sys.argv[3]} if len(sys.argv) > 3 else {}))
    Q = {
     "int_sum": ("MATCH (n:Doc) RETURN sum(n.val)", 60),
     "float_max": ("MATCH (n:Doc) RETURN max(n.score)", 60),
     "int_gt_sum_max": ("MATCH (n:Doc) WHERE n.val > 500000000 RETURN count(n) AS c, sum(n.val) AS s, max(n.score) AS m", 60),
     "str_group": ("MATCH (n:Doc) RETURN n.cat, count(*) AS k ORDER BY k DESC LIMIT 5", 60),
     "control_2hop": ("MATCH (a:Doc)-[:REL]->(b:Doc)-[:REL]->(c:Doc) WHERE a.id < 20000 RETURN count(*)", 20),
    }
    out = []
    for k, (q, reps) in Q.items():
        for _ in range(5): g.cypher(q)
        s = []
        for _ in range(reps):
            t = time.perf_counter(); g.cypher(q); s.append((time.perf_counter() - t) * 1e3)
        out.append(f"{k} {min(s):.2f}")
    print(kglite.__file__.split('/')[-3] if 'site-packages' in kglite.__file__ else 'branch', " | ".join(out))
