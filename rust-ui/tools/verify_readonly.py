#!/usr/bin/env python3
"""Compare project files against a snapshot {path: [size, mtime_ns]} (and optional JSON sha256 map).

    ../.venv/bin/python tools/verify_readonly.py /tmp/ck-rust-project-files-before.json [/tmp/ck-rust-readonly-baseline.json]
Exit 1 on any added/removed/changed file.
"""
import hashlib, json, os, sys

before = json.load(open(sys.argv[1]))
roots = sorted({p.split("/Projects/")[0] + "/Projects/" + p.split("/Projects/")[1].split("/")[0] for p in before})
now = {}
for r in roots:
    for d, _, fs in os.walk(r):
        for f in fs:
            p = os.path.join(d, f)
            st = os.stat(p)
            now[p] = [st.st_size, st.st_mtime_ns]
added = sorted(set(now) - set(before)); removed = sorted(set(before) - set(now))
changed = sorted(p for p in before if p in now and now[p] != before[p])
bad = 0
if len(sys.argv) > 2:
    for p, h in json.load(open(sys.argv[2])).items():
        if hashlib.sha256(open(p, "rb").read()).hexdigest() != h:
            changed.append(p + " (sha256)")
print(f"files before={len(before)} now={len(now)} added={len(added)} removed={len(removed)} changed={len(changed)}")
for k, v in (("added", added), ("removed", removed), ("changed", changed)):
    for p in v[:10]:
        print(" ", k, p)
sys.exit(1 if added or removed or changed else 0)
