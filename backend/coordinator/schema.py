"""Strict wire validation for protocol v1. Everything here is a trust boundary: unknown keys, wrong
types, bools-as-ints, NaN/Infinity, duplicate keys and oversized values are rejected, never coerced."""
import hashlib
import json
import re
import unicodedata

from . import PROTOCOL

MAX_BODY = 1 << 20
MAX_SPEC = 64 << 10
MAX_EVENT = 16 << 10
MAX_BATCH = 256
MAX_DEPTH = 16

# Mirrors backend/service/core.py VALID_* (a test compares them); duplicated so the server never imports torch/cv2 code.
VRAM_PROFILES = ("auto", "low")
EXR_COMPRESSIONS = ("dwab", "piz", "zip", "none")
OUTPUT_FORMATS = ("exr", "png")
HINT_MODES = ("none", "full", "keyframes")
TEMPORAL_WINDOWS = ("auto", "80/40", "48/24", "24/12", "8/4", "1/1")
OUTPUT_KEYS = ("fg", "matte", "comp", "processed")
TERMINAL_STATES = ("succeeded", "failed", "cancelled")
REJECT_CODES = ("missing_map", "missing_model", "incompatible_protocol", "insufficient_resources", "busy", "other")


class ApiError(Exception):
    def __init__(self, status, code, message):
        super().__init__(message)
        self.status, self.code, self.message = status, code, message


def bad(msg, code="invalid"):
    raise ApiError(400, code, msg)


# ---- parsing -------------------------------------------------------------------------------------

def _no_dupes(pairs):
    d = {}
    for k, v in pairs:
        if k in d:
            bad(f"duplicate key {k!r}")
        d[k] = v
    return d


def _no_const(name):
    bad(f"non-finite number {name} is not allowed")


def _float(text):
    v = float(text)
    if v in (float("inf"), float("-inf")):  # 1e400 parses to inf without going through parse_constant
        bad("non-finite number is not allowed")
    return v


def _walk(v, n=0):
    """Depth bound + every string AND object key must be encodable: a lone surrogate (`"\\ud800"`) parses fine but
    later crashes any UTF-8 encoder (a poisoned offer would crash every receiver). A valid pair is one astral char."""
    if n > MAX_DEPTH:
        bad("JSON nested too deeply")
    if isinstance(v, str):
        try:
            v.encode("utf-8")
        except UnicodeEncodeError:
            bad("string is not valid Unicode (lone surrogate)")
    elif isinstance(v, dict):
        for k, x in v.items():
            _walk(k, n + 1)
            _walk(x, n + 1)
    elif isinstance(v, list):
        for x in v:
            _walk(x, n + 1)


def parse_json(raw):
    if len(raw) > MAX_BODY:
        raise ApiError(413, "too_large", "body too large")
    try:
        v = json.loads(raw.decode("utf-8"), object_pairs_hook=_no_dupes, parse_constant=_no_const, parse_float=_float)
    except ApiError:
        raise
    except (ValueError, RecursionError):
        bad("body is not valid UTF-8 JSON")
    if not isinstance(v, dict):
        bad("body must be a JSON object")
    _walk(v)
    return v


def canonical(v):
    return json.dumps(v, sort_keys=True, separators=(",", ":"), ensure_ascii=True, allow_nan=False)


def digest(v):
    return hashlib.sha256(canonical(v).encode()).hexdigest()


# ---- primitives ----------------------------------------------------------------------------------

def obj(v, where, required, optional=()):
    if not isinstance(v, dict):
        bad(f"{where} must be an object")
    extra = set(v) - set(required) - set(optional)
    missing = set(required) - set(v)
    if extra or missing:
        bad(f"{where}: unknown keys {sorted(extra)} / missing keys {sorted(missing)}")
    return v


def integer(v, where, lo=0, hi=10**9):
    if isinstance(v, bool) or not isinstance(v, int) or not lo <= v <= hi:
        bad(f"{where} must be an integer in [{lo}, {hi}]")
    return v


def number01(v, where):
    if isinstance(v, bool) or not isinstance(v, (int, float)) or not 0 <= v <= 1:
        bad(f"{where} must be a number in [0, 1]")
    return v


def boolean(v, where):
    if not isinstance(v, bool):
        bad(f"{where} must be a boolean")
    return v


def text(v, where, hi=128, lo=1):
    if not isinstance(v, str) or not lo <= len(v) <= hi or any(unicodedata.category(c) in ("Cc", "Cs") for c in v):
        bad(f"{where} must be a string of {lo}-{hi} characters without control characters")
    return v


