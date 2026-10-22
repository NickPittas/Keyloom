"""Generalized local worker tasks: hint generation, SAM preview/track, source/hint
normalization and manifest-driven video export (protocol v1 ``task`` command).

Hint kinds never write into the clip's live hint folder while running: they run the existing
service pipelines against an app-owned *view* of exactly the job's In..Out frames, validate the
staged set, then publish it recoverably (old folder -> ``.prev`` only until the swap succeeded;
failure or cancel leaves the old folder untouched). See docs/design/rust-ui/standalone-release-plan.md.
"""
from __future__ import annotations

import json
import logging
import math
import os
import re
import shutil
import subprocess
import sys
import threading
import time
import types
from dataclasses import dataclass, field
from fractions import Fraction
from types import MappingProxyType
from typing import Callable

import cv2
import numpy as np

from ..clip_state import MASK_TRACK_MANIFEST, ClipAsset, ClipEntry, ClipState
from ..errors import CorridorKeyError, JobCancelledError
from ..frame_io import imread_unicode, imwrite_unicode
from .key_job import (
    MANIFEST_NAME, PROTOCOL, KeyJobRejected, PublishError, _clean_abs, _freeze, _inside,
    _is_int, _JOB_ID_RE, _KeyClip, _boot_id, _proc_start_ticks, _link_noclobber, _make_dirs_or_reject, _rm, _rollback,
    _thaw, _write_tmp,
)

logger = logging.getLogger(__name__)

TaskRejected = KeyJobRejected
HINT_LOCK = ".corridorkey_hint.lock"
HINT_PROVENANCE = ".corridorkey_hint.json"
HINT_DST = {"gvm": "AlphaHint", "birefnet": "AlphaHint", "chroma": "AlphaHint",
            "matanyone2": "AlphaHint", "videomama": "AlphaHint", "sam_track": "VideoMamaMaskHint"}
KINDS = (*HINT_DST, "sam_preview", "export_video", "prepare_source", "import_hint")
RANGED = (*HINT_DST, "sam_preview", "export_video")  # prepare_source/import_hint are whole-movie
_JOB_KEYS = ("job_id", "kind", "clip_id", "clip_root", "range", "params", "staging_dir",
             "publish_dir", "require")
_RANGE_KEYS = ("in_point", "out_point", "in_stem", "out_stem")
SAM_IDS = {"small": "facebook/sam2.1-hiera-small", "base_plus": "facebook/sam2.1-hiera-base-plus",
           "large": "facebook/sam2.1-hiera-large"}
SAM_ARTIFACTS = {"small": "sam2-small", "base_plus": "sam2-base-plus", "large": "sam2-large"}
CONTAINERS = {"mp4": ".mp4", "mov_prores4444": ".mov", "webm_alpha": ".webm"}
_ALPHA_CONTAINERS = ("mov_prores4444", "webm_alpha")
_IMG = (".png", ".jpg", ".jpeg", ".tif", ".tiff", ".bmp", ".exr")


def _rej(code: str, message: str):
    raise TaskRejected(code, message)


# ------------------------------------------------------------------ capabilities

def _closure_ready(by_id: dict, aid: str, seen=None) -> bool:
    it = by_id.get(aid)
    if not it or not it.get("ready"):
        return False
    seen = seen or set()
    return all(_closure_ready(by_id, d, seen | {aid}) for d in it.get("depends", ()) if d not in seen)


def birefnet_repos() -> dict:
    """Display variant -> repo suffix (imports the wrapper, hence torch; only call where torch is OK)."""
    from modules.BiRefNetModule.wrapper import BIREFNET_MODELS
    return dict(BIREFNET_MODELS)


def task_capabilities(status: dict | None, *, gpu: bool, ffmpeg: bool, repos: dict | None = None) -> dict:
    """Truthful per-kind capability from the model-status document (artifact ``ready`` = verified).

    Returns {"tasks": {...}, "birefnet_variants": [...], "sam_models": [...], "reasons": {...}}."""
    by_id = {it["artifact"]: it for it in (status or {}).get("artifacts", ())}
    ok = lambda *ids: all(_closure_ready(by_id, i) for i in ids)  # noqa: E731
    variants = [v for v, repo in (repos or {}).items() if ok("birefnet-runtime", f"birefnet-{repo}")]
    sam = [m for m, a in SAM_ARTIFACTS.items() if ok("sam2-package", a)]
    tasks = {"gvm": gpu and ok("gvm"), "birefnet": gpu and bool(variants),
             "chroma": True, "sam_preview": gpu and bool(sam), "sam_track": gpu and bool(sam),
             "matanyone2": gpu and ok("matanyone2"), "videomama": gpu and ok("videomama", "videomama-svd"),
             "export_video": ffmpeg, "prepare_source": ffmpeg, "import_hint": ffmpeg}
    reasons = {}
    if not gpu:
        reasons["gpu"] = "no usable CUDA GPU"
    if not ffmpeg:
        reasons["ffmpeg"] = "FFmpeg is not installed or not usable"
    if status is None:
        reasons["models"] = "model status unavailable; model-backed routes are disabled"
    return {"tasks": tasks, "birefnet_variants": variants, "sam_models": sam, "reasons": reasons}


# ------------------------------------------------------------------------ parsing

def _num(v, lo, hi, what):
    if isinstance(v, bool) or not isinstance(v, (int, float)) or not math.isfinite(v) or not lo <= v <= hi:
        _rej("invalid", f"{what} must be a number in [{lo}, {hi}]")
    return v


def _keys(p, required, optional=(), what="params"):
    if not isinstance(p, dict):
        _rej("invalid", f"{what} must be an object")
    missing, unknown = set(required) - set(p), set(p) - set(required) - set(optional)
    if missing or unknown:
        _rej("invalid", f"{what} keys: missing {sorted(missing)}, unknown {sorted(unknown)}")


def _rel(p, what):
    ok = isinstance(p, str) and p and "\0" not in p and "\\" not in p and not p.startswith("/")
    if ok and (".." in p.split("/") or re.match(r"^[A-Za-z]:", p) or not p.lower().endswith(".png")):
        ok = False
    if not ok:
        _rej("invalid", f"{what} must be a relative .png path inside the staging inputs folder")
    return p


def _prompt(p, what):
    _keys(p, (), ("points", "boxes", "mask_png"), what)
    pts, boxes = p.get("points", []), p.get("boxes", [])
    if not isinstance(pts, list) or not isinstance(boxes, list) or len(boxes) > 1 or len(pts) > 4096:
        _rej("invalid", f"{what}: points must be a list and boxes at most one [x0,y0,x1,y1]")
    for q in pts:
        _keys(q, ("x", "y", "label"), (), f"{what} point")
        _num(q["x"], 0, 1e7, "x"), _num(q["y"], 0, 1e7, "y")
        if q["label"] not in (0, 1) or isinstance(q["label"], bool):
            _rej("invalid", f"{what}: label must be 0 or 1")
    for b in boxes:
        if not isinstance(b, list) or len(b) != 4:
            _rej("invalid", f"{what}: a box is [x0,y0,x1,y1]")
        x0, y0, x1, y1 = (_num(v, 0, 1e7, "box") for v in b)
        if x1 <= x0 or y1 <= y0:
            _rej("invalid", f"{what}: box must have positive size")
    if p.get("mask_png") is not None:
        _rel(p["mask_png"], f"{what} mask_png")
    return {"points": pts, "boxes": boxes, "mask_png": p.get("mask_png")}


