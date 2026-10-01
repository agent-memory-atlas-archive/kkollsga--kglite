"""Out-of-scope finding probe: footprint of `WHERE p.rec_to IS NULL ... LIMIT k` vs the same without the WHERE, on a built Pand disk graph.
usage: limit_probe.py GRAPH LABEL  (run under wd.py; the first query is the one that exceeded 10.5 GB on national Pand)"""
import sys
from common import *
g = kglite.load(sys.argv[1]); r = {"label": sys.argv[2]}
for name, q in (("no_where_limit", "MATCH (p:Pand) RETURN p.id AS id LIMIT 12000"),
                ("is_null_limit", "MATCH (p:Pand) WHERE p.rec_to IS NULL RETURN p.id AS id LIMIT 12000")):
    base = settle(); PEAK[0] = 0; t = time.perf_counter()
    n = len(g.cypher(q, timeout_ms=0).to_list())
    r[name] = {"rows": n, "s": round(time.perf_counter() - t, 3), "peak_over_start_MB": round((PEAK[0] - base) / 1e6, 1)}
    print(json.dumps({name: r[name]}), flush=True)
STOP[0] = True; print(json.dumps(r), flush=True)
