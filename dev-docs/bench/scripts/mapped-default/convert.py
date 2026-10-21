"""convert.py: save each operator graph with the branch build: n/ = new default, c/ = layout="compact" (v8). Needs MD_WORK."""
import kglite, os, sys, time, shutil, json
O = os.environ["MD_WORK"]
print(kglite.__file__, kglite.__version__)
res = {}
for d in ["legal", "sodir", "petrel", "codebase", "music", "rms"]:
    src = [f for f in os.listdir(f"{O}/v8/{d}") if f.endswith(".kgl")][0]
    for sub in ("n", "c"):
        os.makedirs(f"{O}/{sub}/{d}", exist_ok=True)
        for f in os.listdir(f"{O}/v8/{d}"):
            if f.endswith(".kgle"): shutil.copy(f"{O}/v8/{d}/{f}", f"{O}/{sub}/{d}/{f}")
    t = time.time(); g = kglite.load(f"{O}/v8/{d}/{src}"); lt = time.time() - t
    t = time.time(); g.save(f"{O}/n/{d}/{src}"); sn = time.time() - t
    t = time.time(); g.save(f"{O}/c/{d}/{src}", layout="compact"); sc = time.time() - t
    sz = lambda p: os.path.getsize(p)
    r = dict(v8=sz(f"{O}/v8/{d}/{src}"), new_default=sz(f"{O}/n/{d}/{src}"), compact=sz(f"{O}/c/{d}/{src}"),
             load_s=round(lt, 2), save_new_s=round(sn, 2), save_compact_s=round(sc, 2))
    for sub in ("n", "c"):
        with open(f"{O}/{sub}/{d}/{src}", "rb") as f: r[sub + "_magic"] = f.read(8).hex()
    res[d] = r; print(d, r, flush=True)
    del g
json.dump(res, open(f"{O}/convert.json", "w"), indent=1)