def _check_params(kind: str, p) -> dict:
    if kind == "gvm":
        _keys(p, ())
        return {}
    if kind == "birefnet":
        _keys(p, ("variant",))
        if not isinstance(p["variant"], str) or not p["variant"]:
            _rej("invalid", "variant must be a BiRefNet variant name")
        return dict(p)
    if kind == "chroma":
        _keys(p, ("screen_color", "screen_type", "strength", "clip_black", "clip_white",
                  "shrink_grow", "edge_blur"))
        sc = p["screen_color"]
        if sc is not None and not (isinstance(sc, list) and len(sc) == 3 and all(
                _is_int(c) and 0 <= c <= 255 for c in sc)):
            _rej("invalid", "screen_color must be null or [r,g,b] 0..255")
        if p["screen_type"] not in ("green", "blue"):
            _rej("invalid", "screen_type must be green or blue")
        _num(p["strength"], 0.1, 10, "strength"), _num(p["clip_black"], 0, 1, "clip_black")
        _num(p["clip_white"], 0, 1, "clip_white")
        for k, hi in (("shrink_grow", 100), ("edge_blur", 100)):
            if not _is_int(p[k]) or not -hi <= p[k] <= hi:
                _rej("invalid", f"{k} must be an integer in [-{hi}, {hi}]")
        return dict(p)
    if kind in ("sam_preview", "sam_track"):
        extra = ("frame_stem", "prompts") if kind == "sam_preview" else ("frames",)
        _keys(p, ("tracker_model", *extra), ("input_is_linear",))
        if p["tracker_model"] not in SAM_IDS:
            _rej("invalid", f"tracker_model must be one of {sorted(SAM_IDS)}")
        if p.get("input_is_linear") not in (None, True, False):
            _rej("invalid", "input_is_linear must be null or a boolean")
        out = {"tracker_model": p["tracker_model"], "input_is_linear": p.get("input_is_linear")}
        if kind == "sam_preview":
            if not isinstance(p["frame_stem"], str) or not p["frame_stem"]:
                _rej("invalid", "frame_stem must be a string")
            out.update(frame_stem=p["frame_stem"], prompts=_prompt(p["prompts"], "prompts"))
        else:
            if not isinstance(p["frames"], dict) or not p["frames"]:
                _rej("invalid", "frames must be a non-empty object keyed by frame stem")
            out["frames"] = {s: _prompt(v, f"frames.{s}") for s, v in p["frames"].items()}
        return out
    if kind == "matanyone2":
        _keys(p, (), ("mask_stem",))
        return {"mask_stem": p.get("mask_stem")}
    if kind == "videomama":
        _keys(p, (), ("chunk_size",))
        cs = p.get("chunk_size", 16)
        if not _is_int(cs) or not 1 <= cs <= 64:
            _rej("invalid", "chunk_size must be an integer in [1, 64]")
        return {"chunk_size": cs}
    if kind == "export_video":
        _keys(p, ("product", "manifest_dir", "dest", "container", "fps", "allow_partial"), ("audio",))
        if p["product"] not in ("fg", "matte", "comp", "processed"):
            _rej("invalid", "product must be fg, matte, comp or processed")
        if p["container"] not in CONTAINERS:
            _rej("invalid", f"container must be one of {sorted(CONTAINERS)}")
        if p.get("audio", "none") != "none":
            _rej("unsupported", "audio retention is not supported; exports never carry audio")
        if not isinstance(p["allow_partial"], bool):
            _rej("invalid", "allow_partial must be a boolean")
        _num(p["fps"], 0.001, 1000, "fps")
        dest = _clean_abs(p["dest"], "dest", names=True)
        if os.path.splitext(dest)[1].lower() != CONTAINERS[p["container"]]:
            _rej("invalid", f"dest must end with {CONTAINERS[p['container']]} for {p['container']}")
        if p["container"] in _ALPHA_CONTAINERS and p["product"] not in ("processed",):
            _rej("invalid", "alpha containers need the 'processed' product (the only RGBA output)")
        return {"product": p["product"], "manifest_dir": _clean_abs(p["manifest_dir"], "manifest_dir", names=True),
                "dest": dest, "container": p["container"], "fps": p["fps"],
                "allow_partial": p["allow_partial"], "audio": "none"}
    if kind == "prepare_source":
        _keys(p, ("source_path",))
        return {"source_path": _clean_abs(p["source_path"], "source_path")}
    if kind == "import_hint":
        _keys(p, ("hint_path", "source_path", "hint_kind", "channel"))
        if p["hint_kind"] not in ("alpha", "mask") or p["channel"] not in ("gray", "alpha"):
            _rej("invalid", "hint_kind must be alpha|mask and channel gray|alpha")
        return {"hint_path": _clean_abs(p["hint_path"], "hint_path"),
                "source_path": _clean_abs(p["source_path"], "source_path"),
                "hint_kind": p["hint_kind"], "channel": p["channel"]}
    _rej("unsupported", f"unknown task kind {kind!r}")


@dataclass(frozen=True)
class TaskJob:
    job_id: str
    kind: str
    clip_id: str
    clip_root: str
    range: MappingProxyType | None
    params: MappingProxyType
    staging_dir: str | None
    publish_dir: str | None
    require: MappingProxyType

    @property
    def frames(self) -> int:
        return self.range["out_point"] - self.range["in_point"] + 1

    @classmethod
    def from_dict(cls, d) -> "TaskJob":
        """Strict structural parse; touches no disk. Raises TaskRejected."""
        if not isinstance(d, dict):
            _rej("protocol", "job must be an object")
        missing, unknown = set(_JOB_KEYS) - set(d), set(d) - set(_JOB_KEYS)
        if missing or unknown:
            _rej("protocol", f"job keys: missing {sorted(missing)}, unknown {sorted(unknown)}")
        d = _thaw(d)
        if not isinstance(d["job_id"], str) or not _JOB_ID_RE.match(d["job_id"]):
            _rej("protocol", "job_id must match [A-Za-z0-9._-]{1,64}")
        kind = d["kind"]
        if kind not in KINDS:
            _rej("unsupported", f"unknown task kind {kind!r}")
        if not isinstance(d["clip_id"], str) or not d["clip_id"] or len(d["clip_id"]) > 512:
            _rej("protocol", "clip_id must be a non-empty string")
        root = _clean_abs(d["clip_root"], "clip_root")
        r = d["range"]
        if kind in RANGED:
            if (not isinstance(r, dict) or set(r) != set(_RANGE_KEYS) or not _is_int(r["in_point"])
                    or not _is_int(r["out_point"]) or not 0 <= r["in_point"] <= r["out_point"]
                    or not all(isinstance(r[k], str) and r[k] for k in ("in_stem", "out_stem"))):
                _rej("range", "range needs in_point<=out_point (>=0) and in_stem/out_stem")
        elif r is not None:
            _rej("range", f"{kind} is a whole-movie action; range must be null")
        params = _check_params(kind, d["params"])
        stg, pub = d["staging_dir"], d["publish_dir"]
        needs_stg, needs_pub = kind in (*HINT_DST, "sam_preview"), kind in HINT_DST
        if needs_stg != (stg is not None) or needs_pub != (pub is not None):
            _rej("paths", f"{kind}: staging_dir/publish_dir presence is wrong for this kind")
        stg = _clean_abs(stg, "staging_dir", names=True) if stg is not None else None
        pub = _clean_abs(pub, "publish_dir", names=True) if pub is not None else None
        req = d["require"]
        if (not isinstance(req, dict) or set(req) != {"protocol", "models"} or req["protocol"] != PROTOCOL
                or not isinstance(req["models"], dict)
                or not all(isinstance(k, str) and (v is None or isinstance(v, str)) for k, v in req["models"].items())):
            _rej("invalid", f"require must be {{protocol: {PROTOCOL}, models: {{name: sha|null}}}}")
        return cls(d["job_id"], kind, d["clip_id"], root, None if r is None else _freeze(r),
                   _freeze(params), stg, pub, _freeze(req))


