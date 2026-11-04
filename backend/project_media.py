"""Clip scanning from media — image sequences and mixed-media project creation.

Extracted from project.py to keep that module focused on core project
management (creation, JSON helpers, path utilities).
"""
from __future__ import annotations

import logging
import os
import re
import shutil

from .project import (
    is_image_file,
    sanitize_stem,
    write_clip_json,
    write_project_json,
    read_clip_json,
    read_project_json,
    _VIDEO_EXTS,
)

logger = logging.getLogger(__name__)

# keyword -> destination folder/file name at clip root
_KEYWORD_DEST_MAP = {
    "alphahint": "AlphaHint",
    "maskhint": "VideoMamaMaskHint",
}


def _find_companion_hints(source_folder: str) -> dict[str, dict]:
    """Detect companion hint folders or videos next to *source_folder* (referenced in place, never copied).

    Siblings named ``{source_stem}_{keyword}`` (case insensitive), as a folder of frames or a video file:
    ``MyClip_AlphaHint`` -> ``AlphaHint``, ``MyClip_MaskHint`` -> ``VideoMamaMaskHint``.
    The entry must contain both the keyword and the source folder's stem.
    Returns ``{dest_name: {"path": abs path, "kind": "sequence"|"video"}}``.
    """
    source_basename = os.path.basename(source_folder).lower()
    source_stem = os.path.splitext(source_basename)[0]
    parent = os.path.dirname(source_folder)
    if not parent or not os.path.isdir(parent):
        return {}

    found: dict[str, dict] = {}
    for entry in sorted(os.listdir(parent)):
        entry_lower = entry.lower()
        if entry_lower == source_basename:
            continue
        entry_stem, entry_ext = os.path.splitext(entry_lower)
        entry_path = os.path.join(parent, entry)
        is_dir = os.path.isdir(entry_path)
        if not is_dir and not (os.path.isfile(entry_path) and entry_ext in _VIDEO_EXTS):
            continue
        check = entry_lower if is_dir else entry_stem
        for keyword, dest_name in _KEYWORD_DEST_MAP.items():
            if dest_name in found or keyword not in check or source_stem not in check:
                continue
            found[dest_name] = {"path": os.path.abspath(entry_path),
                                "kind": "sequence" if is_dir else "video"}
    return found


def create_clip_from_sequence(
    clips_dir: str,
    source_folder: str,
    *,
    copy_source: bool = False,
    specific_files: list[str] | None = None,
    display_name: str | None = None,
) -> str:
    """Create a clip folder from an image sequence directory.

    When *copy_source* is False (default), the clip references the original
    folder in place — no files are copied. The external path is stored in
    clip.json and find_assets() resolves it at scan time.

    When *copy_source* is True, image files are copied into the clip's
    Frames/ directory.

    When *specific_files* is provided, only those files (basenames) from
    *source_folder* are used. This forces copy_source=True since we can't
    reference a subset of a directory.

    Args:
        clips_dir: Parent clips directory (e.g., project/clips/).
        source_folder: Absolute path to the folder containing image files.
        copy_source: Whether to copy images into the clip folder.
        specific_files: Optional list of specific filenames to import.
        display_name: Optional display name for the clip.

    Returns:
        The clip folder name (not full path).
    """
    # Derive clip name from folder name or display_name
    if display_name and display_name.strip():
        clean = display_name.strip()
        clip_name = re.sub(r"[^\w\-]", "_", clean)
        clip_name = re.sub(r"_+", "_", clip_name).strip("_")[:60]
    else:
        clip_name = sanitize_stem(os.path.basename(source_folder))

    # Deduplicate clip folder names within same project
    clip_dir = os.path.join(clips_dir, clip_name)
    if os.path.exists(clip_dir):
        for i in range(2, 100):
            candidate = os.path.join(clips_dir, f"{clip_name}_{i}")
            if not os.path.exists(candidate):
                clip_dir = candidate
                clip_name = f"{clip_name}_{i}"
                break

    os.makedirs(clip_dir, exist_ok=True)

    # If specific files requested, always copy (can't reference a subset)
    if specific_files:
        copy_source = True

    if copy_source:
        frames_dir = os.path.join(clip_dir, "Frames")
        os.makedirs(frames_dir, exist_ok=True)
        files_to_copy = specific_files or [
            f for f in os.listdir(source_folder)
            if os.path.isfile(os.path.join(source_folder, f)) and is_image_file(f)
        ]
        for f in files_to_copy:
            src = os.path.join(source_folder, f)
            dst = os.path.join(frames_dir, f)
            if os.path.isfile(src) and not os.path.isfile(dst):
                shutil.copy2(src, dst)
        logger.info(
            f"Copied {len(files_to_copy)} frame(s) from {source_folder} -> {frames_dir}"
        )
    else:
        logger.info(f"Referencing image sequence in place: {source_folder}")

    # Write clip.json with sequence source metadata
    clip_display = display_name or os.path.basename(source_folder).replace("_", " ")
    write_clip_json(clip_dir, {
        "source": {
            "type": "sequence",
            "original_path": os.path.abspath(source_folder),
            "copied": copy_source,
        },
        "display_name": clip_display,
    })

    # Companion hint folders/videos next to the source folder: recorded, never copied.
    hints = _find_companion_hints(source_folder)
    if hints:
        data = read_clip_json(clip_dir) or {}
        for kind, key in (("video", "hint_movies"), ("sequence", "hint_sequences")):
            refs = {k: v["path"] for k, v in hints.items() if v["kind"] == kind}
            if refs:
                data["source"][key] = refs
        write_clip_json(clip_dir, data)

    return clip_name


def add_sequences_to_project(
    project_dir: str,
    sequence_folders: list[str],
    *,
    copy_source: bool = False,
) -> list[str]:
    """Add image sequence clips to an existing project.

    Args:
        project_dir: Absolute path to the project folder.
        sequence_folders: List of folder paths containing image sequences.
        copy_source: Whether to copy images into clip folders.

    Returns:
        List of new clip subfolder paths (absolute).
    """
    clips_dir = os.path.join(project_dir, "clips")
    os.makedirs(clips_dir, exist_ok=True)

    new_paths: list[str] = []
    for folder in sequence_folders:
        clip_name = create_clip_from_sequence(
            clips_dir, folder, copy_source=copy_source,
        )
        new_paths.append(os.path.join(clips_dir, clip_name))

    # Update project.json clips list + clear from removed_clips if re-imported
    data = read_project_json(project_dir) or {}
    existing = data.get("clips", [])
    removed = set(data.get("removed_clips", []))
    for p in new_paths:
        name = os.path.basename(p)
        existing.append(name)
        removed.discard(name)
    data["clips"] = existing
    data["removed_clips"] = sorted(removed)
    write_project_json(project_dir, data)

    return new_paths
