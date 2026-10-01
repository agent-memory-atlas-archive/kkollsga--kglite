"""Summarise the P11 save A/B: per mode and wheel, each rep's save wall, peak over start and the stage table.
usage: summ_ab.py OUTDIR  (reads OUTDIR/{who}_r{N}_{mode}_{mode}.json + .err)"""
import glob, json, re, sys, collections
out = sys.argv[1]; rows = collections.defaultdict(list)
STAGES = ["save_consolidate", "save_csr_and_edges", "save_columns_files", "save_type_indices", "save_id_indices", "save_publish", "save_rebase", "save_snapshot_total"]
for f in sorted(glob.glob(f"{out}/*_r[0-9]_*_*.json")):
    m = re.match(r".*/(cand|ref)_r(\d)_(\w+?)_\3\.json", f)
    if not m: continue
    try: d = json.load(open(f))
    except Exception: continue
    st = {}
    for line in open(f.replace(".json", ".err")):
        mm = re.match(r"\[TIMING\] stage=(\w+) dur_ms=([\d.]+)", line)
        if mm: st[mm.group(1)] = float(mm.group(2)) / 1000
    rows[(m.group(3), m.group(1))].append((int(m.group(2)), d["save_s"], d["save_peak_over_start_MB"], d.get("load_s"), st))
print("mode who rep | save_s | peakMB | load_s | " + " ".join(s.replace("save_", "")[:12] for s in STAGES))
for (mode, who) in sorted(rows):
    for rep, s, pk, ld, st in sorted(rows[(mode, who)]):
        print(f"{mode:6} {who:4} {rep} | {s:7.2f} | {pk:6.0f} | {ld} | " + " ".join(f"{st.get(k, 0):6.1f}" for k in STAGES))
    ss = [r[1] for r in rows[(mode, who)]]
    print(f"{mode:6} {who:4} min {min(ss):.2f} mean {sum(ss)/len(ss):.2f}")
