"""Cancellable, resumable movie-to-sequence normalization for local projects."""
from __future__ import annotations

import hashlib
import json
import math
import os
import re
import shutil
import tempfile
from contextlib import contextmanager
from dataclasses import dataclass
from fractions import Fraction
from typing import Callable

from ..frame_io import cv2, imread_unicode
from ..ffmpeg_tools.extraction import extract_frames
from ..ffmpeg_tools.metadata import (
    build_video_metadata,
    read_video_metadata,
)
from ..ffmpeg_tools.probe import (
    probe_video,
    probe_video_timestamps,
    validate_constant_frame_rate_timestamps,
)

_FRAME_RE = re.compile(r"^frame_(\d+)\.exr$", re.IGNORECASE)
_IMAGE_EXTS = {".exr", ".png", ".jpg", ".jpeg", ".tif", ".tiff", ".bmp"}
_MARKER = ".source_normalization.json"
_MARKER_VERSION = 2
_HASH_CHUNK = 1024 * 1024
_LOCK_NAME = ".source_normalization.lock"


class SourceNormalizationError(RuntimeError):
    """The movie cannot safely be published as a canonical frame sequence."""


class _SourceChangedDuringNormalization(SourceNormalizationError):
    pass


class _NormalizationCancelled(SourceNormalizationError):
    pass


@dataclass(frozen=True)
class NormalizedSource:
    source_path: str
    frames_dir: str
    frame_paths: tuple[str, ...]
    frame_stems: tuple[str, ...]
    frame_count: int
    fps: float
    width: int
    height: int
    source_sha256: str


def _is_cancelled(cancel_event) -> bool:
    return cancel_event is not None and cancel_event.is_set()


def _progress(callback, phase: str, current: int, total: int) -> None:
    if callback is not None:
        callback(phase, current, total)


def _source_fingerprint(source_path: str, cancel_event, on_progress) -> str | None:
    total = os.path.getsize(source_path)
    digest = hashlib.sha256()
    read = 0
    with open(source_path, "rb") as source:
        while True:
            if _is_cancelled(cancel_event):
                return None
            block = source.read(_HASH_CHUNK)
            if not block:
                break
            digest.update(block)
            read += len(block)
            _progress(on_progress, "fingerprint", read, total)
    return digest.hexdigest()


def _stat_identity(stat_result) -> tuple[int, int, int, int, int]:
    return (
        stat_result.st_dev,
        stat_result.st_ino,
        stat_result.st_size,
        stat_result.st_mtime_ns,
        stat_result.st_ctime_ns,
    )


def _assert_source_unchanged(source_path: str, initial_stat, fingerprint: str,
                             cancel_event, on_progress) -> None:
    before = os.stat(source_path)
    current = _source_fingerprint(source_path, cancel_event, on_progress)
    after = os.stat(source_path)
    if current is None:
        raise _NormalizationCancelled("Normalization was cancelled while confirming movie identity.")
    if (_stat_identity(before) != _stat_identity(after)
            or _stat_identity(initial_stat) != _stat_identity(after)
            or current != fingerprint):
        raise _SourceChangedDuringNormalization("Movie changed during normalization; staged frames remain hidden.")


def _frame_rate(info: dict) -> Fraction:
    try:
        rate = Fraction(str(info.get("r_frame_rate", "")))
    except (ValueError, ZeroDivisionError):
        rate = Fraction(0, 1)
    if rate <= 0:
        raise SourceNormalizationError(
            "Cannot verify this movie's frame rate; a positive constant frame rate is required."
        )
    return rate


def _discard_unverified_staging(staging_dir: str, reason: str) -> None:
    marker_path = os.path.join(staging_dir, _MARKER)
    marker = _read_marker(staging_dir) or {}
    marker.update({"version": _MARKER_VERSION, "complete": False, "resume_invalid": True})
    _atomic_json(marker_path, marker)
    try:
        shutil.rmtree(staging_dir)
    except OSError as exc:
        raise SourceNormalizationError(
            f"{reason} and staged frames could not be discarded; "
            "the hidden staging folder is invalidated and must be removed before retrying."
        ) from exc


