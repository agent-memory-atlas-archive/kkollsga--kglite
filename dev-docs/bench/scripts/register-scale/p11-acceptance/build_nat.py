"""P11 acceptance build (Probe-2 build ladder on real national Pand (scale.py RUNG 2 disk load schema, images()/split_for_day() verbatim).
usage: scale2.py TAG   env: MAXCH (default 25), SOFTCAP bytes (stop building after a chunk+save above it; default 8.5e9)
Per 1M-voorkomen chunk: add_nodes/add_relationships as scale.py, gc+trim, footprint; save(); footprint; dir size; wall.
Writes one JSON line per chunk to runs/scale_TAG.jsonl."""
import os, sys, time, json, glob, threading, gc, shutil, warnings, ctypes
import numpy as np, pandas as pd, kglite
assert "site-packages" in kglite.__file__, kglite.__file__
warnings.simplefilter("ignore")
TAG = sys.argv[1]
NAT = "/Volumes/EksternalHome/Koding/Rust/KGLite/dev-docs/bench/out/register-scale/nat"
WORK = os.environ["P11_GRAPH"]
OUT = open(os.environ["P11_OUT"] + f"/build_{TAG}.jsonl", "w")
MAXCH = int(os.environ.get("MAXCH", 25)); SOFTCAP = float(os.environ.get("SOFTCAP", 10.0e9))
D = pd.Timestamp("2026-09-01")
NBSEQ = [0]
_lp = ctypes.CDLL("/usr/lib/libproc.dylib")
def fp():
    buf = (ctypes.c_uint64 * 40)(); _lp.proc_pid_rusage(os.getpid(), 2, ctypes.byref(buf)); return int(buf[2 + 7])
PEAK = [0]; STOP = [False]
def sampler():
    while not STOP[0]:
        PEAK[0] = max(PEAK[0], fp()); time.sleep(0.05)
threading.Thread(target=sampler, daemon=True).start()
def settle():
    gc.collect(); kglite.trim_memory(); return fp()
def rd(f):
    x = pd.read_parquet(f)
    for c in x.columns:
        if str(x[c].dtype).startswith("datetime64"):
            x[c] = x[c].astype("datetime64[us]")
    return x
def images(v):
    kill = v[["inactlv", "nietbaglv"]].min(axis=1)
    closed = v.eindreglv.notna(); nb = v.nietbaglv.notna()
    base_id = v.ident.astype("int64") * 4096 + v.vk.astype("int64") * 2
    k = int(nb.sum())
    if k:
        base_id = base_id.copy(); base_id[nb] = (1 << 62) + (NBSEQ[0] + np.arange(k, dtype="int64")) * 2; NBSEQ[0] += k
    r = pd.DataFrame({"id": base_id, "ident": v.ident, "vk": v.vk, "img": "r", "begin": v.begin,
                      "eind": pd.Series(pd.NaT, index=v.index, dtype="datetime64[us]"),
                      "rec_from": v.reglv, "rec_to": np.where(closed, np.fmin(v.eindreglv.values, kill.values), kill.values),
                      "status": v.status})
    e = v[closed]; ke = kill[closed]
    e = pd.DataFrame({"id": base_id[closed] + 1, "ident": e.ident, "vk": e.vk, "img": "e", "begin": e.begin, "eind": e.eind,
                      "rec_from": e.eindreglv, "rec_to": ke.values, "status": e.status})
    for extra in ["bouwjaar"]:
        r[extra] = v[extra].values; e[extra] = v.loc[closed, extra].values
    im = pd.concat([r, e], ignore_index=True)
    im["rec_to"] = pd.to_datetime(im["rec_to"])
    return im[im.rec_to.isna() | (im.rec_to > im.rec_from)]
def split_for_day(im):
    base = im[im.rec_from < D].copy()
    base.loc[base.rec_to >= D, "rec_to"] = pd.NaT
    return base
CT = {"begin": "validFrom", "eind": "validTo", "rec_from": "timestamp", "rec_to": "timestamp"}
files = sorted(glob.glob(f"{NAT}/PND_*.parquet"))
shutil.rmtree(WORK, ignore_errors=True)
f0 = settle()
g = kglite.KnowledgeGraph(storage="disk", path=WORK)
n_vk = n_img = 0; prev = settle()
print(json.dumps({"tag": TAG, "fp0": f0, "so": os.path.getsize(os.path.join(os.path.dirname(kglite.__file__), "kglite.abi3.so"))}), file=OUT, flush=True)
for ci, i in enumerate(range(0, len(files), 100)):
    if ci >= MAXCH: break
    t0 = time.perf_counter()
    v = pd.concat([rd(f) for f in files[i:i + 100]], ignore_index=True); nv = len(v)
    base = split_for_day(images(v)); del v
    empty = base.eind.notna() & (base.begin == base.eind)
    t1 = time.perf_counter(); PEAK[0] = 0
    g.add_nodes(base.loc[~empty], "Pand", "id", "ident", column_types=CT, convention="half_open")
    if empty.any():
        g.add_nodes(base.loc[empty], "Pand", "id", "ident", column_types=CT, convention="half_open")
    g.add_nodes(pd.DataFrame({"id": base.ident.unique()}), "PandObj", "id", conflict_handling="skip")
    g.add_relationships(base[["id", "ident"]], "VAN", "Pand", "id", "PandObj", "ident")
    t_add = time.perf_counter() - t1; peak_add = PEAK[0]
    n_vk += nv; n_img += len(base); del base
    f_chunk = settle()
    PEAK[0] = 0; t2 = time.perf_counter(); g.save(); t_save = time.perf_counter() - t2; peak_save = PEAK[0]
    f_save = settle()
    gens = sorted(os.listdir(os.path.join(WORK, 'generations')))
    cur = os.path.join(WORK, "generations", open(os.path.join(WORK, "CURRENT")).read().strip().split("/")[-1])
    dirb = sum(os.path.getsize(os.path.join(dp, f)) for dp, _, fs in os.walk(cur) for f in fs)
    rec = {"chunk": ci, "vk": n_vk, "images": n_img, "fp_after_chunk": f_chunk, "fp_after_save": f_save,
           "chunk_delta_Bvk": (f_chunk - prev) / nv, "cum_Bvk": (f_save - f0) / n_vk, "peak_add": peak_add, "peak_save": peak_save,
           "prep_s": round(t1 - t0, 2), "add_s": round(t_add, 2), "save_s": round(t_save, 2), "dir_bytes": dirb, "dir_Bvk": dirb / n_vk, "generations": gens}
    prev = f_save
    print(json.dumps(rec), file=OUT, flush=True)
    print(f"chunk {ci}: {n_vk} vk fp chunk={f_chunk/1e9:.2f} save={f_save/1e9:.2f} GB peak_save={peak_save/1e9:.2f} add {t_add:.1f}s save {t_save:.1f}s dir {dirb/1e9:.2f} GB", flush=True)
    if max(f_save, peak_save) > SOFTCAP:
        print("softcap reached", flush=True); break
STOP[0] = True
print(json.dumps({"done": True, "vk": n_vk, "images": n_img, "shape": list(g.shape)}), file=OUT, flush=True)
