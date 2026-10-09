import sys, time, statistics as st, kglite
N = int(sys.argv[1]) if len(sys.argv) > 1 else 100_000
g = kglite.KnowledgeGraph()
s = g.session()
rows = [{"i": i, "g": i % 50, "k": i % 7} for i in range(N)]
s.execute("UNWIND $rows AS r CREATE (:R {i: r.i, g: r.g, k: r.k})", params={"rows": rows})
try: s.execute("CREATE INDEX FOR (n:R) ON (n.i)")
except Exception as e: print("idx", e)
def t(f, n):
    xs = []
    for j in range(n):
        a = time.perf_counter(); f(j); xs.append((time.perf_counter() - a) * 1e3)
    return min(xs), st.median(xs)
def w(j): s.execute("MATCH (a:R {i: $a}), (b:R {i: $b}) CREATE (a)-[:E {w: $j}]->(b)", params={"a": j, "b": j + 1, "j": j})
def w_np(j): s.execute("MATCH (a:R {g: $a}), (b:R {k: $b}) WHERE a.i = $c AND b.i = $d CREATE (a)-[:F]->(b)", params={"a": j % 50, "b": j % 7, "c": j % 50, "d": j % 7 + 50})
def node_w(j): s.execute("CREATE (:Q {x: $j})", params={"j": j})
def r1(j): s.cypher("MATCH (a:R)-[:E]->(b:R) WHERE a.g = $g AND b.k = $k RETURN count(*) AS c", params={"g": j % 50, "k": j % 7})
def r2(j): s.cypher("MATCH (a:R {g: $g})-[:E]->(b:R {k: $k}) RETURN count(*) AS c", params={"g": j % 50, "k": j % 7})
def r3(j): s.cypher("MATCH (a:R {i: $i}) RETURN a.k AS k", params={"i": j})
def r4(j): s.cypher("RETURN 1")
for name, f, n in [("rel_write", w, 100), ("rel_write_ndv2", w_np, 100), ("node_write(ctl)", node_w, 200),
                   ("read_where2", r1, 15), ("read_props2", r2, 15), ("point_read(ctl)", r3, 200), ("return1(ctl)", r4, 200)]:
    mn, md = t(f, n); print(f"{name:18s} min {mn:9.3f}  p50 {md:9.3f} ms")
a = time.perf_counter()
s.cypher("MATCH (a:R {g: 3})-[:E]->(b:R {k: 2}) RETURN a.i AS i ORDER BY i LIMIT 5")
print("plan-result check", s.cypher("EXPLAIN MATCH (a:R {g: 3})-[:E]->(b:R {k: 2}) RETURN count(*)").to_list() if False else "")
