"""Per-clip cache of SAM2 JPEG frames, kept in the project and reused while the sources are unchanged.

Layout (``root`` is the clip's own hidden staging area, so it travels with the project):
  <root>/<settings key>/{<stem>.jpg, manifest.json}
SAM2 reads these JPEGs where they are: nothing is copied or linked. The manifest records, per
original stem, the source stat identity + content digest and the JPEG size + digest. Prompt, model
and cleanup changes do not touch it; a changed source frame, colour interpretation or conversion
setting re-prepares only what differs. Missing frames are converted by ONE FFmpeg process
(JPEG sources go through the app's own reader so EXIF orientation matches the other readers).
Two PCs (Linux + Windows) may share one project: the key uses the source path relative to the clip.
No lock here: a SAM track holds the clip's hint lock, and one job runs per worker (a preview only adds
its one frame; every file and the manifest are written atomically).
"""
from __future__ import annotations

import hashlib
import json
import os
import shutil
import subprocess
import time
import uuid
from typing import Callable, Optional

from PIL import Image

from ..errors import CorridorKeyError
from ..ffmpeg_tools import build_sam_jpeg_vf, find_ffmpeg, get_ffmpeg_install_help
from ..ffmpeg_tools.probe import stop_process_tree

SCHEMA = 3
CONVERSION_VERSION = 3  # bump when the JPEG preparation changes
JPEG_QSCALE = 1  # FFmpeg mjpeg fixed quantiser (1 = best); matches the former quality-95 encoder
MANIFEST = "manifest.json"
_PART = ".part-"
PART_STALE_S = 3600.0  # temporary files younger than this may belong to another PC's running job
WORK_STALE_S = 48 * 3600.0  # per-job work folders untouched this long belong to a killed job


def _key(obj) -> str:
    return hashlib.sha256(json.dumps(obj, sort_keys=True).encode()).hexdigest()[:24]


def _digest(path: str) -> str:
    h = hashlib.blake2b(digest_size=16)
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def _stat(path: str) -> list:
    st = os.stat(path)
    return [st.st_size, st.st_mtime_ns]


def _load_manifest(folder: str, header: dict) -> dict:
    try:
        with open(os.path.join(folder, MANIFEST), encoding="utf-8") as f:
            m = json.load(f)
        if m.get("header") == header and isinstance(m.get("entries"), dict):
            return m
    except (OSError, ValueError, AttributeError):
        pass
    return {"header": header, "entries": {}}


def _save_manifest(folder: str, m: dict) -> None:
    tmp = os.path.join(folder, f"{_PART}{uuid.uuid4().hex}.json")
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(m, f)
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp, os.path.join(folder, MANIFEST))


def _valid(folder: str, stem: str, e: dict, src: str) -> bool:
    """True when the cached JPEG matches its record and the source frame's stat is unchanged.

    A JPEG whose recorded (size, mtime_ns) is unchanged is trusted without reading it (no network
    read per frame); only a differing stat triggers a re-hash.
    """
    jpg = os.path.join(folder, f"{stem}.jpg")
    try:
        if _stat(src) != e["src"]:
            return False
        st = _stat(jpg)
        if st == e["jpeg_stat"]:
            return True
        if st[0] == e["jpeg_size"] and _digest(jpg) == e["jpeg_digest"]:
            e["jpeg_stat"] = st
            return True
        return False
    except (OSError, KeyError, TypeError):
        return False


def _sweep_parts(folder: str) -> None:
    """Remove temporary files left by a killed run; recent ones may be another PC's live job."""
    now = time.time()
    for n in os.listdir(folder):
        if n.startswith(_PART):
            p = os.path.join(folder, n)
            try:
                if now - os.stat(p).st_mtime > PART_STALE_S:
                    os.unlink(p)
            except OSError:
                pass


def sweep_stale_work(clip_staging: str, own_job_dir: str) -> None:
    """Delete ``work-*`` folders (app-made, links only) of OTHER jobs untouched for WORK_STALE_S.

    Never touches the shared ``sam-frames`` cache, the running job's own folder, anything not named
    ``work-*``, or a job folder that still holds other files (it is only removed when empty).
    """
    now, own = time.time(), os.path.realpath(own_job_dir)
    try:
        jobs = [d for d in os.scandir(clip_staging) if d.is_dir(follow_symlinks=False)]
    except OSError:
        return
    for job in jobs:
        if os.path.realpath(job.path) == own:
            continue
        try:
            for w in os.scandir(job.path):
                if not (w.name.startswith("work-") and w.is_dir(follow_symlinks=False)):
                    continue
                newest = max((os.lstat(os.path.join(r, n)).st_mtime
                              for r, ds, fs in os.walk(w.path) for n in ds + fs + ["."]), default=0)
                if now - max(newest, w.stat().st_mtime) > WORK_STALE_S:
                    shutil.rmtree(w.path, ignore_errors=True)
            os.rmdir(job.path)  # only succeeds when empty
        except OSError:
            pass


