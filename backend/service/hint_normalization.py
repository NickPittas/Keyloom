"""Normalize companion matte/mask movies without passing samples through RGB."""
from __future__ import annotations

import ctypes
import hashlib
import json
import math
import os
import re
import stat
import subprocess
import tempfile
import time
from contextlib import contextmanager
from dataclasses import dataclass
from fractions import Fraction
from typing import Callable, Literal

from ..ffmpeg_tools.discovery import find_ffmpeg, find_ffprobe
from ..ffmpeg_tools.probe import (
    probe_video_timestamps,
    stop_process_tree,
    validate_constant_frame_rate_timestamps,
)
from ..frame_io import cv2, imread_unicode
from .source_normalization import NormalizedSource

_SOURCE_MARKER = ".source_normalization.json"
_HINT_MARKER = ".hint_normalization.json"
_VERSION = 1
_HASH_CHUNK = 1024 * 1024
_FRAME_RE = re.compile(r"^frame_(\d+)\.png$", re.IGNORECASE)
_GRAY_PIX_FMTS = {
    "gray", "yuv420p", "yuv422p", "yuv444p",
    "yuvj411p", "yuvj420p", "yuvj422p", "yuvj444p", "yuvj440p",
}
_ALPHA_PIX_FMTS = {
    "rgba", "bgra", "argb", "abgr", "ya8", "yuva420p",
    "yuva422p", "yuva444p", "gbrap",
}
_DESTINATIONS = {"alpha": "AlphaHint", "mask": "VideoMamaMaskHint"}


class HintNormalizationError(RuntimeError):
    """The hint movie cannot safely become a source-aligned image sequence."""


@dataclass(frozen=True)
class NormalizedHint:
    kind: str
    channel: str
    hint_path: str
    frames_dir: str
    frame_paths: tuple[str, ...]
    frame_stems: tuple[str, ...]
    frame_count: int
    fps: float
    width: int
    height: int
    source_sha256: str
    hint_sha256: str


def _cancelled(cancel_event) -> bool:
    return cancel_event is not None and cancel_event.is_set()


def _progress(callback, phase: str, current: int, total: int) -> None:
    if callback is not None:
        callback(phase, current, total)


def _fingerprint(path: str, cancel_event, on_progress) -> str | None:
    digest = hashlib.sha256()
    read = 0
    try:
        total = os.path.getsize(path)
        with open(path, "rb") as source:
            while True:
                if _cancelled(cancel_event):
                    return None
                block = source.read(_HASH_CHUNK)
                if not block:
                    break
                digest.update(block)
                read += len(block)
                _progress(on_progress, "fingerprint", read, total)
    except OSError as exc:
        raise HintNormalizationError(f"Could not fingerprint '{path}': {exc}") from exc
    return digest.hexdigest()


def _stat_identity(path: str) -> tuple[int, int, int, int, int]:
    try:
        value = os.stat(path)
    except OSError as exc:
        raise HintNormalizationError(f"Could not inspect input identity for '{path}': {exc}") from exc
    return value.st_dev, value.st_ino, value.st_size, value.st_mtime_ns, value.st_ctime_ns


def _assert_stat_identity(path: str, expected, label: str) -> None:
    if _stat_identity(path) != tuple(expected):
        raise HintNormalizationError(f"{label} changed during hint normalization (stat identity changed).")


def _read_json(path: str, label: str) -> dict:
    try:
        with open(path, "r", encoding="utf-8") as handle:
            value = json.load(handle)
    except (OSError, json.JSONDecodeError) as exc:
        raise HintNormalizationError(f"{label} is unreadable at '{path}'; files were left untouched.") from exc
    if not isinstance(value, dict):
        raise HintNormalizationError(f"{label} is invalid at '{path}'; files were left untouched.")
    return value


def _write_marker(path: str, value: dict) -> None:
    fd, temporary = tempfile.mkstemp(prefix=os.path.basename(path) + ".", suffix=".tmp",
                                     dir=os.path.dirname(path))
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


def _fraction(value, label: str) -> Fraction:
    try:
        result = Fraction(str(value))
    except (ValueError, ZeroDivisionError, TypeError) as exc:
        raise HintNormalizationError(f"Cannot verify {label} '{value}'.") from exc
    if result <= 0:
        raise HintNormalizationError(f"Cannot verify positive {label} '{value}'.")
    return result