def _atomic_json(path: str, value: dict) -> None:
    fd, temporary = tempfile.mkstemp(prefix=os.path.basename(path) + ".", suffix=".tmp", dir=os.path.dirname(path))
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            json.dump(value, handle, indent=2, ensure_ascii=False)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
    finally:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


@contextmanager
def _clip_lock(clip_root: str):
    lock_path = os.path.join(clip_root, _LOCK_NAME)
    handle = open(lock_path, "a+b")
    try:
        if os.name == "nt":
            import msvcrt

            handle.seek(0, os.SEEK_END)
            if handle.tell() == 0:
                handle.write(b"\0")
                handle.flush()
            handle.seek(0)
            try:
                msvcrt.locking(handle.fileno(), msvcrt.LK_NBLCK, 1)
            except OSError as exc:
                raise SourceNormalizationError("Another normalization owns this clip folder.") from exc
        else:
            import fcntl

            try:
                fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError as exc:
                raise SourceNormalizationError("Another normalization owns this clip folder.") from exc
        yield
    finally:
        try:
            if os.name == "nt":
                import msvcrt

                handle.seek(0)
                msvcrt.locking(handle.fileno(), msvcrt.LK_UNLCK, 1)
            else:
                import fcntl

                fcntl.flock(handle.fileno(), fcntl.LOCK_UN)
        except OSError:
            pass
        handle.close()


def _read_marker(directory: str) -> dict | None:
    path = os.path.join(directory, _MARKER)
    if not os.path.isfile(path):
        return None
    try:
        with open(path, "r", encoding="utf-8") as handle:
            marker = json.load(handle)
    except (OSError, json.JSONDecodeError) as exc:
        raise SourceNormalizationError(
            f"Normalization marker is unreadable at '{path}'; existing files were left untouched."
        ) from exc
    if not isinstance(marker, dict) or marker.get("version") != _MARKER_VERSION:
        raise SourceNormalizationError(
            f"Unsupported normalization marker at '{path}'; existing files were left untouched."
        )
    return marker


def _merge_dicts(existing: dict, update: dict) -> dict:
    merged = dict(existing)
    for key, value in update.items():
        if isinstance(merged.get(key), dict) and isinstance(value, dict):
            merged[key] = _merge_dicts(merged[key], value)
        else:
            merged[key] = value
    return merged


def _frame_paths(directory: str, width: int, height: int, expected_count: int | None,
                 cancel_event, on_progress) -> tuple[str, ...] | None:
    names = sorted(
        name for name in os.listdir(directory)
        if os.path.splitext(name)[1].lower() in _IMAGE_EXTS
    )
    if not names:
        raise SourceNormalizationError("Extraction produced no image frames.")
    indexed: list[tuple[int, str]] = []
    for name in names:
        match = _FRAME_RE.fullmatch(name)
        if not match:
            raise SourceNormalizationError(
                f"Unexpected image '{name}' in the staged sequence; expected frame_NNNNNN.exr files."
            )
        indexed.append((int(match.group(1)), name))
    indexed.sort()
    if [index for index, _ in indexed] != list(range(len(indexed))):
        raise SourceNormalizationError("Extracted frame numbers are missing, duplicated, or do not start at zero.")
    if expected_count is not None and len(indexed) != expected_count:
        raise SourceNormalizationError(
            f"Extracted {len(indexed)} frames, but FFprobe reports {expected_count}."
        )
    paths = []
    for position, (_index, name) in enumerate(indexed, 1):
        if _is_cancelled(cancel_event):
            return None
        path = os.path.join(directory, name)
        image = imread_unicode(path, cv2.IMREAD_UNCHANGED)
        if image is None:
            raise SourceNormalizationError(f"Could not decode extracted frame '{name}'.")
        if image.shape[1] != width or image.shape[0] != height:
            raise SourceNormalizationError(
                f"Frame '{name}' is {image.shape[1]}x{image.shape[0]}; "
                f"the movie probe reported {width}x{height}."
            )
        paths.append(path)
        _progress(on_progress, "validate", position, len(indexed))
    return tuple(paths)


