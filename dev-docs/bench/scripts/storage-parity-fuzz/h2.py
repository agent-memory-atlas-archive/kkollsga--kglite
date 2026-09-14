"""Replayable randomized differential harness with delta-debug minimization."""
from __future__ import annotations
import os, sys, json, random, shutil, tempfile, traceback, signal, copy
import pandas as pd
import kglite
from kglite import KnowledgeGraph
kglite.set_query_warning_policy("silent")

SCRATCH = os.environ.get("SP_SCRATCH", "/tmp/sp")
MODES = os.environ.get("SP_MODES", "memory,mapped,disk").split(",")
CATS = [f"cat_{i}" for i in range(5)]
NCOLS = ["uid", "name", "cat", "score", "rank", "opt"]

# ───────────────────────── model ─────────────────────────
class Model:
    def __init__(self):
        self.nodes = {}
        self.edges = {}
    def clone(self):
        m = Model(); m.nodes = {k: dict(v) for k, v in self.nodes.items()}
        m.edges = {k: dict(v) for k, v in self.edges.items()}; return m
    def del_node(self, key):
        self.nodes.pop(key, None)
        for ek in [e for e in self.edges if e[1] == key or e[2] == key]:
            del self.edges[ek]

def mk(mode, path):
    if mode == "memory": return KnowledgeGraph()
    if mode == "mapped": return KnowledgeGraph(storage="mapped")
    os.makedirs(os.path.dirname(path), exist_ok=True)
    return KnowledgeGraph(storage="disk", path=path)

def rows(res): return [dict(r) for r in res]

# ───────────────────────── steps ─────────────────────────
def apply_step(st, g, ctxpaths, mode, idx):
    t = st["t"]
    if t == "add_nodes":
        cols = st["cols"]
        df = pd.DataFrame([{c: r.get(c) for c in cols} for r in st["recs"]])
        g.add_nodes(df, st["ntype"], "uid", "name")
    elif t == "add_edges":
        tr = st["triples"]
        df = pd.DataFrame({"s": [x[0] for x in tr], "t": [x[1] for x in tr], "w": [x[2] for x in tr]})
        g.add_connections(df, st["etype"], st["st"], "s", st["tt"], "t")
    elif t == "cypher":
        g.cypher(st["q"])
    elif t == "index":
        try:
            if st["drop"]: g.drop_index(st["ntype"], st["prop"])
            else: g.create_index(st["ntype"], st["prop"])
        except Exception: pass
    elif t == "saveload":
        sp = os.path.join(ctxpaths[mode], f"snap_{idx}") + ("" if mode == "disk" else ".kgl")
        g.save(sp)
        return kglite.load(sp)
    elif t == "vacuum":
        try: g.vacuum()
        except Exception: pass
    elif t == "txn_commit":
        with g.begin() as tx: tx.cypher(st["q"])
    elif t == "txn_rollback":
        tx = g.begin(); tx.cypher(st["q"]); tx.rollback()
    elif t == "copy_isolate":
        c = g.copy()
        c.cypher("MATCH (n:A) SET n.rank = 999")
        c.cypher("CREATE (n:A {uid: 88888, name: 'ghost', rank: 999})")
        del c
    elif t == "freeze_write":
        f = g.freeze(); before = f.node_count()
        g.cypher("CREATE (n:B {uid: 77777, name: 'tmpz', rank: 0})")
        after = f.node_count()
        g.cypher("MATCH (n:B) WHERE n.uid = 77777 DETACH DELETE n")
        if before != after:
            raise AssertionError(f"FREEZE_SNAPSHOT_MOVED {before}->{after}")
    elif t == "session_rt":
        s = g.session()
        s.execute(st["q"])
        return s   # caller must handle: we don't use it (skip)
    else:
        raise ValueError(t)
    return g

def model_step(st, m):
    t = st["t"]
    if t == "add_nodes":
        for r in st["recs"]:
            cur = m.nodes.setdefault((st["ntype"], r["uid"]), {})
            for c in st["cols"]:
                # conflict_handling='update' writes only non-null cells;
                # a null cell preserves the existing value (verified contract).
                if r.get(c) is not None:
                    cur[c] = r.get(c)
                else:
                    cur.setdefault(c, None)
    elif t == "add_edges":
        for s, tt_, w in st["triples"]:
            m.edges[(st["etype"], (st["st"], s), (st["tt"], tt_))] = {"w": w}
    elif t == "cypher":
        _model_cypher(st, m)
    elif t == "txn_commit":
        _model_cypher({"eff": st["eff"]}, m)
    elif t in ("index", "saveload", "vacuum", "txn_rollback", "copy_isolate", "freeze_write"):
        pass
    return m