# ---------------------------------------------------------------------- disk helpers

class _Cancel:
    """The ``job`` object the service pipelines poll (``is_cancelled``)."""
    def __init__(self, event: threading.Event):
        self.event = event

    is_cancelled = property(lambda self: self.event.is_set())


def _link(src: str, dst: str) -> None:
    try:
        os.link(src, dst)
    except OSError:
        shutil.copy2(src, dst)


def _images(directory: str) -> dict:
    """stem -> filename for image files directly inside ``directory`` (dotfiles ignored)."""
    if not os.path.isdir(directory):
        return {}
    return {os.path.splitext(f)[0]: f for f in os.listdir(directory)
            if not f.startswith(".") and f.lower().endswith(_IMG)}


def _real_clip(job: TaskJob):
    clip = _KeyClip(name=os.path.basename(job.clip_root), root_path=job.clip_root)
    if not os.path.isdir(job.clip_root):
        _rej("paths", f"clip_root does not exist: {job.clip_root}")
    try:
        clip.find_assets()
    except CorridorKeyError as exc:
        _rej("invalid", f"cannot load clip: {exc}")
    if clip.input_asset is None:
        _rej("invalid", clip.error_message or "clip has no input media")
    if clip.input_asset.asset_type != "sequence":
        _rej("invalid", "This action needs an image sequence; extract the movie first (prepare_source)")
    files = clip.input_asset.get_frame_files()
    stems = [os.path.splitext(f)[0] for f in files]
    a, b = job.range["in_point"], job.range["out_point"]
    if b >= len(stems) or stems[a] != job.range["in_stem"] or stems[b] != job.range["out_stem"]:
        _rej("range", "Source changed: re-set In/Out")
    return clip, files[a:b + 1]


def _atomic_json(path: str, data: dict) -> None:
    tmp = f"{path}.tmp.{os.getpid()}"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(data, f, indent=2)
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp, path)  # our own marker/provenance file, never user data


class _LockBusy(TaskRejected):
    """The hint lock exists; ``extra['lock']`` is what ``inspect_hint_lock`` reports about it."""
    def __init__(self, message: str, lock: dict):
        super().__init__("busy", message)
        self.extra = {"lock": lock}


def _pid_start_ticks(pid: int) -> int | None:
    if sys.platform.startswith("linux"):
        try:
            with open(f"/proc/{pid}/stat", encoding="utf-8") as f:
                return int(f.read().rsplit(")", 1)[1].split()[19])
        except (OSError, ValueError, IndexError):
            return None
    return None


def _pid_alive(pid: int) -> bool | None:
    """Liveness of ``pid`` on this machine; None (unknown) where it cannot be proven safely."""
    if sys.platform == "win32":
        return None  # no ctypes signatures tested for 64-bit HANDLEs: never claim Windows proof
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    except Exception:  # noqa: BLE001 - unknown is a valid answer
        return None


def _pid_ns() -> str | None:
    """Identity of this process's PID namespace (Linux); containers share a hostname but not pids."""
    try:
        return os.readlink("/proc/self/ns/pid")
    except OSError:
        return None


def _read_lock(path: str) -> tuple[dict | None, str | None]:
    """(metadata, problem). Never follows a symlink; wrong owner or unreadable -> problem."""
    try:
        st = os.lstat(path)
    except FileNotFoundError:
        return None, "missing"
    except OSError as exc:
        return None, f"unreadable: {exc}"
    import stat as _st
    if _st.S_ISLNK(st.st_mode) or not _st.S_ISREG(st.st_mode):
        return None, "unsafe: not a regular file"
    if hasattr(os, "getuid") and st.st_uid != os.getuid():
        return None, "unsafe: owned by another user"
    try:
        fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
        with os.fdopen(fd, encoding="utf-8") as f:
            if os.fstat(f.fileno()).st_ino != st.st_ino:
                return None, "unsafe: replaced while reading"
            data = json.load(f)
    except (OSError, ValueError):
        return None, "unreadable: not valid lock metadata"
    if not isinstance(data, dict) or not isinstance(data.get("job_id"), str):
        return None, "unreadable: not a CorridorKey hint lock"
    return data, None


def inspect_hint_lock(clip_root: str, hostname: str | None = None) -> dict | None:
    """Facts about the clip's hint lock for the UI; None when there is no lock."""
    import socket
    path = os.path.join(clip_root, HINT_LOCK)
    if not os.path.lexists(path):
        return None
    data, problem = _read_lock(path)
    host = hostname or socket.gethostname()
    info = {"path": path, "readable": data is not None, "problem": problem}
    if data is None:
        return {**info, "safe_to_clear": False, "same_host": None, "local_dead": None}
    same = data.get("host") == host and data.get("platform") == sys.platform
    # a pid is only comparable inside the namespace that wrote it; unknown namespace = no proof
    ns_ok = data.get("pid_ns") == _pid_ns()
    dead = None
    if same and ns_ok:
        if data.get("boot_id") and _boot_id() and data["boot_id"] != _boot_id():
            dead = True
        elif _is_int(data.get("pid")):
            alive = _pid_alive(data["pid"])
            ticks = _pid_start_ticks(data["pid"]) if alive else None
            if alive is False:
                dead = True
            elif alive and ticks is not None and _is_int(data.get("proc_start_ticks")):
                dead = ticks != data["proc_start_ticks"]  # pid reuse
            elif alive:
                dead = False
    return {**info, **{k: data.get(k) for k in ("job_id", "host", "platform", "pid", "boot_id",
                                                 "proc_start_ticks", "started", "pid_ns")},
            "same_host": same, "same_pid_namespace": ns_ok, "local_dead": dead, "safe_to_clear": dead is True}