def _probe(path: str, label: str) -> dict:
    ffprobe = find_ffprobe()
    if not ffprobe:
        raise HintNormalizationError("FFprobe is required to verify hint movie metadata.")
    command = [
        ffprobe, "-v", "error", "-select_streams", "v:0", "-show_entries",
        "stream=index,width,height,r_frame_rate,avg_frame_rate,time_base,pix_fmt,color_range",
        "-of", "json", path,
    ]
    try:
        result = subprocess.run(
            command, stdin=subprocess.DEVNULL, capture_output=True, text=True,
            encoding="utf-8", errors="replace", timeout=15,
            creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0,
        )
        if result.returncode:
            raise HintNormalizationError(
                f"FFprobe could not read {label}: {result.stderr[-1000:]}"
            )
        data = json.loads(result.stdout)
        streams = data.get("streams", []) if isinstance(data, dict) else []
    except (OSError, subprocess.SubprocessError, json.JSONDecodeError) as exc:
        raise HintNormalizationError(f"Could not probe {label}: {exc}") from exc
    if not isinstance(streams, list) or len(streams) != 1 or not isinstance(streams[0], dict):
        raise HintNormalizationError(f"FFprobe returned invalid metadata for {label}.")
    return streams[0]


def _timeline(path: str, label: str, count: int, cancel_event, info: dict, on_progress=None):
    rate = _fraction(info.get("r_frame_rate"), f"{label} frame rate")
    average_rate = _fraction(info.get("avg_frame_rate"), f"{label} average frame rate")
    if average_rate != rate:
        raise HintNormalizationError(f"{label} advertised frame rates are inconsistent.")
    time_base = _fraction(info.get("time_base"), f"{label} time base")
    if int(info.get("width", 0) or 0) <= 0 or int(info.get("height", 0) or 0) <= 0:
        raise HintNormalizationError(f"FFprobe reported invalid geometry for {label}.")
    stream_index = int(info.get("index", -1))
    if stream_index < 0:
        raise HintNormalizationError(f"FFprobe did not identify the selected video stream for {label}.")
    try:
        timeline = probe_video_timestamps(
            path, stream_index, time_base,
            cancel_event=cancel_event,
            on_progress=lambda current, total: _progress(on_progress, "probe", current, total),
        )
    except (RuntimeError, OSError, ValueError, subprocess.SubprocessError) as exc:
        raise HintNormalizationError(f"Could not validate frame timestamps for {label}: {exc}") from exc
    if timeline is None:
        return None
    if timeline.stream_index != stream_index:
        raise HintNormalizationError(f"{label} timestamp stream does not match the selected video stream.")
    if timeline.time_base != time_base:
        raise HintNormalizationError(f"{label} timestamp time base changed while it was being inspected.")
    if len(timeline.pts) != count:
        raise HintNormalizationError(f"{label} has {len(timeline.pts)} decoded frames; canonical source has {count}.")
    try:
        validate_constant_frame_rate_timestamps(rate, timeline.time_base, timeline.pts)
    except ValueError as exc:
        raise HintNormalizationError(f"{label} has variable or ambiguous frame timestamps: {exc}") from exc
    return rate, timeline.time_base, tuple(value * timeline.time_base for value in timeline.pts)


