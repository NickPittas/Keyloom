"""Range-confined, no-clobber CorridorKey 2.0 Key job (entry point of the native-app worker).

``run_inference`` (Qt) is untouched. This module reads, stages, infers and publishes ONLY the
job's ``in..out`` frames, writes only to the resolved paths the app froze into the job, and never
replaces a file it did not write in this job. See docs/design/rust-ui/ai-implementation-brief.md.
"""
from __future__ import annotations

import errno
import glob
import hashlib
import json
import logging
import ntpath
import os
import re
import shutil
import socket
import sys
import tempfile
import threading
import time
from contextlib import contextmanager
from dataclasses import dataclass
from types import MappingProxyType
from typing import Callable

import cv2
import numpy as np

from ..clip_state import ClipEntry, ClipState
from ..errors import CorridorKeyError, JobCancelledError
from ..frame_io import _linear_to_srgb
from .core import InferenceParams, OutputConfig
from .frame_ops import FrameOpsMixin
from .inference import _scratch_root
from .v2_inputs import V2InputError, prepare_v2_inputs, range_dimensions

logger = logging.getLogger(__name__)

PROTOCOL = 1
MANIFEST_NAME = ".corridorkey_manifest.json"
LOCK_NAME = ".corridorkey_job.lock"
REPLACE_JOURNAL = ".corridorkey_replace.json"  # crash journal of a mode='replace' swap
EXR_PIXEL_TYPE = "float32"  # EXR channels are FLOAT; only the lossless codecs keep every value
LOSSY_EXR = ("dwab",)  # DWAB quantises even FLOAT channels (values > 65504 are clipped)
LOCK_SCHEMA = 1
HEARTBEAT_S = 10.0
PUBLISHED_BATCH_S = 0.25  # <= 4 Hz
_JOB_ID_RE = re.compile(r"^[A-Za-z0-9._-]{1,64}$")
_JOB_KEYS = ("job_id", "clip_id", "clip_root", "range", "params", "output_config",
             "hint_keyframes_abs", "outputs", "manifest_dir", "mode", "publish", "path_map",
             "require")
_RANGE_KEYS = ("in_point", "out_point", "in_stem", "out_stem")
_FORMAT_OF = {"fg": "fg_format", "matte": "matte_format", "comp": "comp_format",
              "processed": "processed_format"}
# errno values meaning "this filesystem has no hard links" (some SMB/NAS mounts)
_NO_LINK = {errno.EPERM, errno.ENOTSUP, errno.EOPNOTSUPP, errno.EXDEV, errno.ENOSYS}


class KeyJobRejected(Exception):
    """Job refused before anything was written. ``code`` is a protocol ``rejected`` code."""

    def __init__(self, code: str, message: str):
        super().__init__(message)
        self.code, self.message = code, message


class PublishError(CorridorKeyError):
    """A file could not be published without risking an overwrite."""


def _rej(code: str, message: str):
    raise KeyJobRejected(code, message)


def _freeze(v):
    if isinstance(v, dict):
        return MappingProxyType({k: _freeze(x) for k, x in v.items()})
    if isinstance(v, (list, tuple)):
        return tuple(_freeze(x) for x in v)
    return v


def _thaw(v):
    if isinstance(v, (dict, MappingProxyType)):
        return {k: _thaw(x) for k, x in v.items()}
    if isinstance(v, (list, tuple)):
        return [_thaw(x) for x in v]
    return v


def _is_int(v) -> bool:
    return isinstance(v, int) and not isinstance(v, bool)


_P = os.path  # path flavour of the OS the worker really runs on (tests swap in ntpath)
_WIN_RESERVED = {"CON", "PRN", "AUX", "NUL", *(f"COM{i}" for i in range(1, 10)),
                 *(f"LPT{i}" for i in range(1, 10))}


def _nt() -> bool:
    return _P is ntpath


def _win_bad_name(comp: str) -> bool:
    return (comp[-1] in " ." or any(c in '<>:"|?*' or ord(c) < 32 for c in comp)
            or comp.split(".")[0].rstrip().upper() in _WIN_RESERVED)


def _clean_abs(p, what: str, names: bool = False) -> str:
    """Absolute, already-normalised path in this OS's flavour; protocol content is never trusted.

    Windows: drive and UNC paths, either slash accepted (returned with backslashes); with
    ``names`` also rejects reserved device names, ':' (alternate streams), '<>"|?*', trailing
    dot/space."""
    ok = isinstance(p, str) and p and "\0" not in p and _P.isabs(p)
    if ok and re.split(r"[\\/]", p).count(".."):
        ok = False
    if ok and _nt() and not _P.splitdrive(p)[0]:  # "\\foo" is rooted but names no drive/share
        ok = False
    norm = _P.normpath(p) if ok else None
    if ok and norm != p and not (_nt() and norm == p.replace("/", "\\")):
        ok = False
    if ok and names and _nt():
        rest = _P.splitdrive(norm)[1]
        ok = not any(_win_bad_name(c) for c in rest.split("\\") if c)
    if not ok:
        _rej("paths", f"{what} must be a clean absolute path without '..'"
                      f"{' or Windows-invalid names' if _nt() else ''}: {p!r}")
    return norm


def _inside(child: str, parent: str) -> bool:
    c, q = _P.normcase(child), _P.normcase(parent)
    return c == q or c.startswith(q.rstrip(_P.sep) + _P.sep)


