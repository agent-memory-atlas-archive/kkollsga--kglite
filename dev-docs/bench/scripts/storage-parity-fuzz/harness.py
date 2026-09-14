"""Randomized differential harness: model vs {memory,mapped,disk}, index on/off, passes on/off."""
from __future__ import annotations
import os, sys, json, random, shutil, tempfile, traceback, signal
import pandas as pd
import kglite
from kglite import KnowledgeGraph
kglite.set_query_warning_policy("silent")

SCRATCH = os.environ.get("SP_SCRATCH", "/tmp/sp")
MODES = os.environ.get("SP_MODES", "memory,mapped,disk").split(",")
CATS = [f"cat_{i}" for i in range(5)]

class Model:
    def __init__(self):
        self.nodes = {}   # (ntype, uid) -> props dict
        self.edges = {}   # (etype, (st,su), (tt,tu)) -> props dict
    def clone(self):
        m = Model()
        m.nodes = {k: dict(v) for k, v in self.nodes.items()}
        m.edges = {k: dict(v) for k, v in self.edges.items()}
        return m
    def del_node(self, key):
        self.nodes.pop(key, None)
        for ek in [e for e in self.edges if e[1] == key or e[2] == key]:
            del self.edges[ek]

def mk(mode, path):
    if mode == "memory":
        return KnowledgeGraph()
    if mode == "mapped":
        return KnowledgeGraph(storage="mapped")
    if mode == "disk":
        return KnowledgeGraph(storage="disk", path=path)
    raise ValueError(mode)

def rows(res):
    return [dict(r) for r in res]

# ---------- query battery ----------
def q_counts(g, m):
    got = {r["t"]: r["c"] for r in rows(g.cypher("MATCH (n:A) RETURN 'A' AS t, count(n) AS c UNION ALL MATCH (n:B) RETURN 'B' AS t, count(n) AS c"))}
    exp = {"A": sum(1 for k in m.nodes if k[0] == "A"), "B": sum(1 for k in m.nodes if k[0] == "B")}
    got = {"A": got.get("A", 0), "B": got.get("B", 0)}
    return got, exp

def q_cat_eq(g, m):
    got = {r["cat"]: r["c"] for r in rows(g.cypher("MATCH (n:A) WHERE n.cat = 'cat_2' RETURN n.cat AS cat, count(n) AS c"))}
    n = sum(1 for k, p in m.nodes.items() if k[0] == "A" and p.get("cat") == "cat_2")
    return got.get("cat_2", 0), n

def q_range(g, m):
    got = rows(g.cypher("MATCH (n:A) WHERE n.score >= 10.0 AND n.score < 40.0 RETURN count(n) AS c"))[0]["c"]
    exp = sum(1 for k, p in m.nodes.items() if k[0] == "A" and isinstance(p.get("score"), float) and 10.0 <= p["score"] < 40.0)
    return got, exp

def q_in(g, m):
    got = sorted(r["u"] for r in rows(g.cypher("MATCH (n:A) WHERE n.rank IN [1,3,5] RETURN n.uid AS u")))
    exp = sorted(k[1] for k, p in m.nodes.items() if k[0] == "A" and p.get("rank") in (1, 3, 5))
    return got, exp

def q_name_eq(g, m):
    got = sorted(r["u"] for r in rows(g.cypher("MATCH (n:A) WHERE n.name = 'nm_7' RETURN n.uid AS u")))
    exp = sorted(k[1] for k, p in m.nodes.items() if k[0] == "A" and p.get("name") == "nm_7")
    return got, exp

def q_null(g, m):
    got = rows(g.cypher("MATCH (n:A) WHERE n.opt IS NULL RETURN count(n) AS c"))[0]["c"]
    exp = sum(1 for k, p in m.nodes.items() if k[0] == "A" and p.get("opt") is None)
    return got, exp

def q_notnull(g, m):
    got = sorted(r["u"] for r in rows(g.cypher("MATCH (n:A) WHERE n.opt IS NOT NULL RETURN n.uid AS u")))
    exp = sorted(k[1] for k, p in m.nodes.items() if k[0] == "A" and p.get("opt") is not None)
    return got, exp

