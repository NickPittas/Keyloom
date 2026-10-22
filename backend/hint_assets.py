"""Delete generated hint assets (AlphaHint / VideoMamaMaskHint) from a clip.

Deletion rules mirror ``ui/widgets/io_tray_actions.py`` (_clear_alpha_batch,
_clear_mask_batch): the ``AlphaHint`` / ``VideoMamaMaskHint`` directory plus
root-level ``<Name>.*`` files with a video extension, and for masks the
mask-track manifest. Frames/Source/Output, the completion manifest,
annotations, clip.json/project metadata and in/out are never touched.
"""
from __future__ import annotations

import glob
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
