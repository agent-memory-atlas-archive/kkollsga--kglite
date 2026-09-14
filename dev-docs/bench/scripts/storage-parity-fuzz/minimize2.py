import os, sys, json, tempfile, shutil
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import h2
def interesting(script, mode, wd):
    try: fin, per, err = h2.run_script(script, mode, wd, check_each=False)
    except Exception: return False
    if not fin: return False
    names = set(b[0] for b in fin)
    return "counts" in names and "stub" not in names
def ddmin(script, mode, wd):
    cur=list(script); n=2
    while len(cur)>=2:
        chunk=max(1,len(cur)//n); red=False
        for i in range(0,len(cur),chunk):
            cand=cur[:i]+cur[i+chunk:]
            if cand and interesting(cand,mode,wd): cur=cand; n=max(n-1,2); red=True; break
        if not red:
            if n>=len(cur): break
            n=min(len(cur),2*n)
    ch=True
    while ch:
        ch=False
        for i in range(len(cur)):
            cand=cur[:i]+cur[i+1:]
            if cand and interesting(cand,mode,wd): cur=cand; ch=True; break
    return cur
if __name__=="__main__":
    path,seed,mode=sys.argv[1],int(sys.argv[2]),sys.argv[3]
    script=next(r["script"] for r in json.load(open(path)) if r["seed"]==seed)
    wd=tempfile.mkdtemp(dir=os.environ["SP_SCRATCH"])
    mini=ddmin(script,mode,wd)
    print(f"# {len(script)} -> {len(mini)} steps")
    for s in mini: print(json.dumps(s))
    fin,per,err=h2.run_script(mini,mode,wd,check_each=True)
    print("# final:",json.dumps(fin,default=str)[:900])
    shutil.rmtree(wd,ignore_errors=True)
