"""App-side generated-mask deletion: ``python -m backend.mask_delete_cli '<json request>'`` prints ONE JSON line.

The app runs this (never the GPU worker) with a short time limit, like ``hint_lock_cli``. Request:
{"cmd": "plan"|"delete", "clip_root", "asset": "prematte"|"hint", "scope": "all"|"frames", "stems": [...],
 "movies": bool, "request_id"}. The app refuses first while the clip has queued/active jobs or an external writer;
the publication lock and interrupted-publish checks are repeated here under the lock.
"""
from __future__ import annotations

import json
import re
import sys

from backend.hint_assets import DeleteRefused, delete_masks, plan_mask_delete


def run(req: dict) -> dict:
    rid = req.get("request_id")
    if not isinstance(rid, str) or not re.fullmatch(r"[A-Za-z0-9._-]{1,64}", rid):
        return {"event": "rejected", "message": "request_id must match [A-Za-z0-9._-]{1,64}", "request_id": None}
    root, stems = req.get("clip_root"), req.get("stems") or []
    if not isinstance(root, str) or not root or not isinstance(stems, list) or not all(isinstance(s, str) for s in stems):
        return {"event": "rejected", "message": "bad clip_root or stems", "request_id": rid}
    args = (root, req.get("asset"), req.get("scope"), stems, bool(req.get("movies")))
    try:
        if req.get("cmd") == "plan":
            return {"event": "mask_plan", "request_id": rid, **plan_mask_delete(*args)}
        if req.get("cmd") == "delete":
            return {"event": "masks_deleted", "request_id": rid, **delete_masks(*args, request_id=rid)}
    except DeleteRefused as exc:
        return {"event": "rejected", "message": str(exc), "request_id": rid}
    except OSError as exc:
        return {"event": "rejected", "message": f"could not read the clip folder: {exc}", "request_id": rid}
    return {"event": "rejected", "message": f"unknown cmd {req.get('cmd')!r}", "request_id": rid}


def main(argv=None) -> int:
    argv = sys.argv[1:] if argv is None else argv
    try:
        req = json.loads(argv[0])
        if not isinstance(req, dict):
            raise ValueError("request must be an object")
    except (IndexError, ValueError) as exc:
        sys.stdout.write(json.dumps({"event": "rejected", "message": f"bad request: {exc}", "request_id": None}) + "\n")
        return 2
    sys.stdout.write(json.dumps(run(req), separators=(",", ":")) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
