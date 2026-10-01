"""Watchdog runner: wd.py LOGFILE -- cmd...  Runs cmd as a child, samples its phys_footprint every 250 ms,
kills the child's PID (only) above 10.5 GB. Prints peak + uptime/rustc before/after."""
import sys, subprocess, time, ctypes, os, json
CAP = 10.5e9
lp = ctypes.CDLL("/usr/lib/libproc.dylib")
def fp(pid):
    buf = (ctypes.c_uint64 * 40)()
    if lp.proc_pid_rusage(pid, 2, ctypes.byref(buf)) != 0: return 0
    return int(buf[2 + 7])
def state():
    up = subprocess.run(["uptime"], capture_output=True, text=True).stdout.strip()
    r = subprocess.run(["pgrep", "-x", "rustc"], capture_output=True, text=True).stdout.split()
    return f"{up} | rustc={len(r)}"
log = sys.argv[1]; cmd = sys.argv[3:]
before = state(); t0 = time.time()
with open(log, "w") as f:
    p = subprocess.Popen(cmd, stdout=f, stderr=subprocess.STDOUT)
    peak = 0; killed = False
    while p.poll() is None:
        v = fp(p.pid); peak = max(peak, v)
        if v > CAP:
            p.kill(); killed = True; p.wait(); break
        time.sleep(0.25)
after = state()
print(json.dumps({"cmd": " ".join(cmd), "rc": p.returncode, "killed_by_watchdog": killed, "peak_fp_GB": round(peak/1e9, 3),
                  "wall_s": round(time.time()-t0, 1), "before": before, "after": after}), flush=True)
