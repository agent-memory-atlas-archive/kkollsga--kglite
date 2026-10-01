"""Reopen a built Pand disk graph, make a small change, time the save and attribute it per stage file.
usage: save_probe.py GRAPH_DIR LABEL MODE [ROWS]     MODE = append | set | noop | set2 | setobj
Prints one JSON line on stdout; stage timings go to stderr ([TIMING] lines, run with KGLITE_LOAD_TIMING=1).
phys_footprint via proc_pid_rusage, peak sampled every 5 ms."""
import sys, os, time, json, gc, ctypes, threading, warnings, stat
import numpy as np, pandas as pd, kglite
import glob
ANCHOR_TYPE, REL, TYPE = "PandObj", "VAN", "Pand"
NAT = "/Volumes/EksternalHome/Koding/Rust/KGLite/dev-docs/bench/out/register-scale/nat"
assert 'site-packages' in kglite.__file__, kglite.__file__
warnings.simplefilter("ignore")
WORK, LABEL, MODE = sys.argv[1], sys.argv[2], sys.argv[3]
ROWS = int(sys.argv[4]) if len(sys.argv) > 4 else 1000
_lp = ctypes.CDLL("/usr/lib/libproc.dylib")
def fp():
    buf = (ctypes.c_uint64 * 40)(); _lp.proc_pid_rusage(os.getpid(), 2, ctypes.byref(buf)); return int(buf[9])
PEAK = [0]; STOP = [False]
def sampler():
    while not STOP[0]:
        PEAK[0] = max(PEAK[0], fp()); time.sleep(0.005)
threading.Thread(target=sampler, daemon=True).start()
def settle():
    gc.collect(); kglite.trim_memory(); time.sleep(0.05); return fp()

def cur_gen(root):
    return os.path.join(root, "generations", open(os.path.join(root, "CURRENT")).read().strip())

def files_of(gen):
    out = {}
    for d, _, fs in os.walk(gen):
        for f in fs:
            p = os.path.join(d, f); st = os.lstat(p)
            out[os.path.relpath(p, gen)] = (st.st_size, st.st_nlink, st.st_ino)
    return out

r = {"label": LABEL, "mode": MODE, "rows": ROWS}
f0 = settle(); PEAK[0] = 0
t = time.perf_counter(); g = kglite.load(WORK); r["load_s"] = round(time.perf_counter() - t, 3)
f1 = settle(); r["reload_delta_MB"] = round((f1 - f0) / 1e6, 1)
prev_gen = cur_gen(WORK); prev_files = files_of(prev_gen)
prev = f1
def timed(name, fn):
    global prev
    PEAK[0] = 0; t = time.perf_counter(); out = fn(); dt = time.perf_counter() - t
    pk = PEAK[0]; cur = settle()
    r[f"{name}_s"] = round(dt, 4); r[f"{name}_retained_MB"] = round((cur - prev) / 1e6, 1); r[f"{name}_peak_over_start_MB"] = round((pk - prev) / 1e6, 1)
    prev = cur; return out

CT = {"begin": "validFrom", "eind": "validTo", "rec_from": "timestamp", "rec_to": "timestamp"}
def frame(j, n):
    tail = pd.read_parquet(sorted(glob.glob(f"{NAT}/PND_*.parquet"))[-1])
    x = tail.iloc[j * n:(j + 1) * n]
    k = len(x)
    return pd.DataFrame({"id": (x.ident.astype("int64") * 4096 + x.vk.astype("int64") * 2 + (1 << 50) + j).values, "ident": x.ident.values,
                         "vk": x.vk.values, "img": "r", "begin": x.begin.astype("datetime64[us]").values,
                         "eind": pd.Series(pd.NaT, index=range(k), dtype="datetime64[us]"),
                         "rec_from": x.reglv.astype("datetime64[us]").values, "rec_to": pd.Series(pd.NaT, index=range(k), dtype="datetime64[us]"),
                         "status": x.status.astype(str).values, "bouwjaar": x.bouwjaar.values})
if MODE == "append":
    new = frame(0, ROWS)
    timed("pand", lambda: g.add_nodes(new, TYPE, "id", "ident", column_types=CT, convention="half_open"))
    timed("obj", lambda: g.add_nodes(pd.DataFrame({"id": np.unique(new.ident)}), ANCHOR_TYPE, "id", conflict_handling="skip"))
    timed("van", lambda: g.add_relationships(new[["id", "ident"]], "VAN", TYPE, "id", ANCHOR_TYPE, "ident"))
elif MODE in ("set", "set2"):
    ids = [int(r["id"]) for r in g.cypher(f"MATCH (p:{TYPE}) RETURN p.id AS id LIMIT {ROWS}", timeout_ms=0).to_list()]
    timed("set_status", lambda: g.cypher(f"UNWIND $ids AS i MATCH (p:{TYPE} {{id: i}}) SET p.status = 'demolished'", params={"ids": ids}, timeout_ms=0))
    if MODE == "set2":
        timed("set_ts", lambda: g.cypher(f"UNWIND $ids AS i MATCH (p:{TYPE} {{id: i}}) SET p.rec_to = datetime('2031-01-01T00:00:00.000001')", params={"ids": ids}, timeout_ms=0))
elif MODE == "setobj":
    ids = [int(r["id"]) for r in g.cypher(f"MATCH (o:{ANCHOR_TYPE}) RETURN o.id AS id LIMIT {ROWS}", timeout_ms=0).to_list()]
    timed("set_obj", lambda: g.cypher(f"UNWIND $ids AS i MATCH (o:{ANCHOR_TYPE} {{id: i}}) SET o.flag = 1", params={"ids": ids}, timeout_ms=0))
elif MODE == "noop":
    pass
else:
    raise SystemExit("bad mode")
timed("save", lambda: g.save())
new_gen = cur_gen(WORK); new_files = files_of(new_gen)
r["prev_gen"] = os.path.basename(prev_gen); r["new_gen"] = os.path.basename(new_gen)
tot = 0; linked = 0; linked_bytes = 0; written_bytes = 0; rows = []
for rel, (size, nlink, ino) in sorted(new_files.items()):
    same = rel in prev_files and prev_files[rel][2] == ino
    tot += size
    if same: linked += 1; linked_bytes += size
    else: written_bytes += size
    rows.append((rel, size, nlink, same))
r["new_gen_files"] = len(new_files); r["new_gen_bytes"] = tot
r["linked_files"] = linked; r["linked_bytes"] = linked_bytes; r["written_bytes"] = written_bytes
r["files"] = [[rel, size, nlink, same] for rel, size, nlink, same in rows if size > 1_000_000 or rel.endswith("json")][:60]
# unique bytes across all retained generations
seen = {}; gens = sorted(os.listdir(os.path.join(WORK, "generations")))
for gname in gens:
    for rel, (size, nl, ino) in files_of(os.path.join(WORK, "generations", gname)).items():
        seen[ino] = size
r["generations_retained"] = [x for x in gens]; r["unique_bytes_all_gens"] = sum(seen.values())
r["final_footprint_MB"] = round(fp() / 1e6, 1)
STOP[0] = True
print(json.dumps(r), flush=True)