def clear_hint_lock(clip_root: str, expect: dict, reason: str, request_id: str,
                    hostname: str | None = None) -> dict:
    """Remove a stale hint lock after exact-identity + liveness proof, then restore any hint
    folder an interrupted publish left in ``.prev``. Returns {"cleared": True, "recovered": [...]}
    or {"cleared": False, "code", "message", "lock"}. Never touches an unreadable, symlinked or
    foreign-owned lock."""
    def refuse(code, message, lock=None):
        return {"cleared": False, "code": code, "message": message, "lock": lock}
    if reason not in ("observed_exit", "user_confirmed_orphan"):
        return refuse("invalid", "reason must be observed_exit or user_confirmed_orphan")
    if not (isinstance(expect, dict) and set(expect) == {"job_id", "host", "pid"}):
        return refuse("invalid", "expect must be exactly {job_id, host, pid}")
    info = inspect_hint_lock(clip_root, hostname)
    if info is None:
        return refuse("missing", "There is no hint lock to clear")
    if not info["readable"]:
        return refuse("unsafe", f"The lock is {info['problem']}; it was left untouched", info)
    if any(info.get(k) != expect[k] for k in ("job_id", "host", "pid")):
        return refuse("mismatch", "The lock now belongs to a different writer; it was left untouched", info)
    if info["same_host"] and info["local_dead"] is False:
        return refuse("alive", "That writer process is still running on this machine", info)
    if reason == "observed_exit" and info["local_dead"] is not True:
        return refuse("unproven", "Cannot prove the writer exited (it ran on another machine or "
                                  "its liveness is unknown); ask the user to confirm it is gone", info)
    path = info["path"]
    tomb = f"{path}.cleared-{request_id}"
    try:
        os.rename(path, tomb)  # atomic: a concurrent acquirer either sees the lock or no lock
    except OSError as exc:
        return refuse("unsafe", f"Could not remove the lock: {exc}", info)
    # the file we moved may not be the one we inspected (replaced in between): verify, else give it back
    moved, problem = _read_lock(tomb)
    same_file = moved is not None and all(
        moved.get(k) == info.get(k) for k in ("job_id", "host", "pid", "boot_id", "proc_start_ticks",
                                              "started", "pid_ns"))
    if not same_file:
        try:
            try:
                os.link(tomb, path, follow_symlinks=False)  # no-clobber: never replaces a newer owner
            except NotImplementedError:  # platform cannot express it; a plain link is still no-clobber
                os.link(tomb, path)
        except FileExistsError:
            return refuse("mismatch", f"The lock changed during removal and a newer lock now exists; "
                                      f"the displaced lock was kept as {tomb}", info)
        except OSError as exc:
            return refuse("unsafe", f"The lock changed during removal and could not be restored "
                                    f"({exc}); it was kept as {tomb}", info)
        _rm(tomb)
        return refuse("mismatch", "The lock changed while it was being cleared; it was restored untouched",
                      inspect_hint_lock(clip_root, hostname))
    _rm(tomb)
    recovered = []
    probe = _HintLock(types.SimpleNamespace(job_id=f"recover-{request_id}", clip_root=clip_root))
    try:
        probe.acquire()  # our own short lock while we restore interrupted publishes
        for name in sorted(set(HINT_DST.values())):
            final = os.path.join(clip_root, name)
            had = os.path.lexists(final)
            _recover_prev(final)
            if not had and os.path.lexists(final):
                recovered.append(final)
    except (TaskRejected, OSError) as exc:  # the lock is gone either way; report, never kill the worker
        return {"cleared": True, "recovered": recovered,
                "warning": getattr(exc, "message", None) or f"hint folder recovery failed: {exc}"}
    finally:
        probe.release()
    return {"cleared": True, "recovered": recovered}


class _HintLock:
    """``<clip>/.corridorkey_hint.lock``: one hint publisher per clip; never stolen."""
    def __init__(self, job):
        self.path, self.job, self.held, self.ident = os.path.join(job.clip_root, HINT_LOCK), job, False, None

    def acquire(self):
        import socket
        try:
            fd = os.open(self.path, os.O_CREAT | os.O_EXCL | os.O_WRONLY | getattr(os, "O_NOFOLLOW", 0), 0o644)
        except FileExistsError:
            info = inspect_hint_lock(self.job.clip_root) or {}
            raise _LockBusy(f"Hints for this clip are locked by job {info.get('job_id')} on "
                            f"{info.get('host')}. The lock is never removed automatically; recover it "
                            "from the app after confirming that writer is gone.", info) from None
        self.held = True
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            self.ident = os.fstat(f.fileno()).st_ino
            json.dump({"schema": 1, "job_id": self.job.job_id, "host": socket.gethostname(),
                       "platform": sys.platform, "pid": os.getpid(), "boot_id": _boot_id(),
                       "proc_start_ticks": _proc_start_ticks(), "pid_ns": _pid_ns(), "started": time.time()}, f)

    def owned(self) -> bool:
        """True while the lock file is still the one WE created (same file, same job)."""
        data, _ = _read_lock(self.path)
        try:
            same = os.lstat(self.path).st_ino == self.ident
        except OSError:
            same = False
        return bool(self.held and data and data.get("job_id") == self.job.job_id and same)

    def verify(self) -> None:
        """Fence: a writer that lost its lock (cleared as 'dead', replaced) must stop publishing."""
        if not self.owned():
            raise PublishError(f"The hint lock {self.path} was lost or taken by another job; this "
                               "worker stops writing. The previous hints were kept.")

    def release(self):
        if self.held:
            mine = self.owned()  # never remove a lock that now belongs to someone else
            self.held = False
            if mine:
                _rm(self.path)


def _recover_prev(final: str) -> None:
    """Finish an interrupted publish. Caller holds the hint lock. Only app-named folders next to
    ``final`` are touched, never symlinks, and nothing is guessed:
    - exactly one ``<final>.prev-*`` and no ``final``: it is the last good folder, restore it;
    - several ``.prev-*`` and no ``final``: ambiguous, leave all and say so;
    - ``final`` present: a ``.prev-<job>`` whose job published ``final`` (provenance job_id) is
      leftover from a completed swap and is removed; any other ``.prev-*`` is left alone;
    - ``<final>.new-*``: a half-built merge, removed."""
    parent, base = os.path.split(final)
    names = sorted(os.listdir(parent))
    real = lambda n: os.path.isdir(os.path.join(parent, n)) and not os.path.islink(os.path.join(parent, n))  # noqa: E731
    prevs = [n for n in names if n.startswith(base + ".prev-") and real(n)]
    if not os.path.lexists(final):
        if len(prevs) == 1:
            os.rename(os.path.join(parent, prevs[0]), final)
            logger.warning("restored %s from %s after an interrupted publish", final, prevs[0])
        elif prevs:
            logger.error("%d interrupted-publish folders next to %s; not guessing which is current: %s",
                         len(prevs), final, prevs)
    elif os.path.isdir(final) and not os.path.islink(final):
        try:
            with open(os.path.join(final, HINT_PROVENANCE), encoding="utf-8") as f:
                owner = json.load(f).get("job_id")
        except (OSError, ValueError, AttributeError):
            owner = None
        for n in prevs:
            if owner and n == f"{base}.prev-{owner}":
                shutil.rmtree(os.path.join(parent, n), ignore_errors=True)
    for n in names:
        if n.startswith(base + ".new-") and real(n):
            shutil.rmtree(os.path.join(parent, n), ignore_errors=True)


def _validate_set(staged: str, stems: list, size: tuple | None, cancel) -> tuple:
    """Exactly ``stems`` as readable images of one size (matching ``size`` when given)."""
    got = _images(staged)
    if set(got) != set(stems) or len(got) != len(stems):
        bad = sorted(set(got) ^ set(stems))[:3]
        raise CorridorKeyError(f"Generated hint set does not match the range (e.g. {bad}); "
                               "nothing was published.")
    dims = size
    for s in stems:
        if cancel.is_set():
            raise JobCancelledError("hint", 0)
        img = imread_unicode(os.path.join(staged, got[s]), cv2.IMREAD_UNCHANGED | cv2.IMREAD_ANYDEPTH)
        if img is None or img.size == 0:
            raise CorridorKeyError(f"Generated hint frame {got[s]} is unreadable; nothing was published.")
        if dims is None:
            dims = img.shape[:2]
        elif img.shape[:2] != tuple(dims):
            raise CorridorKeyError(f"Generated hint frame {got[s]} is {img.shape[1]}x{img.shape[0]}, "
                                   f"expected {dims[1]}x{dims[0]}; nothing was published.")
    return dims


