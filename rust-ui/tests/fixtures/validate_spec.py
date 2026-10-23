"""Run the REAL coordinator validators on a submit body read from stdin: `validate_spec.py REPO_ROOT`.
Prints one JSON line: {"ok": true, "info": ...} or {"ok": false, "error": "..."}. Stdlib only. The `backend`
package __init__ imports cv2/torch, so a stub package with the same __path__ is registered first (the coordinator
modules themselves import only the standard library)."""
import json, sys, types

repo = sys.argv[1]
pkg = types.ModuleType("backend")
pkg.__path__ = [repo + "/backend"]
sys.modules["backend"] = pkg
from backend.coordinator.schema import ApiError, ident, obj, parse_json, validate_spec, validate_target  # noqa: E402

try:
    body = parse_json(sys.stdin.buffer.read())
    obj(body, "submission", ("submission_id", "spec", "target"))
    ident(body["submission_id"], "submission_id")
    validate_target(body["target"])
    info = validate_spec(body["spec"])
    print(json.dumps({"ok": True, "info": info}))
except ApiError as e:
    print(json.dumps({"ok": False, "error": e.message}))
