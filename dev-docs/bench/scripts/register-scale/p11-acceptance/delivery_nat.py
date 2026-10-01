"""P11 item 5: peak-day delivery on a reopened national graph: 12 k closes (SET rec_to, 3 x 4 k statements) + 12 k closing 'e' images +
13 k new 'r' images (2 k of them on new anchors) as add_nodes in 1 k chunks, anchors, VAN edges; then save().
usage: delivery_nat.py GRAPH LABEL    prints one JSON line. Run on a clone, under wd.py."""
import sys
from common import *
WORK, LABEL = sys.argv[1], sys.argv[2]
r = {"label": LABEL}
f0 = settle(); PEAK[0] = 0
g = kglite.load(WORK); prev = settle()
t = time.perf_counter()
_fs = sorted(glob.glob(f"{NAT}/PND_*.parquet")); _m = len(_fs) // 2
_x = pd.concat([pd.read_parquet(f) for f in _fs[_m:_m + 6]], ignore_index=True)
_x = _x[_x.eindreglv.isna() & _x.inactlv.isna() & _x.nietbaglv.isna() & (_x.reglv < pd.Timestamp("2026-09-01"))].drop_duplicates(["ident", "vk"])
ids = (_x.ident.astype("int64") * 4096 + _x.vk.astype("int64") * 2).tolist()[:12000]
assert len(ids) == 12000, len(ids)
r["select_open_ids_s"] = round(time.perf_counter() - t, 2); r["n_close"] = len(ids)
_n = g.cypher("UNWIND $ids AS i MATCH (p:Pand {id: i}) WHERE p.rec_to IS NULL RETURN count(p) AS c", params={"ids": ids}, timeout_ms=0).to_list()[0]["c"]
r["close_ids_open_in_graph"] = _n
closing = frame(0, 0, 12000, 0)  # shape donor for the closing 'e' rows
e = pd.DataFrame({"id": np.array(ids, dtype="int64") + 1, "ident": np.array(ids, dtype="int64") // 4096, "vk": 1, "img": "e",
                  "begin": closing.begin.values[:len(ids)], "eind": closing.begin.values[:len(ids)] + np.timedelta64(1, "D"),
                  "rec_from": pd.Timestamp("2026-09-02").to_datetime64(), "rec_to": pd.Series(pd.NaT, index=range(len(ids)), dtype="datetime64[us]"),
                  "status": "closed", "bouwjaar": closing.bouwjaar.values[:len(ids)]})
e["rec_from"] = e["rec_from"].astype("datetime64[us]"); e["eind"] = e["eind"].astype("datetime64[us]")
reg = frame(-1, 2000, 13000, 1 << 51); reg.loc[reg.index[-2000:], ["ident"]] = reg.ident[-2000:] + (1 << 43)
reg.loc[reg.index[-2000:], "id"] = reg.id[-2000:] + ((1 << 43) * 4096)
settle(); PEAK[0] = 0; T0 = time.perf_counter(); r["fp_start_MB"] = round(prev / 1e6)
t = time.perf_counter()
for j in range(3):
    chunk = ids[j * 4000:(j + 1) * 4000]
    g.cypher("UNWIND $ids AS i MATCH (p:Pand {id: i}) SET p.rec_to = datetime('2026-09-02T00:00:00')", params={"ids": chunk}, timeout_ms=0)
r["close_sets_s"] = round(time.perf_counter() - t, 3)
t = time.perf_counter(); n_chunks = 0
for df in (e, reg):
    for i in range(0, len(df), 1000):
        append_frame(g, df.iloc[i:i + 1000].reset_index(drop=True)); n_chunks += 1
r["creates_s"] = round(time.perf_counter() - t, 3); r["create_chunks"] = n_chunks
r["apply_s"] = round(time.perf_counter() - T0, 3); r["apply_peak_over_start_MB"] = round((PEAK[0] - prev) / 1e6, 1)
cur = settle(); r["apply_retained_MB"] = round((cur - prev) / 1e6, 1); prev = cur; PEAK[0] = 0
t = time.perf_counter(); g.save(); r["publish_s"] = round(time.perf_counter() - t, 2)
r["publish_peak_over_start_MB"] = round((PEAK[0] - prev) / 1e6, 1); r["publish_peak_abs_GB"] = round(PEAK[0] / 1e9, 3)
cur = settle(); r["publish_retained_MB"] = round((cur - prev) / 1e6, 1)
r["delivery_total_s"] = round(r["apply_s"] + r["publish_s"], 2); r["shape"] = list(g.shape)
a = g.cypher("MATCH (p:Pand) WHERE p.rec_to IS NULL RETURN count(*) AS n", valid_at="2020-01-01").to_list()
r["asof_2020_after"] = a[0]["n"]
STOP[0] = True; print(json.dumps(r), flush=True)
