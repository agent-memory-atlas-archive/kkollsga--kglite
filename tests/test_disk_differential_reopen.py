"""Seeded memory-versus-disk differential with interleaved save and reopen.

Every step applies one random write (CREATE, SET across value kinds and null,
REMOVE, DELETE and recreate, MERGE, tail-appending add_nodes, edge writes,
committed and rolled-back transactions) to an in-memory graph, the oracle, and
to a disk graph, and compares the full observable state after the step. Steps
also save the disk graph and reopen it, so a defect that only a persisted
generation shows (a lost SET, an index that went empty, a stale title lookup)
surfaces as a divergence from memory.
"""

from __future__ import annotations

import datetime as dt
import json
import math
import random
import re
from pathlib import Path

import pandas as pd

import pytest

import kglite

T0 = dt.datetime(2020, 1, 1)


# ----------------------------------------------------------------- helpers
def norm(v):
    if isinstance(v, dict):
        return {k: norm(v[k]) for k in sorted(v)}
    if isinstance(v, (list, tuple)):
        return [norm(x) for x in v]
    if isinstance(v, float):
        if math.isnan(v):
            return "NaN"
        return round(v, 9)
    if isinstance(v, (dt.datetime, dt.date)):
        return "TS:" + v.isoformat()
    if hasattr(v, "isoformat"):
        return "TS:" + v.isoformat()
    return v


def rows(rv):
    return [norm(r) for r in (rv.to_list() if hasattr(rv, "to_list") else list(rv))]


def stamp(i):
    return T0 + dt.timedelta(days=int(i))


def q(g, text, **params):
    return rows(g.cypher(text, params=params or None))


# --------------------------------------------------------------- snapshot
def snapshot(g, touched_props, sample_ids, declared):
    snap = {}
    snap["types"] = sorted(g.node_types)
    snap["nodes"] = sorted(
        q(g, "MATCH (n) RETURN labels(n) AS l, properties(n) AS p"),
        key=lambda r: json.dumps(r, sort_keys=True, default=str),
    )
    snap["edges"] = sorted(
        q(
            g,
            "MATCH (a)-[r]->(b) RETURN labels(a) AS la, a.id AS a, type(r) AS t, "
            "labels(b) AS lb, b.id AS b, properties(r) AS p",
        ),
        key=lambda r: json.dumps(r, sort_keys=True, default=str),
    )
    for t in snap["types"]:
        snap[f"count:{t}"] = q(g, f"MATCH (n:{t}) RETURN count(n) AS c")
        snap[f"scan:{t}"] = q(g, f"MATCH (n:{t}) RETURN n.id AS id ORDER BY n.id")
    for t, i in sample_ids:
        snap[f"lookup:{t}:{i}"] = q(g, f"MATCH (n:{t} {{id: $x}}) RETURN n.id AS id, n.title AS title, n.score AS score", x=i)
        snap[f"lookup_any:{i}"] = sorted(
            q(g, "MATCH (n {id: $x}) RETURN labels(n) AS l, n.id AS id", x=i),
            key=lambda r: json.dumps(r, sort_keys=True, default=str),
        )
    for t, p in touched_props:
        if t in snap["types"]:
            snap[f"order:{t}:{p}"] = q(
                g, f"MATCH (n:{t}) WHERE n.{p} IS NOT NULL RETURN n.id AS id, n.{p} AS v ORDER BY n.{p}, n.id"
            )
            snap[f"nulls:{t}:{p}"] = q(g, f"MATCH (n:{t}) WHERE n.{p} IS NULL RETURN n.id AS id ORDER BY n.id")
    # property-index lookups (index created on Person.tag at start)
    for tag in ("t0", "t1", "t2", "newtag"):
        snap[f"idx:Person:tag={tag}"] = q(g, "MATCH (n:Person {tag: $t}) RETURN n.id AS id ORDER BY n.id", t=tag)
        snap[f"idxw:Person:tag={tag}"] = q(g, "MATCH (n:Person) WHERE n.tag = $t RETURN n.id AS id ORDER BY n.id", t=tag)
    snap["title:renamed"] = q(g, "MATCH (n {title: 'renamed'}) RETURN n.id AS id ORDER BY n.id")
    snap["title:literal"] = q(g, "MATCH (n:Person) WHERE n.title = 're' RETURN n.id AS id ORDER BY n.id")
    if declared:
        for inst in (dt.datetime(2020, 1, 10), dt.datetime(2020, 2, 15), dt.datetime(2021, 1, 1)):
            snap[f"asof:{inst.date()}"] = rows(
                g.cypher("MATCH (n:Person) RETURN n.id AS id ORDER BY n.id", valid_at=inst)
            )
    return snap