def q_edges(g, m):
    got = sorted((r["s"], r["t"], r["w"]) for r in rows(g.cypher("MATCH (a:A)-[r:R]->(b:B) RETURN a.uid AS s, b.uid AS t, r.w AS w")))
    exp = sorted((k[1][1], k[2][1], p.get("w")) for k, p in m.edges.items() if k[0] == "R")
    return got, exp

def q_edge_count(g, m):
    got = rows(g.cypher("MATCH ()-[r]->() RETURN count(r) AS c"))[0]["c"]
    return got, len(m.edges)

def q_neigh(g, m):
    got = sorted((r["s"], r["t"]) for r in rows(g.cypher("MATCH (a:A)-[:R]->(b:B)-[:S]->(c:A) RETURN a.uid AS s, c.uid AS t")))
    exp = []
    for k1 in m.edges:
        if k1[0] != "R":
            continue
        for k2 in m.edges:
            if k2[0] == "S" and k2[1] == k1[2]:
                exp.append((k1[1][1], k2[2][1]))
    return got, sorted(exp)

def q_sum(g, m):
    got = rows(g.cypher("MATCH (n:A) RETURN sum(n.rank) AS s"))[0]["s"]
    exp = sum(p["rank"] for k, p in m.nodes.items() if k[0] == "A" and isinstance(p.get("rank"), int))
    return got, exp

def q_distinct(g, m):
    got = sorted(r["c"] for r in rows(g.cypher("MATCH (n:A) RETURN DISTINCT n.cat AS c")) if r["c"] is not None)
    exp = sorted({p["cat"] for k, p in m.nodes.items() if k[0] == "A" and p.get("cat") is not None})
    return got, exp

def q_order(g, m):
    got = [(r["u"], r["s"]) for r in rows(g.cypher("MATCH (n:A) WHERE n.score IS NOT NULL RETURN n.uid AS u, n.score AS s ORDER BY n.score DESC, n.uid ASC LIMIT 5"))]
    cand = sorted(((k[1], p["score"]) for k, p in m.nodes.items() if k[0] == "A" and p.get("score") is not None),
                  key=lambda x: (-x[1], x[0]))[:5]
    return got, cand

def q_groupby(g, m):
    got = sorted((r["c"], r["n"]) for r in rows(g.cypher("MATCH (n:A) WHERE n.cat IS NOT NULL RETURN n.cat AS c, count(n) AS n")))
    d = {}
    for k, p in m.nodes.items():
        if k[0] == "A" and p.get("cat") is not None:
            d[p["cat"]] = d.get(p["cat"], 0) + 1
    return got, sorted(d.items())

def q_optional(g, m):
    got = sorted((r["u"], r["t"]) for r in rows(g.cypher("MATCH (a:A) OPTIONAL MATCH (a)-[:R]->(b:B) RETURN a.uid AS u, b.uid AS t")))
    exp = []
    for k in m.nodes:
        if k[0] != "A":
            continue
        tg = [e[2][1] for e in m.edges if e[0] == "R" and e[1] == k]
        if tg:
            exp.extend((k[1], t) for t in tg)
        else:
            exp.append((k[1], None))
    return got, sorted(exp, key=lambda x: (x[0], -1 if x[1] is None else x[1]))

def q_point(g, m):
    got = sorted((r["u"], r["c"], r["s"]) for r in rows(g.cypher("MATCH (n:A) WHERE n.uid = 4 RETURN n.uid AS u, n.cat AS c, n.score AS s")))
    exp = sorted((k[1], p.get("cat"), p.get("score")) for k, p in m.nodes.items() if k == ("A", 4))
    return got, exp

def q_degree(g, m):
    got = sorted((r["u"], r["d"]) for r in rows(g.cypher("MATCH (n:A) OPTIONAL MATCH (n)-[r:R]->() RETURN n.uid AS u, count(r) AS d")))
    exp = []
    for k in m.nodes:
        if k[0] == "A":
            exp.append((k[1], sum(1 for e in m.edges if e[0] == "R" and e[1] == k)))
    return got, sorted(exp)

