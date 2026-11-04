"""Delete generated hint assets (AlphaHint / VideoMamaMaskHint) from a clip.

Deletion rules mirror ``ui/widgets/io_tray_actions.py`` (_clear_alpha_batch,
_clear_mask_batch): the ``AlphaHint`` / ``VideoMamaMaskHint`` directory plus
root-level ``<Name>.*`` files with a video extension, and for masks the
mask-track manifest. Frames/Source/Output, the completion manifest,
annotations, clip.json/project metadata and in/out are never touched.
"""
from __future__ import annotations

import glob
import json
import os
import shutil

from .clip_state import MASK_TRACK_MANIFEST, ClipEntry
from .hint_keyframes import KEYFRAMES_FILENAME
from .project import is_video_file

ALPHA_NAME = "AlphaHint"
MASK_NAME = "VideoMamaMaskHint"


def _inside(path: str, real_root: str) -> bool:
    real = os.path.realpath(path)
    return os.path.commonpath([real, real_root]) == real_root


def _remove(path: str, real_root: str, removed: list[str]) -> None:
    """Remove one path (link, file or real directory) confined to the clip."""
    if not os.path.lexists(path):
        return
    if os.path.islink(path):
        # Unlink the link only; never follow it out of the clip.
        if not _inside(os.path.dirname(path), real_root):
            raise OSError(f"Refusing to delete outside clip: {path}")
        os.unlink(path)
    else:
        if not _inside(path, real_root):
            raise OSError(f"Refusing to delete outside clip: {path}")
        if os.path.isdir(path):
            shutil.rmtree(path)
        else:
            os.remove(path)
    removed.append(path)


def _clear_named(root: str, real_root: str, name: str, removed: list[str]) -> None:
    _remove(os.path.join(root, name), real_root, removed)
    for candidate in sorted(glob.glob(os.path.join(root, f"{name}.*"))):
        if (os.path.islink(candidate) or os.path.isfile(candidate)) and is_video_file(candidate):
            _remove(candidate, real_root, removed)


def clear_hint_assets(
    clip: ClipEntry, *, clear_alpha: bool = True, clear_masks: bool = True,
    clear_keyframes: bool = False,
) -> list[str]:
    """Delete the clip's alpha and/or mask hint assets; return removed paths.

    Idempotent (missing assets are fine). Raises OSError on any failure.
    ``clear_keyframes`` also deletes the sparse-keyframe sidecar. It is opt-in
    (never cleared by a plain replacement/mask clear); Reset Hint and tray
    Clear Alpha / Clear All pass True. It is removed last.
    Does not rescan the clip; callers should call ``clip.find_assets()``.
    """
    root = clip.root_path
    real_root = os.path.realpath(root)
    removed: list[str] = []
    if clear_alpha:
        _clear_named(root, real_root, ALPHA_NAME, removed)
    if clear_masks:
        _clear_named(root, real_root, MASK_NAME, removed)
        _remove(os.path.join(root, MASK_TRACK_MANIFEST), real_root, removed)
    if clear_keyframes:
        _remove(os.path.join(root, KEYFRAMES_FILENAME), real_root, removed)
    return removed


def has_hint_assets(clip: ClipEntry) -> bool:
    """True if the clip has any alpha/mask hint asset that clear_hint_assets would remove."""
    root = clip.root_path
    for name in (ALPHA_NAME, MASK_NAME):
        if os.path.lexists(os.path.join(root, name)):
            return True
        if any(is_video_file(p) for p in glob.glob(os.path.join(root, f"{name}.*"))):
            return True
    return (
        os.path.lexists(os.path.join(root, MASK_TRACK_MANIFEST))
        or os.path.lexists(os.path.join(root, KEYFRAMES_FILENAME))
    )


# ---------------------------------------------------------------- exact deletion of generated masks (A8)
# App-side only (backend/mask_delete_cli.py); the GPU worker never deletes. Pre-matte = VideoMamaMaskHint (+ its
# manifest), Hint = AlphaHint. Prompts, marks, the other asset, renders, models and sam-frames are never touched.

PROVENANCE = ".corridorkey_hint.json"
PUBLISH_LOCK = ".corridorkey_hint.lock"
ASSET_NAMES = {"prematte": MASK_NAME, "hint": ALPHA_NAME}
_IMG = (".png", ".jpg", ".jpeg", ".tif", ".tiff", ".bmp", ".exr")


class DeleteRefused(Exception):
    """The deletion must not run; the message is shown to the user as is."""


def _stems_of(folder: str) -> dict[str, str]:
    """stem -> file name of the images directly inside ``folder`` (last name wins on a duplicate stem)."""
    if not os.path.isdir(folder):
        return {}
    return {os.path.splitext(f)[0]: f for f in sorted(os.listdir(folder))
            if not f.startswith(".") and f.lower().endswith(_IMG)}


def _check_idle(root: str, name: str, own_lock: bool) -> None:
    if not own_lock and os.path.lexists(os.path.join(root, PUBLISH_LOCK)):
        raise DeleteRefused("A hint job holds this clip's publication lock; wait for it or recover the lock first.")
    left = [n for n in os.listdir(root) if n.startswith((f"{name}.prev-", f"{name}.new-"))]
    if left:
        raise DeleteRefused(f"An interrupted publish left {left[0]} next to {name}; recover the hints first.")


