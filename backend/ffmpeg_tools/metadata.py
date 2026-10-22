"""Video metadata sidecar read/write."""
from __future__ import annotations

import json
import logging
import os

logger = logging.getLogger(__name__)

_METADATA_FILENAME = ".video_metadata.json"


def write_video_metadata(clip_root: str, metadata: dict) -> None:
    """Write video metadata sidecar JSON to clip root.

    Metadata typically includes: source_path, fps, width, height,
    frame_count, codec, duration, plus optional diagnostic fields such
    as source_probe and exr_vf for extraction bug reports.
    """
    path = os.path.join(clip_root, _METADATA_FILENAME)
    tmp_path = path + ".tmp"
    with open(tmp_path, 'w') as f:
        json.dump(metadata, f, indent=2)
    os.replace(tmp_path, path)  # atomic: a crash never leaves a half-written sidecar
    logger.debug(f"Video metadata written: {path}")


def build_video_metadata(video_path: str, info: dict, extracted: int) -> dict:
    """The sidecar dict written after extraction (shared by the Qt worker and the native importer)."""
    from .color import build_exr_vf
    return {
        "source_path": video_path,
        "fps": info.get("fps", 24.0),
        "width": info.get("width", 0),
        "height": info.get("height", 0),
        "frame_count": extracted,
        "codec": info.get("codec", "unknown"),
        "duration": info.get("duration", 0),
        "exr_vf": build_exr_vf(info),
        "source_probe": {
            "frame_count": info.get("frame_count", 0),
            "pix_fmt": info.get("pix_fmt", ""),
            "color_space": info.get("color_space", ""),
            "color_primaries": info.get("color_primaries", ""),
            "color_transfer": info.get("color_transfer", ""),
            "color_range": info.get("color_range", ""),
            "chroma_location": info.get("chroma_location", ""),
            "bits_per_raw_sample": info.get("bits_per_raw_sample", 0),
        },
    }


def read_video_metadata(clip_root: str) -> dict | None:
    """Read video metadata sidecar from clip root. Returns None if not found."""
    path = os.path.join(clip_root, _METADATA_FILENAME)
    if not os.path.isfile(path):
        return None
    try:
        with open(path, 'r') as f:
            return json.load(f)
    except (json.JSONDecodeError, OSError) as e:
        logger.debug(f"Failed to read video metadata: {e}")
        return None