@dataclass(frozen=True)
class KeyJob:
    """Immutable snapshot of a submitted Key job (mappings are read-only, lists are tuples)."""
    job_id: str
    clip_id: str
    clip_root: str
    range: MappingProxyType
    params: MappingProxyType
    output_config: MappingProxyType
    hint_keyframes_abs: tuple | None
    outputs: MappingProxyType
    manifest_dir: str
    mode: str
    publish: str
    path_map: tuple
    require: MappingProxyType

    @property
    def frames(self) -> int:
        return self.range["out_point"] - self.range["in_point"] + 1

    def to_dict(self) -> dict:
        return {k: _thaw(getattr(self, k)) for k in _JOB_KEYS}

    @classmethod
    def from_dict(cls, d) -> "KeyJob":
        """Strict structural parse; touches no disk. Raises KeyJobRejected."""
        if not isinstance(d, dict):
            _rej("protocol", "job must be an object")
        missing, unknown = set(_JOB_KEYS) - set(d), set(d) - set(_JOB_KEYS)
        if missing or unknown:
            _rej("protocol", f"job keys: missing {sorted(missing)}, unknown {sorted(unknown)}")
        d = _thaw(d)  # private deep copy: later mutation of the caller's dict cannot leak in
        jid = d["job_id"]
        if not isinstance(jid, str) or not _JOB_ID_RE.match(jid):
            _rej("protocol", "job_id must match [A-Za-z0-9._-]{1,64}")
        if not isinstance(d["clip_id"], str) or not d["clip_id"] or len(d["clip_id"]) > 512:
            _rej("protocol", "clip_id must be a non-empty string")
        d["clip_root"] = _clean_abs(d["clip_root"], "clip_root")
        r = d["range"]
        if (not isinstance(r, dict) or set(r) != set(_RANGE_KEYS)
                or not _is_int(r["in_point"]) or not _is_int(r["out_point"])
                or not 0 <= r["in_point"] <= r["out_point"]
                or not all(isinstance(r[k], str) and r[k] for k in ("in_stem", "out_stem"))):
            _rej("range", "range needs in_point<=out_point (>=0) and in_stem/out_stem")
        n = r["out_point"] - r["in_point"] + 1
        try:
            if not isinstance(d["params"], dict) or not isinstance(d["output_config"], dict):
                raise ValueError("params/output_config must be objects")
            params = InferenceParams.from_dict(d["params"])
            cfg = OutputConfig.from_dict(d["output_config"])
            cfg.validate_selection()
            same = params.to_dict() == d["params"] and cfg.to_dict() == d["output_config"]
        except (ValueError, TypeError) as exc:
            _rej("invalid", f"params/output_config rejected: {exc}")
        if not same:
            _rej("invalid", "params/output_config has unknown or non-canonical fields")
        marks = d["hint_keyframes_abs"]
        if params.hint_mode == "keyframes":
            if (not isinstance(marks, list) or not marks or not all(_is_int(k) for k in marks)
                    or marks != sorted(set(marks))
                    or not all(r["in_point"] <= k <= r["out_point"] for k in marks)):
                _rej("hint", "keyframes mode needs sorted unique marks inside In/Out")
        elif marks is not None:
            _rej("hint", "hint_keyframes_abs is only valid in keyframes mode")
        mdir = _clean_abs(d["manifest_dir"], "manifest_dir", names=True)
        outs = d["outputs"]
        if not isinstance(outs, dict) or set(outs) != set(cfg.enabled_outputs):
            _rej("paths", f"outputs must be exactly the enabled outputs {cfg.enabled_outputs}")
        seen: set[str] = set()
        for name, paths in outs.items():
            ext = "." + getattr(cfg, _FORMAT_OF[name])
            if not isinstance(paths, list) or len(paths) != n:
                _rej("paths", f"outputs.{name} needs {n} paths (one per range frame)")
            for k, p in enumerate(paths):
                p = paths[k] = _clean_abs(p, f"outputs.{name} path", names=True)
                base = _P.basename(p)
                got = _P.splitext(base)[1]
                if (not _inside(p, mdir) or _P.normcase(p) == _P.normcase(mdir)
                        or (got.lower() if _nt() else got) != ext
                        or base.startswith(".") or _P.normcase(p) in seen):
                    _rej("paths", f"outputs.{name}: bad, duplicate or out-of-folder path {p!r}")
                seen.add(_P.normcase(p))
        if d["mode"] not in ("new", "resume", "replace"):
            _rej("invalid", "mode must be 'new', 'resume' or 'replace'")
        if d["publish"] != "final":
            _rej("invalid", "only publish='final' is supported by this worker")
        if d["path_map"] != []:
            _rej("invalid", "path_map is not supported by this worker; send resolved paths")
        req = d["require"]
        if (not isinstance(req, dict) or set(req) != {"protocol", "models"}
                or req["protocol"] != PROTOCOL or not isinstance(req["models"], dict)
                or not all(isinstance(k, str) and (v is None or isinstance(v, str))
                           for k, v in req["models"].items())):
            _rej("invalid", f"require must be {{protocol: {PROTOCOL}, models: {{name: sha|null}}}}")
        return cls(jid, d["clip_id"], d["clip_root"], _freeze(r), _freeze(d["params"]),
                   _freeze(d["output_config"]), None if marks is None else tuple(marks),
                   _freeze(outs), mdir, d["mode"], d["publish"], (), _freeze(req))


# ------------------------------------------------------------------- disk validation

class _KeyClip(ClipEntry):
    def _resolve_state(self) -> None:  # base class reads manifests via QSettings; not needed here
        self.state = ClipState.RAW


def _resolve(path: str) -> str:
    """realpath that tolerates a missing tail (nothing missing can be a symlink)."""
    path, tail = os.path.abspath(path), []
    while not os.path.lexists(path):
        path, t = os.path.split(path)
        tail.append(t)
    return os.path.join(os.path.realpath(path), *reversed(tail))


def _open_clip(job: KeyJob) -> tuple[_KeyClip, list[str]]:
    clip = _KeyClip(name=os.path.basename(job.clip_root), root_path=job.clip_root)
    if not os.path.isdir(job.clip_root):
        _rej("paths", f"clip_root does not exist: {job.clip_root}")
    try:
        clip.find_assets()
    except CorridorKeyError as exc:
        _rej("invalid", f"cannot load clip: {exc}")
    asset = clip.input_asset
    if asset is None:
        _rej("invalid", clip.error_message or "clip has no input media")
    if asset.asset_type != "sequence":
        _rej("invalid", "Range keying needs an image sequence; re-import or extract this clip")
    files = asset.get_frame_files()
    stems = [os.path.splitext(f)[0] for f in files]
    r = job.range
    a, b = r["in_point"], r["out_point"]
    if b >= len(stems) or stems[a] != r["in_stem"] or stems[b] != r["out_stem"]:
        _rej("range", "Source changed: re-set In/Out")
    mode = job.params["hint_mode"]
    if mode != "none" and clip.alpha_asset is None:
        _rej("hint", "Needs hint: this clip has no AlphaHint")
    if mode != "none" and clip.alpha_asset.asset_type == "video":
        _rej("hint", "Range keying needs the AlphaHint as an image sequence, not a video "
                     "(decoding it would read frames outside In/Out). Extract the AlphaHint to "
                     "frames or regenerate it for this range.")
    if mode == "keyframes":
        if clip.hint_keyframes_error:
            _rej("hint", f"Keyframes are unusable: {clip.hint_keyframes_error}")
        inside = [k for k in clip.hint_keyframes if a <= k <= b]
        if not inside:
            _rej("hint", "Needs keyframe marks (none inside In/Out)")
        if inside != list(job.hint_keyframes_abs):
            _rej("hint", "Keyframe marks changed since the job was submitted")
    return clip, files[a:b + 1]