def publish_hint_dir(staged: str, final: str, stems: list, job: TaskJob, provenance: dict,
                     guard: Callable[[], None] | None = None) -> int:
    """Merge ``staged`` into ``final`` (range frames replaced, others kept), recoverably."""
    parent = os.path.dirname(final)
    _recover_prev(final)
    if os.path.lexists(final) and (os.path.islink(final) or not os.path.isdir(final)):
        raise PublishError(f"{final} is not a regular folder; left untouched")
    new, prev = f"{final}.new-{job.job_id}", f"{final}.prev-{job.job_id}"
    os.mkdir(new)
    try:
        replaced = set(stems)
        if os.path.isdir(final):
            for stem, fname in _images(final).items():
                if stem not in replaced:
                    _link(os.path.join(final, fname), os.path.join(new, fname))
        for fname in os.listdir(staged):
            if not fname.startswith("."):
                shutil.move(os.path.join(staged, fname), os.path.join(new, fname))
        _atomic_json(os.path.join(new, HINT_PROVENANCE), provenance)
        if guard:
            guard()  # lock ownership, immediately before the swap
        moved = os.path.isdir(final)
        if moved:
            os.rename(final, prev)
        try:
            if guard:
                guard()
            os.rename(new, final)
        except BaseException:
            if moved:
                os.rename(prev, final)
            raise
        if moved:
            shutil.rmtree(prev, ignore_errors=True)
    except BaseException:
        shutil.rmtree(new, ignore_errors=True)
        raise
    return len(stems)


# ------------------------------------------------------------------------ the view

def _build_view(job: TaskJob, clip, files: list, work: str, *, masks: dict | None = None):
    """App-owned clip folder holding links to exactly the range frames (and chosen masks)."""
    view = os.path.join(work, "clip")
    frames = os.path.join(view, "Frames")
    os.makedirs(frames)
    for f in files:
        _link(os.path.join(clip.input_asset.path, f), os.path.join(frames, f))
    for name in ("clip.json", "holdout_strokes.json"):
        src = os.path.join(job.clip_root, name)
        if os.path.isfile(src):
            shutil.copy2(src, os.path.join(view, name))
    vc = ClipEntry(name=clip.name, root_path=view)
    vc.input_asset, vc.source_type = ClipAsset(frames, "sequence"), clip.source_type
    if masks:
        mdir = os.path.join(view, "VideoMamaMaskHint")
        os.makedirs(mdir)
        for stem, src in masks.items():
            _link(src, os.path.join(mdir, os.path.basename(src)))
        vc.mask_asset = ClipAsset(mdir, "sequence")
        vc.state = ClipState.MASKED
    return vc


def _mask_sources(job: TaskJob, stems: list) -> dict:
    mdir = os.path.join(job.clip_root, "VideoMamaMaskHint")
    have = _images(mdir)
    return {s: os.path.join(mdir, have[s]) for s in stems if s in have}


# ------------------------------------------------------------------------- context

@dataclass
class _Ctx:
    job: TaskJob
    clip: object = None
    files: list = field(default_factory=list)
    stems: list = field(default_factory=list)
    lock: _HintLock | None = None
    created: list = field(default_factory=list)
    work: str | None = None
    masks: dict = field(default_factory=dict)
    prompts: dict = field(default_factory=dict)  # stem -> parsed sam prompt (+ loaded mask)
    partial: bool = False

    def release(self):
        if self.work and os.path.isdir(self.work):
            shutil.rmtree(self.work, ignore_errors=True)  # only our own work-<job_id> folder
        if self.lock:
            self.lock.release()
        self.work = None


def _check_staging(job: TaskJob, ctx: _Ctx) -> None:
    stg = job.staging_dir
    if _inside(stg, job.clip_root) or _inside(job.clip_root, stg):
        _rej("paths", "staging_dir must be outside the clip folder")
    if job.publish_dir and (_inside(stg, job.publish_dir) or _inside(job.publish_dir, stg)):
        _rej("paths", "staging_dir overlaps publish_dir")
    if os.path.islink(stg):
        _rej("paths", "staging_dir is a symlink")
    _make_dirs_or_reject([stg], ctx.created)
    ctx.work = os.path.join(stg, f"work-{job.job_id}")
    if os.path.lexists(ctx.work):
        ctx.work = None
        _rej("paths", f"{stg}/work-{job.job_id} already exists; use a fresh job_id")
    os.mkdir(ctx.work)


def _load_prompt_mask(job: TaskJob, rel: str, size: tuple) -> np.ndarray:
    """Regular, non-symlink PNG under <staging>/inputs; same size as the frame; binarised."""
    base = os.path.join(job.staging_dir, "inputs")
    path = os.path.join(base, *rel.split("/"))
    if (os.path.islink(base) or not os.path.isdir(base) or not _inside(os.path.realpath(path), os.path.realpath(base))
            or os.path.islink(path) or not os.path.isfile(path)):
        _rej("paths", f"mask_png {rel!r} is not a regular file under {base}")
    m = imread_unicode(path, cv2.IMREAD_GRAYSCALE)
    if m is None or m.shape != tuple(size):
        _rej("invalid", f"mask_png {rel!r} is unreadable or not {size[1]}x{size[0]}")
    return np.where(m > 127, 255, 0).astype(np.uint8)


def _preflight(job: TaskJob) -> _Ctx:
    ctx = _Ctx(job)
    try:
        if job.kind in ("prepare_source", "import_hint"):
            if not os.path.isdir(job.clip_root):
                _rej("paths", f"clip_root does not exist: {job.clip_root}")
            return ctx
        if job.kind == "export_video":
            ctx.stems, ctx.files, ctx.partial = _export_plan(job)
            return ctx
        ctx.clip, ctx.files = _real_clip(job)
        ctx.stems = [os.path.splitext(f)[0] for f in ctx.files]
        if job.kind in HINT_DST:
            want = os.path.join(job.clip_root, HINT_DST[job.kind])
            if os.path.normcase(os.path.abspath(job.publish_dir)) != os.path.normcase(want):
                _rej("paths", f"publish_dir must be {want}")
            ctx.lock = _HintLock(job)
            ctx.lock.acquire()
            _recover_prev(job.publish_dir)  # restore an interrupted publish before any engine work
        _check_staging(job, ctx)
        k, p = job.kind, job.params
        if k == "birefnet":
            if p["variant"] not in birefnet_repos():
                _rej("invalid", f"unknown BiRefNet variant {p['variant']!r}")
        elif k == "matanyone2":
            if p["mask_stem"] not in (None, job.range["in_stem"]):
                _rej("invalid", "MatAnyone2 seeds from the first frame of the range; mask_stem must "
                                "be the In frame")
            ctx.masks = _mask_sources(job, ctx.stems[:1])
            if not ctx.masks:
                _rej("invalid", f"MatAnyone2 needs a mask for {ctx.stems[0]} in VideoMamaMaskHint "
                                "(run Track Mask or import a mask)")
        elif k == "videomama":
            ctx.masks = _mask_sources(job, ctx.stems)
            miss = [s for s in ctx.stems if s not in ctx.masks]
            if miss:
                _rej("invalid", f"VideoMaMa needs dense masks; {len(miss)} range frame(s) have none "
                                f"(first: {miss[0]}). Run Track Mask first.")
        elif k in ("sam_preview", "sam_track"):
            frames = ({p["frame_stem"]: p["prompts"]} if k == "sam_preview" else dict(p["frames"]))
            outside = [s for s in frames if s not in ctx.stems]
            if outside:
                _rej("range", f"prompt frame {outside[0]!r} is outside In/Out")
            first = imread_unicode(os.path.join(ctx.clip.input_asset.path, ctx.files[0]),
                                   cv2.IMREAD_UNCHANGED | cv2.IMREAD_ANYDEPTH)
            if first is None:
                _rej("invalid", "cannot read the first range frame")
            size = first.shape[:2]
            for s, pr in frames.items():
                for q in pr["points"]:
                    if not (0 <= q["x"] < size[1] and 0 <= q["y"] < size[0]):
                        _rej("invalid", f"point ({q['x']}, {q['y']}) is outside the {size[1]}x{size[0]} frame")
                for b in pr["boxes"]:
                    if b[2] > size[1] or b[3] > size[0]:
                        _rej("invalid", "box extends outside the frame")
                mask = _load_prompt_mask(job, pr["mask_png"], size) if pr["mask_png"] else None
                ctx.prompts[s] = {**pr, "mask": mask}
            if not any(any(q["label"] == 1 for q in v["points"]) or v["boxes"]
                       or (v["mask"] is not None and v["mask"].any()) for v in ctx.prompts.values()):
                _rej("invalid", "SAM2 needs at least one non-empty foreground prompt")
        return ctx
    except BaseException:
        ctx.release()
        _rollback(ctx.created)
        raise