QUERIES = [
    ("counts", q_counts), ("cat_eq", q_cat_eq), ("range", q_range), ("in_list", q_in),
    ("name_eq", q_name_eq), ("is_null", q_null), ("not_null", q_notnull),
    ("edges", q_edges), ("edge_count", q_edge_count), ("two_hop", q_neigh),
    ("sum_rank", q_sum), ("distinct_cat", q_distinct), ("order_limit", q_order),
    ("group_by", q_groupby), ("optional", q_optional), ("point", q_point), ("degree", q_degree),
]

def norm(x):
    if isinstance(x, tuple):
        return [norm(i) for i in x]
    if isinstance(x, list):
        return [norm(i) for i in x]
    if isinstance(x, dict):
        return {str(k): norm(v) for k, v in sorted(x.items(), key=lambda kv: str(kv[0]))}
    if isinstance(x, float) and x == int(x):
        return int(x)
    return x

def battery(g, m, tag, findings, ctx):
    for name, fn in QUERIES:
        try:
            got, exp = fn(g, m)
        except Exception as e:
            findings.append({"kind": "query_error", "query": name, "where": tag, "err": repr(e), "ctx": ctx})
            continue
        if norm(got) != norm(exp):
            findings.append({"kind": "mismatch", "query": name, "where": tag,
                             "got": repr(got)[:400], "exp": repr(exp)[:400], "ctx": ctx})

# ---------- ops ----------
def props_for(rng, uid, ntype):
    p = {"uid": uid, "name": f"nm_{uid % 12}", "cat": rng.choice(CATS),
         "score": float(rng.randrange(0, 100)), "rank": rng.randrange(0, 8)}
    if rng.random() < 0.4:
        p["opt"] = f"o_{rng.randrange(0,3)}"
    return p

def op_add_nodes(rng, state, models, graphs):
    ntype = rng.choice(["A", "B"])
    k = rng.randrange(1, 6)
    uids = [rng.randrange(0, 24) for _ in range(k)]
    seen, recs = set(), []
    for u in uids:
        if u in seen:
            continue
        seen.add(u)
        recs.append(props_for(rng, u, ntype))
    cols = ["uid", "name", "cat", "score", "rank", "opt"]
    df = pd.DataFrame([{c: r.get(c) for c in cols} for r in recs])
    # 'opt' NaN -> None handled by engine as null; drop col if fully absent
    if df["opt"].isna().all():
        df = df.drop(columns=["opt"])
    def apply(g):
        g.add_nodes(df, ntype, "uid", "name")
    def mapply(m):
        for r in recs:
            key = (ntype, r["uid"])
            cur = m.nodes.setdefault(key, {})
            for c in cols:
                if c in df.columns:
                    v = r.get(c)
                    cur[c] = v  # update semantics: cols present in call
    return f"add_nodes({ntype}, {[r['uid'] for r in recs]})", apply, mapply

def op_add_edges(rng, state, models, graphs):
    m = models[MODES[0]]
    As = [k for k in m.nodes if k[0] == "A"]
    Bs = [k for k in m.nodes if k[0] == "B"]
    if not As or not Bs:
        return None
    etype = rng.choice(["R", "S"])
    if etype == "R":
        st, tt, src, tgt = "A", "B", As, Bs
    else:
        st, tt, src, tgt = "B", "A", Bs, As
    pairs = set()
    for _ in range(rng.randrange(1, 5)):
        a = rng.choice(src); b = rng.choice(tgt)
        pairs.add((a[1], b[1]))
    pairs = sorted(pairs)
    # skip pairs already present (avoid parallel-edge model ambiguity)
    pairs = [p for p in pairs if (etype, (st, p[0]), (tt, p[1])) not in m.edges]
    if not pairs:
        return None
    w = [rng.randrange(1, 50) for _ in pairs]
    df = pd.DataFrame({"s": [p[0] for p in pairs], "t": [p[1] for p in pairs], "w": w})
    def apply(g):
        g.add_connections(df, etype, st, "s", tt, "t")
    def mapply(mm):
        for (s, t), ww in zip(pairs, w):
            mm.edges[(etype, (st, s), (tt, t))] = {"w": ww}
    return f"add_edges({etype}, {pairs})", apply, mapply