def choice(v, where, options):
    if v not in options or not isinstance(v, str):
        bad(f"{where} must be one of {list(options)}")
    return v


_ID = re.compile(r"[A-Za-z0-9_-]{16,64}\Z")


def ident(v, where):
    if not isinstance(v, str) or not _ID.match(v):
        bad(f"{where} must be 16-64 characters of A-Z a-z 0-9 _ -")
    return v


# ---- shared references (mirror of rust-ui/src/shared_paths.rs) ----------------------------------

_NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,63}\Z")


def _device(stem):
    return stem in ("CON", "PRN", "AUX", "NUL") or (len(stem) == 4 and stem[:3] in ("COM", "LPT") and stem[3] in "123456789")


def shared_ref(v, where):
    """`shared:<root>/<a>/<b>` -> (root, components). Never an absolute or traversing path."""
    if not isinstance(v, str) or not v.startswith("shared:") or len(v) > 1024:
        bad(f"{where} must be a shared: reference")
    root, *rel = v[7:].split("/")
    if not _NAME.match(root):
        bad(f"{where}: invalid location name")
    for c in rel:
        if (not c or c in (".", "..") or c[-1] in ". " or _device(c.split(".")[0].upper()) or len(c) > 255
                or any(unicodedata.category(ch) in ("Cc", "Cs") or ch in '/\\:*?"<>|' for ch in c)):
            bad(f"{where}: path component {c!r} is not allowed")
    return root, rel


_TOKENS = ("project", "name", "output", "resolution", "date", "version")


def _templates(folder, file, enabled):
    text(folder, "output.folder_template", 256)
    text(file, "output.file_template", 256)
    if folder.startswith("/") or ":" in folder or "\\" in folder or any(s in ("", ".", "..") for s in folder.split("/")):
        bad("output.folder_template must be a relative folder path")
    if "/" in file or "\\" in file:
        bad("output.file_template must not contain a separator")
    runs = [m.start() for m in re.finditer("#", file)]
    if not runs or any(b != a + 1 for a, b in zip(runs, runs[1:])):
        bad("output.file_template needs one contiguous run of # frame digits")
    for t in (folder, file):
        rest = t
        while "{" in rest:
            j = rest.find("}", rest.index("{"))
            if j < 0 or rest[rest.index("{") + 1:j] not in _TOKENS:
                bad("output templates contain an unknown or unterminated {token}")
            rest = rest[j + 1:]
    if enabled > 1 and "{output}" not in folder + file:
        bad("several outputs enabled but templates lack {output}: they would collide")


# ---- frozen Key job ------------------------------------------------------------------------------

_PARAMS = {
    "input_is_linear": boolean, "use_alpha_hint": boolean, "carry_hint": boolean, "cutout_linear": boolean,
    "hint_quality": number01, "carry_hint_quality": number01, "despill_strength": number01,
}


JOB_KINDS = ("key", "workflow")
_HINT_KEYS = ("guide", "final", "bypass", "birefnet_variant", "sam_model", "chroma", "videomama_chunk", "mask_cleanup")