def _export_plan(job: TaskJob) -> tuple:
    """(stems, absolute frame paths, partial) of exactly the job's range from the manifest; gaps rejected."""
    p = job.params
    mpath = os.path.join(p["manifest_dir"], MANIFEST_NAME)
    try:
        with open(mpath, encoding="utf-8") as f:
            m = json.load(f)
    except (OSError, ValueError):
        _rej("invalid", f"{mpath} is missing or unreadable")
    r, files = m.get("range") if isinstance(m, dict) else None, (m or {}).get("files")
    stems_all = m.get("source_stems") if isinstance(m, dict) else None
    if (not isinstance(r, dict) or not isinstance(files, dict) or not isinstance(stems_all, list)
            or not isinstance(files.get(p["product"]), list) or m.get("engine") != "corridorkey-v2"
            or m.get("clip_id") != job.clip_id or len(files[p["product"]]) != len(stems_all)):
        _rej("invalid", "manifest is not a CorridorKey v3 result for this clip and product")
    if not m.get("complete") and not p["allow_partial"]:
        _rej("invalid", "The result is incomplete; export a deliberately chosen range with allow_partial")
    a, b = job.range["in_point"] - r["in_point"], job.range["out_point"] - r["in_point"]
    if a < 0 or b >= len(stems_all) or stems_all[a] != job.range["in_stem"] or stems_all[b] != job.range["out_stem"]:
        _rej("range", "export range is outside the result's range or its source frames changed")
    paths = []
    for rel in files[p["product"]][a:b + 1]:
        full = os.path.normpath(os.path.join(p["manifest_dir"], *rel.split("/")))
        if not _inside(full, p["manifest_dir"]) or ".." in rel.split("/"):
            _rej("paths", f"manifest path escapes its folder: {rel!r}")
        if os.path.islink(full) or not os.path.isfile(full):
            _rej("invalid", f"gap in the result: {rel} is missing; choose a range without gaps")
        paths.append(full)
    exts = {os.path.splitext(x)[1].lower() for x in paths}
    if len(exts) != 1:
        _rej("invalid", "export frames mix formats")
    if os.path.lexists(p["dest"]):
        _rej("exists", f"{p['dest']} already exists; it was not replaced")
    return stems_all[a:b + 1], paths, not m.get("complete")


# --------------------------------------------------------------------------- runners

class _Progress:
    def __init__(self, ev, phase):
        self.ev, self.phase, self.last = ev, phase, 0.0

    def __call__(self, *a, **kw):
        cur, tot = [x for x in a if isinstance(x, (int, float)) and not isinstance(x, bool)][-2:] or (0, 0)
        now = time.monotonic()
        if now - self.last >= 0.1 or cur >= tot:
            self.last = now
            self.ev("progress", phase=self.phase, done=int(cur), total=int(tot))


def _chroma(job: TaskJob, ctx: _Ctx, view, cancel, prog) -> None:
    """Headless chroma loop over the view (``run_chroma_key`` imports Qt for the holdout)."""
    from CorridorKeyModule.core.chroma_key import chroma_key_matte
    p = dict(job.params)
    out = os.path.join(view.root_path, "AlphaHint")
    os.makedirs(out)
    holdout, hpath = None, os.path.join(job.clip_root, "holdout_strokes.json")
    strokes = []
    if os.path.isfile(hpath):
        try:
            with open(hpath, encoding="utf-8") as f:
                strokes = json.load(f).get("0", [])
        except (OSError, ValueError, AttributeError):
            _rej("invalid", "holdout_strokes.json is unreadable")
    files = view.input_asset.get_frame_files()
    for i, fname in enumerate(files):
        if cancel.is_set():
            raise JobCancelledError(view.name, i)
        path = os.path.join(view.input_asset.path, fname)
        bgr = imread_unicode(path, cv2.IMREAD_ANYCOLOR | cv2.IMREAD_ANYDEPTH if fname.lower().endswith(".exr")
                             else cv2.IMREAD_COLOR)
        if bgr is None:
            raise CorridorKeyError(f"Cannot read source frame {fname}")
        rgb = cv2.cvtColor(bgr, cv2.COLOR_BGR2RGB)
        if rgb.dtype != np.uint8:
            rgb = (np.clip(rgb, 0.0, 1.0) * 255.0).astype(np.uint8)
        if strokes and holdout is None:  # 128 neutral, 255 force fg, 0 force bg
            h, w = rgb.shape[:2]
            holdout = np.full((h, w), 128, np.uint8)
            for s in strokes:
                col, rad = (255 if s.get("brush_type", "fg") == "fg" else 0), max(1, int(round(s.get("radius", 15.0))))
                pts = [(int(round(x)), int(round(y))) for x, y in s["points"]]
                for q in pts:
                    cv2.circle(holdout, q, rad, col, -1)
                for q0, q1 in zip(pts, pts[1:]):
                    cv2.line(holdout, q0, q1, col, rad * 2)
        matte = chroma_key_matte(
            rgb, holdout_mask=holdout, screen_color=tuple(p["screen_color"]) if p["screen_color"] else None,
            screen_type=p["screen_type"], strength=p["strength"], clip_black=p["clip_black"],
            clip_white=p["clip_white"], shrink_grow=p["shrink_grow"], edge_blur=p["edge_blur"])
        if not imwrite_unicode(os.path.join(out, f"{os.path.splitext(fname)[0]}.png"), matte):
            raise CorridorKeyError(f"Cannot write hint frame for {fname}")
        prog(view.name, i + 1, len(files))