def diff(a, b):
    out = []
    for k in sorted(set(a) | set(b)):
        if a.get(k) != b.get(k):
            out.append((k, a.get(k), b.get(k)))
    return out


# ------------------------------------------------------------- operations
def initial_frame(t, ids):
    return pd.DataFrame(
        {
            "id": ids,
            "title": [f"{t}-{i}" for i in ids],
            "score": [i * 10 for i in ids],
            "tag": [f"t{i % 3}" for i in ids],
            "status": ["open" for _ in ids],
            "ts": [stamp(i) for i in ids],
            "vf": [stamp(i) for i in ids],
            "vt": [stamp(i + 40) if i % 4 else pd.NaT for i in ids],
        }
    )


def apply(g, op):
    """Apply one op to graph `g`. Returns (ok, result_or_errname)."""
    k = op["k"]
    try:
        if k == "cypher":
            r = q(g, op["q"], **op.get("p", {}))
            return True, r
        if k == "add_nodes":
            df = pd.DataFrame(op["df"])
            for c in op.get("ts_cols", []):
                df[c] = pd.to_datetime(df[c])
            r = g.add_nodes(df, op["t"], "id", "title")
            return True, "add_nodes"
        if k == "tx":
            tx = g.begin()
            errs = []
            for s in op["stmts"]:
                try:
                    tx.cypher(s["q"], params=s.get("p") or None)
                except Exception as e:  # noqa: BLE001
                    errs.append(type(e).__name__)
            if op["commit"]:
                tx.commit()
            else:
                tx.rollback()
            return True, errs
        if k == "create_index":
            g.create_index(op["t"], op["prop"])
            return True, "idx"
        if k == "set_temporal":
            g.set_temporal(op["t"], "vf", "vt", convention="half_open")
            return True, "temporal"
        raise ValueError(k)
    except Exception as e:  # noqa: BLE001
        return False, type(e).__name__ + ": " + str(e)[:120]


class Model:
    """Only drives op generation; never the oracle."""

    def __init__(self, rng):
        self.rng = rng
        self.order = {"Person": [], "Department": []}  # insertion order of live ids per type
        self.next_id = 100
        self.saved_once = False
        self.touched = set()
        self.declared = False

    def live(self, t):
        return self.order.get(t, [])

    def pick(self, t):
        l = self.live(t)
        return self.rng.choice(l) if l else None

    def fresh(self):
        self.next_id += 1
        return self.next_id