def _check_paths(job: KeyJob, clip: ClipEntry, range_files: list[str]) -> str:
    """Containment (symlinks included) and source/output overlap. Returns resolved manifest_dir."""
    rmd = _resolve(job.manifest_dir)
    protected = {_resolve(a.path) for a in (clip.input_asset, clip.alpha_asset, clip.mask_asset)
                 if a is not None}
    for name in ("Frames", "Input", "Source", "AlphaHint", "VideoMamaMaskHint"):
        protected.add(_resolve(os.path.join(clip.root_path, name)))  # even if not detected
    for pat in ("[Ii]nput.*", "AlphaHint.*", "VideoMamaMaskHint.*"):
        protected.update(_resolve(g) for g in glob.glob(os.path.join(glob.escape(clip.root_path), pat)))
    nc = os.path.normcase
    sources = {nc(os.path.realpath(os.path.join(clip.input_asset.path, f))) for f in range_files}
    if any(_inside(rmd, p) or _inside(p, rmd) for p in protected):
        _rej("paths", "manifest_dir overlaps the clip's source, AlphaHint or mask folder")
    for name in (MANIFEST_NAME, LOCK_NAME):
        if os.path.islink(os.path.join(job.manifest_dir, name)):
            _rej("paths", f"{name} is a symlink")
    seen: set[str] = set()
    for paths in job.outputs.values():
        for p in paths:
            r = _resolve(p)
            if r == rmd or not _inside(r, rmd):
                _rej("paths", f"output escapes manifest_dir (symlink or traversal): {p}")
            if nc(r) in sources or any(_inside(r, q) for q in protected):
                _rej("paths", f"output overlaps a source file: {p}")
            if nc(r) in seen:
                _rej("paths", f"two outputs resolve to the same file: {p}")
            seen.add(nc(r))
            if _nt() and max(len(_tmp_path(p, job)), len(_bak_path(p, job))) > 259 and not p.startswith("\\\\?\\"):
                _rej("paths", f"path too long for Windows (260 chars incl. temp name): {p}. "
                              "Use a shorter output folder or enable long paths.")
    return rmd


def _read_manifest(mdir: str) -> dict | None:
    path = os.path.join(mdir, MANIFEST_NAME)
    if not os.path.lexists(path):
        return None
    if os.path.islink(path) or not os.path.isfile(path):
        _rej("exists", f"{path} is not a regular file")
    try:
        with open(path, encoding="utf-8") as f:
            data = json.load(f)
    except (OSError, ValueError):
        _rej("exists", f"{path} is unreadable; it may belong to another tool")
    if not isinstance(data, dict):
        _rej("exists", f"{path} is not a CorridorKey manifest")
    return data


def _rel_files(job: KeyJob) -> dict:
    # manifest paths are always '/'-separated so every OS (and the Rust app) reads the same file
    return {o: [_P.relpath(p, job.manifest_dir).replace(_P.sep, "/") for p in ps]
            for o, ps in job.outputs.items()}


def _formats(cfg: OutputConfig) -> dict:
    return {"fg": cfg.fg_format, "matte": cfg.matte_format, "comp": cfg.comp_format,
            "processed": cfg.processed_format}


def _pinned_profile(params) -> dict | None:
    """The temporal profile when the window is pinned (then it is fully determined, no GPU needed)."""
    if params["temporal_window"] == "auto":
        return None
    chunk, stride = (int(x) for x in params["temporal_window"].split("/"))
    return {"hann_chunk": chunk, "hann_stride": stride, "low_vram": params["vram_profile"] == "low"}


def resume_problem(manifest, job: KeyJob, stems=None) -> str | None:
    """Why ``manifest`` cannot be resumed by ``job`` (None = compatible). Section 5.2."""
    if not isinstance(manifest, dict) or manifest.get("version") != 3:
        return "the existing result is not a v3 manifest"
    if manifest.get("engine") != "corridorkey-v2" or manifest.get("clip_id") != job.clip_id:
        return "the existing result belongs to another clip"
    if manifest.get("range") != _thaw(job.range):
        return "In/Out changed; a range change is never resumable"
    cfg = OutputConfig.from_dict(_thaw(job.output_config))
    recorded = manifest.get("params")
    if isinstance(recorded, dict):
        recorded = dict(recorded)  # same legacy defaults as InferenceMixin._v2_resume_compatible
        recorded.setdefault("hint_mode", "full" if recorded.get("use_alpha_hint") else "none")
        recorded.setdefault("temporal_window", "auto")
    marks = None if job.hint_keyframes_abs is None else list(job.hint_keyframes_abs)
    checks = [
        (recorded == _thaw(job.params), "settings changed"),
        (manifest.get("enabled_outputs") == cfg.enabled_outputs, "enabled outputs changed"),
        (manifest.get("formats") == _formats(cfg), "output formats changed"),
        (manifest.get("exr_compression") == cfg.exr_compression, "EXR compression changed"),
        (manifest.get("exr_pixel_type") == EXR_PIXEL_TYPE, "EXR pixel type changed"),
        (manifest.get("hint_keyframes") == marks, "keyframe marks changed"),
        (manifest.get("files") == _rel_files(job), "output paths changed"),
        (isinstance(manifest.get("effective_profile"), dict), "no recorded temporal profile"),
        (_pinned_profile(job.params) in (None, manifest.get("effective_profile")),
         "the recorded temporal profile differs from the pinned window"),
        (stems is None or manifest.get("source_stems") == list(stems), "source frames changed"),
    ]
    return next((msg for ok, msg in checks if not ok), None)


def replace_problem(manifest, job: KeyJob, stems=None) -> str | None:
    """Why ``job`` cannot replace ``manifest``'s result (None = it can). Settings may differ; the
    set of files, the range and the source frames may not (only files the manifest lists are swapped)."""
    if not isinstance(manifest, dict) or manifest.get("version") != 3:
        return "the existing result is not a v3 manifest"
    if manifest.get("engine") != "corridorkey-v2" or manifest.get("clip_id") != job.clip_id:
        return "the existing result belongs to another clip"
    checks = [
        (manifest.get("range") == _thaw(job.range), "In/Out changed; write a new version"),
        (manifest.get("files") == _rel_files(job), "the output files differ (enabled outputs, formats "
                                                  "or names changed); write a new version"),
        (stems is None or manifest.get("source_stems") == list(stems), "source frames changed"),
    ]
    return next((msg for ok, msg in checks if not ok), None)


def resume_compatible(manifest, job: KeyJob, stems=None) -> bool:
    return resume_problem(manifest, job, stems) is None


