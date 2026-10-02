#!/usr/bin/env python3
"""Compare eve-dogma `eft_export` with Pyfa's exportEft (oracle/pyfa_eft_export.py output).
usage: compare_eft_export.py pyfa.jsonl req_dir [--write tests/eft_export_expected.jsonl]"""
import json, os, subprocess, sys

pyfa = [json.loads(l) for l in open(sys.argv[1])]
rdir = sys.argv[2]
bin_ = os.environ.get("EVE_DOGMA", "target/release/eve-dogma")
reqs = []
for i, p in enumerate(pyfa):
    if "text" not in p:
        continue
    fit = json.load(open(os.path.join(rdir, p["file"])))
    reqs.append((p, json.dumps({"id": i, "method": "eft_export", "params": {"fit": fit, "name": p["name"]}})))
out = subprocess.run([bin_, "serve-stdio"], input="\n".join(r for _, r in reqs) + "\n", capture_output=True, text=True).stdout.splitlines()
ok = bad = 0
expected = []
for (p, _), line in zip(reqs, out):
    ours = json.loads(line).get("result", {}).get("text")
    expected.append({"file": p["file"], "name": p["name"], "fit": json.load(open(os.path.join(rdir, p["file"]))), "text": p["text"]})
    if ours == p["text"]:
        ok += 1
    else:
        bad += 1
        print("MISMATCH", p["file"])
        if bad <= int(os.environ.get("SHOW", "0")):
            print("--- pyfa\n" + p["text"]); print("--- ours\n" + str(ours))
print(f"eft_export parity: {ok}/{ok + bad}")
if "--write" in sys.argv:
    with open(sys.argv[sys.argv.index("--write") + 1], "w") as f:
        for e in expected:
            f.write(json.dumps(e) + "\n")
sys.exit(1 if bad else 0)