def _source_contract(source: NormalizedSource) -> tuple[dict, tuple[str, ...], dict]:
    frame_count = int(source.frame_count)
    frames_dir = os.path.realpath(os.path.abspath(os.fspath(source.frames_dir)))
    if frame_count <= 0 or len(source.frame_paths) != frame_count or len(source.frame_stems) != frame_count:
        raise HintNormalizationError("Canonical source frame count, paths, and stems do not agree.")
    expected_stems = tuple(f"frame_{index:06d}" for index in range(frame_count))
    if tuple(source.frame_stems) != expected_stems:
        raise HintNormalizationError("Canonical source stems are not a complete zero-based frame sequence.")
    for stem, path in zip(expected_stems, source.frame_paths):
        if (os.path.realpath(os.path.dirname(path)) != frames_dir
                or os.path.splitext(os.path.basename(path))[0] != stem
                or not os.path.isfile(path)):
            raise HintNormalizationError(f"Canonical source frame '{stem}' is missing or has a different name.")

    marker = _read_json(os.path.join(frames_dir, _SOURCE_MARKER), "Canonical source marker")
    metadata = marker.get("metadata")
    if (set(marker) != {"version", "complete", "source_path", "source_sha256", "metadata"}
            or marker.get("version") != 2 or marker.get("complete") is not True
            or marker.get("source_sha256") != source.source_sha256
            or marker.get("source_path") != os.path.realpath(os.path.abspath(os.fspath(source.source_path)))
            or not isinstance(metadata, dict)):
        raise HintNormalizationError("Canonical source marker does not match the supplied normalized source.")
    probe = metadata.get("source_probe")
    expected_metadata_keys = {
        "source_path", "fps", "width", "height", "frame_count", "codec", "duration",
        "exr_vf", "source_probe", "source_sha256", "frame_rate_validation",
    }
    if (set(metadata) not in (expected_metadata_keys, expected_metadata_keys | {"source_stat_identity"})
            or not isinstance(probe, dict)):
        raise HintNormalizationError("Canonical source marker lacks exact frame-rate/time-base metadata.")
    if (metadata.get("frame_rate_validation") != "per_frame_presentation_timestamps"
            or metadata.get("source_sha256") != source.source_sha256
            or metadata.get("source_path") != marker.get("source_path")
            or int(probe.get("timestamp_count", 0) or 0) != frame_count
            or probe.get("frame_count_exact") is not True
            or int(probe.get("stream_index", -1)) < 0
            or not probe.get("time_base") or not probe.get("r_frame_rate")
            or not probe.get("avg_frame_rate")):
        raise HintNormalizationError("Canonical source marker lacks the v2 per-frame timestamp validation contract.")
    if (int(metadata.get("frame_count", 0) or 0) != frame_count
            or int(metadata.get("width", 0) or 0) != int(source.width)
            or int(metadata.get("height", 0) or 0) != int(source.height)):
        raise HintNormalizationError("Canonical source marker geometry/count disagrees with the source result.")
    if not math.isclose(float(metadata.get("fps", 0)), float(source.fps), rel_tol=0, abs_tol=1e-6):
        raise HintNormalizationError("Canonical source marker frame rate disagrees with the source result.")
    return marker, expected_stems, probe


def _extract_filter(channel: str, pix_fmt: str, color_range: str = "") -> str:
    pix_fmt = (pix_fmt or "").lower()
    if channel == "gray":
        if pix_fmt not in _GRAY_PIX_FMTS:
            raise HintNormalizationError(
                f"Gray/luma extraction does not support pixel format '{pix_fmt}'; "
                "RGB display conversion and non-8-bit rescaling are intentionally disabled."
            )
        range_value = (color_range or "").lower()
        if pix_fmt == "gray":
            if range_value in {"tv", "mpeg"}:
                return "format=gray,lut=y='(val-16)*255/219'"
            if range_value in {"pc", "jpeg"}:
                return "format=gray"
            raise HintNormalizationError("Gray pixel format has ambiguous color range; refusing to normalize matte values.")
        if pix_fmt.startswith("yuvj"):
            if range_value in {"tv", "mpeg"}:
                raise HintNormalizationError("YUVJ pixel format conflicts with limited-range metadata.")
            return "extractplanes=y"
        if range_value in {"tv", "mpeg"}:
            return "extractplanes=y,lut=y='(val-16)*255/219'"
        if range_value in {"pc", "jpeg"}:
            return "extractplanes=y"
        raise HintNormalizationError(
            f"YUV luma range '{color_range or 'unspecified'}' is ambiguous; refusing to normalize matte values."
        )
    if channel == "alpha":
        if pix_fmt not in _ALPHA_PIX_FMTS:
            raise HintNormalizationError(
                f"Alpha extraction needs an 8-bit alpha pixel format; got '{pix_fmt}'."
            )
        return "alphaextract"
    raise HintNormalizationError("channel must be exactly 'gray' or 'alpha'.")


def _staged_frames(frames_dir: str, stems: tuple[str, ...], width: int, height: int,
                   *, repair_partial: bool = False) -> int:
    if not os.path.isdir(frames_dir) or os.path.islink(frames_dir):
        raise HintNormalizationError("Hint frame staging path is not a regular directory.")
    expected = {f"{stem}.png": index for index, stem in enumerate(stems)}
    names = os.listdir(frames_dir)
    if any(name not in expected and name not in {_HINT_MARKER} for name in names):
        raise HintNormalizationError("Unexpected files exist in the hidden hint staging directory.")
    present = sorted((expected[name], name) for name in names if name in expected)
    if [index for index, _ in present] != list(range(len(present))):
        raise HintNormalizationError("Hidden hint staging frames are missing or out of order.")
    for position, (_index, name) in enumerate(present):
        path = os.path.join(frames_dir, name)
        if not stat.S_ISREG(os.lstat(path).st_mode):
            raise HintNormalizationError(f"Staged hint frame '{name}' is not a regular file.")
        image = imread_unicode(path, cv2.IMREAD_UNCHANGED)
        if image is None:
            if repair_partial and position == len(present) - 1:
                os.remove(path)  # A canceled FFmpeg write can leave only its final PNG incomplete.
                return position
            raise HintNormalizationError(f"Staged hint frame '{name}' is unreadable.")
        if image.ndim != 2 or image.dtype != "uint8" or image.shape != (height, width):
            raise HintNormalizationError(
                f"Staged hint frame '{name}' is not {width}x{height} single-channel 8-bit grayscale."
            )
    return len(present)