def _check_targets(job: KeyJob, mdir: str, stems: list) -> dict | None:
    """Ownership + existing-file rules (section 4 rule 1). Returns the owned manifest or None."""
    manifest = _read_manifest(mdir)
    if manifest is not None and manifest.get("clip_id") != job.clip_id:
        _rej("exists", "this folder holds another clip's result; choose another version/folder")
    existing = [p for ps in job.outputs.values() for p in ps if os.path.lexists(p)]
    if job.mode == "new" and manifest is not None:  # never rewrite an earlier result's file map
        _rej("exists", "this folder already holds a result (manifest); nothing was written. "
                       "Use Resume, or write a new version.")
    if job.mode == "resume":
        problem = resume_problem(manifest, job, stems) if manifest is not None else "no existing result"
        if problem:
            _rej("exists", f"Cannot resume: {problem}. Write as a new version.")
        bad = [p for p in existing if os.path.islink(p) or not os.path.isfile(p)]
        if bad:
            _rej("exists", f"not a regular file, refusing to touch it: {bad[0]}")
    elif job.mode == "replace":
        problem = replace_problem(manifest, job, stems) if manifest is not None else "no existing result"
        if problem:
            _rej("exists", f"Cannot replace: {problem}.")
        bad = [p for p in existing if os.path.islink(p) or not os.path.isfile(p)]
        if bad:
            _rej("exists", f"not a regular file, refusing to touch it: {bad[0]}")
    elif existing:
        _rej("exists", f"{len(existing)} output file(s) already exist (e.g. {existing[0]}); "
                       "nothing was written. Use Resume or write a new version.")
    return manifest


# ------------------------------------------------------------------------- lease

def _proc_start_ticks() -> int | None:
    """Process creation time: /proc starttime ticks on Linux, FILETIME (100 ns) on Windows."""
    if sys.platform == "win32":  # not exercised by the Linux test run
        try:
            import ctypes
            from ctypes import wintypes
            ft = [wintypes.FILETIME() for _ in range(4)]
            k32 = ctypes.windll.kernel32
            if k32.GetProcessTimes(k32.GetCurrentProcess(), *[ctypes.byref(x) for x in ft]):
                return (ft[0].dwHighDateTime << 32) | ft[0].dwLowDateTime
        except Exception:  # noqa: BLE001 - unknown is a valid answer
            pass
        return None
    try:
        with open("/proc/self/stat", encoding="utf-8") as f:
            return int(f.read().rsplit(")", 1)[1].split()[19])  # field 22: start time since boot
    except (OSError, ValueError, IndexError):
        return None


def _boot_id() -> str | None:
    try:
        with open("/proc/sys/kernel/random/boot_id", encoding="utf-8") as f:
            return f.read().strip() or None
    except OSError:
        return None


class _Lease:
    """``.corridorkey_job.lock`` (schema 1, see tests/fixtures/worker_protocol/lock.json).
    ``platform`` tells how to read ``proc_start_ticks`` (linux: /proc ticks, win32: FILETIME).

    The file's MTIME is the heartbeat (touched every HEARTBEAT_S). A stale heartbeat never lets
    anyone take it over: the old writer may still be running (shared storage). Cleanup needs proof
    of death by the host named in the lock (pid gone / different start time / different boot_id)."""

    def __init__(self, job: KeyJob, info: dict):
        self.path = os.path.join(job.manifest_dir, LOCK_NAME)
        self.job_id, self.clip_id, self.info = job.job_id, job.clip_id, info
        self._stop = threading.Event()
        self._beat: threading.Thread | None = None
        self.held = False  # True only after WE created the lock file

    def acquire(self) -> None:
        try:
            fd = os.open(self.path, os.O_CREAT | os.O_EXCL | os.O_WRONLY | getattr(os, "O_NOFOLLOW", 0), 0o644)
        except FileExistsError:
            holder = {}
            try:
                with open(self.path, encoding="utf-8") as f:
                    holder = json.load(f)
                age = f"{time.time() - os.path.getmtime(self.path):.0f}s since heartbeat"
            except (OSError, ValueError):
                age = "unreadable lock"
            _rej("busy", f"This folder is locked by job {holder.get('job_id')} on "
                         f"{holder.get('host')} ({age}). If you have confirmed that writer is "
                         f"gone, delete {self.path} and retry.")
        self.held = True
        now = time.time()
        try:
            self._write(fd, now)
        except BaseException:
            self.release()
            raise

        def beat():
            while not self._stop.wait(HEARTBEAT_S):
                try:
                    os.utime(self.path)
                except OSError:
                    pass
        self._beat = threading.Thread(target=beat, daemon=True)
        self._beat.start()

    def _write(self, fd: int, now: float) -> None:
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            json.dump({"schema": LOCK_SCHEMA, "job_id": self.job_id, "clip_id": self.clip_id,
                       "host": self.info.get("host"), "platform": sys.platform, "pid": os.getpid(),
                       "boot_id": _boot_id(),
                       "proc_start_ticks": _proc_start_ticks(),
                       "worker_rev": self.info.get("worker_rev"), "started": now,
                       "heartbeat": now, "heartbeat_interval_s": HEARTBEAT_S}, f)

    def verify(self) -> None:
        """Fence: refuse to write if the lock is gone or now belongs to another job (e.g. it was
        removed because this worker was wrongly judged dead)."""
        try:
            with open(self.path, encoding="utf-8") as f:
                mine = json.load(f).get("job_id") == self.job_id
        except (OSError, ValueError):
            mine = False
        if not mine:
            raise PublishError(f"The lock {self.path} was lost or taken by another job; this "
                               "worker stops writing. Nothing further was published.")

    def release(self) -> None:
        """Stop and JOIN the heartbeat first: on Windows a utime in flight blocks the delete."""
        self._stop.set()
        if self._beat is not None and self._beat is not threading.current_thread():
            self._beat.join(5)
        if not self.held:
            return  # never remove a lock we did not create
        self.held = False
        try:
            with open(self.path, encoding="utf-8") as f:
                mine = json.load(f).get("job_id") == self.job_id
            if mine:
                os.remove(self.path)
        except (OSError, ValueError):
            pass


# --------------------------------------------------------------------- publication

def _rm(path: str) -> None:
    """Best-effort delete (Windows can refuse while a scanner/reader holds the file)."""
    try:
        os.remove(path)
    except OSError:
        pass


def _replace_retry(src: str, dst: str, guard=None) -> None:
    """os.replace of OUR manifest; Windows raises PermissionError while a reader has it open.
    ``guard`` (lock ownership check) runs before every attempt."""
    for attempt in range(8):
        try:
            if guard is not None:
                guard()
            return os.replace(src, dst)
        except PermissionError:
            if attempt == 7:
                raise
            time.sleep(0.05 * (attempt + 1))