def prepare(
    source_asset, names: list, clip_name: str, root: str, *, input_is_linear: bool,
    on_status: Optional[Callable[[str], None]] = None,
    check_cancel: Optional[Callable[[], None]] = None,
    rel_base: Optional[str] = None,
) -> tuple:
    """Make sure a JPEG exists for each of ``names`` (original files in ``source_asset``).

    Returns ``(jpeg_paths_in_order, count, (height, width))``. Only missing/changed frames are
    converted (one FFmpeg process); the paths are the cached files themselves. ``rel_base`` (the clip
    folder) makes the cache key independent of drive letters / mount points.
    """
    check = check_cancel or (lambda: None)
    src_dir = os.path.realpath(source_asset.path)
    stems = [os.path.splitext(n)[0] for n in names]
    header = {"schema": SCHEMA, "conversion": CONVERSION_VERSION, "qscale": JPEG_QSCALE,
              "source": _relative(src_dir, rel_base), "colour": "linear-exr-to-srgb" if input_is_linear else "as-is"}
    folder = os.path.join(root, _key(header))
    try:
        os.makedirs(folder, exist_ok=True)
        _sweep_parts(folder)
        m = _load_manifest(folder, header)
        ent = m["entries"]
        todo = []
        for stem, name in zip(stems, names):
            check()
            src = os.path.join(src_dir, name)
            e = ent.get(stem)
            if not e or not _valid(folder, stem, e, src):
                ent.pop(stem, None)
                todo.append((stem, name, src))
        if todo:
            if on_status:
                on_status(f"Preparing JPEG frames for SAM2 ({len(todo)} new)")
            _convert(todo, clip_name, folder, ent, input_is_linear, check)
        _save_manifest(folder, m)  # new entries and refreshed stat identities
        paths = [os.path.join(folder, f"{s}.jpg") for s in stems]
        return paths, len(stems), (ent[stems[0]]["h"], ent[stems[0]]["w"])
    except OSError as exc:
        raise CorridorKeyError(
            f"Cannot write the prepared-frame cache at {folder}: {exc}. "
            "Check free space and permissions of the project folder and try again.") from exc


def _relative(src_dir: str, base: Optional[str]) -> str:
    """Source folder relative to ``base`` with '/' separators; the absolute path if it is outside."""
    if base:
        try:
            r = os.path.relpath(src_dir, os.path.realpath(base))
        except ValueError:  # other drive on Windows
            return src_dir
        if r != ".." and not r.startswith(".." + os.sep) and not os.path.isabs(r):
            return r.replace(os.sep, "/")
    return src_dir


def _convert(todo, clip_name, folder, ent, linear, check) -> None:
    """FFmpeg-convert ``todo`` into ``folder``; add manifest entries only for verified frames."""
    # source stat BEFORE conversion (no content read); checked again after, so a replaced frame is never cached
    before = {}
    for stem, _, src in todo:
        check()
        try:
            before[stem] = _stat(src)
        except OSError as exc:
            raise CorridorKeyError(f"Cannot read frame {src} of {clip_name}: {exc}") from exc
    outs = [None] * len(todo)
    try:
        # JPEG sources: the app's own reader (EXIF orientation); everything else: one FFmpeg process
        py = [i for i, t in enumerate(todo) if os.path.splitext(t[2])[1].lower() in (".jpg", ".jpeg")]
        ff = [i for i in range(len(todo)) if i not in py]
        for idx, fn in ((ff, lambda srcs: _ffmpeg_batch(srcs, folder, linear, check)),
                        (py, lambda srcs: _python_jpegs(srcs, folder, check))):
            if idx:
                for i, o in zip(idx, fn([todo[i][2] for i in idx])):
                    outs[i] = o
        size = None
        for (stem, _, src), part in zip(todo, outs):
            check()
            with open(part, "rb") as f:
                f.seek(-2, os.SEEK_END)
                whole = f.read(2) == b"\xff\xd9"
            with Image.open(part) as im:
                w, h = im.size
            if not whole:
                raise CorridorKeyError(f"Prepared JPEG for {stem} is truncated; nothing was cached.")
            if size not in (None, (h, w)):
                raise CorridorKeyError(
                    f"Frame {stem} of {clip_name} is {w}x{h} but earlier frames are {size[1]}x{size[0]}; "
                    "all frames of a clip must have the same size.")
            size = (h, w)
            if _stat(src) != before[stem]:
                raise CorridorKeyError(
                    f"Frame {src} changed while its SAM2 JPEG was being prepared; nothing was cached. "
                    "Track again once the frames are stable.")
        for (stem, _, src), part in zip(todo, outs):
            final = os.path.join(folder, f"{stem}.jpg")
            os.replace(part, final)
            st = _stat(final)
            ent[stem] = {"src": before[stem], "jpeg_size": st[0], "jpeg_stat": st,
                         "jpeg_digest": _digest(final), "h": size[0], "w": size[1]}
    finally:
        for part in outs:  # whatever was not moved into place
            if part:
                _unlink(part)