def gen_op(m: Model):
    rng = m.rng
    types = [t for t in m.order if m.order[t]]
    choices = [
        "set_val", "set_null", "set_type", "set_newprop", "remove", "create_existing",
        "create_newtype", "delete_first", "delete_last", "delete_mid", "delete_all",
        "detach_delete", "delete_nodetach", "delete_recreate", "merge_existing",
        "merge_new", "create_edge", "delete_edge", "add_nodes", "tx_rollback", "tx_commit",
        "set_title", "set_vt", "unwind_set",
    ]
    weights = [6, 3, 4, 3, 3, 4, 2, 2, 2, 3, 1, 3, 1, 3, 3, 2, 4, 2, 3, 3, 2, 2, 2, 2]
    kind = rng.choices(choices, weights)[0]
    t = rng.choice(types) if types else "Person"
    i = m.pick(t)
    if i is None and kind not in ("create_existing", "create_newtype", "add_nodes", "merge_new"):
        kind = "create_existing"
    cy = lambda _q, **p: {"k": "cypher", "q": _q, "p": p, "tag": kind}  # noqa: E731
    if kind == "set_val":
        prop = rng.choice(["score", "tag", "status"])
        val = {"score": rng.randint(0, 999), "tag": rng.choice(["t0", "t1", "t2", "newtag"]), "status": rng.choice(["open", "closed"])}[prop]
        m.touched.add((t, prop))
        return cy(f"MATCH (n:{t} {{id: $x}}) SET n.{prop} = $v RETURN n.id AS id", x=i, v=val)
    if kind == "set_null":
        prop = rng.choice(["score", "tag", "status", "ts", "extra"])
        m.touched.add((t, prop))
        return cy(f"MATCH (n:{t} {{id: $x}}) SET n.{prop} = null RETURN n.id AS id", x=i)
    if kind == "set_type":
        prop = rng.choice(["score", "tag", "ts"])
        m.touched.add((t, prop))
        expr = rng.choice(["'str-' + toString($x)", "datetime('2021-03-04T05:06:07')", "[1, 2, $x]", "$x * 1.5", "true", "$x"])
        return cy(f"MATCH (n:{t} {{id: $x}}) SET n.{prop} = {expr} RETURN n.id AS id", x=i)
    if kind == "set_newprop":
        prop = rng.choice(["extra", "extra2"])
        m.touched.add((t, prop))
        v = rng.choice([7, "seven", 7.5])
        return cy(f"MATCH (n:{t} {{id: $x}}) SET n.{prop} = $v RETURN n.id AS id", x=i, v=v)
    if kind == "remove":
        prop = rng.choice(["score", "tag", "status", "ts", "extra"])
        m.touched.add((t, prop))
        return cy(f"MATCH (n:{t} {{id: $x}}) REMOVE n.{prop} RETURN n.id AS id", x=i)
    if kind == "set_title":
        return cy(f"MATCH (n:{t} {{id: $x}}) SET n.title = 'renamed' RETURN n.id AS id", x=i)
    if kind == "set_vt":
        if t != "Person":
            t = "Person"
            i = m.pick("Person")
            if i is None:
                return gen_op(m)
        d = rng.choice(["2020-01-05", "2020-01-20", "2020-03-01", None])
        m.touched.add(("Person", "vt"))
        if d is None:
            return cy("MATCH (n:Person {id: $x}) SET n.vt = null RETURN n.id AS id", x=i)
        return cy(f"MATCH (n:Person {{id: $x}}) SET n.vt = datetime('{d}') RETURN n.id AS id", x=i)
    if kind == "unwind_set":
        ids = [m.pick(t) for _ in range(min(5, len(m.live(t))))]
        m.touched.add((t, "status"))
        return cy(f"UNWIND $ids AS i MATCH (n:{t} {{id: i}}) SET n.status = 'bulk' RETURN count(n) AS c", ids=ids)
    if kind == "create_existing":
        nid = m.fresh()
        tt = t if t in ("Person", "Department") else "Person"
        m.order.setdefault(tt, []).append(nid)
        return cy(
            f"CREATE (n:{tt} {{id: $x, title: $ti, score: $s, tag: 't1', status: 'new', ts: datetime('2020-05-05'), vf: datetime('2020-01-01'), vt: datetime('2020-06-01')}}) RETURN n.id AS id",
            x=nid, ti=f"{tt}-{nid}", s=nid * 10,
        )
    if kind == "create_newtype":
        if not m.saved_once:
            return gen_op(m)
        nid = m.fresh()
        m.order.setdefault("Team", []).append(nid)
        return cy("CREATE (n:Team {id: $x, title: $ti, kind: 'c'}) RETURN n.id AS id", x=nid, ti=f"Team-{nid}")
    if kind in ("delete_first", "delete_last", "delete_mid"):
        l = m.live(t)
        idx = {"delete_first": 0, "delete_last": len(l) - 1, "delete_mid": len(l) // 2}[kind]
        nid = l[idx]
        l.pop(idx)
        return cy(f"MATCH (n:{t} {{id: $x}}) DETACH DELETE n", x=nid)
    if kind == "delete_all":
        m.order[t] = []
        return cy(f"MATCH (n:{t}) DETACH DELETE n")
    if kind == "detach_delete":
        m.live(t).remove(i)
        return cy(f"MATCH (n:{t} {{id: $x}}) DETACH DELETE n", x=i)
    if kind == "delete_nodetach":
        # succeeds only when the node has no edges; model cannot know -> keep id, fix after
        return {"k": "cypher", "q": f"MATCH (n:{t} {{id: $x}}) DELETE n", "p": {"x": i}, "tag": kind, "maybe_delete": [t, i]}
    if kind == "delete_recreate":
        m.live(t).remove(i)
        m.live(t).append(i)
        tt = t
        if tt == "Team":
            return cy("MATCH (n:Team {id: $x}) DETACH DELETE n WITH count(*) AS c CREATE (m:Team {id: $x, title: 're', kind: 'c2'}) RETURN m.id AS id", x=i)
        return cy(
            f"MATCH (n:{tt} {{id: $x}}) DETACH DELETE n WITH count(*) AS c CREATE (m:{tt} {{id: $x, title: 're', score: 1, tag: 't2', status: 're', ts: datetime('2020-07-07'), vf: datetime('2020-02-01'), vt: datetime('2020-03-01')}}) RETURN m.id AS id",
            x=i,
        )
    if kind == "merge_existing":
        m.touched.add((t, "status"))
        return cy(f"MERGE (n:{t} {{id: $x}}) ON MATCH SET n.status = 'matched' ON CREATE SET n.status = 'created', n.title = 'm' RETURN n.id AS id", x=i)
    if kind == "merge_new":
        nid = m.fresh()
        tt = t if t in ("Person", "Department") else "Person"
        m.order.setdefault(tt, []).append(nid)
        return cy(f"MERGE (n:{tt} {{id: $x}}) ON CREATE SET n.title = $ti, n.status = 'created', n.score = $s RETURN n.id AS id", x=nid, ti=f"{tt}-{nid}", s=nid)
    if kind == "create_edge":
        a = m.pick("Person")
        b = m.pick(rng.choice([x for x in m.order if m.order[x]] or ["Person"]))
        if a is None or b is None:
            return gen_op(m)
        bt = next(x for x in m.order if b in m.order[x])
        return cy(f"MATCH (a:Person {{id: $a}}), (b:{bt} {{id: $b}}) CREATE (a)-[r:WORKS_IN {{w: $w}}]->(b) RETURN count(r) AS c", a=a, b=b, w=rng.randint(1, 9))
    if kind == "delete_edge":
        a = m.pick("Person")
        if a is None:
            return gen_op(m)
        return cy("MATCH (a:Person {id: $a})-[r:WORKS_IN]->(b) WITH r, b ORDER BY r.w, b.id LIMIT 1 DELETE r", a=a)
    if kind == "add_nodes":
        n = rng.randint(1, 6)
        ids = [m.fresh() for _ in range(n)]
        tt = rng.choice(["Person", "Department", "Office"] if m.saved_once else ["Person", "Department"])
        m.order.setdefault(tt, []).extend(ids)
        df = initial_frame(tt, ids)
        if rng.random() < 0.5:
            df["extra"] = [x % 2 for x in ids]
        df = df.drop(columns=["vt"]) if tt != "Person" and rng.random() < 0.5 else df
        rec = {c: [None if (isinstance(v, float) and math.isnan(v)) or v is pd.NaT else (v.isoformat() if hasattr(v, "isoformat") else v) for v in df[c]] for c in df.columns}
        return {"k": "add_nodes", "t": tt, "df": rec, "ts_cols": [c for c in ("ts", "vf", "vt") if c in df.columns], "tag": kind}
    if kind in ("tx_rollback", "tx_commit"):
        stmts = []
        for _ in range(rng.randint(1, 3)):
            tt = rng.choice(types) if types else "Person"
            j = m.pick(tt)
            if j is None:
                continue
            prop = rng.choice(["score", "tag", "status"])
            expr = rng.choice(["$v", "'txs'", "null", "datetime('2022-02-02')"])
            stmts.append({"q": f"MATCH (n:{tt} {{id: $x}}) SET n.{prop} = {expr}", "p": {"x": j, "v": rng.randint(0, 50)}})
            if kind == "tx_commit":
                m.touched.add((tt, prop))
        if rng.random() < 0.3 and types:
            tt = rng.choice(types)
            j = m.pick(tt)
            if j is not None:
                stmts.append({"q": f"MATCH (n:{tt} {{id: $x}}) DETACH DELETE n", "p": {"x": j}})
                if kind == "tx_commit":
                    m.live(tt).remove(j)
        if rng.random() < 0.3:
            nid = m.fresh()
            stmts.append({"q": "CREATE (n:Person {id: $x, title: 'tx', score: 3, tag: 't0', status: 'tx', ts: datetime('2020-09-09'), vf: datetime('2020-01-01'), vt: datetime('2020-12-01')})", "p": {"x": nid}})
            if kind == "tx_commit":
                m.order["Person"].append(nid)
        if not stmts:
            return gen_op(m)
        return {"k": "tx", "stmts": stmts, "commit": kind == "tx_commit", "tag": kind}
    raise AssertionError(kind)


# ------------------------------------------------------------------- run
class Pair:
    def __init__(self, workdir):
        self.workdir = Path(workdir)
        self.disk_path = str(self.workdir / "disk")
        self.mem = kglite.KnowledgeGraph()
        self.disk = kglite.KnowledgeGraph(storage="disk", path=self.disk_path)
        self.declared = False

    def reopen(self):
        self.disk.save(self.disk_path)
        del self.disk
        self.disk = kglite.load(self.disk_path)

    def save_only(self):
        self.disk.save(self.disk_path)


def setup_ops(seed):
    ids_a = list(range(1, 25))
    ids_b = list(range(50, 70))
    ops = []
    for t, ids in (("Person", ids_a), ("Department", ids_b)):
        df = initial_frame(t, ids)
        rec = {c: [None if v is pd.NaT or (isinstance(v, float) and math.isnan(v)) else (v.isoformat() if hasattr(v, "isoformat") else v) for v in df[c]] for c in df.columns}
        ops.append({"k": "add_nodes", "t": t, "df": rec, "ts_cols": ["ts", "vf", "vt"], "tag": "setup"})
    ops.append({"k": "create_index", "t": "Person", "prop": "tag", "tag": "setup"})
    if seed % 2 == 0:
        ops.append({"k": "set_temporal", "t": "Person", "tag": "setup"})
    for a in range(1, 25, 3):
        ops.append({"k": "cypher", "q": "MATCH (a:Person {id: $a}), (b:Department {id: $b}) CREATE (a)-[:WORKS_IN {w: $w}]->(b) RETURN count(*) AS c", "p": {"a": a, "b": 50 + a % 20, "w": a}, "tag": "setup"})
    return ops


def run_ops(ops, workdir, verbose=False, compare_every=True):
    """Apply ops to a fresh pair. Returns (divergence_index_or_None, details)."""
    pair = Pair(workdir)
    touched = set()
    declared = False
    sample_ids = [("Person", 1), ("Person", 12), ("Person", 24), ("Department", 50), ("Department", 69), ("Person", 101), ("Person", 105), ("Team", 102), ("Office", 110)]
    for idx, op in enumerate(ops):
        if op["k"] == "reopen":
            pair.reopen()
        elif op["k"] == "save":
            pair.save_only()
        else:
            if op["k"] == "set_temporal":
                declared = True
            if op["k"] == "cypher":
                m = re.search(r"SET n\.(\w+)|REMOVE n\.(\w+)", op["q"])
                mt = re.search(r"\(n:(\w+)", op["q"])
                if m and mt:
                    touched.add((mt.group(1), m.group(1) or m.group(2)))
            r1 = apply(pair.mem, op)
            r2 = apply(pair.disk, op)
            if r1[0] != r2[0] or (r1[0] and norm(r1[1]) != norm(r2[1])):
                return idx, {"step": idx, "op": op, "mem": r1, "disk": r2, "what": "result"}
        if compare_every or idx == len(ops) - 1:
            s1 = snapshot(pair.mem, touched, sample_ids, declared)
            s2 = snapshot(pair.disk, touched, sample_ids, declared)
            d = diff(s1, s2)
            if d:
                return idx, {"step": idx, "op": op, "what": "state", "diff": d[:6]}
    return None, None


def generate(seed, steps):
    rng = random.Random(seed)
    m = Model(rng)
    m.order["Person"] = list(range(1, 25))
    m.order["Department"] = list(range(50, 70))
    ops = setup_ops(seed)
    ops.append({"k": "reopen"})
    m.saved_once = True
    while len(ops) < steps:
        r = rng.random()
        if r < 0.07:
            ops.append({"k": "reopen"})
        elif r < 0.10:
            ops.append({"k": "save"})
            if rng.random() < 0.5:
                ops.append({"k": "save"})
        else:
            ops.append(gen_op(m))
    return ops




@pytest.mark.parity
@pytest.mark.parametrize("seed", [1, 2])
def test_disk_graph_tracks_memory_across_save_and_reopen(tmp_path, seed):
    ops = generate(seed, 100)
    index, detail = run_ops(ops, str(tmp_path))
    assert index is None, json.dumps(detail, indent=1, default=str)[:3000]