@contextmanager
def _scratch_dir(root: str):
    """Scratch for one job; failing to delete it (Windows file locks) never fails the job."""
    path = tempfile.mkdtemp(prefix="key_", dir=root)
    try:
        yield path
    finally:
        for attempt in range(4):
            try:
                shutil.rmtree(path)
                break
            except FileNotFoundError:
                break
            except OSError:
                time.sleep(0.1 * (attempt + 1))
        else:
            logger.warning("Could not remove scratch directory %s; delete it manually", path)
            shutil.rmtree(path, ignore_errors=True)


def _tmp_path(final: str, job: KeyJob) -> str:
    """Unique per (full job id, final file); short whatever the output name length."""
    d, name = _P.split(final)
    tag = hashlib.sha1(final.encode("utf-8", "surrogateescape")).hexdigest()[:12]
    return _P.join(d, f".ck-{job.job_id}-{tag}{_P.splitext(name)[1]}")


def _bak_path(final: str, job: KeyJob) -> str:
    """Where the old file of a replace waits until the commit; same folder, hence same volume."""
    d, name = _P.split(final)
    tag = hashlib.sha1(final.encode("utf-8", "surrogateescape")).hexdigest()[:12]
    return _P.join(d, f".ck-old-{job.job_id}-{tag}{_P.splitext(name)[1]}")


def _write_tmp(path: str, data: bytes, owned: list | None = None) -> None:
    """Create ``path`` exclusively and without following a symlink, then fill it.

    ``owned`` gets the path as soon as WE created it; callers delete only what is listed there,
    so a pre-existing (foreign) file of the same name is never removed."""
    try:
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
                     | getattr(os, "O_BINARY", 0), 0o644)  # Windows: O_EXCL never follows links
    except FileExistsError:
        raise PublishError(f"temporary file {path} already exists; it was left untouched") from None
    if owned is not None:
        owned.append(path)
    try:
        with os.fdopen(fd, "wb") as f:
            f.write(data)
            f.flush()
            os.fsync(f.fileno())
    except BaseException:
        _rm(path)
        raise


def _rm_owned(owned: list) -> None:
    for p in owned:
        _rm(p)
    owned.clear()


def _no_link_message(exc: OSError) -> str:
    return (f"This filesystem cannot publish files without risking an overwrite "
            f"(hard links unavailable: {exc.strerror or exc}"
            f"{', winerror ' + str(exc.winerror) if getattr(exc, 'winerror', None) else ''}). Choose an output folder on a local "
            f"or POSIX-compatible disk. Existing outputs were left untouched.")


def _link_noclobber(tmp: str, final: str) -> None:
    """Atomic create-if-absent: hard link, then drop tmp. No exists()+replace fallback, ever."""
    try:
        os.link(tmp, final)
    except FileExistsError:
        raise PublishError(f"{final} appeared while the job ran; it was NOT overwritten") from None
    except OSError as exc:
        if exc.errno in _NO_LINK:
            raise PublishError(_no_link_message(exc)) from None
        raise PublishError(f"Could not publish {final}: {exc}") from None
    finally:
        _rm(tmp)


def _probe_link(directory: str, job: KeyJob) -> None:
    """Prove this directory supports exclusive create + hard link BEFORE any expensive work.

    Any failure of the link step counts as "unsupported" (SMB/Windows report it with assorted
    errno/winerror values); there is deliberately no fallback. Names: .ck-probe-<job_id>-a/-b;
    only files this call created are removed."""
    a = os.path.join(directory, f".ck-probe-{job.job_id}-a")
    b = os.path.join(directory, f".ck-probe-{job.job_id}-b")
    owned: list = []
    try:
        try:
            _write_tmp(a, b"x", owned)
        except (OSError, PublishError) as exc:
            _rej("paths", f"Cannot write into {directory}: {exc}")
        try:
            os.link(a, b)
            owned.append(b)  # the link exists only if link() succeeded
        except OSError as exc:
            _rej("paths", _no_link_message(exc))
    finally:
        _rm_owned(owned)


def _mkdirs(path: str, created: list) -> None:
    """Create missing folders one level at a time; ``created`` lists only those WE made."""
    missing, p = [], path
    while not os.path.lexists(p):
        missing.append(p)
        p = os.path.dirname(p)
    for d in reversed(missing):
        try:
            os.mkdir(d)
            created.append(d)
        except FileExistsError:  # raced with someone else: theirs, so not ours to roll back
            if not os.path.isdir(d):
                raise
    if not os.path.isdir(path):
        raise NotADirectoryError(errno.ENOTDIR, "not a folder", path)


def _make_dirs_or_reject(paths, created: list) -> None:
    for d in paths:
        try:
            _mkdirs(d, created)
        except OSError as exc:
            _rej("paths", f"Cannot create output folder {d}: {exc.strerror or exc}. A file may "
                          "already have that name, or the location is not writable.")


def _rollback(created: list) -> None:
    for d in reversed(created):  # only directories this job created, only if still empty
        try:
            os.rmdir(d)
        except OSError:
            pass


def _put_manifest(path: str, data: dict, job: KeyJob, *, replace_ok: bool, guard=None) -> None:
    """``replace_ok`` only for a manifest this job owns (verified resume, or its own update)."""
    tmp = f"{path}.tmp.{job.job_id}"
    _write_tmp(tmp, json.dumps(data, indent=2).encode("utf-8"))
    try:
        if replace_ok:
            _replace_retry(tmp, path, guard)
        else:
            _link_noclobber(tmp, path)
    finally:
        _rm(tmp)


def _write_exr_float32(path: str, img: np.ndarray, compression: str) -> None:
    """EXR with FLOAT channels (no half conversion, no clipping) and the chosen codec."""
    import Imath
    import OpenEXR
    codec = {"dwab": Imath.Compression.DWAB_COMPRESSION, "piz": Imath.Compression.PIZ_COMPRESSION,
             "zip": Imath.Compression.ZIP_COMPRESSION, "none": Imath.Compression.NO_COMPRESSION}
    if img.ndim == 2:
        planes = {"Y": img}
    elif img.ndim == 3 and img.shape[2] in (3, 4):  # cv2 order BGR(A) -> R, G, B(, A)
        planes = {"R": img[:, :, 2], "G": img[:, :, 1], "B": img[:, :, 0]}
        if img.shape[2] == 4:
            planes["A"] = img[:, :, 3]
    else:
        raise CorridorKeyError(f"Unsupported image shape for EXR write: {img.shape}")
    header = OpenEXR.Header(img.shape[1], img.shape[0])
    header["compression"] = Imath.Compression(codec[compression])
    flt = Imath.Channel(Imath.PixelType(Imath.PixelType.FLOAT))
    header["channels"] = {k: flt for k in planes}
    out = OpenEXR.OutputFile(path, header)
    try:
        out.writePixels({k: np.ascontiguousarray(v, dtype=np.float32).tobytes()
                         for k, v in planes.items()})
    finally:
        out.close()


