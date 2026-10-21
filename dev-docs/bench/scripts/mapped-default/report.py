"""report.py: markdown tables from MD_WORK/matrix_run{1,2}.json (+ convert.json). Prints to stdout."""
import json, os, statistics as st
O = os.environ["MD_WORK"]
runs = [json.load(open(f"{O}/matrix_run{r}.json")) for r in (1, 2, 3, 4) if os.path.exists(f"{O}/matrix_run{r}.json")]
GR = ["legal", "sodir", "petrel", "codebase", "music", "rms"]
V = ["A_wheel", "N_default", "N_heap", "C_compact"]
def cell(r, g, v): return r.get(f"{g}|{v}")  # run 3 covers only some graphs
def mean(x): return st.mean(x) if isinstance(x, list) else x
def pair(g, v, key, d=0, scale=1.0):
    vals = []
    for r in runs:
        c = cell(r, g, v)
        if c is None or c.get(key) is None: vals.append(None); continue
        vals.append(mean(c[key]) / scale)
    return " / ".join("-" if x is None else f"{x:.{d}f}" for x in vals)
print("## Per graph (r1 / r2)\n")
print("| graph | variant | boot ms | fp boot MB | fp work MB | RSS work MB | spill MB at boot | cold q1 ms |")
print("|---|---|---|---|---|---|---|---|")
for g in GR:
    for v in V:
        if cell(runs[0], g, v) is None: continue
        print(f"| {g} | {v} | {pair(g,v,'boot_ms')} | {pair(g,v,'fp_boot')} | {pair(g,v,'fp_work')} | {pair(g,v,'rss_work')} | {pair(g,v,'spill_boot',1,1048576)} | {pair(g,v,'cold_ms',1)} |")
print("\n## Warm latency, min of 50 ms; delta vs A_wheel (r1 / r2)\n")
print("| graph | query | A_wheel | N_default | delta | C_compact | delta |")
print("|---|---|---|---|---|---|---|")
for g in GR:
    for q in cell(runs[0], g, "A_wheel")["warm"]:
        def mins(v):
            return [cell(r, g, v)["warm"][q]["min"] if cell(r, g, v) else None for r in runs]
        a, n, c = mins("A_wheel"), mins("N_default"), mins("C_compact")
        fmt = lambda xs: " / ".join("-" if x is None else f"{x:.3f}" for x in xs)
        dl = lambda xs: " / ".join("-" if (x is None or y is None) else f"{(x/y-1)*100:+.1f}%" for x, y in zip(xs, a))
        print(f"| {g} | {q} | {fmt(a)} | {fmt(n)} | {dl(n)} | {fmt(c)} | {dl(c)} |")
