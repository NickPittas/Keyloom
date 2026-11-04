"""App-side hint-lock recovery: ``python -m backend.hint_lock_cli '<json request>'`` prints ONE JSON result line.

The app runs this (never the render worker). The writer checks stay in ``inspect_hint_lock`` / ``clear_hint_lock``
(exact job id + host + pid, liveness proof, ``.prev`` restore); the app refuses to ask while any job runs.
Request: {"cmd": "inspect"|"clear", "request_id": ..., "clip_root": ..., "expect": {...}, "reason": ...}.
"""
from __future__ import annotations

import json
import socket
import sys


def run(req: dict) -> dict:
    from backend.service.key_job import KeyJobRejected, _JOB_ID_RE, _clean_abs
    from backend.service.task_job import clear_hint_lock, inspect_hint_lock
    rid = req.get("request_id")
    if not isinstance(rid, str) or not _JOB_ID_RE.match(rid):
        return {"event": "rejected", "code": "protocol", "message": "request_id must match [A-Za-z0-9._-]{1,64}", "request_id": None}
    try:
        root = _clean_abs(req.get("clip_root"), "clip_root")
    except KeyJobRejected as exc:
        return {"event": "rejected", "code": exc.code, "message": exc.message, "request_id": rid}
    host = socket.gethostname()
    if req.get("cmd") == "inspect":
        return {"event": "hint_lock", "request_id": rid, "clip_root": root, "lock": inspect_hint_lock(root, host)}
    if req.get("cmd") != "clear":
        return {"event": "rejected", "code": "protocol", "message": f"unknown cmd {req.get('cmd')!r}", "request_id": rid}
    res = clear_hint_lock(root, req.get("expect"), req.get("reason"), rid, host)
    if res["cleared"]:
        return {"event": "hint_lock_cleared", "request_id": rid, "clip_root": root,
                "recovered": res["recovered"], "warning": res.get("warning")}
    return {"event": "hint_lock_refused", "request_id": rid, "clip_root": root,
            "code": res["code"], "message": res["message"], "lock": res["lock"]}


def main(argv=None) -> int:
    argv = sys.argv[1:] if argv is None else argv
    try:
        req = json.loads(argv[0])
        if not isinstance(req, dict):
            raise ValueError("request must be an object")
    except (IndexError, ValueError) as exc:
        sys.stdout.write(json.dumps({"event": "rejected", "code": "protocol", "message": f"bad request: {exc}", "request_id": None}) + "\n")
        return 2
    sys.stdout.write(json.dumps(run(req), separators=(",", ":")) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