def op_create(rng, state, models, graphs):
    ntype = rng.choice(["A", "B"])
    uid = state["next_uid"]; state["next_uid"] += 1
    p = props_for(rng, uid, ntype)
    parts = ", ".join(f"{k}: {json.dumps(v)}" for k, v in p.items())
    qy = f"CREATE (n:{ntype} {{{parts}}})"
    def apply(g):
        g.cypher(qy)
    def mapply(m):
        m.nodes[(ntype, uid)] = dict(p)
    return qy, apply, mapply

def op_set(rng, state, models, graphs):
    cat = rng.choice(CATS)
    val = rng.randrange(0, 8)
    qy = f"MATCH (n:A) WHERE n.cat = '{cat}' SET n.rank = {val}"
    def apply(g):
        g.cypher(qy)
    def mapply(m):
        for k, p in m.nodes.items():
            if k[0] == "A" and p.get("cat") == cat:
                p["rank"] = val
    return qy, apply, mapply

def op_set_new(rng, state, models, graphs):
    val = rng.randrange(0, 100)
    qy = f"MATCH (n:A) WHERE n.rank > 4 SET n.score = {float(val)}"
    def apply(g):
        g.cypher(qy)
    def mapply(m):
        for k, p in m.nodes.items():
            if k[0] == "A" and isinstance(p.get("rank"), int) and p["rank"] > 4:
                p["score"] = float(val)
    return qy, apply, mapply

def op_remove(rng, state, models, graphs):
    thr = rng.randrange(0, 8)
    qy = f"MATCH (n:A) WHERE n.rank >= {thr} REMOVE n.opt"
    def apply(g):
        g.cypher(qy)
    def mapply(m):
        for k, p in m.nodes.items():
            if k[0] == "A" and isinstance(p.get("rank"), int) and p["rank"] >= thr:
                p["opt"] = None
    return qy, apply, mapply

def op_delete(rng, state, models, graphs):
    m = models[MODES[0]]
    keys = [k for k in m.nodes]
    if not keys:
        return None
    ntype = rng.choice(["A", "B"])
    cands = [k[1] for k in keys if k[0] == ntype]
    if not cands:
        return None
    u = rng.choice(cands)
    qy = f"MATCH (n:{ntype}) WHERE n.uid = {u} DETACH DELETE n"
    def apply(g):
        g.cypher(qy)
    def mapply(mm):
        mm.del_node((ntype, u))
    return qy, apply, mapply

def op_merge(rng, state, models, graphs):
    u = rng.randrange(0, 24)
    val = rng.randrange(0, 8)
    qy = (f"MERGE (n:A {{uid: {u}}}) ON CREATE SET n.rank = {val}, n.name = 'nm_{u % 12}' "
          f"ON MATCH SET n.rank = {val}")
    def apply(g):
        g.cypher(qy)
    def mapply(m):
        key = ("A", u)
        if key in m.nodes:
            m.nodes[key]["rank"] = val
        else:
            m.nodes[key] = {"uid": u, "rank": val, "name": f"nm_{u % 12}"}
    return qy, apply, mapply

def op_index(rng, state, models, graphs):
    prop = rng.choice(["cat", "score", "rank", "name", "opt", "uid"])
    ntype = rng.choice(["A", "B"])
    drop = rng.random() < 0.35
    def apply(g):
        try:
            if drop:
                g.drop_index(ntype, prop)
            else:
                g.create_index(ntype, prop)
        except Exception:
            pass
    return f"{'drop' if drop else 'create'}_index({ntype}.{prop})", apply, (lambda m: None)

def op_saveload(rng, state, models, graphs):
    return "SAVELOAD", None, None   # handled specially

def op_vacuum(rng, state, models, graphs):
    def apply(g):
        try:
            g.vacuum()
        except Exception:
            pass
    return "vacuum()", apply, (lambda m: None)

def op_txn_commit(rng, state, models, graphs):
    val = rng.randrange(0, 8)
    qy = f"MATCH (n:B) SET n.rank = {val}"
    def apply(g):
        with g.begin() as tx:
            tx.cypher(qy)
    def mapply(m):
        for k, p in m.nodes.items():
            if k[0] == "B":
                p["rank"] = val
    return f"TXN_COMMIT[{qy}]", apply, mapply

