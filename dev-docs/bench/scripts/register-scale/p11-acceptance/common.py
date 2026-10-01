"""Shared helpers for the P11 probes: phys_footprint, peak sampler, settle, frames from the national parquet parts."""
import os, sys, time, json, gc, glob, ctypes, threading, warnings
import numpy as np, pandas as pd, kglite
assert "site-packages" in kglite.__file__, kglite.__file__
warnings.simplefilter("ignore")
NAT = "/Volumes/EksternalHome/Koding/Rust/KGLite/dev-docs/bench/out/register-scale/nat"
CT = {"begin": "validFrom", "eind": "validTo", "rec_from": "timestamp", "rec_to": "timestamp"}
_lp = ctypes.CDLL("/usr/lib/libproc.dylib")
def fp():
    buf = (ctypes.c_uint64 * 40)(); _lp.proc_pid_rusage(os.getpid(), 2, ctypes.byref(buf)); return int(buf[9])
PEAK = [0]; STOP = [False]
def _sampler():
    while not STOP[0]:
        PEAK[0] = max(PEAK[0], fp()); time.sleep(0.005)
threading.Thread(target=_sampler, daemon=True).start()
def settle():
    gc.collect(); kglite.trim_memory(); time.sleep(0.05); return fp()
def frame(parquet_idx, start, n, id_off, ident_off=0):
    """n new 'r' images shaped like the national rows; ids offset so they never collide with built ids."""
    files = sorted(glob.glob(f"{NAT}/PND_*.parquet"))
    idxs = [parquet_idx + j for j in range(8)] if parquet_idx >= 0 else [len(files) + parquet_idx - j for j in range(8)]
    x = pd.concat([pd.read_parquet(files[i]) for i in idxs], ignore_index=True)
    x = x.drop_duplicates(["ident", "vk"]).iloc[start:start + n]
    assert len(x) == n, (len(x), n)
    k = len(x); ident = x.ident.astype("int64").values + ident_off
    return pd.DataFrame({"id": ident * 4096 + x.vk.astype("int64").values * 2 + id_off, "ident": ident,
                         "vk": x.vk.values, "img": "r", "begin": x.begin.astype("datetime64[us]").values,
                         "eind": pd.Series(pd.NaT, index=range(k), dtype="datetime64[us]"),
                         "rec_from": x.reglv.astype("datetime64[us]").values,
                         "rec_to": pd.Series(pd.NaT, index=range(k), dtype="datetime64[us]"),
                         "status": x.status.astype(str).values, "bouwjaar": x.bouwjaar.values})
def append_frame(g, new):
    g.add_nodes(new, "Pand", "id", "ident", column_types=CT, convention="half_open")
    g.add_nodes(pd.DataFrame({"id": np.unique(new.ident)}), "PandObj", "id", conflict_handling="skip")
    g.add_relationships(new[["id", "ident"]], "VAN", "Pand", "id", "PandObj", "ident")
