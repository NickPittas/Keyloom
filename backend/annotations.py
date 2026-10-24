"""Per-frame brush-stroke annotations (prompt authoring) and their mask export. Qt-free: used by the backend pipelines.

Strokes are stored per frame in image-pixel coordinates and persisted to {clip_root}/annotations.json
(or holdout_strokes.json for the static holdout mask)."""
from __future__ import annotations

import json
import logging
import os
from dataclasses import dataclass, field

import cv2
import numpy as np

from backend.frame_io import imwrite_unicode

logger = logging.getLogger(__name__)


# ── Data Model ──────────────────────────────────────────────────────────────

@dataclass
class AnnotationStroke:
    """A single brush stroke in image-pixel coordinates."""
    points: list[tuple[float, float]] = field(default_factory=list)
    brush_type: str = "fg"   # "fg" (foreground/keep) or "bg" (background/remove)
    radius: float = 15.0     # brush radius in image pixels


class AnnotationModel:
    """Per-frame stroke storage with optional raster export."""

    def __init__(self, filename: str = "annotations.json") -> None:
        # stem_index -> list of completed strokes
        self._strokes: dict[int, list[AnnotationStroke]] = {}
        # In-progress stroke (not yet finished)
        self._current_stroke: AnnotationStroke | None = None
        self._current_frame: int = -1
        self._FILENAME = filename

    def start_stroke(self, stem_idx: int, x: float, y: float,
                     brush_type: str, radius: float) -> None:
        """Begin a new stroke on the given frame."""
        self._current_stroke = AnnotationStroke(
            points=[(x, y)],
            brush_type=brush_type,
            radius=radius,
        )
        self._current_frame = stem_idx

    def add_point(self, x: float, y: float) -> None:
        """Extend the current stroke."""
        if self._current_stroke is not None:
            self._current_stroke.points.append((x, y))

    def finish_stroke(self) -> None:
        """Finalize the current stroke and store it."""
        if self._current_stroke is not None and self._current_frame >= 0:
            if len(self._current_stroke.points) >= 1:
                strokes = self._strokes.setdefault(self._current_frame, [])
                strokes.append(self._current_stroke)
        self._current_stroke = None

    @property
    def current_stroke(self) -> AnnotationStroke | None:
        return self._current_stroke

    def undo(self, stem_idx: int) -> bool:
        """Pop the last stroke on a frame. Returns True if a stroke was removed."""
        strokes = self._strokes.get(stem_idx)
        if strokes:
            strokes.pop()
            if not strokes:
                del self._strokes[stem_idx]
            return True
        return False

    def clear(self, stem_idx: int | None = None) -> None:
        """Clear annotations for one frame or all frames."""
        if stem_idx is not None:
            self._strokes.pop(stem_idx, None)
        else:
            self._strokes.clear()
        self._current_stroke = None

    def get_strokes(self, stem_idx: int) -> list[AnnotationStroke]:
        """Return completed strokes for a frame."""
        return self._strokes.get(stem_idx, [])

    def has_annotations(self, stem_idx: int | None = None) -> bool:
        """Check if any annotations exist (optionally for a specific frame)."""
        if stem_idx is not None:
            return bool(self._strokes.get(stem_idx))
        return bool(self._strokes)

    def annotated_frame_count(self) -> int:
        return len(self._strokes)

    # ── Persistence ────────────────────────────────────────────────────────

    def save(self, clip_root: str) -> None:
        """Persist all strokes to {clip_root}/annotations.json."""
        if not clip_root:
            return
        data: dict[str, list[dict]] = {}
        for idx, strokes in self._strokes.items():
            data[str(idx)] = [
                {
                    "points": s.points,
                    "brush_type": s.brush_type,
                    "radius": s.radius,
                }
                for s in strokes
            ]
        path = os.path.join(clip_root, self._FILENAME)
        try:
            if data:
                with open(path, "w") as f:
                    json.dump(data, f)
            elif os.path.isfile(path):
                os.remove(path)
        except OSError as e:
            logger.warning(f"Failed to save annotations: {e}")

    def load(self, clip_root: str) -> None:
        """Load strokes from {clip_root}/annotations.json if it exists."""
        self._strokes.clear()
        self._current_stroke = None
        if not clip_root:
            return
        path = os.path.join(clip_root, self._FILENAME)
        if not os.path.isfile(path):
            return
        try:
            with open(path, "r") as f:
                data = json.load(f)
            for idx_str, stroke_list in data.items():
                idx = int(idx_str)
                self._strokes[idx] = [
                    AnnotationStroke(
                        points=[tuple(p) for p in s["points"]],
                        brush_type=s.get("brush_type", "fg"),
                        radius=s.get("radius", 15.0),
                    )
                    for s in stroke_list
                ]
            logger.info(f"Loaded annotations for {len(self._strokes)} frames from {path}")
        except (json.JSONDecodeError, OSError, KeyError, TypeError) as e:
            logger.warning(f"Failed to load annotations: {e}")

    def export_masks(self, clip_root: str, frame_stems: list[str],
                     width: int, height: int,
                     start_index: int = 0) -> str:
        """Rasterize annotations to binary mask PNGs for VideoMamaMaskHint.

        Args:
            clip_root: Path to clip directory.
            frame_stems: Ordered list of frame stem names to export.
            width: Input frame width (pixels).
            height: Input frame height (pixels).
            start_index: Absolute frame index of the first stem (for stroke lookup
                         when exporting a sub-range via in/out points).

        Returns:
            Path to the created VideoMamaMaskHint directory.
        """
        mask_dir = os.path.join(clip_root, "VideoMamaMaskHint")
        os.makedirs(mask_dir, exist_ok=True)

        for i, stem in enumerate(frame_stems):
            abs_idx = start_index + i
            strokes = self._strokes.get(abs_idx, [])
            if strokes:
                mask = self._rasterize_strokes(strokes, width, height)
            else:
                # Unannotated frame: all-black (user chose "everything is background")
                mask = np.zeros((height, width), dtype=np.uint8)

            out_path = os.path.join(mask_dir, f"{stem}.png")
            imwrite_unicode(out_path, mask)

        logger.info(
            f"Exported {len(frame_stems)} mask frames to {mask_dir} "
            f"({self.annotated_frame_count()} annotated)"
        )
        return mask_dir

    @staticmethod
    def _rasterize_strokes(strokes: list[AnnotationStroke],
                           width: int, height: int) -> np.ndarray:
        """Render strokes into a binary mask (0=bg, 255=fg).

        Foreground strokes draw white, background strokes draw black.
        Strokes are applied in order, so later strokes override earlier ones.
        """
        mask = np.zeros((height, width), dtype=np.uint8)

        for stroke in strokes:
            color = 255 if stroke.brush_type == "fg" else 0
            radius = max(1, int(round(stroke.radius)))

            # Draw circles along the stroke path
            for px, py in stroke.points:
                cx, cy = int(round(px)), int(round(py))
                cv2.circle(mask, (cx, cy), radius, color, -1)

            # Connect consecutive points with thick lines for smooth strokes
            if len(stroke.points) >= 2:
                for i in range(len(stroke.points) - 1):
                    p1 = (int(round(stroke.points[i][0])),
                           int(round(stroke.points[i][1])))
                    p2 = (int(round(stroke.points[i + 1][0])),
                           int(round(stroke.points[i + 1][1])))
                    cv2.line(mask, p1, p2, color, radius * 2)

        return mask

    @staticmethod
    def rasterize_holdout_mask(strokes: list[AnnotationStroke],
                               width: int, height: int) -> np.ndarray:
        """Render holdout strokes into a 3-state mask.

        128 = neutral (no effect), 255 = force foreground, 0 = force background.
        Strokes applied in order so later strokes override earlier ones.
        """
        mask = np.full((height, width), 128, dtype=np.uint8)

        for stroke in strokes:
            color = 255 if stroke.brush_type == "fg" else 0
            radius = max(1, int(round(stroke.radius)))

            for px, py in stroke.points:
                cx, cy = int(round(px)), int(round(py))
                cv2.circle(mask, (cx, cy), radius, color, -1)

            if len(stroke.points) >= 2:
                for i in range(len(stroke.points) - 1):
                    p1 = (int(round(stroke.points[i][0])),
                           int(round(stroke.points[i][1])))
                    p2 = (int(round(stroke.points[i + 1][0])),
                           int(round(stroke.points[i + 1][1])))
                    cv2.line(mask, p1, p2, color, radius * 2)

        return mask
