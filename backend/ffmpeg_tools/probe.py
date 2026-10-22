"""Video probing via ffprobe."""
from __future__ import annotations

import json
import logging
import os
import signal
import subprocess
import threading
import time
from dataclasses import dataclass
from fractions import Fraction
from typing import Sequence

from .discovery import require_ffmpeg_install

logger = logging.getLogger(__name__)


@dataclass(frozen=True)
class VideoTimestamps:
    stream_index: int
    time_base: Fraction
    pts: tuple[int, ...]


def stop_process_tree(proc: subprocess.Popen, timeout: float = 5.0) -> None:
    """Stop and reap a subprocess and the descendants owned by its process group."""
    if os.name == "nt":
        if proc.poll() is None:
            try:
                subprocess.run(
                    ["taskkill", "/PID", str(proc.pid), "/T", "/F"],
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                    timeout=timeout,
                    creationflags=subprocess.CREATE_NO_WINDOW,
                    check=False,
                )
            except (OSError, subprocess.TimeoutExpired):
                pass
        if proc.poll() is None:
            proc.kill()
    else:
        try:
            os.killpg(proc.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            proc.wait(timeout=min(timeout, 1.0))
        except subprocess.TimeoutExpired:
            pass
        try:
            os.killpg(proc.pid, 0)
        except ProcessLookupError:
            pass
        else:
            try:
                os.killpg(proc.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
    try:
        proc.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=timeout)


def probe_video(path: str) -> dict:
    """Probe a video file for metadata.

    Returns dict with keys: fps (float), width (int), height (int),
    frame_count (int), codec (str), duration (float), pix_fmt (str),
    color_range (str), color_space (str), color_primaries (str),
    color_transfer (str), chroma_location (str), bits_per_raw_sample (int).
    Raises RuntimeError if ffprobe fails.
    """
    validation = require_ffmpeg_install(require_probe=True)
    ffprobe = validation.ffprobe_path
    if not ffprobe:
        raise RuntimeError("FFprobe not found")

    cmd = [
        ffprobe,
        "-v", "quiet",
        "-print_format", "json",
        "-show_streams",
        "-show_format",
        path,
    ]

    result = subprocess.run(
        cmd, capture_output=True, text=True, timeout=30,
        encoding="utf-8", errors="replace",
        creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0,
    )
    if result.returncode != 0:
        raise RuntimeError(f"ffprobe failed: {result.stderr[:500]}")

    data = json.loads(result.stdout)

    # Find first video stream
    video_stream = None
    for stream in data.get("streams", []):
        if stream.get("codec_type") == "video":
            video_stream = stream
            break

    if not video_stream:
        raise RuntimeError(f"No video stream found in {path}")

    # Keep both advertised rates: r_frame_rate alone cannot distinguish a
    # constant-rate source from a variable-rate stream.
    fps_str = video_stream.get("r_frame_rate", "24/1")
    avg_fps_str = video_stream.get("avg_frame_rate", "")
    if "/" in fps_str:
        num, den = fps_str.split("/")
        fps = float(num) / float(den) if float(den) != 0 else 24.0
    else:
        fps = float(fps_str)

    # Frame count: prefer nb_frames, fall back to duration * fps
    frame_count = 0
    frame_count_exact = False
    if "nb_frames" in video_stream:
        try:
            frame_count = int(video_stream["nb_frames"])
            frame_count_exact = frame_count > 0
        except (ValueError, TypeError):
            pass

    if frame_count <= 0:
        duration = float(video_stream.get("duration", 0) or
                         data.get("format", {}).get("duration", 0))
        if duration > 0:
            frame_count = int(duration * fps)

    return {
        "fps": round(fps, 4),
        "stream_index": int(video_stream.get("index", 0)),
        "r_frame_rate": fps_str,
        "avg_frame_rate": avg_fps_str,
        "time_base": video_stream.get("time_base", ""),
        "width": int(video_stream.get("width", 0)),
        "height": int(video_stream.get("height", 0)),
        "frame_count": frame_count,
        "frame_count_exact": frame_count_exact,
        "codec": video_stream.get("codec_name", "unknown"),
        "duration": float(video_stream.get("duration", 0) or
                          data.get("format", {}).get("duration", 0)),
        # Color metadata for building explicit conversion filters
        "pix_fmt": video_stream.get("pix_fmt", ""),
        "color_space": video_stream.get("color_space", ""),
        "color_primaries": video_stream.get("color_primaries", ""),
        "color_transfer": video_stream.get("color_transfer", ""),
        "color_range": video_stream.get("color_range", ""),
        "chroma_location": video_stream.get("chroma_location", ""),
        "bits_per_raw_sample": int(video_stream.get("bits_per_raw_sample", 0) or 0),
    }


def probe_video_timestamps(
    path: str,
    stream_index: int,
    time_base: str | Fraction,
    *,
    cancel_event=None,
    on_progress=None,
) -> VideoTimestamps | None:
    """Read one selected stream's decoded presentation timestamps as ffprobe JSON."""
    validation = require_ffmpeg_install(require_probe=True)
    ffprobe = validation.ffprobe_path
    if not ffprobe:
        raise RuntimeError("FFprobe not found")

    cmd = [
        ffprobe, "-v", "error", "-select_streams", str(stream_index),
        "-show_frames",
        "-show_entries", "frame=stream_index,best_effort_timestamp",
        "-of", "json", path,
    ]
    proc = subprocess.Popen(
        cmd, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        text=True, encoding="utf-8", errors="replace",
        creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0,
        start_new_session=os.name != "nt",
    )
    stdout_parts: list[str] = []
    stderr_parts: list[str] = []
    stdout_done = threading.Event()
    stderr_done = threading.Event()

    def read_stdout() -> None:
        try:
            while chunk := proc.stdout.read(64 * 1024):
                stdout_parts.append(chunk)
        finally:
            stdout_done.set()

    def read_stderr() -> None:
        try:
            stderr_parts.append(proc.stderr.read())
        except (OSError, ValueError):
            pass
        finally:
            stderr_done.set()

    out_thread = threading.Thread(target=read_stdout, daemon=True)
    err_thread = threading.Thread(target=read_stderr, daemon=True)
    out_thread.start()
    err_thread.start()
    try:
        while proc.poll() is None:
            if cancel_event is not None and cancel_event.is_set():
                return None
            time.sleep(0.1)
        proc.wait(timeout=30)
        out_thread.join(timeout=5)
        err_thread.join(timeout=5)
        if not stdout_done.is_set() or not stderr_done.is_set():
            raise RuntimeError("FFprobe timestamp output did not close after the process exited.")
        stderr = "".join(stderr_parts)
        if proc.returncode != 0:
            raise RuntimeError(f"ffprobe timestamp scan failed: {stderr[-500:]}")
        try:
            data = json.loads("".join(stdout_parts))
        except json.JSONDecodeError as exc:
            raise RuntimeError(f"FFprobe returned invalid JSON frame timestamps: {exc}") from exc
        frames = data.get("frames") if isinstance(data, dict) else None
        if not isinstance(frames, list) or not frames:
            raise RuntimeError("FFprobe returned no decoded frame timestamps.")
        timestamps: list[int] = []
        for frame in frames:
            if not isinstance(frame, dict):
                raise RuntimeError("FFprobe returned an invalid frame timestamp record.")
            try:
                row_stream = int(frame["stream_index"])
                timestamp = int(frame["best_effort_timestamp"])
            except (KeyError, TypeError, ValueError) as exc:
                raise RuntimeError("FFprobe returned a missing or invalid frame timestamp.") from exc
            if row_stream != stream_index:
                raise RuntimeError(
                    f"FFprobe timestamp stream {row_stream} does not match selected stream {stream_index}."
                )
            timestamps.append(timestamp)
            if on_progress is not None:
                on_progress(len(timestamps), 0)
            if cancel_event is not None and cancel_event.is_set():
                return None
        try:
            parsed_time_base = Fraction(time_base)
        except (ValueError, ZeroDivisionError, TypeError) as exc:
            raise RuntimeError(f"Invalid stream time base {time_base!r}.") from exc
        if parsed_time_base <= 0:
            raise RuntimeError(f"Invalid stream time base {time_base!r}.")
        return VideoTimestamps(stream_index, parsed_time_base, tuple(timestamps))
    except BaseException:
        stop_process_tree(proc)
        raise
    finally:
        if proc.poll() is None:
            stop_process_tree(proc)
        for stream in (proc.stdout, proc.stderr):
            if stream is not None:
                try:
                    stream.close()
                except OSError:
                    pass
        out_thread.join(timeout=5)
        err_thread.join(timeout=5)


def validate_constant_frame_rate_timestamps(
    rate: str | Fraction,
    time_base: str | Fraction,
    timestamps: Sequence[int],
) -> None:
    """Reject VFR timelines, allowing one stream tick of timestamp quantization."""
    try:
        frame_rate = Fraction(rate)
        tick = Fraction(time_base)
    except (ValueError, ZeroDivisionError, TypeError) as exc:
        raise ValueError("A positive frame rate and stream time base are required.") from exc
    if frame_rate <= 0 or tick <= 0 or len(timestamps) < 2:
        raise ValueError("At least two timestamped frames and positive rates are required.")
    expected_interval = Fraction(1, 1) / frame_rate
    expected_ticks = expected_interval / tick
    if expected_ticks < 1:
        raise ValueError("The stream time base is too coarse to validate frame cadence.")
    tolerance = tick if expected_ticks.denominator != 1 else Fraction(0, 1)
    first = timestamps[0]
    for index, (previous, current) in enumerate(zip(timestamps, timestamps[1:]), 1):
        actual_interval = (current - previous) * tick
        elapsed = (current - first) * tick
        expected_elapsed = index * expected_interval
        if (
            current <= previous
            or abs(actual_interval - expected_interval) > tolerance
            or abs(elapsed - expected_elapsed) > tolerance
        ):
            raise ValueError("Variable or unsupported frame timing detected.")