def _python_jpegs(srcs: list, folder: str, check) -> list:
    """JPEG -> JPEG through the app's reader, which applies EXIF orientation (FFmpeg would not)."""
    import cv2

    from ..frame_io import imread_unicode
    run, outs = uuid.uuid4().hex, []
    try:
        for n, s in enumerate(srcs):
            check()
            img = imread_unicode(s)
            if img is None:
                raise CorridorKeyError(f"Cannot read frame {s} for SAM2.")
            ok, buf = cv2.imencode(".jpg", img, [cv2.IMWRITE_JPEG_QUALITY, 95])
            if not ok:
                raise CorridorKeyError(f"Cannot encode a SAM2 JPEG for {s}.")
            out = os.path.join(folder, f"{_PART}{run}-py-{n:06d}.jpg")
            outs.append(out)
            with open(out, "wb") as f:
                f.write(buf.tobytes())
    except BaseException:
        for o in outs:
            _unlink(o)
        raise
    return outs


def _ffmpeg_batch(srcs: list, folder: str, linear: bool, check) -> list:
    """Convert ``srcs`` with one FFmpeg process per source type; returns output paths in ``srcs`` order."""
    ffmpeg = find_ffmpeg()
    if not ffmpeg:
        raise CorridorKeyError(f"FFmpeg is needed to prepare SAM2 frames. {get_ffmpeg_install_help()}")
    run = uuid.uuid4().hex
    groups = {}  # a concat input must hold one codec, so a mixed folder is split by extension
    for i, s in enumerate(srcs):
        groups.setdefault(os.path.splitext(s)[1].lower(), []).append(i)
    outs = [None] * len(srcs)
    junk = []
    try:
        for ext, idx in groups.items():
            base = os.path.join(folder, f"{_PART}{run}-{ext.lstrip('.')}")
            listing = base + ".txt"
            junk += [listing, base + ".log"]
            with open(listing, "w", encoding="utf-8") as f:
                for i in idx:
                    p = srcs[i].replace("\\", "/") if os.name == "nt" else srcs[i]
                    f.write("file '" + p.replace("'", "'\\''") + "'\nduration 1\n")
            pattern = base + "-%06d.jpg"
            expected = [pattern % (n + 1) for n in range(len(idx))]
            junk += expected
            for i, o in zip(idx, expected):
                outs[i] = o
            cmd = [ffmpeg, "-hide_banner", "-nostdin", "-loglevel", "error", "-y",
                   "-f", "concat", "-safe", "0", "-i", listing, "-an", "-sn",
                   "-vf", build_sam_jpeg_vf(linear and ext == ".exr"),
                   "-c:v", "mjpeg", "-q:v", str(JPEG_QSCALE), "-qmin", str(JPEG_QSCALE),
                   "-qmax", str(JPEG_QSCALE), "-fps_mode", "passthrough",
                   "-start_number", "1", pattern]
            _run(cmd, base + ".log", check)
            if not all(os.path.isfile(o) for o in expected):
                raise CorridorKeyError("FFmpeg did not produce every SAM2 frame; nothing was cached.")
    except BaseException:
        for j in junk:
            _unlink(j)
        raise
    for j in junk:
        if not j.endswith(".jpg"):
            _unlink(j)
    return outs


def _unlink(path: str) -> None:
    try:
        os.unlink(path)
    except OSError:
        pass


def _run(cmd: list, log_path: str, check) -> None:
    with open(log_path, "wb") as log:
        proc = subprocess.Popen(
            cmd, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=log,
            creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0,
            start_new_session=os.name != "nt")
        try:
            while True:
                try:
                    proc.wait(timeout=0.25)
                    break
                except subprocess.TimeoutExpired:
                    check()
        except BaseException:
            stop_process_tree(proc)
            raise
    if proc.returncode != 0:
        with open(log_path, "rb") as f:
            tail = f.read()[-800:].decode("utf-8", "replace").strip()
        raise CorridorKeyError(f"FFmpeg could not prepare the SAM2 frames (exit {proc.returncode}): {tail}")