def _run_hint(svc, job: TaskJob, ctx: _Ctx, ev, cancel) -> dict:
    prog = _Progress(ev, job.kind)
    status = lambda m: ev("status", message=m)  # noqa: E731
    warn = lambda m: ev("warning", message=str(m))  # noqa: E731
    cj, k, p = _Cancel(cancel), job.kind, job.params
    view = _build_view(job, ctx.clip, ctx.files, ctx.work,
                       masks=ctx.masks if k in ("videomama", "matanyone2") else None)
    n = len(ctx.stems)
    if k == "sam_track":
        masks = _sam(svc, job, ctx, view, ev, cancel, prog, status, warn)
        staged = os.path.join(view.root_path, "VideoMamaMaskHint")
        os.makedirs(staged)
        for stem, m in zip(ctx.stems, masks):
            if not imwrite_unicode(os.path.join(staged, f"{stem}.png"), np.where(np.asarray(m) > 0, 255, 0).astype(np.uint8)):
                raise CorridorKeyError(f"Cannot write mask for {stem}")
    else:
        ev("status", message="Loading model..." if k != "chroma" else "Keying...")
        if k == "gvm":
            svc.run_gvm(view, job=cj, on_progress=prog, on_warning=warn)
        elif k == "birefnet":
            svc.run_birefnet(view, usage=p["variant"], job=cj, on_progress=prog, on_warning=warn, on_status=status)
        elif k == "chroma":
            _chroma(job, ctx, view, cancel, prog)
        elif k == "matanyone2":
            svc.run_matanyone2(view, job=cj, on_progress=prog, on_warning=warn, on_status=status)
        elif k == "videomama":
            svc.run_videomama(view, job=cj, on_progress=prog, on_warning=warn, on_status=status,
                              chunk_size=p["chunk_size"])
        staged = os.path.join(view.root_path, "AlphaHint")
    if cancel.is_set():
        raise JobCancelledError(view.name, 0)
    first = imread_unicode(os.path.join(view.input_asset.path, ctx.files[0]), cv2.IMREAD_UNCHANGED | cv2.IMREAD_ANYDEPTH)
    size = first.shape[:2] if first is not None else None
    ev("status", message="Validating generated hints...")
    _validate_set(staged, ctx.stems, size, cancel)
    if cancel.is_set():
        raise JobCancelledError(view.name, 0)
    prov = {"kind": k, "job_id": job.job_id, "clip_id": job.clip_id, "range": _thaw(job.range),
            "params": _thaw(job.params), "frames": n, "written": time.time()}
    prog(view.name, n, n)
    ev("status", message="Publishing hints...")
    published = publish_hint_dir(staged, job.publish_dir, ctx.stems, job, prov, guard=ctx.lock.verify)
    if k == "sam_track":
        ctx.lock.verify()
        _atomic_json(os.path.join(job.clip_root, MASK_TRACK_MANIFEST), {
            "source": "sam2", "frame_stems": sorted(_images(job.publish_dir)),
            "model_id": SAM_IDS[p["tracker_model"]]})
    return {"published": published, "expected": n, "hint_dir": job.publish_dir, "complete": True,
            "outputs": [job.publish_dir]}


def _sam(svc, job: TaskJob, ctx: _Ctx, view, ev, cancel, prog, status, warn):
    """Run the SAM2 tracker on ``view`` frames with the job's explicit prompts; returns masks."""
    from sam2_tracker import PromptFrame, SAM2NotInstalledError
    p, cj = job.params, _Cancel(cancel)

    def chk():
        if cancel.is_set():
            raise JobCancelledError(view.name, 0)
    svc.set_sam2_model(SAM_IDS[p["tracker_model"]])
    local = []
    for stem, pr in ctx.prompts.items():
        local.append(PromptFrame(
            frame_index=ctx.stems.index(stem) if job.kind == "sam_track" else 0,
            positive_points=[(q["x"], q["y"]) for q in pr["points"] if q["label"] == 1],
            negative_points=[(q["x"], q["y"]) for q in pr["points"] if q["label"] == 0],
            box=tuple(pr["boxes"][0]) if pr["boxes"] else None, mask=pr["mask"]))
    local.sort(key=lambda f: f.frame_index)
    with svc._gpu_lock:
        tracker = svc._get_sam2_tracker(on_progress=lambda c, t: prog(view.name, c, t), on_status=status)
        chk()
        names = view.input_asset.get_frame_files()
        frames = svc._load_named_sequence_frames(
            view.input_asset, names, view.name,
            gamma_correct_exr=svc._resolve_sequence_input_is_linear(view, p["input_is_linear"]),
            job=cj, on_status=status)
        chk()
        try:
            masks = tracker.track_video([f for _, f in frames], local, on_progress=lambda c, t: prog(view.name, c, t),
                                        on_status=status, check_cancel=chk)
        except SAM2NotInstalledError as exc:
            raise CorridorKeyError(str(exc)) from exc
        except ValueError as exc:
            warn(str(exc))
            raise CorridorKeyError(str(exc)) from exc
    chk()
    return masks


def _run_preview(svc, job: TaskJob, ctx: _Ctx, ev, cancel) -> dict:
    stem = job.params["frame_stem"]
    view = _build_view(job, ctx.clip, [ctx.files[ctx.stems.index(stem)]], ctx.work)
    prog, status = _Progress(ev, "sam_preview"), (lambda m: ev("status", message=m))
    masks = _sam(svc, job, ctx, view, ev, cancel, prog, status, lambda m: ev("warning", message=str(m)))
    mask = np.where(np.asarray(masks[0]) > 0, 255, 0).astype(np.uint8)
    out = os.path.join(job.staging_dir, f"preview-{job.job_id}.png")  # ours; Rust removes it after use
    ok, buf = cv2.imencode(".png", mask)
    if not ok:
        raise CorridorKeyError("PNG encoding of the preview mask failed")
    _write_tmp(out, buf.tobytes())
    h, w = mask.shape[:2]
    ev("preview", frame_stem=stem, width=w, height=h, mask_png=out, score=None)
    return {"published": 0, "expected": 1, "hint_dir": None, "complete": True, "outputs": [out],
            "fill": float((mask > 0).mean())}


def _run_prepare_source(job: TaskJob, ev, cancel) -> dict:
    from .source_normalization import normalize_source_movie
    prog = _Progress(ev, "prepare_source")
    res = normalize_source_movie(job.params["source_path"], job.clip_root, cancel_event=cancel,
                                 on_progress=lambda ph, c, t: ev("progress", phase=ph, done=c, total=t))
    if res is None:
        raise JobCancelledError("source", 0)
    return {"published": res.frame_count, "expected": res.frame_count, "hint_dir": None, "complete": True,
            "outputs": [res.frames_dir], "frames_dir": res.frames_dir, "frames": res.frame_count,
            "fps": res.fps, "width": res.width, "height": res.height, "source_sha256": res.source_sha256,
            "first_stem": res.frame_stems[0], "last_stem": res.frame_stems[-1]}


def _run_import_hint(job: TaskJob, ev, cancel) -> dict:
    from .hint_normalization import normalize_hint_movie
    from .source_normalization import normalize_source_movie
    p = job.params
    on_prog = lambda ph, c, t: ev("progress", phase=ph, done=c, total=t)  # noqa: E731
    ev("status", message="Verifying the normalized source...")
    src = normalize_source_movie(p["source_path"], job.clip_root, cancel_event=cancel, on_progress=on_prog)
    if src is None:
        raise JobCancelledError("source", 0)
    res = normalize_hint_movie(p["hint_path"], job.clip_root, src, kind=p["hint_kind"], channel=p["channel"],
                               cancel_event=cancel, on_progress=on_prog)
    if res is None:
        raise JobCancelledError("hint", 0)
    if p["hint_kind"] == "mask":  # an imported dense mask is VideoMaMa-ready; never replace an existing record
        mpath = os.path.join(job.clip_root, MASK_TRACK_MANIFEST)
        if not os.path.lexists(mpath):
            _write_tmp(mpath, json.dumps({"source": "imported", "frame_stems": list(res.frame_stems),
                                          "model_id": None}, indent=2).encode("utf-8"))
    return {"published": res.frame_count, "expected": res.frame_count, "hint_dir": res.frames_dir,
            "complete": True, "outputs": [res.frames_dir], "fps": res.fps, "width": res.width,
            "height": res.height}