def op_txn_rollback(rng, state, models, graphs):
    val = rng.randrange(100, 200)
    qy = f"MATCH (n:A) SET n.rank = {val}"
    def apply(g):
        tx = g.begin()
        tx.cypher(qy)
        tx.rollback()
    return f"TXN_ROLLBACK[{qy}]", apply, (lambda m: None)

def op_copy_isolate(rng, state, models, graphs):
    def apply(g):
        c = g.copy()
        c.cypher("MATCH (n:A) SET n.rank = 999")
        c.cypher("CREATE (n:A {uid: 88888, name: 'ghost'})")
        del c
    return "COPY_MUTATE_DISCARD", apply, (lambda m: None)

def op_freeze_write(rng, state, models, graphs):
    def apply(g):
        f = g.freeze()
        before = f.node_count()
        g.cypher("CREATE (n:B {uid: 77777, name: 'tmpz', rank: 0})")
        after = f.node_count()
        g.cypher("MATCH (n:B) WHERE n.uid = 77777 DETACH DELETE n")
        if before != after:
            raise AssertionError(f"freeze snapshot moved: {before} -> {after}")
    return "FREEZE_THEN_WRITE", apply, (lambda m: None)

OPS = [
    (op_add_nodes, 16), (op_add_edges, 14), (op_create, 8), (op_set, 8), (op_set_new, 6),
    (op_remove, 6), (op_delete, 10), (op_merge, 7), (op_index, 8), (op_saveload, 6),
    (op_vacuum, 3), (op_txn_commit, 4), (op_txn_rollback, 3), (op_copy_isolate, 2),
    (op_freeze_write, 2),
]
OP_POP = [o for o, w in OPS for _ in range(w)]

def run_seed(seed, n_ops, findings, workdir):
    rng = random.Random(seed)
    state = {"next_uid": 1000 + seed * 100}
    graphs, models, paths = {}, {}, {}
    for mode in MODES:
        d = os.path.join(workdir, f"s{seed}_{mode}")
        paths[mode] = d
        os.makedirs(d, exist_ok=True)
        graphs[mode] = mk(mode, os.path.join(d, "g"))
        models[mode] = Model()
    log = []
    for i in range(n_ops):
        opf = rng.choice(OP_POP)
        made = opf(rng, state, models, graphs)
        if made is None:
            continue
        desc, apply, mapply = made
        log.append(desc)
        if desc == "SAVELOAD":
            for mode in MODES:
                try:
                    sp = os.path.join(paths[mode], f"snap_{i}") + ("" if mode == "disk" else ".kgl")
                    graphs[mode].save(sp)
                    graphs[mode] = kglite.load(sp)
                except Exception as e:
                    findings.append({"kind": "op_error", "op": desc, "mode": mode, "seed": seed,
                                     "step": i, "err": repr(e), "log": log[-6:]})
        else:
            for mode in MODES:
                try:
                    apply(graphs[mode])
                except Exception as e:
                    findings.append({"kind": "op_error", "op": desc, "mode": mode, "seed": seed,
                                     "step": i, "err": repr(e)[:300], "log": log[-6:]})
            mapply(models[MODES[0]])
        for mode in MODES:
            if mode != MODES[0]:
                models[mode] = models[MODES[0]]
        ctx = {"seed": seed, "step": i, "op": desc, "log_tail": log[-5:]}
        for mode in MODES:
            battery(graphs[mode], models[MODES[0]], mode, findings, ctx)
        if findings and len(findings) > 60:
            return log
    return log

def main():
    seeds = [int(x) for x in sys.argv[1:]] or list(range(30))
    n_ops = int(os.environ.get("SP_OPS", "40"))
    findings = []
    workdir = tempfile.mkdtemp(dir=SCRATCH)
    for s in seeds:
        try:
            signal.alarm(300)
            run_seed(s, n_ops, findings, workdir)
            signal.alarm(0)
        except Exception as e:
            signal.alarm(0)
            findings.append({"kind": "harness_error", "seed": s, "err": repr(e)[:400],
                             "tb": traceback.format_exc()[-1500:]})
        sys.stderr.write(f"seed {s} done ({len(findings)} findings)\n")
        if len(findings) > 60:
            break
    print(json.dumps(findings, indent=1, default=str))
    shutil.rmtree(workdir, ignore_errors=True)

if __name__ == "__main__":
    main()