def _publish_writer_frames(writer_dir: str, staging: str, start: int, count: int,
                           width: int, height: int, *, allow_partial: bool) -> int:
    if os.path.islink(writer_dir) or not os.path.isdir(writer_dir):
        raise HintNormalizationError("Private hint writer directory is missing or unsafe.")
    names = os.listdir(writer_dir)
    indexed = {}
    for name in names:
        match = _FRAME_RE.fullmatch(name)
        if match is None:
            raise HintNormalizationError("Unexpected output exists in private hint writer directory.")
        index = int(match.group(1))
        if name != f"frame_{index:06d}.png" or index in indexed:
            raise HintNormalizationError("Private hint writer produced an ambiguous frame ordinal.")
        indexed[index] = name
    if any(index < start or index >= count for index in indexed):
        raise HintNormalizationError("Private hint writer produced an out-of-range frame ordinal.")
    if not allow_partial and set(indexed) != set(range(start, count)):
        raise HintNormalizationError(
            f"FFmpeg produced {len(indexed)} writer frames; expected {count - start}."
        )
    if indexed and sorted(indexed) != list(range(start, max(indexed) + 1)):
        raise HintNormalizationError("Private hint writer frames are missing or out of order.")

    next_index = start
    for index in range(start, count):
        name = indexed.get(index)
        if name is None:
            break
        path = os.path.join(writer_dir, name)
        if os.path.islink(path) or not stat.S_ISREG(os.lstat(path).st_mode):
            raise HintNormalizationError(f"Private writer frame '{name}' is not a regular file.")
        image = imread_unicode(path, cv2.IMREAD_UNCHANGED)
        if image is None or image.ndim != 2 or image.dtype != "uint8" or image.shape != (height, width):
            if allow_partial and index == max(indexed):
                os.unlink(path)
                break
            raise HintNormalizationError(f"Private writer frame '{name}' is incomplete or has invalid geometry.")
        target = os.path.join(staging, name)
        try:
            _rename_noreplace(path, target)
        except OSError as exc:
            raise HintNormalizationError(
                f"Could not publish validated frame '{name}' without replacement: {exc}."
            ) from exc
        next_index += 1
    if not allow_partial and next_index != count:
        raise HintNormalizationError(f"Published {next_index - start} writer frames; expected {count - start}.")
    return next_index


def _invalidate_staging(staging: str, identity: dict) -> None:
    if not os.path.isdir(staging) or os.path.islink(staging):
        return
    marker_path = os.path.join(staging, _HINT_MARKER)
    try:
        for name in os.listdir(staging):
            if _FRAME_RE.fullmatch(name):
                path = os.path.join(staging, name)
                if stat.S_ISDIR(os.lstat(path).st_mode):
                    raise OSError(f"unexpected directory at {path}")
                os.unlink(path)
        if os.path.islink(marker_path):
            os.unlink(marker_path)
        _write_marker(marker_path, {**identity, "complete": False})
    except OSError as exc:
        try:
            if os.path.islink(marker_path):
                os.unlink(marker_path)
            _write_marker(marker_path, {**identity, "complete": False, "resume_invalid": True})
        except OSError:
            pass
        raise HintNormalizationError(
            "Input changed during hint normalization and staged bytes could not be invalidated; "
            "the staging set is fail-closed and must not be resumed."
        ) from exc


def _terminate(process) -> None:
    stop_process_tree(process, timeout=3)