def _render(svc, product: str, fmt: str, src: str, tmp: str, cfg: OutputConfig,
            owned: list) -> None:
    img = svc._v2_read(product, src)  # float32, finite, cv2 channel order
    if fmt == "exr":
        _write_tmp(tmp, b"", owned)  # exclusive, no-follow creation; the EXR library then fills it
        _write_exr_float32(tmp, img, cfg.exr_compression)
        return
    if product == "cutout":
        img = np.concatenate([_linear_to_srgb(np.clip(img[:, :, :3], 0.0, None)),
                              img[:, :, 3:4]], axis=-1)
    elif product != "alpha":
        img = _linear_to_srgb(np.clip(img, 0.0, None))
    ok, buf = cv2.imencode(".png", (np.clip(img, 0.0, 1.0) * 255.0).astype(np.uint8))
    if not ok:
        raise CorridorKeyError(f"PNG encoding failed for {src}")
    _write_tmp(tmp, buf.tobytes(), owned)


def _error_kind(exc: BaseException) -> str:
    text = f"{type(exc).__name__} {exc}".lower()
    if "out of memory" in text or "outofmemory" in text:
        return "oom"
    if isinstance(exc, V2InputError):
        return "input"
    if isinstance(exc, (PublishError, OSError)) or "not enough free disk" in text:
        return "disk"
    if type(exc).__name__ == "V2EngineError":
        return "engine"
    return "internal"


def _manifest(job, cfg, params, a, b, prepared, profile, worker, jid) -> dict:
    manifest = {
        "version": 3, "engine": "corridorkey-v2", "clip_id": job.clip_id, "job_id": jid,
        "enabled_outputs": cfg.enabled_outputs, "formats": _formats(cfg),
        "exr_compression": cfg.exr_compression, "exr_pixel_type": EXR_PIXEL_TYPE,
        "exr_lossy": cfg.exr_compression in LOSSY_EXR,
        "params": params.to_dict(),
        "range": _thaw(job.range), "frame_range": [a, b],
        "source_stems": list(prepared.source_stems), "files": _rel_files(job),
        "effective_profile": profile,
        "worker": {k: worker.get(k) for k in ("host", "worker_rev", "gpu")},
        "complete": False,
        "alpha_convention": {"processed_exr": "straight-linear",
                             "processed_png": "straight-srgb"},
    }
    if job.hint_keyframes_abs is not None:
        manifest["hint_keyframes"] = list(job.hint_keyframes_abs)
    return manifest


# ---------------------------------------------------------------- replace (swap journal)

def _journal_entries(mdir: str, journal) -> list:
    """Validated entries of a journal found on disk (its content is not trusted blindly).

    Every ``final`` must be the manifest, the settings file, or a file the manifest itself lists
    (read from whichever of the manifest / its backup / its staged copy survived the crash); every
    ``new``/``bak`` must sit beside its ``final`` and carry this journal's job id."""
    ents = journal.get("items") if isinstance(journal, dict) else None
    jid = journal.get("job_id") if isinstance(journal, dict) else None
    if (not isinstance(ents, list) or journal.get("schema") != 1 or not isinstance(jid, str)
            or not _JOB_ID_RE.match(jid) or journal.get("phase") not in ("prepare", "committed")):
        _rej("exists", "the replace journal is malformed; it was left untouched")
    nc = _P.normcase
    for e in ents:
        ok = (isinstance(e, dict) and set(e) == {"final", "new", "bak", "had_old"}
              and isinstance(e["had_old"], bool)
              and all(isinstance(e[k], str) and _P.isabs(e[k]) and _inside(e[k], mdir) for k in ("final", "new", "bak"))
              and _P.dirname(e["new"]) == _P.dirname(e["bak"]) == _P.dirname(e["final"])
              and _P.basename(e["new"]).startswith(f".ck-{jid}-")
              and _P.basename(e["bak"]).startswith(f".ck-old-{jid}-")
              and nc(e["final"]) not in (nc(e["new"]), nc(e["bak"])))
        if not ok:
            _rej("exists", "the replace journal lists a path it should not; it was left untouched")
    top = [e for e in ents if nc(e["final"]) == nc(_P.join(mdir, MANIFEST_NAME))]
    listed = None
    for cand in ([top[0]["final"], top[0]["bak"], top[0]["new"]] if len(top) == 1 else []):
        try:
            with open(cand, encoding="utf-8") as f:
                files = json.load(f).get("files")
            if isinstance(files, dict):
                listed = {nc(_P.normpath(_P.join(mdir, *r.split("/")))) for v in files.values() for r in v}
                break
        except (OSError, ValueError, AttributeError, TypeError):
            continue
    allowed = (listed or set()) | {nc(_P.join(mdir, MANIFEST_NAME)), nc(_P.join(mdir, "corridorkey_v2_settings.txt"))}
    if listed is None or any(nc(e["final"]) not in allowed for e in ents):
        _rej("exists", "the replace journal does not match the result's manifest; it was left untouched")
    return ents


def _undo_swap(ents: list) -> None:
    """Back to the old result: every ``final`` the journal replaced gets its ``bak`` back."""
    for e in reversed(ents):
        if os.path.lexists(e["bak"]):
            os.replace(e["bak"], e["final"])  # final is the journal's own new file (or already gone)
        elif not e["had_old"] and not os.path.lexists(e["new"]) and os.path.lexists(e["final"]):
            _rm(e["final"])  # there was no earlier file: the one in place is ours from this swap
        _rm(e["new"])


def _finish_swap(ents: list) -> None:
    for e in ents:
        _rm(e["bak"])
        _rm(e["new"])


def recover_replace(mdir: str) -> str | None:
    """Finish or undo an interrupted replace (caller holds the lease). 'prepare' means the commit
    flag was never written: restore the old result; 'committed': keep the new one, drop the old."""
    path = os.path.join(mdir, REPLACE_JOURNAL)
    if not os.path.lexists(path):
        return None
    if os.path.islink(path) or not os.path.isfile(path):
        _rej("exists", f"{path} is not a regular file")
    try:
        with open(path, encoding="utf-8") as f:
            journal = json.load(f)
    except (OSError, ValueError):
        _rej("exists", f"{path} is unreadable; an interrupted replace needs manual attention")
    ents = _journal_entries(mdir, journal)
    try:
        (_finish_swap if journal["phase"] == "committed" else _undo_swap)(ents)
    except OSError as exc:
        _rej("exists", f"Cannot finish the interrupted replace: {exc}. The journal was kept.")
    _rm(path)
    return journal["phase"]


