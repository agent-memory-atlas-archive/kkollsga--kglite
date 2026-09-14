import os, sys, json, tempfile, shutil
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import h2, kglite

path, seed, mode, stop = sys.argv[1], int(sys.argv[2]), sys.argv[3], int(sys.argv[4])
script = next(r["script"] for r in json.load(open(path)) if r["seed"]==seed)
wd = tempfile.mkdtemp(dir=os.environ["SP_SCRATCH"])
d = os.path.join(wd, mode); os.makedirs(d, exist_ok=True)
g = h2.mk(mode, os.path.join(d,"g")); m = h2.Model()
for i, st in enumerate(script[:stop+1]):
    if i == stop:
        nt = st["eff"]["ntype"]; uid = st["eff"]["uid"]
        print(f"--- step {stop}: {st['q']}")
        print("  BEFORE all %s:" % nt, g.cypher(f"MATCH (n:{nt}) RETURN n.uid AS u, n.id AS i, n.name AS nm ORDER BY u").to_list())
        print("  BEFORE match:", g.cypher(f"MATCH (n:{nt}) WHERE n.uid = {uid} RETURN n.uid AS u, n.id AS i, n.name AS nm").to_list())
        print("  BEFORE props:", g.cypher(f"MATCH (n:{nt}) WHERE n.uid = {uid} RETURN properties(n) AS p").to_list())
        print("  model has   :", sorted(k[1] for k in m.nodes if k[0]==nt))
    g = h2.apply_step(st, g, {mode:d}, mode, i)
    h2.model_step(st, m)
    if i == stop:
        print("  AFTER  all %s:" % nt, g.cypher(f"MATCH (n:{nt}) RETURN n.uid AS u, n.id AS i, n.name AS nm ORDER BY u").to_list())
        print("  model after :", sorted(k[1] for k in m.nodes if k[0]==nt))
        print("  bad:", json.dumps(h2.check(g,m), default=str)[:400])
shutil.rmtree(wd, ignore_errors=True)
