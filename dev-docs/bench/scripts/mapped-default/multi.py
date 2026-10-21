"""multi.py BINARY GRAPH LABEL [NPROC]: N concurrent servers on the SAME graph file; sums footprint, RSS and spill. Needs MD_WORK."""
import sys, os, time, json, subprocess
sys.path.insert(0, os.path.dirname(__file__))
import mcpc
W = os.environ["MD_WORK"]
binary, graph, label = sys.argv[1], sys.argv[2], sys.argv[3]
n = int(sys.argv[4]) if len(sys.argv) > 4 else 4
Q = [("MATCH (m:WellboreMud) WHERE m.wlbMudType='WATER BASED' AND m.wlbMudWeightAtMD > 1.5 RETURN count(m) AS n"),
     ("MATCH (w:Wellbore) WHERE w.wlbCompletionYear >= 2010 AND w.wlbContent = 'OIL' RETURN w.title AS t, w.wlbDrillingOperator AS o ORDER BY t LIMIT 25"),
     ("MATCH (p:SeismicPolygon) RETURN p.seaSurveyName AS s, count(*) AS n ORDER BY n DESC LIMIT 10")]
servers = []; t0 = time.perf_counter()
for i in range(n):
    tmp = f"{W}/multi_tmp_{i}"
    servers.append(mcpc.Server(binary, ["--graph", graph], tmp, f"{W}/cwd"))
for s in servers:
    dt, err, txt = s.call("graph_overview"); assert not err, txt[:200]
boot = (time.perf_counter() - t0) * 1000
for s in servers:
    for q in Q:
        for _ in range(3):
            dt, err, txt = s.call("cypher_query", {"query": q}); assert not err, txt[:200]
time.sleep(2)
fps = [mcpc.footprint2(s.pid)[0] for s in servers]
rss = [mcpc.rss(s.pid) for s in servers]
spill = [mcpc.du_bytes(s.tmpdir) for s in servers]
out = dict(label=label, n=n, boot_all_ms=round(boot), fp_sum_mb=round(sum(fps)), fp_each=[round(x) for x in fps],
           rss_sum_mb=round(sum(rss)), rss_each=[round(x) for x in rss], spill_sum_mb=round(sum(spill) / 1048576, 1))
print(json.dumps(out)); 
open(f"{W}/multi_{label}.json", "w").write(json.dumps(out))
for s in servers: s.close()