def _valid_image(path: str) -> bool:
    try:
        if os.path.getsize(path) == 0:
            return False
        if path.lower().endswith(".png"):
            return cv2.imdecode(np.fromfile(path, np.uint8), cv2.IMREAD_UNCHANGED) is not None
        import OpenEXR
        OpenEXR.InputFile(path).header()
        return True
    except Exception:  # noqa: BLE001 - anything unreadable is invalid
        return False


def _commit_swap(job: KeyJob, items: list, manifest_path: str, mnew: str) -> None:
    jpath = os.path.join(job.manifest_dir, REPLACE_JOURNAL)
    ents = [{"final": f, "new": n, "bak": _bak_path(f, job), "had_old": os.path.lexists(f)} for f, n in items]
    ents.append({"final": manifest_path, "new": mnew, "bak": _bak_path(manifest_path, job), "had_old": True})
    journal = {"schema": 1, "job_id": job.job_id, "clip_id": job.clip_id, "phase": "prepare", "items": ents}
    jtmp = f"{jpath}.tmp.{job.job_id}"
    _write_tmp(jtmp, json.dumps(journal, indent=1).encode("utf-8"))
    try:
        _link_noclobber(jtmp, jpath)  # a leftover journal was recovered earlier; never replace one
    finally:
        _rm(jtmp)
    try:
        for e in ents:  # the manifest swaps last
            if e["had_old"]:
                os.rename(e["final"], e["bak"])
            os.rename(e["new"], e["final"])
        journal["phase"] = "committed"  # the commit point
        _write_tmp(jtmp, json.dumps(journal, indent=1).encode("utf-8"))
        _replace_retry(jtmp, jpath)
    except BaseException:
        _rm(jtmp)
        _undo_swap(ents)  # on failure here the journal stays and the next job recovers
        _rm(jpath)
        raise
    _finish_swap(ents)
    _rm(jpath)


def _replace_publish(service, job: KeyJob, cfg: OutputConfig, result, manifest: dict, lease,
                     check_cancel, ev, done: dict, stems: list) -> None:
    """Render the whole new result next to the old files, validate it, then swap. The old result
    is untouched by anything that can fail (model, inference, cancel, a bad frame)."""
    n, mine, items = job.frames, [], []
    ez_of, fmts = FrameOpsMixin.V2_PRODUCT_TO_EZ, _formats(cfg)
    manifest_path = os.path.join(job.manifest_dir, MANIFEST_NAME)
    try:
        ev("status", message="Writing outputs...")
        for i in range(n):
            check_cancel()
            lease.verify()
            for product in service._v2_required_products(cfg):
                final = job.outputs[ez_of[product]][i]
                new = _tmp_path(final, job)
                os.makedirs(os.path.dirname(final), exist_ok=True)
                _render(service, product, fmts[ez_of[product]], service._v2_runtime_file(
                    product, result.product_dirs[product], i), new, cfg, mine)
                items.append((final, new))
            ev("progress", phase="publish", done=i + 1, total=n)
        bad = [new for _, new in items if not _valid_image(new)]
        if bad:
            raise PublishError(f"{len(bad)} new file(s) failed validation (e.g. {bad[0]}); "
                               "the previous result was kept.")
        settings = os.path.join(job.manifest_dir, "corridorkey_v2_settings.txt")
        if result.settings_path and os.path.isfile(result.settings_path) and os.path.isfile(settings) \
                and not os.path.islink(settings):  # an owned settings file from the old result is swapped too
            snew = _P.join(job.manifest_dir, f".ck-{job.job_id}-settings.txt")
            with open(result.settings_path, "rb") as f:
                _write_tmp(snew, f.read(), mine)
            items.append((settings, snew))
        mnew = _P.join(job.manifest_dir, f".ck-{job.job_id}-manifest.json")
        _write_tmp(mnew, json.dumps(manifest, indent=2).encode("utf-8"), mine)
        check_cancel()  # the last point where cancelling keeps everything as it was
        lease.verify()
        _commit_swap(job, items, manifest_path, mnew)
    finally:
        _rm_owned(mine)
    done.update(published=n, skipped=0, manifest=manifest_path, manifest_complete=True)
    ev("published", stems=list(stems))


# ------------------------------------------------------------------------- the job