def _validate_workflow(spec, enabled_outputs):
    """The Hint and Deliver stages of a `workflow` job. Shapes only: the receiver rebuilds and fully validates them on
    its own clip. A `key` job carries none of these keys, so an old Key payload keeps its exact meaning."""
    refs = []
    if "source" in spec:  # the external image-sequence folder: the receiver must map this location to take the job
        refs.append(shared_ref(spec["source"], "source"))
    if "hint" in spec:
        h = obj(spec["hint"], "hint", (), _HINT_KEYS)
        if len(canonical(h)) > 4096:
            bad("hint settings too large")
        for k in ("guide", "final", "birefnet_variant", "sam_model"):
            if k in h:
                text(h[k], f"hint.{k}", 64)
        if "bypass" in h:
            boolean(h["bypass"], "hint.bypass")
        if "videomama_chunk" in h:
            integer(h["videomama_chunk"], "hint.videomama_chunk", 1, 4096)
        for k in ("chroma", "mask_cleanup"):
            if k in h and not isinstance(h[k], dict):
                bad(f"hint.{k} must be an object")
        st = spec.get("hint_stages")
        if not isinstance(st, list) or len(st) > 64:
            bad("hint needs hint_stages: a list of at most 64 task kinds")
        [text(x, "hint_stages[]", 32) for x in st]
    elif "hint_stages" in spec:
        bad("hint_stages without hint settings")
    if "export" in spec:
        e = obj(spec["export"], "export", ("fps", "allow_partial", "root", "outputs", "files"))
        if e["fps"] is not None and (isinstance(e["fps"], bool) or not isinstance(e["fps"], (int, float)) or not 0 < e["fps"] <= 1000):
            bad("export.fps must be null or a number in (0, 1000]")
        boolean(e["allow_partial"], "export.allow_partial")
        refs.append(shared_ref(e["root"], "export.root"))
        outs, files = e["outputs"], e["files"]
        if not isinstance(outs, dict) or not isinstance(files, dict) or len(outs) > 4 or len(files) > 4:
            bad("export.outputs/files must be objects of at most 4 products")
        for k, d in outs.items():
            choice(k, "export.outputs key", OUTPUT_KEYS)
            if k not in enabled_outputs:
                bad(f"export delivers {k}, which is not an enabled output")
            kind = obj(d, f"export.outputs.{k}", ("kind",), ("container", "codec", "quality"))["kind"]
            choice(kind, "export.outputs kind", ("sequence", "video"))
            if kind == "video":
                obj(d, f"export.outputs.{k}", ("kind", "container", "codec", "quality"))
                text(d["container"], "export container", 32)
                text(d["codec"], "export codec", 32)
                integer(d["quality"], "export quality", 0, 100)
        videos = {k for k, d in outs.items() if d["kind"] == "video"}
        if not videos or set(files) != videos:
            bad("export needs exactly one file name per video delivery")
        for k, f in files.items():
            shared_ref("shared:x/" + text(f, f"export.files.{k}", 255), f"export.files.{k}")
    return refs


def validate_spec(spec):
    """Exact schema of a network job. `key`: Key only, new-only (the first release's payload, unchanged).
    `workflow`: the same fields plus optional frozen `hint` / `hint_stages` / `export` for Hint -> Key -> Deliver on one
    receiver. Resume/Replace/other kinds are refused."""
    if isinstance(spec, dict) and spec.get("kind") == "workflow":
        obj(spec, "spec", ("protocol", "kind", "mode", "clip_id", "clip_root", "range", "params", "output_config",
                           "hint_keyframes_abs", "output", "manifest_dir", "publish", "require"),
            ("preset_name", "hint", "hint_stages", "source", "export"))
    else:
        obj(spec, "spec", ("protocol", "kind", "mode", "clip_id", "clip_root", "range", "params", "output_config",
                           "hint_keyframes_abs", "output", "manifest_dir", "publish", "require"), ("preset_name",))
    if spec["protocol"] != PROTOCOL or isinstance(spec["protocol"], bool):
        bad(f"unsupported protocol (this server speaks {PROTOCOL})", "unsupported_protocol")
    if spec["kind"] not in JOB_KINDS:
        bad(f"job kind must be one of {list(JOB_KINDS)}", "unsupported_kind")
    if spec["mode"] != "new":
        bad("network jobs support mode \"new\" only; Resume/Replace need ownership continuity", "unsupported_mode")
    if spec["publish"] != "final":
        bad("publish must be \"final\"")
    if "preset_name" in spec:
        text(spec["preset_name"], "preset_name")  # descriptive only; never an instruction to look anything up
    text(spec["clip_id"], "clip_id")
    refs = [shared_ref(spec["clip_root"], "clip_root"), shared_ref(spec["manifest_dir"], "manifest_dir")]

    r = obj(spec["range"], "range", ("in_point", "out_point", "in_stem", "out_stem"))
    lo, hi = integer(r["in_point"], "range.in_point"), integer(r["out_point"], "range.out_point")
    if hi < lo:
        bad("range.out_point precedes in_point")
    text(r["in_stem"], "range.in_stem", 255)
    text(r["out_stem"], "range.out_stem", 255)

    p = obj(spec["params"], "params", (*_PARAMS, "vram_profile", "hint_mode", "temporal_window"))
    for k, check in _PARAMS.items():
        check(p[k], f"params.{k}")
    choice(p["vram_profile"], "params.vram_profile", VRAM_PROFILES)
    choice(p["hint_mode"], "params.hint_mode", HINT_MODES)
    choice(p["temporal_window"], "params.temporal_window", TEMPORAL_WINDOWS)
    if p["use_alpha_hint"] != (p["hint_mode"] != "none"):
        bad("params.use_alpha_hint disagrees with params.hint_mode")

    c = obj(spec["output_config"], "output_config",
            [f"{k}_{s}" for k in OUTPUT_KEYS for s in ("enabled", "format")] + ["exr_compression"])
    enabled = sum(boolean(c[f"{k}_enabled"], f"output_config.{k}_enabled") for k in OUTPUT_KEYS)
    for k in OUTPUT_KEYS:
        choice(c[f"{k}_format"], f"output_config.{k}_format", OUTPUT_FORMATS)
    choice(c["exr_compression"], "output_config.exr_compression", EXR_COMPRESSIONS)
    if not enabled:
        bad("output_config enables no output")

    kf = spec["hint_keyframes_abs"]
    if kf is not None:
        if not isinstance(kf, list) or len(kf) > 4096:
            bad("hint_keyframes_abs must be null or a list of at most 4096 frame numbers")
        nums = [integer(x, "hint_keyframes_abs[]") for x in kf]
        if nums != sorted(set(nums)) or any(not lo <= n <= hi for n in nums):
            bad("hint_keyframes_abs must be ascending, unique and inside the range")
    if (p["hint_mode"] == "keyframes") != (kf is not None):
        bad("hint_keyframes_abs is required exactly when hint_mode is keyframes")

    o = obj(spec["output"], "output", ("root", "folder_template", "file_template", "values"))
    refs.append(shared_ref(o["root"], "output.root"))
    v = obj(o["values"], "output.values", ("project", "name", "resolution", "date", "version"))
    for k in ("project", "name", "resolution", "date"):
        text(v[k], f"output.values.{k}")
    integer(v["version"], "output.values.version", 0, 99999)
    _templates(o["folder_template"], o["file_template"], enabled)

    if spec["kind"] == "workflow":
        enabled_outputs = {k for k in OUTPUT_KEYS if c[f"{k}_enabled"]}
        refs.extend(_validate_workflow(spec, enabled_outputs))
    rq = obj(spec["require"], "require", ("protocol", "models"))
    if rq["protocol"] != PROTOCOL or isinstance(rq["protocol"], bool):
        bad("require.protocol mismatch", "unsupported_protocol")
    m = rq["models"]
    if not isinstance(m, dict) or len(m) > 32:
        bad("require.models must be an object of at most 32 models")
    for name, d in m.items():
        text(name, "require.models name", 64)
        if d is not None:
            text(d, "require.models digest", 128)
    if len(canonical(spec)) > MAX_SPEC:
        raise ApiError(413, "too_large", "job spec too large")
    return {"roots": sorted({r for r, _ in refs}), "models": dict(m), "kind": spec["kind"]}