def _model_cypher(st, m):
    e = st["eff"]; k = e["k"]
    if k == "create":
        m.nodes[(e["ntype"], e["props"]["uid"])] = dict(e["props"])
    elif k == "set_rank_by_cat":
        for key, p in m.nodes.items():
            if key[0] == "A" and p.get("cat") == e["cat"]: p["rank"] = e["val"]
    elif k == "set_score_by_rank":
        for key, p in m.nodes.items():
            if key[0] == "A" and isinstance(p.get("rank"), int) and p["rank"] > 4: p["score"] = float(e["val"])
    elif k == "remove_opt":
        for key, p in m.nodes.items():
            if key[0] == "A" and isinstance(p.get("rank"), int) and p["rank"] >= e["thr"]: p["opt"] = None
    elif k == "delete":
        m.del_node((e["ntype"], e["uid"]))
    elif k == "merge":
        key = ("A", e["uid"])
        if key in m.nodes: m.nodes[key]["rank"] = e["val"]
        else: m.nodes[key] = {"uid": e["uid"], "rank": e["val"], "name": f"nm_{e['uid'] % 12}"}
    elif k == "set_rank_all_B":
        for key, p in m.nodes.items():
            if key[0] == "B": p["rank"] = e["val"]
    elif k == "noop":
        pass
    else:
        raise ValueError(k)

# ───────────────────────── queries ─────────────────────────
def Q(g, q): return rows(g.cypher(q))

def q_counts(g, m):
    got = {r["t"]: r["c"] for r in Q(g, "MATCH (n:A) RETURN 'A' AS t, count(n) AS c UNION ALL MATCH (n:B) RETURN 'B' AS t, count(n) AS c")}
    return {"A": got.get("A", 0), "B": got.get("B", 0)}, {"A": sum(1 for k in m.nodes if k[0]=="A"), "B": sum(1 for k in m.nodes if k[0]=="B")}
def q_cat_eq(g, m):
    got = {r["cat"]: r["c"] for r in Q(g, "MATCH (n:A) WHERE n.cat = 'cat_2' RETURN n.cat AS cat, count(n) AS c")}
    return got.get("cat_2", 0), sum(1 for k,p in m.nodes.items() if k[0]=="A" and p.get("cat")=="cat_2")
def q_range(g, m):
    return Q(g,"MATCH (n:A) WHERE n.score >= 10.0 AND n.score < 40.0 RETURN count(n) AS c")[0]["c"], \
           sum(1 for k,p in m.nodes.items() if k[0]=="A" and isinstance(p.get("score"),float) and 10.0<=p["score"]<40.0)
def q_in(g, m):
    return sorted(r["u"] for r in Q(g,"MATCH (n:A) WHERE n.rank IN [1,3,5] RETURN n.uid AS u")), \
           sorted(k[1] for k,p in m.nodes.items() if k[0]=="A" and p.get("rank") in (1,3,5))
def q_name_eq(g, m):
    return sorted(r["u"] for r in Q(g,"MATCH (n:A) WHERE n.name = 'nm_7' RETURN n.uid AS u")), \
           sorted(k[1] for k,p in m.nodes.items() if k[0]=="A" and p.get("name")=="nm_7")
def q_null(g, m):
    return Q(g,"MATCH (n:A) WHERE n.opt IS NULL RETURN count(n) AS c")[0]["c"], \
           sum(1 for k,p in m.nodes.items() if k[0]=="A" and p.get("opt") is None)
def q_notnull(g, m):
    return sorted(r["u"] for r in Q(g,"MATCH (n:A) WHERE n.opt IS NOT NULL RETURN n.uid AS u")), \
           sorted(k[1] for k,p in m.nodes.items() if k[0]=="A" and p.get("opt") is not None)
def q_edges(g, m):
    return sorted((r["s"],r["t"],r["w"]) for r in Q(g,"MATCH (a:A)-[r:R]->(b:B) RETURN a.uid AS s, b.uid AS t, r.w AS w")), \
           sorted((k[1][1],k[2][1],p.get("w")) for k,p in m.edges.items() if k[0]=="R")
def q_edges_s(g, m):
    return sorted((r["s"],r["t"],r["w"]) for r in Q(g,"MATCH (a:B)-[r:S]->(b:A) RETURN a.uid AS s, b.uid AS t, r.w AS w")), \
           sorted((k[1][1],k[2][1],p.get("w")) for k,p in m.edges.items() if k[0]=="S")
