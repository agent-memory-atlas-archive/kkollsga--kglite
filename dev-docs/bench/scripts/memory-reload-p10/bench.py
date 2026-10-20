"""bench.py FILE LABEL [embeddings columns] [--cold|--warm]. One JSON line on stdout."""
import sys, json, time, statistics, numpy as np, kglite
sys.path.insert(0, __file__.rsplit("/", 1)[0]); from common import phys_footprint_mb
path, label = sys.argv[1], sys.argv[2]
mode = "cold" if "--cold" in sys.argv else "warm"
args = [a for a in sys.argv[3:] if not a.startswith("--")]
kw = {}
if args: kw = {"embeddings": args[0], "columns": args[1]}
f0 = phys_footprint_mb()
t = time.perf_counter(); g = kglite.load(path, **kw); load_ms = (time.perf_counter() - t) * 1e3
f1 = phys_footprint_mb()
Q = np.load(path + ".q.npy"); q = [Q[i].tolist() for i in range(len(Q))]
CY = "CALL db.node_embeddings.query({type:'Doc', text_column:'summary', vector:$v, top_k:10, exact:%s}) YIELD node, score RETURN node.id AS id, score"
cells = {
 "bf_py":   lambda i: g.select("Doc").vector_search("summary", q[i % 64], top_k=10, exact=True),
 "hnsw_py": lambda i: g.select("Doc").vector_search("summary", q[i % 64], top_k=10, exact=False),
 "bf_cy":   lambda i: g.cypher(CY % "true", params={"v": q[i % 64]}),
 "hnsw_cy": lambda i: g.cypher(CY % "false", params={"v": q[i % 64]}),
 "col_str": lambda i: g.cypher("MATCH (n:Doc) RETURN n.cat AS c, count(*) AS k ORDER BY k DESC LIMIT 5"),
 "col_int": lambda i: g.cypher("MATCH (n:Doc) WHERE n.val > 500000000 RETURN count(n) AS c, sum(n.val) AS s, max(n.score) AS m"),
 "control_topo": lambda i: g.cypher("MATCH (a:Doc)-[:REL]->(b:Doc)-[:REL]->(c:Doc) RETURN count(*) AS k"),
}
out = {"label": label, "mode": mode, "load_ms": load_ms, "fp_before": f0, "fp_after_load": f1}
if mode == "cold":
    for name, fn in cells.items():
        t = time.perf_counter(); fn(0); out["first_" + name] = (time.perf_counter() - t) * 1e3
    out["fp_after_queries"] = phys_footprint_mb()
else:
    for name, fn in cells.items():
        n = {"control_topo": 15, "col_str": 40, "col_int": 40}.get(name, 150)
        for i in range(3): fn(i)
        s = []
        for i in range(n):
            t = time.perf_counter(); fn(i); s.append((time.perf_counter() - t) * 1e3)
        out[name] = {"min": min(s), "med": statistics.median(s), "mean": statistics.fmean(s)}
    out["fp_after_queries"] = phys_footprint_mb()
print(json.dumps(out))