def _extract_frames(
    hint_path: str, frames_dir: str, *, filter_name: str,
    start: int, count: int, cancel_event, on_progress,
) -> bool:
    if start >= count:
        return True
    ffmpeg = find_ffmpeg()
    if not ffmpeg:
        raise HintNormalizationError("FFmpeg is required to normalize hint movies.")
    filter_graph = f"select=gte(n\\,{start}),{filter_name}"
    command = [
        ffmpeg, "-hide_banner", "-loglevel", "error", "-nostdin", "-n",
        "-i", hint_path, "-map", "0:v:0", "-vf", filter_graph,
        "-fps_mode", "passthrough", "-frames:v", str(count - start),
        "-start_number", str(start), "-f", "image2",
        os.path.join(frames_dir, "frame_%06d.png"),
    ]
    with tempfile.TemporaryFile() as stderr:
        try:
            process = subprocess.Popen(
                command, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=stderr,
                creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0,
                start_new_session=os.name != "nt",
            )
        except OSError as exc:
            raise HintNormalizationError(f"Could not start FFmpeg: {exc}") from exc
        try:
            last_count = -1
            while process.poll() is None:
                if _cancelled(cancel_event):
                    _terminate(process)
                    return False
                completed = sum(bool(_FRAME_RE.fullmatch(name)) for name in os.listdir(frames_dir))
                if completed != last_count:
                    _progress(on_progress, "extract", completed, count)
                    last_count = completed
                time.sleep(0.05)
        except BaseException:
            _terminate(process)
            raise
        return_code = process.returncode
        stderr.seek(0)
        error = stderr.read().decode("utf-8", errors="replace")[-2000:]
    if _cancelled(cancel_event):
        return False
    if return_code:
        raise HintNormalizationError(f"FFmpeg hint extraction failed ({return_code}): {error}")
    return True


@contextmanager
def _exclusive_lock(path: str):
    flags = os.O_CREAT | os.O_RDWR
    if os.name != "nt":
        flags |= getattr(os, "O_NOFOLLOW", 0)
    try:
        fd = os.open(path, flags, 0o600)
    except OSError as exc:
        raise HintNormalizationError(f"Cannot create normalization lock '{path}': {exc}") from exc
    try:
        if not stat.S_ISREG(os.fstat(fd).st_mode):
            raise HintNormalizationError("Normalization lock is not a regular file.")
        if os.name == "nt":
            import msvcrt

            if os.fstat(fd).st_size == 0:
                os.write(fd, b"\0")
            os.lseek(fd, 0, os.SEEK_SET)
            msvcrt.locking(fd, msvcrt.LK_NBLCK, 1)
        else:
            import fcntl

            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError as exc:
        os.close(fd)
        raise HintNormalizationError("Another normalization owns this source/hint staging path.") from exc
    except BaseException:
        os.close(fd)
        raise
    try:
        yield
    finally:
        if os.name == "nt":
            import msvcrt

            os.lseek(fd, 0, os.SEEK_SET)
            msvcrt.locking(fd, msvcrt.LK_UNLCK, 1)
        else:
            import fcntl

            fcntl.flock(fd, fcntl.LOCK_UN)
        os.close(fd)


def _rename_noreplace(source: str, destination: str) -> None:
    if os.name == "nt":
        os.rename(source, destination)  # Windows rename fails atomically when destination exists.
        return
    if os.name != "posix":
        raise HintNormalizationError("Atomic no-clobber directory publication is unsupported on this platform.")
    libc = ctypes.CDLL(None, use_errno=True)
    renameat2 = getattr(libc, "renameat2", None)
    if renameat2 is None:
        raise HintNormalizationError("Atomic no-clobber directory publication is unavailable on this platform.")
    renameat2.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
    renameat2.restype = ctypes.c_int
    if renameat2(-100, os.fsencode(source), -100, os.fsencode(destination), 1) == 0:
        return
    error = ctypes.get_errno()
    raise OSError(error, os.strerror(error), destination)


def _result(kind: str, channel: str, hint_path: str, frames_dir: str, stems: tuple[str, ...],
            fps: Fraction, width: int, height: int, source_sha256: str, hint_sha256: str) -> NormalizedHint:
    return NormalizedHint(
        kind=kind, channel=channel, hint_path=hint_path, frames_dir=frames_dir,
        frame_paths=tuple(os.path.join(frames_dir, f"{stem}.png") for stem in stems),
        frame_stems=stems, frame_count=len(stems), fps=float(fps), width=width, height=height,
        source_sha256=source_sha256, hint_sha256=hint_sha256,
    )


def _verify_input_identity(source_path: str, source_sha256: str, source_stat_identity,
                           hint_path: str, hint_sha256: str, hint_stat_identity,
                           cancel_event) -> bool:
    _assert_stat_identity(source_path, source_stat_identity, "Canonical source movie")
    actual_source = _fingerprint(source_path, cancel_event, None)
    if actual_source is None:
        return False
    _assert_stat_identity(source_path, source_stat_identity, "Canonical source movie")
    if actual_source != source_sha256:
        raise HintNormalizationError("Canonical source movie changed during hint normalization.")
    _assert_stat_identity(hint_path, hint_stat_identity, "Hint movie")
    actual_hint = _fingerprint(hint_path, cancel_event, None)
    if actual_hint is None:
        return False
    _assert_stat_identity(hint_path, hint_stat_identity, "Hint movie")
    if actual_hint != hint_sha256:
        raise HintNormalizationError("Hint movie changed during normalization; staged frames remain hidden.")
    _assert_stat_identity(source_path, source_stat_identity, "Canonical source movie")
    _assert_stat_identity(hint_path, hint_stat_identity, "Hint movie")
    return not _cancelled(cancel_event)