def q_edge_count(g, m):
    return Q(g,"MATCH ()-[r]->() RETURN count(r) AS c")[0]["c"], len(m.edges)
def q_two_hop(g, m):
    got = sorted((r["s"],r["t"]) for r in Q(g,"MATCH (a:A)-[:R]->(b:B)-[:S]->(c:A) RETURN a.uid AS s, c.uid AS t"))
    exp = [(k1[1][1], k2[2][1]) for k1 in m.edges if k1[0]=="R" for k2 in m.edges if k2[0]=="S" and k2[1]==k1[2]]
    return got, sorted(exp)
def q_sum(g, m):
    return Q(g,"MATCH (n:A) RETURN sum(n.rank) AS s")[0]["s"], \
           sum(p["rank"] for k,p in m.nodes.items() if k[0]=="A" and isinstance(p.get("rank"),int))
def q_distinct(g, m):
    return sorted(r["c"] for r in Q(g,"MATCH (n:A) RETURN DISTINCT n.cat AS c") if r["c"] is not None), \
           sorted({p["cat"] for k,p in m.nodes.items() if k[0]=="A" and p.get("cat") is not None})
def q_order(g, m):
    got=[(r["u"],r["s"]) for r in Q(g,"MATCH (n:A) WHERE n.score IS NOT NULL RETURN n.uid AS u, n.score AS s ORDER BY n.score DESC, n.uid ASC LIMIT 5")]
    exp=sorted(((k[1],p["score"]) for k,p in m.nodes.items() if k[0]=="A" and p.get("score") is not None), key=lambda x:(-x[1],x[0]))[:5]
    return got, exp
def q_group(g, m):
    got=sorted((r["c"],r["n"]) for r in Q(g,"MATCH (n:A) WHERE n.cat IS NOT NULL RETURN n.cat AS c, count(n) AS n"))
    d={}
    for k,p in m.nodes.items():
        if k[0]=="A" and p.get("cat") is not None: d[p["cat"]]=d.get(p["cat"],0)+1
    return got, sorted(d.items())
def q_optional(g, m):
    got=sorted((r["u"], -1 if r["t"] is None else r["t"]) for r in Q(g,"MATCH (a:A) OPTIONAL MATCH (a)-[:R]->(b:B) RETURN a.uid AS u, b.uid AS t"))
    exp=[]
    for k in m.nodes:
        if k[0]!="A": continue
        tg=[e[2][1] for e in m.edges if e[0]=="R" and e[1]==k]
        exp.extend((k[1],t) for t in tg) if tg else exp.append((k[1],-1))
    return got, sorted(exp)
def q_point(g, m):
    return sorted((r["u"],r["c"],r["s"]) for r in Q(g,"MATCH (n:A) WHERE n.uid = 4 RETURN n.uid AS u, n.cat AS c, n.score AS s")), \
           sorted((k[1],p.get("cat"),p.get("score")) for k,p in m.nodes.items() if k==("A",4))
def q_degree(g, m):
    got=sorted((r["u"],r["d"]) for r in Q(g,"MATCH (n:A) OPTIONAL MATCH (n)-[r:R]->() RETURN n.uid AS u, count(r) AS d"))
    exp=sorted((k[1], sum(1 for e in m.edges if e[0]=="R" and e[1]==k)) for k in m.nodes if k[0]=="A")
    return got, exp
def q_props_all(g, m):
    got=sorted((r["u"],r["c"],r["r"],r["o"]) for r in Q(g,"MATCH (n:A) RETURN n.uid AS u, n.cat AS c, n.rank AS r, n.opt AS o"))
    exp=sorted((k[1],p.get("cat"),p.get("rank"),p.get("opt")) for k,p in m.nodes.items() if k[0]=="A")
    return got, exp

def q_stub(g, m):
    """Vivified stub nodes (null uid) — expected zero: every endpoint we
    reference was in the model when the step was generated."""
    got = Q(g, "MATCH (n) WHERE n.uid IS NULL RETURN count(n) AS c")[0]["c"]
    return got, 0

QUERIES=[("stub",q_stub),("counts",q_counts),("cat_eq",q_cat_eq),("range",q_range),("in_list",q_in),("name_eq",q_name_eq),
 ("is_null",q_null),("not_null",q_notnull),("edges_R",q_edges),("edges_S",q_edges_s),("edge_count",q_edge_count),
 ("two_hop",q_two_hop),("sum_rank",q_sum),("distinct_cat",q_distinct),("order_limit",q_order),("group_by",q_group),
 ("optional",q_optional),("point",q_point),("degree",q_degree),("props_all",q_props_all)]