def _ffprobe_check(path: str, n: int, fps: float, alpha: bool) -> None:
    from ..ffmpeg_tools.discovery import find_ffprobe
    probe = find_ffprobe()
    if not probe:
        raise CorridorKeyError("ffprobe not found; cannot validate the export")
    out = subprocess.run(
        [probe, "-v", "error", "-select_streams", "v:0", "-count_packets", "-show_entries",
         "stream=nb_read_packets,r_frame_rate,pix_fmt", "-of", "json", path],
        capture_output=True, text=True, timeout=60, check=False,
        creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0)
    try:
        s = json.loads(out.stdout)["streams"][0]
        num, den = s["r_frame_rate"].split("/")
        got_fps, got_n = int(num) / int(den), int(s["nb_read_packets"])
    except (ValueError, KeyError, IndexError, ZeroDivisionError):
        raise CorridorKeyError(f"Exported video could not be validated: {out.stderr.strip()[:200]}") from None
    if got_n != n:
        raise CorridorKeyError(f"Exported video has {got_n} frames, expected {n}; it was not published.")
    if abs(got_fps - fps) > max(0.01, fps * 1e-3):
        raise CorridorKeyError(f"Exported video runs at {got_fps} fps, expected {fps}; it was not published.")
    if alpha and not str(s.get("pix_fmt", "")).startswith(("yuva", "rgba", "argb", "bgra", "gbrap")):
        raise CorridorKeyError(f"Exported video lost its alpha channel (pix_fmt {s.get('pix_fmt')}); not published.")


def _run_export(job: TaskJob, ctx: _Ctx, ev, cancel) -> dict:
    from ..ffmpeg_tools.discovery import require_ffmpeg_install
    p, n = job.params, len(ctx.files)
    ffmpeg = require_ffmpeg_install(require_probe=True).ffmpeg_path
    created: list = []
    _make_dirs_or_reject([os.path.dirname(p["dest"])], created)
    part = f"{p['dest']}.part-{job.job_id}"
    from .inference import _scratch_root
    scratch = os.path.join(_scratch_root(), f"export-{job.job_id}")
    os.makedirs(scratch)
    ext = os.path.splitext(ctx.files[0])[1].lower()
    owned_part = False
    try:
        for i, src in enumerate(ctx.files):  # sequential names only inside our scratch; gaps were rejected
            _link(src, os.path.join(scratch, f"f{i:08d}{ext}"))
        alpha = p["container"] in _ALPHA_CONTAINERS
        cmd = [ffmpeg, "-hide_banner", "-n", "-framerate", repr(float(p["fps"])), "-i", os.path.join(scratch, f"f%08d{ext}")]
        if ext == ".exr" and p["product"] != "matte":  # linear colour -> sRGB, as stitch_video does
            cmd += ["-vf", "zscale=tin=linear:t=iec61966-2-1:pin=709:p=709:min=gbr:m=gbr:rin=full:r=full"]
            ev("warning", message="EXR frames are linear; the video is encoded with the sRGB transfer.")
        elif ext == ".exr":
            ev("warning", message="Matte EXR values are written as-is (no sRGB curve) to a gray video.")
        if p["container"] == "mov_prores4444":
            cmd += ["-c:v", "prores_ks", "-profile:v", "4444", "-qscale:v", "9", "-pix_fmt", "yuva444p10le",
                    "-vendor", "apl0", "-f", "mov"]
        elif p["container"] == "webm_alpha":
            cmd += ["-c:v", "libvpx-vp9", "-pix_fmt", "yuva420p", "-b:v", "0", "-crf", "18", "-auto-alt-ref", "0",
                    "-row-mt", "1", "-f", "webm"]
        else:
            cmd += ["-c:v", "libx264", "-crf", "18", "-pix_fmt", "yuv420p", "-f", "mp4"]
            if p["product"] in ("processed",):
                ev("warning", message="MP4 has no alpha channel; transparency is not kept.")
        cmd += ["-an", "-progress", "pipe:1", "-nostats", part]
        ev("warning", message="Audio is not included in exported videos.")
        owned_part = True
        proc = subprocess.Popen(cmd, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                text=True, encoding="utf-8", errors="replace",
                                creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0)
        err: list = []
        threading.Thread(target=lambda: err.extend(proc.stderr), daemon=True).start()
        for line in proc.stdout:
            if cancel.is_set():
                proc.terminate()
                break
            if line.startswith("frame="):
                ev("progress", phase="export_video", done=min(n, int(line.split("=")[1] or 0)), total=n)
        try:
            proc.wait(timeout=30 if cancel.is_set() else None)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
        if cancel.is_set():
            raise JobCancelledError("export", 0)
        if proc.returncode != 0:
            raise CorridorKeyError("FFmpeg export failed:\n" + "".join(err[-10:]))
        _ffprobe_check(part, n, float(p["fps"]), alpha)
        _link_noclobber(part, p["dest"])  # hard link then drop the .part; never replaces
        owned_part = False
    except BaseException:
        _rollback(created)
        raise
    finally:
        if owned_part:
            _rm(part)
        shutil.rmtree(scratch, ignore_errors=True)
    return {"published": n, "expected": n, "hint_dir": None, "complete": True, "outputs": [p["dest"]],
            "frames": n, "fps": float(p["fps"]), "container": p["container"], "audio": "none",
            "alpha": alpha, "partial": ctx.partial}


def _error_kind(exc: BaseException) -> str:
    text = f"{type(exc).__name__} {exc}".lower()
    if "out of memory" in text or "outofmemory" in text:
        return "oom"
    if isinstance(exc, (PublishError, OSError)) or "no space" in text:
        return "disk"
    if type(exc).__name__ in ("V2EngineError", "GPURequiredError", "SAM2NotInstalledError"):
        return "engine"
    if "normalization" in type(exc).__name__.lower():
        return "input"
    return "internal"


# ------------------------------------------------------------------------- the job

def run_task_job(service, job: TaskJob, *, emit: Callable[[dict], None], cancel: threading.Event,
                 worker: dict | None = None) -> dict:
    """Run one task. Raises TaskRejected (nothing written, no ``accepted``) or returns ``done``."""
    ev = lambda event, **kw: emit({"event": event, "job_id": job.job_id, **kw})  # noqa: E731
    ctx = _preflight(job)
    done = {"event": "done", "job_id": job.job_id, "kind": job.kind, "outcome": "failed", "message": None,
            "error_kind": None, "result": None}
    try:
        ev("accepted")
        k = job.kind
        if k in HINT_DST:
            done["result"] = _run_hint(service, job, ctx, ev, cancel)
        elif k == "sam_preview":
            done["result"] = _run_preview(service, job, ctx, ev, cancel)
        elif k == "export_video":
            done["result"] = _run_export(job, ctx, ev, cancel)
        elif k == "prepare_source":
            done["result"] = _run_prepare_source(job, ev, cancel)
        else:
            done["result"] = _run_import_hint(job, ev, cancel)
        done["outcome"] = "complete"
    except JobCancelledError:
        done.update(outcome="cancelled", message="Cancelled; the previous result was kept.")
    except Exception as exc:  # noqa: BLE001 - every accepted job must end in `done`
        logger.exception("task %s (%s) failed", job.job_id, job.kind)
        done.update(message=str(exc) or type(exc).__name__, error_kind=_error_kind(exc))
    finally:
        ctx.release()
        _rollback(ctx.created)
    return done
