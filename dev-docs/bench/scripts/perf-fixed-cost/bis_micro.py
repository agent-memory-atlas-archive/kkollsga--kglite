import sys, time, statistics
sys.path.insert(0, ".")
import test_bench_core as t
from kglite import KnowledgeGraph
g = t._hop1_graph("memory")
e = KnowledgeGraph()
cases = {
 "hop1": (g, "MATCH (a:Person)-[:KNOWS]->(b) RETURN count(*) AS c"),
 "return1": (e, "RETURN 1 AS x"),
}
out = []
for name, (gr, q) in cases.items():
    for _ in range(2000): gr.cypher(q).to_list()
    best = []
    for rep in range(15):
        n = 20000; s = time.perf_counter()
        for _ in range(n): gr.cypher(q).to_list()
        best.append((time.perf_counter() - s) / n * 1e9)
    out.append(f"{name} min={min(best):.0f}ns med={statistics.median(best):.0f}ns")
print(" | ".join(out))
