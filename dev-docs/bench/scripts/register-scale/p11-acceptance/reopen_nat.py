"""P11 items 2-4: reload delta/time, as-of count (must be 11,064,590), two id lookups + a title lookup, reopen + 1 k append (+ save).
usage: reopen_nat.py GRAPH LABEL VK    prints one JSON line. Run on a clone of the built graph, under wd.py."""
import sys
from common import *
WORK, LABEL, VK = sys.argv[1], sys.argv[2], int(sys.argv[3])
new = frame(-1, 0, 1000, 1 << 50)
r = {"label": LABEL}
f0 = settle(); PEAK[0] = 0
t = time.perf_counter(); g = kglite.load(WORK); r["load_s"] = round(time.perf_counter() - t, 3)
f1 = settle(); r["reload_delta_MB"] = round((f1 - f0) / 1e6, 2); r["reload_Bvk"] = round((f1 - f0) / VK, 3); r["reload_peak_MB"] = round((PEAK[0] - f0) / 1e6, 1)
prev = f1; PEAK[0] = 0
t = time.perf_counter(); ans = g.cypher("MATCH (p:Pand) WHERE p.rec_to IS NULL RETURN count(*) AS n", valid_at="2020-01-01").to_list()
r["asof_s"] = round(time.perf_counter() - t, 3); r["asof_ans"] = ans[0]["n"]; r["asof_ok"] = ans[0]["n"] == 11064590
cur = settle(); r["asof_retained_MB"] = round((cur - prev) / 1e6, 1); r["asof_peak_over_start_MB"] = round((PEAK[0] - prev) / 1e6, 1); prev = cur
x0 = pd.read_parquet(sorted(glob.glob(f"{NAT}/PND_*.parquet"))[0]).iloc[:5]
for k, row in enumerate([x0.iloc[0], x0.iloc[3]]):
    i = int(row.ident) * 4096 + int(row.vk) * 2
    t = time.perf_counter(); a = g.cypher("MATCH (n:Pand {id: $id}) RETURN n.vk AS vk, n.id AS id", params={"id": i}).to_list()
    r[f"idlookup{k}_s"] = round(time.perf_counter() - t, 4); r[f"idlookup{k}_rows"] = len(a)
t = time.perf_counter(); a = g.cypher("MATCH (n:Pand {title: $t}) RETURN count(*) AS c", params={"t": int(x0.ident.iloc[0])}).to_list()
r["titlelookup_s"] = round(time.perf_counter() - t, 4); r["titlelookup_ans"] = a
prev = settle(); PEAK[0] = 0
t = time.perf_counter(); append_frame(g, new); r["append1k_s"] = round(time.perf_counter() - t, 3)
cur = settle(); r["append1k_retained_MB"] = round((cur - prev) / 1e6, 1); r["append1k_peak_over_start_MB"] = round((PEAK[0] - prev) / 1e6, 1)
r["append1k_peak_abs_GB"] = round(PEAK[0] / 1e9, 3); prev = cur; PEAK[0] = 0
t = time.perf_counter(); g.save(); r["save_after_append_s"] = round(time.perf_counter() - t, 2)
cur = settle(); r["save_peak_over_start_MB"] = round((PEAK[0] - prev) / 1e6, 1); r["save_retained_MB"] = round((cur - prev) / 1e6, 1)
r["shape"] = list(g.shape); r["fp_final_GB"] = round(cur / 1e9, 3)
STOP[0] = True; print(json.dumps(r), flush=True)