def validate_target(v):
    return None if v is None else ident(v, "target")


# ---- session / claim / events --------------------------------------------------------------------

def validate_session(b):
    obj(b, "session", ("protocol", "session_id", "build", "gpus"), ("name",))
    if "name" in b:  # the PC's display name; updates the same installation row, never creates identity
        text(b["name"], "name", 64)
    if b["protocol"] != PROTOCOL or isinstance(b["protocol"], bool):
        bad("unsupported protocol", "unsupported_protocol")
    ident(b["session_id"], "session_id")
    text(b["build"], "build", 64)
    g = b["gpus"]
    if not isinstance(g, list) or len(g) != 1:
        bad("exactly one GPU per installation is supported in this release", "unsupported_multi_gpu")
    obj(g[0], "gpus[0]", ("uuid", "name", "vram_total_mb", "ram_total_mb"))
    if not re.fullmatch(r"[A-Za-z0-9-]{8,64}", g[0]["uuid"] if isinstance(g[0]["uuid"], str) else ""):
        bad("gpus[0].uuid must be a verified GPU UUID (8-64 characters of A-Z a-z 0-9 -)")
    text(g[0]["name"], "gpus[0].name", 128)
    integer(g[0]["vram_total_mb"], "gpus[0].vram_total_mb", 1)
    integer(g[0]["ram_total_mb"], "gpus[0].ram_total_mb", 1)
    return b


