import os, sys, json, tempfile, shutil
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import h2

SCRATCH = os.environ["SP_SCRATCH"]

def interesting(script, mode, workdir, target):
    try:
        fin, per, err = h2.run_script(script, mode, workdir, check_each=True)
    except Exception:
        return False
    for i, st, bad in per:
        for name, got, exp in bad:
            if name == target:
                return True
    if fin:
        for name, got, exp in fin:
            if name == target:
                return True
    return False

def ddmin(script, mode, workdir, target):
    cur = list(script); n = 2
    while len(cur) >= 2:
        chunk = max(1, len(cur) // n); reduced = False
        for i in range(0, len(cur), chunk):
            cand = cur[:i] + cur[i+chunk:]
            if cand and interesting(cand, mode, workdir, target):
                cur = cand; n = max(n - 1, 2); reduced = True; break
        if not reduced:
            if n >= len(cur): break
            n = min(len(cur), 2 * n)
    # final single-step sweep
    changed = True
    while changed:
        changed = False
        for i in range(len(cur)):
            cand = cur[:i] + cur[i+1:]
            if cand and interesting(cand, mode, workdir, target):
                cur = cand; changed = True; break
    return cur

if __name__ == "__main__":
    path, seed, mode, target = sys.argv[1], int(sys.argv[2]), sys.argv[3], sys.argv[4]
    data = json.load(open(path))
    script = next(r["script"] for r in data if r["seed"] == seed)
    wd = tempfile.mkdtemp(dir=SCRATCH)
    mini = ddmin(script, mode, wd, target)
    print(f"# minimized {len(script)} -> {len(mini)} steps (mode={mode}, target={target})")
    for s in mini:
        print(json.dumps(s))
    fin, per, err = h2.run_script(mini, mode, wd, check_each=True)
    print("# err:", err)
    print("# per-step:", json.dumps(per, default=str)[:2000])
    print("# final:", json.dumps(fin, default=str)[:2000])
    shutil.rmtree(wd, ignore_errors=True)
