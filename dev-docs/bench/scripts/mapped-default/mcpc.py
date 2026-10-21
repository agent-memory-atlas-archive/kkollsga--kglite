"""Minimal stdio MCP client + process metrics (stdlib only, macOS)."""
import json, os, subprocess, time, re, shutil

def footprint(pid):
    """phys_footprint in MB read from a separate process (footprint -p)."""
    try:
        out = subprocess.run(["footprint", "-p", str(pid)], capture_output=True, text=True, timeout=60).stdout
    except Exception:
        return None
    m = re.search(r"Footprint:\s+([\d.]+)\s*([KMG]B)", out)
    if not m:
        m = re.search(r"phys_footprint:\s+([\d.]+)\s*([KMG]B)", out)
    if not m:
        return None
    v = float(m.group(1)); u = m.group(2)
    return v / 1024 if u == "KB" else v * 1024 if u == "GB" else v

def _mb(v, u):
    v = float(v)
    return v / 1024 if u == "KB" else v * 1024 if u == "GB" else v / 1048576 if u == "B" else v

def footprint2(pid):
    """(phys_footprint MB, clean-resident MB from the TOTAL row) via a separate process."""
    try:
        out = subprocess.run(["footprint", "-p", str(pid)], capture_output=True, text=True, timeout=60).stdout
    except Exception:
        return None, None
    m = re.search(r"Auxiliary data:\s+phys_footprint:\s+([\d.]+)\s*([KMG]?B)", out)
    fp = _mb(*m.groups()) if m else None
    t = re.search(r"^\s*([\d.]+)\s*([KMG]?B)\s+([\d.]+)\s*([KMG]?B)\s+[\d.]+\s*[KMG]?B\s+\d+\s+TOTAL", out, re.M)
    clean = _mb(t.group(3), t.group(4)) if t else None
    return fp, clean

def rss(pid):
    out = subprocess.run(["ps", "-o", "rss=", "-p", str(pid)], capture_output=True, text=True).stdout.strip()
    return int(out) / 1024 if out else None

def du_bytes(path):
    tot = 0
    for r, _, fs in os.walk(path):
        for f in fs:
            try:
                tot += os.lstat(os.path.join(r, f)).st_size
            except OSError:
                pass
    return tot

class Server:
    def __init__(self, binary, args, tmpdir, cwd):
        shutil.rmtree(tmpdir, ignore_errors=True); os.makedirs(tmpdir)
        self.tmpdir = tmpdir
        env = dict(os.environ, KGLITE_TMPDIR=tmpdir)
        self.t0 = time.perf_counter()
        self.p = subprocess.Popen([binary] + args, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=subprocess.DEVNULL, cwd=cwd, env=env, bufsize=0)
        self.pid = self.p.pid
        self.n = 0
        self.buf = b""
        self._rpc("initialize", {"protocolVersion": "2025-03-26", "capabilities": {},
                                  "clientInfo": {"name": "bench", "version": "0"}})
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def _send(self, o):
        self.p.stdin.write((json.dumps(o) + "\n").encode())

    def _readline(self):
        while b"\n" not in self.buf:
            c = os.read(self.p.stdout.fileno(), 1 << 20)
            if not c:
                raise EOFError("server closed")
            self.buf += c
        line, self.buf = self.buf.split(b"\n", 1)
        return line

    def _rpc(self, method, params):
        self.n += 1
        i = self.n
        self._send({"jsonrpc": "2.0", "id": i, "method": method, "params": params})
        while True:
            m = json.loads(self._readline())
            if m.get("id") == i:
                return m

    def call(self, tool, args=None):
        t = time.perf_counter()
        r = self._rpc("tools/call", {"name": tool, "arguments": args or {}})
        dt = (time.perf_counter() - t) * 1000
        res = r.get("result", {})
        text = "".join(c.get("text", "") for c in res.get("content", []))
        err = bool(res.get("isError")) or "error" in r
        return dt, err, text

    def close(self):
        try:
            self.p.stdin.close()
        except Exception:
            pass
        try:
            self.p.wait(timeout=10)
        except Exception:
            self.p.terminate()
            try:
                self.p.wait(timeout=5)
            except Exception:
                self.p.kill(); self.p.wait()
        shutil.rmtree(self.tmpdir, ignore_errors=True)