def validate_claim(b):
    obj(b, "claim", ("session_id", "claim_id", "capabilities", "resources"))
    ident(b["session_id"], "session_id")
    ident(b["claim_id"], "claim_id")
    c = obj(b["capabilities"], "capabilities", ("protocol", "kinds", "maps", "models"))
    if c["protocol"] != PROTOCOL or isinstance(c["protocol"], bool):
        bad("unsupported protocol", "unsupported_protocol")
    for k, hi in (("kinds", 8), ("maps", 64)):
        if not isinstance(c[k], list) or len(c[k]) > hi:
            bad(f"capabilities.{k} must be a list of at most {hi}")
        [text(x, f"capabilities.{k}[]", 64) for x in c[k]]
    if not isinstance(c["models"], dict) or len(c["models"]) > 64:
        bad("capabilities.models must be an object of at most 64 models")
    for n, d in c["models"].items():
        text(n, "capabilities.models name", 64)
        if d is not None:
            text(d, "capabilities.models digest", 128)
    r = b["resources"]
    if r is not None:  # None = no report; the server never claims fresh-memory placement without one
        obj(r, "resources", ("sampled_at", "free_vram_mb", "free_ram_mb"))
        integer(r["sampled_at"], "resources.sampled_at", 1, 10**11)
        integer(r["free_vram_mb"], "resources.free_vram_mb")
        integer(r["free_ram_mb"], "resources.free_ram_mb")
    return b


def validate_event(e):
    obj(e, "event", ("seq", "type", "data"))
    integer(e["seq"], "event.seq", 1, 10**9)
    t = choice(e["type"], "event.type", ("progress", "note", "terminal"))
    d = e["data"]
    if t == "progress":
        obj(d, "data", ("done", "total"))
        integer(d["done"], "data.done")
        integer(d["total"], "data.total")
        if d["done"] > d["total"]:
            bad("progress done exceeds total")
    elif t == "note":
        text(obj(d, "data", ("message",))["message"], "data.message", 1024)
    else:
        obj(d, "data", ("state",), ("message", "code"))
        choice(d["state"], "data.state", TERMINAL_STATES)
        if "message" in d:
            text(d["message"], "data.message", 1024)
        if "code" in d:
            text(d["code"], "data.code", 64)
    if len(canonical(e)) > MAX_EVENT:
        raise ApiError(413, "too_large", "event too large")
    return e


# ---- live stats (heartbeat) ----------------------------------------------------------------------

MAX_BYTES = 1 << 50


def _opt(d, k, fn, *a):
    if d.get(k) is not None:
        fn(d[k], k, *a)


def validate_stats(b):
    """Latest-only machine stats. Every metric is optional/nullable (= unavailable, never 0). The client sends
    how old its sample was, not a timestamp, so it cannot make a stale sample look fresh."""
    obj(b, "stats", ("session_id", "seq", "measured_age_ms"), ("cpu_percent", "ram_used_bytes", "ram_total_bytes", "gpus"))
    ident(b["session_id"], "session_id")
    integer(b["seq"], "seq", 1, 10**12)
    integer(b["measured_age_ms"], "measured_age_ms", 0, 60_000)
    if b.get("cpu_percent") is not None:
        pct(b["cpu_percent"], "cpu_percent")
    _opt(b, "ram_used_bytes", integer, 0, MAX_BYTES)
    _opt(b, "ram_total_bytes", integer, 0, MAX_BYTES)
    if None not in (b.get("ram_used_bytes"), b.get("ram_total_bytes")) and b["ram_used_bytes"] > b["ram_total_bytes"]:
        bad("ram_used_bytes exceeds ram_total_bytes")
    gpus = b.get("gpus")
    if gpus is not None:
        if not isinstance(gpus, list) or len(gpus) > 8:
            bad("gpus must be a list of at most 8")
        for g in gpus:
            obj(g, "gpus[]", ("uuid", "index", "name"), ("utilization_percent", "vram_used_bytes", "vram_total_bytes"))
            if not re.fullmatch(r"[A-Za-z0-9-]{8,64}", g["uuid"] if isinstance(g["uuid"], str) else ""):
                bad("gpus[].uuid invalid")
            integer(g["index"], "gpus[].index", 0, 64)
            text(g["name"], "gpus[].name", 128)
            if g.get("utilization_percent") is not None:
                pct(g["utilization_percent"], "gpus[].utilization_percent")
            _opt(g, "vram_used_bytes", integer, 0, MAX_BYTES)
            _opt(g, "vram_total_bytes", integer, 0, MAX_BYTES)
            if None not in (g.get("vram_used_bytes"), g.get("vram_total_bytes")) and g["vram_used_bytes"] > g["vram_total_bytes"]:
                bad("vram_used_bytes exceeds vram_total_bytes")
    return b


def pct(v, where):
    if isinstance(v, bool) or not isinstance(v, (int, float)) or not 0 <= v <= 100:
        bad(f"{where} must be a number in [0, 100]")
    return v
