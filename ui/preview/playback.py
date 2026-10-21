"""Pure playback helpers (no Qt widgets, no inference imports)."""
from __future__ import annotations

import math
from collections.abc import Iterable

import cv2

from backend.ffmpeg_tools.metadata import read_video_metadata
from backend.frame_io import open_video


def _valid_fps(value) -> float | None:
    try:
        fps = float(value)
    except (TypeError, ValueError):
        return None
    return fps if math.isfinite(fps) and fps > 0 else None


def resolve_clip_fps(clip_root: str, *, video_path: str | None = None,
                     default: float = 24.0) -> float:
    """Sidecar fps, else video container fps, else ``default``."""
    meta = read_video_metadata(clip_root)
    if isinstance(meta, dict):
        fps = _valid_fps(meta.get("fps"))
        if fps is not None:
            return fps
    if video_path:
        cap = open_video(video_path)
        try:
            fps = _valid_fps(cap.get(cv2.CAP_PROP_FPS)) if cap.isOpened() else None
        finally:
            cap.release()
        if fps is not None:
            return fps
    return default


def choose_ready_frame(target: int, lo: int, hi: int,
                       last_presented: int | None,
                       ready: Iterable[int]) -> int | None:
    """Newest ready index in [lo, min(target, hi)] not behind last_presented."""
    top = min(target, hi)
    floor = lo
    if last_presented is not None and last_presented <= target:
        floor = max(lo, last_presented)
    best = None
    for i in ready:
        if floor <= i <= top and (best is None or i > best):
            best = i
    return best