def plan_mask_delete(root: str, asset: str, scope: str, stems=(), movies: bool = False, *, own_lock: bool = False) -> dict:
    """What a deletion would remove: {"folder", "whole", "files": [names], "blocked": [{"path","message"}],
    "movies": [paths]}. Raises DeleteRefused. ``scope`` is "all" or "frames" (``stems`` = the selected frames)."""
    if asset not in ASSET_NAMES or scope not in ("all", "frames"):
        raise DeleteRefused(f"unknown asset/scope {asset!r}/{scope!r}")
    if not os.path.isdir(root):
        raise DeleteRefused(f"clip folder does not exist: {root}")
    name = ASSET_NAMES[asset]
    real_root = os.path.realpath(root)
    _check_idle(root, name, own_lock)
    folder = os.path.join(root, name)
    if os.path.islink(folder) or (os.path.lexists(folder) and not os.path.isdir(folder)):
        raise DeleteRefused(f"{name} is a link or not a folder; it was left untouched.")
    have = _stems_of(folder)
    pick = set(have) if scope == "all" else {s for s in stems if s in have}
    files, blocked = [], []
    for s in sorted(pick):
        p = os.path.join(folder, have[s])
        if os.path.islink(p) and not _inside(p, real_root):
            blocked.append({"path": p, "message": "link leaves the clip folder; not deleted"})
        else:
            files.append(have[s])
    whole = scope == "all" or (bool(have) and pick == set(have))
    mv = []
    if movies:
        mv = [c for c in sorted(glob.glob(os.path.join(root, f"{name}.*")))
              if (os.path.islink(c) or os.path.isfile(c)) and is_video_file(c)]
    return {"folder": folder, "whole": whole, "files": files, "blocked": blocked, "movies": mv,
            "manifest": asset == "prematte" and whole and os.path.lexists(os.path.join(root, MASK_TRACK_MANIFEST))}


def _atomic_write(path: str, data: dict) -> None:
    tmp = f"{path}.tmp.{os.getpid()}"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(data, f, indent=2)
    os.replace(tmp, path)


def _drop_records(folder: str, gone: set[str]) -> None:
    """Remove ``gone`` stems from the folder's provenance records (empty ranges vanish)."""
    path = os.path.join(folder, PROVENANCE)
    try:
        with open(path, encoding="utf-8") as f:
            prov = json.load(f)
    except (OSError, ValueError):
        return
    ranges = prov.get("ranges") if isinstance(prov, dict) else None
    if not isinstance(ranges, list):
        # legacy/unreadable record: it can no longer certify the frames that remain, so it goes
        os.remove(path)
        return
    out = []
    for r in ranges:
        if not isinstance(r, dict) or not isinstance(r.get("stems"), list):
            continue
        keep = [s for s in r["stems"] if s not in gone]
        if keep:
            out.append({**r, "stems": keep,
                        "digests": {s: d for s, d in (r.get("digests") or {}).items() if s in keep},
                        "stats": {s: d for s, d in (r.get("stats") or {}).items() if s in keep}})
    _atomic_write(path, {**prov, "ranges": out})


def delete_masks(root: str, asset: str, scope: str, stems=(), movies: bool = False, request_id: str = "mask-delete") -> dict:
    """Delete the selected generated frames under the clip's publication lock.

    Returns {"deleted": [paths], "errors": [{"path","message"}]}; a per-file failure never stops the rest.
    Raises DeleteRefused (nothing changed) when a writer, interrupted publish or unsafe link is in the way."""
    import types
    from .service.task_job import TaskRejected, _HintLock
    lock = _HintLock(types.SimpleNamespace(job_id=f"delete-{request_id}", clip_root=root))
    try:
        lock.acquire()
    except TaskRejected as exc:
        raise DeleteRefused(exc.message) from None
    try:
        plan = plan_mask_delete(root, asset, scope, stems, movies, own_lock=True)  # re-checked under the lock
        real_root, folder = os.path.realpath(root), plan["folder"]
        deleted: list[str] = []
        errors = [dict(b) for b in plan["blocked"]]
        gone_stems: set[str] = set()
        for fname in plan["files"]:
            p = os.path.join(folder, fname)
            try:
                _remove(p, real_root, deleted)
                gone_stems.add(os.path.splitext(fname)[0])
            except OSError as exc:
                errors.append({"path": p, "message": str(exc)})
        try:
            if plan["whole"] and not errors:
                _remove(folder, real_root, deleted)  # folder + its records
                if asset == "prematte":
                    _remove(os.path.join(root, MASK_TRACK_MANIFEST), real_root, deleted)
            elif gone_stems and os.path.isdir(folder):
                _drop_records(folder, gone_stems)
                if asset == "prematte":
                    _drop_manifest(root, gone_stems)
        except OSError as exc:
            errors.append({"path": folder, "message": str(exc)})
        for m in plan["movies"]:
            try:
                _remove(m, real_root, deleted)
            except OSError as exc:
                errors.append({"path": m, "message": str(exc)})
        return {"deleted": deleted, "errors": errors}
    finally:
        lock.release()


def _drop_manifest(root: str, gone: set[str]) -> None:
    path = os.path.join(root, MASK_TRACK_MANIFEST)
    try:
        with open(path, encoding="utf-8") as f:
            m = json.load(f)
        m["frame_stems"] = [s for s in m.get("frame_stems", []) if s not in gone]
        _atomic_write(path, m)
    except (OSError, ValueError, AttributeError):
        pass  # no manifest (or not ours): nothing to keep in step
