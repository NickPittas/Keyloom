"""Bounded raw-EXR peak sampling: heuristic evidence about input colour.

Reads at most the first, middle and last frame. Never infers a transfer
function, never modifies files, never switches settings. Values <= 1 are
only heuristic evidence (display-coded vs scene-linear is not decidable).
"""
from __future__ import annotations

from dataclasses import dataclass
from typing import Sequence

import cv2
import numpy as np

from backend.frame_io import imread_unicode


@dataclass(frozen=True)
class InputColourSample:
    peak: float | None
    sampled_indices: tuple[int, ...]
    dimensions: tuple[int, int] | None  # (width, height)
    frame_count: int
    error: str | None = None


def _unavailable(indices: tuple[int, ...], count: int, error: str | None,
                 dims: tuple[int, int] | None = None) -> InputColourSample:
    return InputColourSample(None, indices, dims, count, error)


def sample_input_colour(frame_paths: Sequence[str]) -> InputColourSample:
    paths = [str(p) for p in frame_paths]
    n = len(paths)
    if n == 0:
        return _unavailable((), 0, None)
    if not all(p.lower().endswith(".exr") for p in paths):
        return _unavailable((), n, None)
    indices = tuple(sorted({0, n // 2, n - 1}))
    peak: float | None = None
    dims: tuple[int, int] | None = None
    for i in indices:
        try:
            img = imread_unicode(paths[i], cv2.IMREAD_UNCHANGED)
        except Exception as exc:  # decoder failure -> unavailable
            return _unavailable(indices, n, f"decode failed: {exc}")
        if img is None or img.ndim not in (2, 3) or img.size == 0:
            return _unavailable(indices, n, f"decode failed: {paths[i]}")
        if dims is None:
            dims = (int(img.shape[1]), int(img.shape[0]))
        colour = img if img.ndim == 2 or img.shape[2] < 4 else img[..., :3]
        finite = np.asarray(colour, dtype=np.float32)
        finite = finite[np.isfinite(finite)]
        del img, colour
        if finite.size == 0:
            return _unavailable(indices, n, "no finite colour values", dims)
        m = float(finite.max())
        peak = m if peak is None else max(peak, m)
    return InputColourSample(peak, indices, dims, n)
