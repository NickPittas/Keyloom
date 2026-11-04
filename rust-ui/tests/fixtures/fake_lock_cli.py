"""Fake `python -m backend.hint_lock_cli` for Rust tests: one request on argv[1], one JSON line on stdout.
Same answers as the real helper's writer checks, stdlib only. Behaviour from FAKE_CFG (JSON env)."""
import json, os, sys

cfg = json.loads(os.environ.get("FAKE_CFG", "{}"))
c = json.loads(sys.argv[1])
rid, root = c["request_id"], c["clip_root"]
path = os.path.join(root, ".corridorkey_hint.lock")
info = None
if os.path.lexists(path):
    d = json.load(open(path))
    dead = cfg.get("lock_dead", True)
    info = {"path": path, "readable": True, "problem": None, "job_id": d.get("job_id"), "host": d.get("host", "h"),
            "pid": d.get("pid", 1), "same_host": True, "local_dead": dead, "safe_to_clear": dead}
if cfg.get("log"):
    with open(cfg["log"], "a") as f:
        f.write(json.dumps({"event": "lock_cmd", "cmd": c["cmd"], "expect": c.get("expect"), "reason": c.get("reason")}) + "\n")


def out(o):
    print(json.dumps(o))


if c["cmd"] == "inspect":
    out({"event": "hint_lock", "request_id": rid, "clip_root": root, "lock": info})
else:
    e = c.get("expect") or {}
    if info is None or any(info[k] != e.get(k) for k in ("job_id", "host", "pid")):
        out({"event": "hint_lock_refused", "request_id": rid, "clip_root": root, "code": "mismatch", "message": "different writer", "lock": info})
    elif info["local_dead"] is False:
        out({"event": "hint_lock_refused", "request_id": rid, "clip_root": root, "code": "alive", "message": "still running", "lock": info})
    else:
        os.remove(path)
        out({"event": "hint_lock_cleared", "request_id": rid, "clip_root": root, "recovered": [], "warning": None})
