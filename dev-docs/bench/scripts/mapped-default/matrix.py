"""Boot/footprint/latency matrix. usage: matrix.py RUN [graphs...]"""
import sys, os, json, time, statistics as st, subprocess
sys.path.insert(0, os.path.dirname(__file__))
import mcpc
W = os.environ["MD_WORK"]            # scratch dir holding v8/ n/ c/ copies and the reference venv
O = W
SCR = f"{W}/cwd"   # scratch cwd
os.makedirs(SCR, exist_ok=True)
REF = os.environ["MD_REF_SERVER"]; PYB = f"{W}/branch-py-server"
VEC = json.load(open(f"{W}/legal_vec.json"))
vq = "MATCH (m:CourtDecision) RETURN m.title AS t, vector_score(m,'sammendrag_emb',%s) AS s ORDER BY s DESC LIMIT 10" % json.dumps(VEC)
def gen(x, y):
    return [("trav_agg", f"MATCH (a:{x})--(b:{y}) RETURN b.title AS t, count(a) AS n ORDER BY n DESC LIMIT 10"),
            ("prop_filter", f"MATCH (a:{x}) WHERE a.title CONTAINS 'e' RETURN a.title AS t ORDER BY t LIMIT 50")]
G = {
 "legal": ("legal/norwegian_law.kgl", [
    ("cites_agg", "MATCH (c:CourtDecision)-[:CITES]->(s:LawSection) RETURN s.title AS t, count(c) AS n ORDER BY n DESC LIMIT 10"),
    ("prop_filter", "MATCH (c:CourtDecision) WHERE c.court_level='hoyesterett' AND c.year>=2015 RETURN c.title AS t, c.sammendrag AS s ORDER BY c.dato DESC LIMIT 50"),
    ("kw_agg", "MATCH (c:CourtDecision)-[:HAS_KEYWORD]->(k:Keyword) RETURN k.title AS t, count(c) AS n ORDER BY n DESC LIMIT 10"),
    ("vector_top10", vq)], 60),
 "sodir": ("sodir/sodir_graph.kgl", [
    ("drilled_by_agg", "MATCH (w:Wellbore)-[:DRILLED_BY]->(c:Company) RETURN c.title AS t, count(w) AS n ORDER BY n DESC LIMIT 10"),
    ("mud_filter", "MATCH (m:WellboreMud) WHERE m.wlbMudType='WATER BASED' AND m.wlbMudWeightAtMD > 1.5 RETURN count(m) AS n"),
    ("seismic_colagg", "MATCH (p:SeismicPolygon) RETURN p.seaSurveyName AS s, count(*) AS n, avg(p.seaPolygonNSDeg) AS a ORDER BY n DESC LIMIT 10"),
    ("wellbore_filter", "MATCH (w:Wellbore) WHERE w.wlbCompletionYear >= 2010 AND w.wlbContent = 'OIL' RETURN w.title AS t, w.wlbDrillingOperator AS o, w.wlbFinalVerticalDepth AS d ORDER BY d DESC LIMIT 25")], 60),
 "petrel": ("petrel/petrel_help.kgl", gen("Article", "Keyword"), 15),
 "codebase": ("codebase/kglite_codebase.kgl", gen("Function", "File"), 15),
 "music": ("music/music.kgl", gen("Track", "Artist"), 15),
 "rms": ("rms/graph.kgl", gen("Chunk", "Section"), 15),
}
VARIANTS = {"A_wheel": (REF, "v8", []), "N_default": (PYB, "n", []), "N_heap": (PYB, "n", ["--embeddings", "memory", "--columns", "memory"]),
            "C_compact": (PYB, "c", [])}
FULL = ("legal", "sodir")  # graphs that also get C_mapped_heap
NFRESH = 5; WARM = 50

def run_cell(g, vname, out):
    rel, queries, idle = G[g]
    binary, ver, flags = VARIANTS[vname]
    path = f"{O}/{ver}/{rel}"
    cell = dict(boot_ms=[], fp_boot=[], rss_boot=[], clean_boot=[], spill_boot=[], cold_ms=[])
    for i in range(NFRESH):
        tmp = f"{O}/tmp_spill"
        t0 = time.perf_counter()
        s = mcpc.Server(binary, ["--graph", path] + flags, tmp, SCR)
        dt, err, txt = s.call("graph_overview")
        boot = (time.perf_counter() - t0) * 1000
        assert not err, txt[:300]
        cell["boot_ms"].append(boot)
        time.sleep(0.3)
        fp, cl = mcpc.footprint2(s.pid)
        cell["fp_boot"].append(fp); cell["clean_boot"].append(cl); cell["rss_boot"].append(mcpc.rss(s.pid))
        cell["spill_boot"].append(mcpc.du_bytes(tmp))
        dt, err, txt = s.call("cypher_query", {"query": queries[0][1]})
        assert not err, txt[:300]
        cell["cold_ms"].append(dt)
        if i == 0:
            lat = {}
            for name, q in queries:
                for _ in range(3): s.call("cypher_query", {"query": q})
                ts = []
                for _ in range(WARM):
                    d, e, _t = s.call("cypher_query", {"query": q}); assert not e
                    ts.append(d)
                lat[name] = dict(min=min(ts), median=st.median(ts))
            cell["warm"] = lat
            cell["fp_work"], cell["clean_work"] = mcpc.footprint2(s.pid); cell["rss_work"] = mcpc.rss(s.pid)
            cell["spill_work"] = mcpc.du_bytes(tmp)
            time.sleep(idle)
            cell["fp_idle"], cell["clean_idle"] = mcpc.footprint2(s.pid); cell["rss_idle"] = mcpc.rss(s.pid)
        s.close()
    out[f"{g}|{vname}"] = cell
    print(g, vname, "boot", round(st.mean(cell["boot_ms"])), "fp", cell["fp_boot"][0], "work", cell["fp_work"], "idle", cell["fp_idle"], flush=True)

if __name__ == "__main__":
    run = sys.argv[1]; graphs = sys.argv[2:] or list(G)
    out = {"uptime": subprocess.run(["uptime"], capture_output=True, text=True).stdout.strip(), "py": sys.version}
    fn = f"{O}/matrix_run{run}.json"
    for g in graphs:
        for v in VARIANTS:
            run_cell(g, v, out)
            json.dump(out, open(fn, "w"), indent=1)
