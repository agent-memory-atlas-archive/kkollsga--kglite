"""attr_probe.py FILE COLUMNS_POLICY: warm min (ms) per query, plus where the columns live."""
import sys, time, kglite
g = kglite.load(sys.argv[1], columns=sys.argv[2])
Q = {
 "int_sum": "MATCH (n:Doc) RETURN sum(n.val)",
 "float_max": "MATCH (n:Doc) RETURN max(n.score)",
 "int_gt_sum_max": "MATCH (n:Doc) WHERE n.val > 500000000 RETURN count(n) AS c, sum(n.val) AS s, max(n.score) AS m",
 "str_eq": "MATCH (n:Doc) WHERE n.cat = 'category-005-xxxxx' RETURN count(n)",
 "str_contains": "MATCH (n:Doc) WHERE n.cat CONTAINS 'e-05' RETURN count(n)",
 "str_group": "MATCH (n:Doc) RETURN n.cat, count(*) AS k ORDER BY k DESC LIMIT 5",
}
out = []
for k, q in Q.items():
    for _ in range(5): g.cypher(q)
    s = []
    for _ in range(60):
        t = time.perf_counter(); g.cypher(q); s.append((time.perf_counter() - t) * 1e3)
    out.append(f"{k} {min(s):.3f}")
print(" | ".join(out))
