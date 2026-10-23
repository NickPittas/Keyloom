"""Stand-in for backend/netlink.py (bridge protocol v1). The mode (argv[1]) picks the behaviour. No network, no files."""
import json, sys, time

mode = sys.argv[1]

def out(o):
    sys.stdout.write((o if isinstance(o, str) else json.dumps(o)) + "\n"); sys.stdout.flush()

if mode != "silent":
    out({"event": "ready", "protocol": 99 if mode == "badproto" else 1})
if mode in ("eof", "badproto"):
    sys.exit(0) if mode == "eof" else time.sleep(60)
if mode == "garbage":
    out("this is not json"); time.sleep(60)
if mode == "oversize":
    sys.stdout.write("x" * (2 << 20) + "\n"); sys.stdout.flush(); time.sleep(60)
if mode in ("hang", "silent"):
    time.sleep(60)
if mode == "stray":
    out({"id": "r999", "ok": True, "result": {}}); time.sleep(60)
if mode == "stderr-utf8":
    pass
if mode == "stderr":
    sys.stderr.write("x" * 100000); sys.stderr.flush()
held = []
if mode == "stderr-utf8":
    sys.stderr.buffer.write(("a" + "é" * 1500).encode()); sys.stderr.flush()
for line in sys.stdin:
    req = json.loads(line)
    rid, op = req["id"], req["op"]
    if op == "close":
        out({"id": rid, "ok": True, "result": {}}); sys.exit(0)
    if op == "installations" and mode == "reverse":
        held.append(rid)  # answered after the NEXT request, i.e. out of order
        continue
    if op == "job":
        out({"id": rid, "ok": False, "error": {"code": "busy", "message": "another link holds these credentials"}})
    else:
        out({"id": rid, "ok": True, "result": {"op": op, "args": req.get("args")}})
    while held:
        out({"id": held.pop(), "ok": True, "result": {"op": "installations", "late": True}})
if mode == "ignore-eof":
    time.sleep(60)