def _confirm_input_identity(source_path: str, source_sha256: str, hint_path: str,
                            hint_sha256: str, staging: str, identity: dict) -> None:
    try:
        _verify_input_identity(
            source_path, source_sha256, identity["source_stat_identity"],
            hint_path, hint_sha256, identity["hint_stat_identity"], None,
        )
    except HintNormalizationError:
        _invalidate_staging(staging, identity)
        raise


def _normalize_locked(
    *, destination: str, staging: str, identity: dict,
    source_path: str, source_sha256: str, hint_path: str, hint_sha256: str,
    kind: str, channel: str, stems: tuple[str, ...], source_rate: Fraction,
    width: int, height: int, filter_name: str, cancel_event, on_progress,
) -> NormalizedHint | None:
    if os.path.lexists(destination):
        if os.path.islink(destination) or not os.path.isdir(destination):
            raise HintNormalizationError(f"'{destination}' already exists and was left untouched.")
        marker_path = os.path.join(destination, _HINT_MARKER)
        if os.path.islink(marker_path):
            raise HintNormalizationError(f"Published hint marker '{marker_path}' is unsafe and was left untouched.")
        if os.path.isfile(marker_path):
            marker = _read_json(marker_path, "Published hint marker")
            if marker.get("complete") is True and all(marker.get(key) == value for key, value in identity.items()):
                count = _staged_frames(destination, stems, width, height)
                if count == len(stems) and _verify_input_identity(
                    source_path, source_sha256, identity["source_stat_identity"],
                    hint_path, hint_sha256, identity["hint_stat_identity"], cancel_event,
                ):
                    return _result(kind, channel, hint_path, destination, stems, source_rate,
                                   width, height, source_sha256, hint_sha256)
                if _cancelled(cancel_event):
                    return None
        raise HintNormalizationError(
            f"'{destination}' already contains a hint sequence; it was preserved and not replaced."
        )

    if os.path.islink(staging) or (os.path.lexists(staging) and not os.path.isdir(staging)):
        raise HintNormalizationError("Hidden hint staging path is not a regular directory.")
    if os.path.isdir(staging):
        marker_path = os.path.join(staging, _HINT_MARKER)
        marker_tmp = marker_path + ".tmp"
        if os.path.lexists(marker_tmp):
            if os.path.islink(marker_tmp) or not os.path.isfile(marker_tmp):
                raise HintNormalizationError("Hint staging marker temporary path is unsafe.")
            os.remove(marker_tmp)
        if os.path.islink(marker_path):
            raise HintNormalizationError("Hint staging marker is a symbolic link.")
        marker = _read_json(marker_path, "Hint staging marker") if os.path.isfile(marker_path) else None
        if marker is None:
            if os.listdir(staging):
                raise HintNormalizationError("Unrecognized files exist in hidden hint staging; they were left untouched.")
            marker = {**identity, "complete": False}
            _write_marker(marker_path, marker)
        elif marker.get("resume_invalid") is True:
            raise HintNormalizationError("Hint staging was invalidated after input mutation and cannot be resumed.")
        elif marker.get("version") != _VERSION:
            raise HintNormalizationError("Hidden hint staging belongs to a different source or hint movie.")
        else:
            stat_keys = {"source_stat_identity", "hint_stat_identity"}
            if not all(marker.get(key) == value for key, value in identity.items() if key not in stat_keys):
                raise HintNormalizationError("Hidden hint staging belongs to a different source or hint movie.")
            if any(marker.get(key) != identity[key] for key in stat_keys):
                _invalidate_staging(staging, identity)
                marker = {**identity, "complete": False}
    else:
        os.mkdir(staging)
        marker = {**identity, "complete": False}
        _write_marker(os.path.join(staging, _HINT_MARKER), marker)

    if _cancelled(cancel_event):
        _confirm_input_identity(source_path, source_sha256, hint_path, hint_sha256, staging, identity)
        return None
    completed = _staged_frames(staging, stems, width, height, repair_partial=True)
    if completed < len(stems):
        try:
            # FFmpeg never writes into resumable/public staging: image2 follows output symlinks.
            with tempfile.TemporaryDirectory(prefix=".hint-writer-", dir=os.path.dirname(staging)) as writer_dir:
                try:
                    extracted = _extract_frames(
                        hint_path, writer_dir, filter_name=filter_name,
                        start=completed, count=len(stems), cancel_event=cancel_event,
                        on_progress=on_progress,
                    )
                except BaseException:
                    _confirm_input_identity(
                        source_path, source_sha256, hint_path, hint_sha256, staging, identity,
                    )
                    _publish_writer_frames(
                        writer_dir, staging, completed, len(stems), width, height, allow_partial=True,
                    )
                    raise
                _confirm_input_identity(
                    source_path, source_sha256, hint_path, hint_sha256, staging, identity,
                )
                if extracted:
                    _publish_writer_frames(
                        writer_dir, staging, completed, len(stems), width, height, allow_partial=False,
                    )
                else:
                    _publish_writer_frames(
                        writer_dir, staging, completed, len(stems), width, height, allow_partial=True,
                    )
                    return None
        except OSError as exc:
            raise HintNormalizationError(f"Could not manage private hint writer staging: {exc}") from exc
    if _cancelled(cancel_event):
        _confirm_input_identity(source_path, source_sha256, hint_path, hint_sha256, staging, identity)
        return None
    completed = _staged_frames(staging, stems, width, height)
    if completed != len(stems):
        raise HintNormalizationError(f"FFmpeg produced {completed} hint frames; expected {len(stems)}.")
    _confirm_input_identity(source_path, source_sha256, hint_path, hint_sha256, staging, identity)
    marker.update({"complete": True})
    _write_marker(os.path.join(staging, _HINT_MARKER), marker)
    _confirm_input_identity(source_path, source_sha256, hint_path, hint_sha256, staging, identity)
    if _cancelled(cancel_event):
        marker.update({"complete": False})
        _write_marker(os.path.join(staging, _HINT_MARKER), marker)
        return None
    try:
        _rename_noreplace(staging, destination)
    except OSError as exc:
        raise HintNormalizationError(
            f"Could not publish hint sequence without replacing existing data: {exc}; staged files remain hidden."
        ) from exc
    return _result(kind, channel, hint_path, destination, stems, source_rate,
                   width, height, source_sha256, hint_sha256)