def norm(x):
    if isinstance(x,(tuple,list)): return [norm(i) for i in x]
    if isinstance(x,dict): return {str(k):norm(v) for k,v in sorted(x.items(),key=lambda kv:str(kv[0]))}
    if isinstance(x,float) and x==int(x): return int(x)
    return x

def check(g, m):
    """Return list of (qname, got, exp) mismatches."""
    bad=[]
    for name,fn in QUERIES:
        try: got,exp=fn(g,m)
        except Exception as e: bad.append((name,"ERROR "+repr(e)[:200],"")); continue
        if norm(got)!=norm(exp): bad.append((name,repr(got)[:300],repr(exp)[:300]))
    return bad

# ───────────────────────── generation ─────────────────────────
def props_for(rng, uid):
    p={"uid":uid,"name":f"nm_{uid%12}","cat":rng.choice(CATS),"score":float(rng.randrange(0,100)),"rank":rng.randrange(0,8)}
    if rng.random()<0.4: p["opt"]=f"o_{rng.randrange(0,3)}"
    return p

def gen_script(seed, n_ops):
    rng=random.Random(seed); m=Model(); script=[]
    nxt=[1000+seed*100]
    def emit(st):
        script.append(st); model_step(st,m)
    # deterministic seed prologue: declare both types with the full column set,
    # so Cypher CREATE never introduces an undeclared property and never
    # predates the type's unique-id declaration (see findings F1/F3).
    for nt in ("A","B"):
        recs=[props_for(rng,u) for u in (0,1)]
        for r_ in recs:
            for c in NCOLS: r_.setdefault(c,None)
            r_["opt"]=r_.get("opt") or "o_0"
        emit({"t":"add_nodes","ntype":nt,"recs":recs,"cols":list(NCOLS)})
    for i in range(n_ops):
        r=rng.random()
        if r<0.17:
            ntype=rng.choice(["A","B"]); seen=set(); recs=[]
            for _ in range(rng.randrange(1,6)):
                u=rng.randrange(0,24)
                if u in seen: continue
                seen.add(u); recs.append(props_for(rng,u))
            cols=[c for c in NCOLS if any(c in r_ for r_ in recs)]
            for r_ in recs:
                for c in cols: r_.setdefault(c,None)
            emit({"t":"add_nodes","ntype":ntype,"recs":recs,"cols":cols})
        elif r<0.32:
            As=[k[1] for k in m.nodes if k[0]=="A"]; Bs=[k[1] for k in m.nodes if k[0]=="B"]
            if not As or not Bs: continue
            etype=rng.choice(["R","S"])
            st_,tt_,src,tgt=("A","B",As,Bs) if etype=="R" else ("B","A",Bs,As)
            pairs=set()
            for _ in range(rng.randrange(1,5)): pairs.add((rng.choice(src),rng.choice(tgt)))
            pairs=[p for p in sorted(pairs) if (etype,(st_,p[0]),(tt_,p[1])) not in m.edges]
            if not pairs: continue
            emit({"t":"add_edges","etype":etype,"st":st_,"tt":tt_,
                  "triples":[[a,b,rng.randrange(1,50)] for a,b in pairs]})
        elif r<0.40:
            ntype=rng.choice(["A","B"]); uid=nxt[0]; nxt[0]+=1; p=props_for(rng,uid)
            q=f"CREATE (n:{ntype} {{"+", ".join(f"{k}: {json.dumps(v)}" for k,v in p.items())+"}})"
            q=q.replace("}})","})")
            emit({"t":"cypher","q":q,"eff":{"k":"create","ntype":ntype,"props":p}})
        elif r<0.48:
            cat=rng.choice(CATS); val=rng.randrange(0,8)
            emit({"t":"cypher","q":f"MATCH (n:A) WHERE n.cat = '{cat}' SET n.rank = {val}",
                  "eff":{"k":"set_rank_by_cat","cat":cat,"val":val}})
        elif r<0.54:
            val=rng.randrange(0,100)
            emit({"t":"cypher","q":f"MATCH (n:A) WHERE n.rank > 4 SET n.score = {float(val)}",
                  "eff":{"k":"set_score_by_rank","val":val}})
        elif r<0.60:
            thr=rng.randrange(0,8)
            emit({"t":"cypher","q":f"MATCH (n:A) WHERE n.rank >= {thr} REMOVE n.opt",
                  "eff":{"k":"remove_opt","thr":thr}})
        elif r<0.70:
            ntype=rng.choice(["A","B"]); cands=[k[1] for k in m.nodes if k[0]==ntype]
            if not cands: continue
            u=rng.choice(cands)
            emit({"t":"cypher","q":f"MATCH (n:{ntype}) WHERE n.uid = {u} DETACH DELETE n",
                  "eff":{"k":"delete","ntype":ntype,"uid":u}})
        elif r<0.77:
            u=rng.randrange(0,24); val=rng.randrange(0,8)
            emit({"t":"cypher","q":f"MERGE (n:A {{uid: {u}}}) ON CREATE SET n.rank = {val}, n.name = 'nm_{u%12}' ON MATCH SET n.rank = {val}",
                  "eff":{"k":"merge","uid":u,"val":val}})
        elif r<0.85:
            emit({"t":"index","ntype":rng.choice(["A","B"]),"prop":rng.choice(["cat","score","rank","name","opt","uid"]),
                  "drop":rng.random()<0.35})
        elif r<0.90:
            emit({"t":"saveload"})
        elif r<0.92:
            emit({"t":"vacuum"})
        elif r<0.95:
            val=rng.randrange(0,8)
            emit({"t":"txn_commit","q":f"MATCH (n:B) SET n.rank = {val}","eff":{"k":"set_rank_all_B","val":val}})
        elif r<0.97:
            emit({"t":"txn_rollback","q":f"MATCH (n:A) SET n.rank = {rng.randrange(100,200)}"})
        elif r<0.985:
            emit({"t":"copy_isolate"})
        else:
            emit({"t":"freeze_write"})
    return script

