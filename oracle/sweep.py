#!/usr/bin/env python3
"""Gap finder: run raw FitRequest JSON files through eve-dogma and the Pyfa oracle and list metric mismatches.
usage: sweep.py req.json ... > report.jsonl   (uses compare.py's metric mapping; nothing is written to tests/)"""
import json, os, subprocess, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import compare as C  # noqa: E402

files = sys.argv[1:]
env = dict(os.environ, PYTHONPATH=f"{C.REF}/stubs", ORACLE_REPEAT="0")
pr = subprocess.run([f"{C.REF}/pyfa-venv/bin/python", os.path.join(os.path.dirname(os.path.abspath(__file__)), "pyfa_oracle.py"), *files],
                    capture_output=True, text=True, cwd=f"{C.REF}/pyfa", env=env)
orc = {json.loads(l)["file"]: json.loads(l) for l in pr.stdout.splitlines() if l.startswith("{")}
inp = "".join(open(f).read().replace("\n", " ") + "\n" for f in files)
outs = subprocess.run([C.BIN, "batch"], input=inp, capture_output=True, text=True).stdout.splitlines()
for f, line in zip(files, outs):
    o = orc.get(os.path.basename(f))
    if not o or "error" in o:
        print(json.dumps({"file": f, "skip": (o or {}).get("error", "no oracle output")})); continue
    st = json.loads(line)
    if "error" in st:
        print(json.dumps({"file": f, "ours_error": st["error"]})); continue
    a, b = C.ours(st), C.pyfa(o["stats"])
    bad = {k: (a.get(k), b[k]) for k in b if k != "cap_state" and k in a and not C.close(a[k], b[k])}
    print(json.dumps({"file": f, "bad": bad}, default=str))
