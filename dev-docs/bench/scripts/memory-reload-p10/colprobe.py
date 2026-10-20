"""colprobe.py FILE COLUMNS_POLICY: per-column-type warm min."""
import sys, time, kglite
g = kglite.load(sys.argv[1], columns=sys.argv[2], embeddings="auto") if sys.argv[2] != "none" else kglite.load(sys.argv[1])
Q = {
 "int_count_gt": "MATCH (n:Doc) WHERE n.val > 500000000 RETURN count(n)",
 "int_sum": "MATCH (n:Doc) RETURN sum(n.val)",
 "float_max": "MATCH (n:Doc) RETURN max(n.score)",
 "float_gt": "MATCH (n:Doc) WHERE n.score > 0.5 RETURN count(n)",
 "str_eq": "MATCH (n:Doc) WHERE n.cat = 'category-005-xxxxx' RETURN count(n)",
 "str_group": "MATCH (n:Doc) RETURN n.cat, count(*) AS k ORDER BY k DESC LIMIT 5",
 "int_gt_sum_max": "MATCH (n:Doc) WHERE n.val > 500000000 RETURN count(n) AS c, sum(n.val) AS s, max(n.score) AS m",
}
for k, q in Q.items():
    for _ in range(5): g.cypher(q)
    s = []
    for _ in range(60):
        t = time.perf_counter(); g.cypher(q); s.append((time.perf_counter() - t) * 1e3)
    print(f"{k:16s} min {min(s):.3f}", end="  |  ")
print()