# ───────────────────────── run ─────────────────────────
def run_script(script, mode, workdir, check_each=True):
    """Returns (final_bad, per_step_bad, error). per_step_bad only when check_each."""
    d=os.path.join(workdir,mode); shutil.rmtree(d,ignore_errors=True); os.makedirs(d,exist_ok=True)
    paths={mode:d}
    g=mk(mode, os.path.join(d,"g")); m=Model()
    per=[]
    for i,st in enumerate(script):
        try:
            g=apply_step(st,g,paths,mode,i)
        except Exception as e:
            return None, per, {"step":i,"op":st,"err":repr(e)[:300],"tb":traceback.format_exc()[-800:]}
        model_step(st,m)
        if check_each:
            bad=check(g,m)
            if bad: per.append((i,st,bad))
    return check(g,m), per, None

def minimize(script, mode, workdir, predicate):
    """ddmin over steps: keep shortest subsequence where predicate(result) holds."""
    cur=list(script); n=2
    while len(cur)>=2:
        chunk=max(1,len(cur)//n); reduced=False
        for i in range(0,len(cur),chunk):
            cand=cur[:i]+cur[i+chunk:]
            if not cand: continue
            try: res=run_script(cand,mode,workdir,check_each=False)
            except Exception: continue
            if predicate(res):
                cur=cand; n=max(n-1,2); reduced=True; break
        if not reduced:
            if n>=len(cur): break
            n=min(len(cur),2*n)
    return cur

def main():
    seeds=[int(x) for x in sys.argv[1:]] or list(range(30))
    n_ops=int(os.environ.get("SP_OPS","40"))
    workdir=tempfile.mkdtemp(dir=SCRATCH)
    out=[]
    for s in seeds:
        script=gen_script(s,n_ops)
        results={}
        for mode in MODES:
            try:
                signal.alarm(600)
                results[mode]=run_script(script,mode,workdir)
                signal.alarm(0)
            except Exception as e:
                signal.alarm(0)
                results[mode]=(None,[],{"step":-1,"err":"HARNESS "+repr(e)[:300]})
        rec={"seed":s,"n_ops":len(script)}
        interesting=False
        for mode,(fin,per,err) in results.items():
            if err: rec.setdefault("errors",{})[mode]=err; interesting=True
            if fin: rec.setdefault("final_mismatch",{})[mode]=fin; interesting=True
            if per: rec.setdefault("first_step_mismatch",{})[mode]={"step":per[0][0],"op":per[0][1],"bad":per[0][2]}; interesting=True
        if interesting:
            rec["script"]=script
            out.append(rec)
        sys.stderr.write(f"seed {s}: {'MISMATCH' if interesting else 'clean'}\n"); sys.stderr.flush()
    json.dump(out, open(os.environ.get("SP_OUT","/tmp/sp_out.json"),"w"), indent=1, default=str)
    print(f"{len(out)} interesting of {len(seeds)} seeds -> {os.environ.get('SP_OUT','/tmp/sp_out.json')}")
    shutil.rmtree(workdir,ignore_errors=True)

if __name__=="__main__":
    main()