def run_key_job(service, job: KeyJob, *, emit: Callable[[dict], None],
                cancel: threading.Event, worker: dict | None = None) -> dict:
    """Run one job. Raises KeyJobRejected (nothing written, no ``accepted`` sent) or returns the
    ``done`` payload (every run after ``accepted`` ends in one, including cancel and failure).
    ``worker`` is {"host","worker_rev","gpu"} for the manifest."""
    worker = worker or {"host": socket.gethostname(), "worker_rev": "unknown", "gpu": None}
    jid = job.job_id
    ev = lambda event, **kw: emit({"event": event, "job_id": jid, **kw})  # noqa: E731
    clip, range_files = _open_clip(job)
    mdir = _check_paths(job, clip, range_files)
    try:
        a, b = job.range["in_point"], job.range["out_point"]
        n = job.frames
        cfg = OutputConfig.from_dict(_thaw(job.output_config))
        params = InferenceParams.from_dict(_thaw(job.params))
    except Exception as exc:  # cannot happen for a parsed job; if it does, nothing is accepted
        raise KeyJobRejected("invalid", f"job settings rejected: {exc}") from exc
    manifest_path = os.path.join(job.manifest_dir, MANIFEST_NAME)
    done = dict(event="done", job_id=jid, outcome="failed", message=None, error_kind=None,
                manifest=None, published=0, skipped=0, expected=n, manifest_complete=False)
    stems = [os.path.splitext(f)[0] for f in range_files]
    if not os.path.lexists(os.path.join(job.manifest_dir, REPLACE_JOURNAL)):
        _check_targets(job, mdir, stems)  # early, lock-free; with a journal the folder is mid-swap
    lease, created = _Lease(job, worker), []
    try:
        _make_dirs_or_reject([job.manifest_dir], created)
        lease.acquire()
        recover_replace(job.manifest_dir)  # an interrupted replace is settled before anything else
        owned = _check_targets(job, mdir, stems)  # the disk may have changed while we waited
        dirs = sorted({os.path.dirname(p) for ps in job.outputs.values() for p in ps})
        _make_dirs_or_reject(dirs, created)
        for d in dirs:
            _probe_link(d, job)  # fail now, not after minutes of GPU work
    except BaseException as exc:  # nothing was accepted: give back exactly what we took
        lease.release()
        _rollback(created)
        if isinstance(exc, KeyJobRejected) or not isinstance(exc, Exception):
            raise
        raise KeyJobRejected("paths", f"Cannot prepare the output location: {exc}") from exc
    sent_stems: list[str] = []
    last_flush = [time.monotonic()]

    def flush(force=False):
        if sent_stems and (force or time.monotonic() - last_flush[0] >= PUBLISHED_BATCH_S):
            ev("published", stems=list(sent_stems))
            sent_stems.clear()
            last_flush[0] = time.monotonic()

    def check_cancel():
        if cancel.is_set():
            raise JobCancelledError(clip.name)

    try:
        ev("accepted")  # from here every exit, even a failed emit, ends in `done` + lease release
        if (job.mode == "resume" and owned.get("complete") is True
                and all(os.path.isfile(p) for ps in job.outputs.values() for p in ps)):
            # Nothing to do: do not rerun the model and never demote a complete result.
            done.update(outcome="complete", manifest=manifest_path, skipped=n,
                        manifest_complete=True)
            return done
        ev("status", message="Loading model...")
        root = _scratch_root()
        service._v2_check_disk(clip, cfg, root, count=n, dims=range_dimensions(clip, (a, b)))
        with _scratch_dir(root) as scratch:
            with service._gpu_lock:
                engine = service._get_v2_engine(lambda m: ev("status", message=m))
                prepared = prepare_v2_inputs(
                    clip, use_alpha_hint=params.hint_mode != "none", hint_mode=params.hint_mode,
                    staging_dir=os.path.join(scratch, "staging"), check_cancel=check_cancel,
                    frame_range=(a, b))
                check_cancel()
                os.makedirs(os.path.join(scratch, "tmp"), exist_ok=True)
                result = engine.run_clip(
                    prepared.input_path, os.path.join(scratch, "out"), params,
                    outputs=service._v2_required_products(cfg),
                    temp_dir=os.path.join(scratch, "tmp"), frame_dir_fps=prepared.fps,
                    hint_video_path=prepared.hint_path,
                    initial_hint_path=prepared.initial_hint_path,
                    initial_hint_index=prepared.initial_hint_index,
                    hint_frame_indices=prepared.hint_frame_indices,
                    ffmpeg_bin=service._v2_ffmpeg_bin(),
                    on_progress=lambda ph, cur, tot: ev("progress", phase=ph, done=cur, total=tot),
                    check_cancel=check_cancel, on_status=lambda m: ev("status", message=m))
            for w in getattr(result, "warnings", ()) or ():
                ev("warning", message=str(w))
            profile = {"hann_chunk": result.hann_chunk, "hann_stride": result.hann_stride,
                       "low_vram": bool(result.low_vram)}
            ev("profile", **profile)
            if job.mode == "resume" and owned["effective_profile"] != profile:
                old = owned["effective_profile"]
                raise V2InputError(
                    f"This machine chose window {profile['hann_chunk']}/{profile['hann_stride']}; "
                    f"existing frames used {old.get('hann_chunk')}/{old.get('hann_stride')}. "
                    "Write as v002, or pin the temporal window.")
            manifest = _manifest(job, cfg, params, a, b, prepared, profile, worker, jid)
            if job.mode == "replace":
                manifest.update(replaces=owned.get("job_id"), complete=True)
                _replace_publish(service, job, cfg, result, manifest, lease, check_cancel, ev, done,
                                 prepared.source_stems)
                done["outcome"] = "complete"
                return done
            lease.verify()
            # before any output; only a verified resume may replace the earlier manifest
            _put_manifest(manifest_path, manifest, job, replace_ok=job.mode == "resume",
                          guard=lease.verify)
            done["manifest"] = manifest_path
            if cfg.exr_compression in LOSSY_EXR and "exr" in _formats(cfg).values():
                ev("warning", message="EXR compression DWAB is lossy: values above 65504 and "
                                      "exact float precision are not preserved. Use PIZ, ZIP or "
                                      "None for exact float32.")
            ev("status", message="Writing outputs...")
            ez_of = FrameOpsMixin.V2_PRODUCT_TO_EZ
            products = service._v2_required_products(cfg)
            fmts = _formats(cfg)
            for i in range(n):
                check_cancel()
                lease.verify()
                wrote = False
                for product in products:
                    ez = ez_of[product]
                    final = job.outputs[ez][i]
                    if job.mode == "resume" and os.path.lexists(final):
                        if os.path.islink(final) or not os.path.isfile(final):
                            raise PublishError(f"{final} is not a regular file; left untouched")
                        continue  # written by the job being resumed (manifest verified)
                    os.makedirs(os.path.dirname(final), exist_ok=True)
                    tmp, mine = _tmp_path(final, job), []
                    try:
                        src = service._v2_runtime_file(product, result.product_dirs[product], i)
                        _render(service, product, fmts[ez], src, tmp, cfg, mine)
                        _link_noclobber(tmp, final)
                    finally:
                        _rm_owned(mine)  # never a pre-existing file that blocked our create
                    wrote = True
                if wrote:
                    done["published"] += 1
                    sent_stems.append(prepared.source_stems[i])
                else:
                    done["skipped"] += 1
                ev("progress", phase="publish", done=i + 1, total=n)
                flush()
            flush(True)
            if all(os.path.isfile(p) for ps in job.outputs.values() for p in ps):
                if result.settings_path and os.path.isfile(result.settings_path):
                    dst = os.path.join(job.manifest_dir, "corridorkey_v2_settings.txt")
                    tmp = f"{dst}.tmp.{jid}"
                    mine = []
                    try:
                        with open(result.settings_path, "rb") as f:
                            _write_tmp(tmp, f.read(), mine)
                        _link_noclobber(tmp, dst)
                    except PublishError:
                        pass  # an earlier run's settings file stays; the manifest is the record
                    finally:
                        _rm_owned(mine)
                manifest["complete"] = True
                _put_manifest(manifest_path, manifest, job, replace_ok=True,  # our own manifest
                              guard=lease.verify)
                done["manifest_complete"] = True
        done["outcome"] = "complete"
    except JobCancelledError:
        flush(True)
        done.update(outcome="cancelled", message="Cancelled; the previous result was kept." if job.mode == "replace"
                    else "Cancelled; published frames were kept.")
    except Exception as exc:  # noqa: BLE001 - every accepted job must end in `done`
        logger.exception("Key job %s failed", jid)
        flush(True)
        done.update(message=str(exc) or type(exc).__name__, error_kind=_error_kind(exc))
    finally:
        lease.release()
    return done