def _read_old_metadata(clip_root: str) -> dict:
    path = os.path.join(clip_root, ".video_metadata.json")
    if os.path.isfile(path):
        metadata = read_video_metadata(clip_root)
        if metadata is None:
            raise SourceNormalizationError(
                f"Existing video metadata is unreadable at '{path}'; it was left untouched."
            )
        return metadata
    return {}


def _publish_metadata(clip_root: str, metadata: dict) -> None:
    path = os.path.join(clip_root, ".video_metadata.json")
    _atomic_json(path, _merge_dicts(_read_old_metadata(clip_root), metadata))


def _result(source_path: str, frames_dir: str, frame_paths: tuple[str, ...],
            metadata: dict, fingerprint: str) -> NormalizedSource:
    return NormalizedSource(
        source_path=source_path,
        frames_dir=frames_dir,
        frame_paths=frame_paths,
        frame_stems=tuple(os.path.splitext(os.path.basename(path))[0] for path in frame_paths),
        frame_count=len(frame_paths),
        fps=float(metadata["fps"]),
        width=int(metadata["width"]),
        height=int(metadata["height"]),
        source_sha256=fingerprint,
    )


def _normalize_source_movie_locked(
    source_path: str,
    clip_root: str,
    *,
    cancel_event=None,
    on_progress: Callable[[str, int, int], None] | None = None,
) -> NormalizedSource | None:
    """Normalize a CFR movie into ``clip_root/Frames`` without exposing partial frames.

    Cancellation returns ``None`` and keeps resumable data under a hidden,
    source-fingerprinted staging directory. A populated destination is never
    replaced; a completed destination from this API is validated and reused.
    Progress phases are ``fingerprint``, ``probe``, ``extract``, ``compress`` and ``validate``.
    """
    source_path = os.path.realpath(os.path.abspath(os.fspath(source_path)))
    clip_root = os.path.abspath(os.fspath(clip_root))
    frames_dir = os.path.join(clip_root, "Frames")
    if not os.path.isfile(source_path):
        raise SourceNormalizationError(f"Movie source is not a file: '{source_path}'.")
    if not os.path.isdir(clip_root):
        raise SourceNormalizationError(f"Clip folder does not exist: '{clip_root}'.")
    if _is_cancelled(cancel_event):
        return None

    initial_stat = os.stat(source_path)
    info = probe_video(source_path)
    rate = _frame_rate(info)
    width, height = int(info.get("width", 0)), int(info.get("height", 0))
    if width <= 0 or height <= 0:
        raise SourceNormalizationError("Movie probe reported invalid frame geometry.")
    try:
        probed_fps = float(info.get("fps", 0))
    except (TypeError, ValueError):
        probed_fps = 0.0
    if not math.isfinite(probed_fps) or probed_fps <= 0:
        raise SourceNormalizationError("Movie probe reported an invalid frame rate.")
    timeline = probe_video_timestamps(
        source_path,
        int(info.get("stream_index", 0)),
        info.get("time_base", ""),
        cancel_event=cancel_event,
        on_progress=lambda current, total: _progress(on_progress, "probe", current, total),
    )
    if timeline is None:
        return None
    if timeline.stream_index != int(info.get("stream_index", 0)):
        raise SourceNormalizationError("FFprobe timestamp stream does not match the selected movie stream.")
    try:
        validate_constant_frame_rate_timestamps(rate, timeline.time_base, timeline.pts)
    except ValueError as exc:
        raise SourceNormalizationError(
            "Variable or unsupported frame timing detected; only constant-frame-rate movies can be normalized."
        ) from exc
    exact_count = len(timeline.pts)
    if _is_cancelled(cancel_event):
        return None

    fingerprint = _source_fingerprint(source_path, cancel_event, on_progress)
    if fingerprint is None:
        return None
    if _stat_identity(initial_stat) != _stat_identity(os.stat(source_path)):
        raise SourceNormalizationError("Movie changed while it was being probed; no frames were published.")
    _read_old_metadata(clip_root)  # fail before extraction rather than replace unreadable sidecar data
    staging_dir = os.path.join(clip_root, f".Frames.normalizing-{fingerprint[:16]}")

    if os.path.lexists(frames_dir):
        if os.path.islink(frames_dir) or not os.path.isdir(frames_dir):
            raise SourceNormalizationError("Frames/ exists but is not a regular directory; it was left untouched.")
        marker = _read_marker(frames_dir)
        if (marker and marker.get("complete") is True
                and marker.get("source_sha256") == fingerprint
                and marker.get("source_path") == source_path):
            metadata = marker.get("metadata")
            if not isinstance(metadata, dict):
                raise SourceNormalizationError("Completed Frames/ has invalid normalization metadata.")
            expected = int(metadata["frame_count"]) if metadata.get("frame_count") else None
            paths = _frame_paths(frames_dir, width, height, expected, cancel_event, on_progress)
            if paths is None:
                return None
            if _is_cancelled(cancel_event):
                return None
            _assert_source_unchanged(source_path, initial_stat, fingerprint, cancel_event, on_progress)
            _publish_metadata(clip_root, metadata)
            return _result(source_path, frames_dir, paths, metadata, fingerprint)
        if os.listdir(frames_dir):
            raise SourceNormalizationError(
                "Frames/ already contains files without a matching source identity; "
                "they were left untouched. Move or relink them explicitly before extraction."
            )

    if os.path.islink(staging_dir) or (os.path.lexists(staging_dir) and not os.path.isdir(staging_dir)):
        raise SourceNormalizationError("The hidden normalization staging path is not a regular directory.")
    if os.path.isdir(staging_dir):
        marker = _read_marker(staging_dir)
        if marker and marker.get("resume_invalid") is True:
            raise SourceNormalizationError(
                "Hidden staging data was invalidated after a source change; remove the staging folder before retrying."
            )
        if marker is None:
            names = set(os.listdir(staging_dir))
            marker_tmp = os.path.join(staging_dir, _MARKER + ".tmp")
            if names == {_MARKER + ".tmp"}:
                os.remove(marker_tmp)  # interrupted atomic marker write; FFmpeg has not started yet
                names.clear()
            if names:
                raise SourceNormalizationError(
                    "Unrecognized files exist in the hidden normalization staging folder; they were left untouched."
                )
        if marker and (marker.get("source_sha256") != fingerprint or marker.get("source_path") != source_path):
            raise SourceNormalizationError("Hidden staging data belongs to a different movie; it was left untouched.")
        if marker:
            metadata = marker.get("metadata")
            saved_identity = marker.get("source_stat_identity")
            if saved_identity is None and isinstance(metadata, dict):
                saved_identity = metadata.get("source_stat_identity")
            if saved_identity != list(_stat_identity(initial_stat)):
                _discard_unverified_staging(
                    staging_dir,
                    "Movie stat identity no longer matches hidden staging data",
                )
    if not os.path.isdir(staging_dir):
        os.mkdir(staging_dir)

    _atomic_json(os.path.join(staging_dir, _MARKER), {
        "version": _MARKER_VERSION,
        "complete": False,
        "source_path": source_path,
        "source_sha256": fingerprint,
        "source_stat_identity": list(_stat_identity(initial_stat)),
        "fps": float(rate),
        "width": width,
        "height": height,
        "time_base": info.get("time_base", ""),
        "stream_index": int(info.get("stream_index", 0)),
        "extractor_revision": 2,
    })
    if _is_cancelled(cancel_event):
        return None

    extracted = extract_frames(
        source_path,
        staging_dir,
        on_progress=lambda current, total: _progress(on_progress, "extract", current, total),
        on_recompress_progress=lambda current, total: _progress(on_progress, "compress", current, total),
        cancel_event=cancel_event,
        total_frames=exact_count,
        stream_index=int(info.get("stream_index", 0)),
        expected_frame_count=exact_count,
    )
    if _is_cancelled(cancel_event):
        return None
    if extracted <= 0 or not os.path.isfile(os.path.join(staging_dir, ".dwab_done")):
        raise SourceNormalizationError("Extraction did not complete; resumable frames remain hidden.")

    try:
        paths = _frame_paths(staging_dir, width, height, exact_count, cancel_event, on_progress)
        if paths is not None and not _is_cancelled(cancel_event):
            if len(paths) != extracted:
                raise SourceNormalizationError(
                    f"Extractor reported {extracted} frames but the staged sequence contains {len(paths)}."
                )
            _assert_source_unchanged(source_path, initial_stat, fingerprint, cancel_event, on_progress)
    except BaseException as exc:
        if isinstance(exc, _SourceChangedDuringNormalization):
            _discard_unverified_staging(staging_dir, "Movie changed during normalization")
            raise
        if isinstance(exc, _NormalizationCancelled):
            _discard_unverified_staging(staging_dir, "Normalization was cancelled during source identity validation")
            return None
        try:
            os.remove(os.path.join(staging_dir, ".dwab_done"))
        except FileNotFoundError:
            pass
        raise
    if paths is None:
        try:
            os.remove(os.path.join(staging_dir, ".dwab_done"))
        except FileNotFoundError:
            pass
        return None
    if _is_cancelled(cancel_event):
        try:
            os.remove(os.path.join(staging_dir, ".dwab_done"))
        except FileNotFoundError:
            pass
        return None

    metadata = build_video_metadata(source_path, {**info, "fps": float(rate)}, len(paths))
    metadata["source_sha256"] = fingerprint
    metadata["source_stat_identity"] = list(_stat_identity(initial_stat))
    metadata["frame_rate_validation"] = "per_frame_presentation_timestamps"
    metadata["source_probe"].update({
        "frame_count": exact_count,
        "r_frame_rate": info.get("r_frame_rate", ""),
        "avg_frame_rate": info.get("avg_frame_rate", ""),
        "time_base": info.get("time_base", ""),
        "frame_count_exact": True,
        "stream_index": int(info.get("stream_index", 0)),
        "timestamp_count": exact_count,
    })
    _atomic_json(os.path.join(staging_dir, _MARKER), {
        "version": _MARKER_VERSION,
        "complete": True,
        "source_path": source_path,
        "source_sha256": fingerprint,
        "metadata": metadata,
    })

    if _is_cancelled(cancel_event):
        return None
    try:
        _assert_source_unchanged(source_path, initial_stat, fingerprint, cancel_event, on_progress)
    except BaseException as exc:
        if isinstance(exc, _SourceChangedDuringNormalization):
            _discard_unverified_staging(staging_dir, "Movie changed during normalization")
            raise
        if isinstance(exc, _NormalizationCancelled):
            _discard_unverified_staging(staging_dir, "Normalization was cancelled during source identity validation")
            return None
        _discard_unverified_staging(staging_dir, "Source identity validation failed")
        raise
    if os.path.isdir(frames_dir):
        # Only an empty placeholder may be removed; a concurrent writer wins safely if it populated it.
        try:
            os.rmdir(frames_dir)
        except OSError as exc:
            raise SourceNormalizationError("Frames/ appeared during extraction; staged files remain hidden.") from exc
    try:
        os.rename(staging_dir, frames_dir)
    except OSError as exc:
        raise SourceNormalizationError(f"Could not publish normalized Frames/: {exc}") from exc
    _publish_metadata(clip_root, metadata)
    paths = tuple(os.path.join(frames_dir, os.path.basename(path)) for path in paths)
    return _result(source_path, frames_dir, paths, metadata, fingerprint)


def normalize_source_movie(
    source_path: str,
    clip_root: str,
    *,
    cancel_event=None,
    on_progress: Callable[[str, int, int], None] | None = None,
) -> NormalizedSource | None:
    """Normalize a movie while holding exclusive ownership of its clip folder."""
    clip_root = os.path.abspath(os.fspath(clip_root))
    if not os.path.isdir(clip_root):
        raise SourceNormalizationError(f"Clip folder does not exist: '{clip_root}'.")
    with _clip_lock(clip_root):
        return _normalize_source_movie_locked(
            source_path,
            clip_root,
            cancel_event=cancel_event,
            on_progress=on_progress,
        )