def normalize_hint_movie(
    hint_path: str,
    clip_root: str,
    source: NormalizedSource,
    *,
    kind: Literal["alpha", "mask"],
    channel: Literal["gray", "alpha"],
    cancel_event=None,
    on_progress: Callable[[str, int, int], None] | None = None,
) -> NormalizedHint | None:
    """Extract an aligned movie matte to a hidden, resumable, no-clobber sequence.

    ``gray`` accepts 8-bit gray or planar YUV with known range; limited range is
    expanded to full-range matte values. ``alpha`` copies supported 8-bit alpha
    planes unchanged. Other formats and ambiguous ranges are rejected.
    Cancellation returns ``None`` and retains only identity-verified staging frames.
    """
    if kind not in _DESTINATIONS:
        raise HintNormalizationError("kind must be exactly 'alpha' or 'mask'.")
    if channel not in {"gray", "alpha"}:
        raise HintNormalizationError("channel must be exactly 'gray' or 'alpha'.")
    hint_path = os.path.realpath(os.path.abspath(os.fspath(hint_path)))
    clip_root = os.path.realpath(os.path.abspath(os.fspath(clip_root)))
    if not os.path.isfile(hint_path):
        raise HintNormalizationError(f"Hint movie is not a file: '{hint_path}'.")
    if not os.path.isdir(clip_root):
        raise HintNormalizationError(f"Clip folder does not exist: '{clip_root}'.")
    if _cancelled(cancel_event):
        return None

    _source_marker, stems, source_probe = _source_contract(source)
    if os.path.realpath(os.path.abspath(os.fspath(source.frames_dir))) != os.path.join(clip_root, "Frames"):
        raise HintNormalizationError("Canonical source Frames/ does not belong to the requested clip folder.")
    source_path = os.path.realpath(os.path.abspath(os.fspath(source.source_path)))
    source_sha256 = source.source_sha256
    source_width, source_height = int(source.width), int(source.height)
    if source_width <= 0 or source_height <= 0:
        raise HintNormalizationError("Canonical source has invalid frame geometry.")
    source_initial_stat = _stat_identity(source_path)
    initial_hint_stat = _stat_identity(hint_path)
    actual_source_sha256 = _fingerprint(source_path, cancel_event, on_progress)
    if actual_source_sha256 is None:
        return None
    _assert_stat_identity(source_path, source_initial_stat, "Canonical source movie")
    if actual_source_sha256 != source_sha256:
        raise HintNormalizationError("Canonical source movie no longer matches the supplied normalized source.")
    hint_sha256 = _fingerprint(hint_path, cancel_event, on_progress)
    if hint_sha256 is None:
        return None
    _assert_stat_identity(source_path, source_initial_stat, "Canonical source movie")
    _assert_stat_identity(hint_path, initial_hint_stat, "Hint movie")
    if _cancelled(cancel_event):
        return None

    source_info = _probe(source_path, "canonical source movie")
    source_timeline = _timeline(
        source_path, "canonical source movie", len(stems), cancel_event, source_info, on_progress,
    )
    if source_timeline is None:
        return None
    source_rate, source_time_base, source_start = source_timeline
    recorded_rate = _fraction(source_probe.get("r_frame_rate"), "canonical source frame rate")
    recorded_average = _fraction(source_probe.get("avg_frame_rate"), "canonical source average frame rate")
    recorded_time_base = _fraction(source_probe.get("time_base"), "canonical source time base")
    if (recorded_rate != source_rate or recorded_average != source_rate
            or recorded_average != _fraction(source_info.get("avg_frame_rate"), "canonical source average frame rate")
            or recorded_time_base != source_time_base
            or int(source_probe.get("stream_index", -1)) != int(source_info.get("index", -2))
            or int(source_info.get("width", 0) or 0) != source_width
            or int(source_info.get("height", 0) or 0) != source_height
            or not math.isclose(float(source.fps), float(source_rate), rel_tol=0, abs_tol=5e-5)):
        raise HintNormalizationError("Canonical source marker and movie timeline/geometry disagree.")
    _assert_stat_identity(source_path, source_initial_stat, "Canonical source movie")

    hint_info = _probe(hint_path, "hint movie")
    width, height = int(hint_info.get("width", 0) or 0), int(hint_info.get("height", 0) or 0)
    if (width, height) != (source_width, source_height):
        raise HintNormalizationError(
            f"Hint movie geometry is {width}x{height}; canonical source is {source_width}x{source_height}."
        )
    hint_timeline = _timeline(hint_path, "hint movie", len(stems), cancel_event, hint_info, on_progress)
    if hint_timeline is None:
        return None
    hint_rate, hint_time_base, hint_start = hint_timeline
    if hint_rate != source_rate:
        raise HintNormalizationError(f"Hint frame rate {hint_rate} does not match source rate {source_rate}.")
    alignment_tolerance = (source_time_base + hint_time_base) / 2
    if alignment_tolerance >= Fraction(1, 2) / source_rate:
        raise HintNormalizationError("Source and hint time bases are too coarse to prove frame alignment.")
    if any(abs(source_time - hint_time) > alignment_tolerance
           for source_time, hint_time in zip(source_start, hint_start)):
        raise HintNormalizationError("Hint and source presentation times do not align by frame ordinal.")
    filter_name = _extract_filter(
        channel, str(hint_info.get("pix_fmt", "")), str(hint_info.get("color_range", "")),
    )

    _assert_stat_identity(source_path, source_initial_stat, "Canonical source movie")
    _assert_stat_identity(hint_path, initial_hint_stat, "Hint movie")
    if _cancelled(cancel_event):
        return None
    destination = os.path.join(clip_root, _DESTINATIONS[kind])
    identity = {
        "version": _VERSION, "kind": kind, "channel": channel,
        "source_path": source_path, "source_sha256": source_sha256,
        "hint_path": hint_path, "hint_sha256": hint_sha256,
        "source_stat_identity": list(source_initial_stat),
        "hint_stat_identity": list(initial_hint_stat),
        "frame_count": len(stems), "fps": str(source_rate),
        "source_time_base": str(source_time_base), "hint_time_base": str(hint_time_base),
        "width": width, "height": height, "frame_stems": list(stems),
        "pixel_format": str(hint_info.get("pix_fmt", "")),
        "color_range": str(hint_info.get("color_range", "")),
    }
    staging = os.path.join(
        clip_root, f".{_DESTINATIONS[kind]}.normalizing-{source_sha256[:16]}-{hint_sha256[:16]}"
    )
    # This advisory lock coordinates normalizers; it is not a boundary against external writers.
    with _exclusive_lock(staging + ".lock"):
        return _normalize_locked(
            destination=destination, staging=staging, identity=identity,
            source_path=source_path, source_sha256=source_sha256,
            hint_path=hint_path, hint_sha256=hint_sha256, kind=kind, channel=channel,
            stems=stems, source_rate=source_rate, width=width, height=height,
            filter_name=filter_name,
            cancel_event=cancel_event, on_progress=on_progress,
        )
