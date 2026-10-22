import hashlib
import json
import os
import subprocess
import threading
from dataclasses import replace
from fractions import Fraction
from pathlib import Path

import numpy as np
import pytest

from backend.ffmpeg_tools.discovery import find_ffmpeg
from backend.ffmpeg_tools import extraction as source_extraction
from backend.ffmpeg_tools.probe import VideoTimestamps
from backend.frame_io import cv2
from backend.service import hint_normalization as normalization
from backend.service.source_normalization import NormalizedSource, normalize_source_movie


def _unit_source(tmp_path, frame_count=2, *, rate="25/1", time_base="1/1000"):
    clip_root = tmp_path / "clip"
    frames_dir = clip_root / "Frames"
    frames_dir.mkdir(parents=True)
    source_path = tmp_path / "source.mov"
    source_path.write_bytes(b"canonical source")
    stems = tuple(f"frame_{index:06d}" for index in range(frame_count))
    paths = []
    for stem in stems:
        path = frames_dir / f"{stem}.exr"
        path.write_bytes(b"canonical frame")
        paths.append(str(path))
    info = {
        "fps": float(Fraction(rate)), "r_frame_rate": rate, "avg_frame_rate": rate,
        "time_base": time_base, "index": 0, "width": 4, "height": 2,
        "frame_count": frame_count, "frame_count_exact": True, "pix_fmt": "gray",
        "color_range": "pc",
    }
    sha = hashlib.sha256(source_path.read_bytes()).hexdigest()
    source_probe = {
        "r_frame_rate": rate, "avg_frame_rate": rate,
        "time_base": time_base, "frame_count_exact": True,
        "stream_index": 0, "timestamp_count": frame_count,
    }
    source_stat = source_path.stat()
    marker = {
        "version": 2, "complete": True, "source_path": str(source_path.resolve()),
        "source_sha256": sha, "metadata": {
            "source_path": str(source_path.resolve()), "fps": info["fps"],
            "width": 4, "height": 2, "frame_count": frame_count,
            "codec": "synthetic", "duration": 0.0, "exr_vf": "",
            "source_probe": source_probe, "source_sha256": sha,
            "source_stat_identity": [source_stat.st_dev, source_stat.st_ino, source_stat.st_size,
                                     source_stat.st_mtime_ns, source_stat.st_ctime_ns],
            "frame_rate_validation": "per_frame_presentation_timestamps",
        },
    }
    (frames_dir / ".source_normalization.json").write_text(json.dumps(marker), encoding="utf-8")
    source = NormalizedSource(
        str(source_path.resolve()), str(frames_dir.resolve()), tuple(paths), stems,
        frame_count, info["fps"], 4, 2, sha,
    )
    return clip_root, source, info


def _png(path, values):
    ok, data = cv2.imencode(".png", np.asarray(values, dtype=np.uint8))
    assert ok
    Path(path).write_bytes(data.tobytes())


def _unit_probes(monkeypatch, source, source_info, hint_info=None, hint_pts=(0, 40), source_pts=None):
    hint_info = dict(hint_info or source_info)
    source_path = source.source_path
    monkeypatch.setattr(
        normalization, "_probe",
        lambda path, _label: dict(source_info if path == source_path else hint_info),
    )
    monkeypatch.setattr(
        normalization, "probe_video_timestamps",
        lambda path, stream_index, time_base, **_kwargs: VideoTimestamps(
            stream_index, Fraction(time_base),
            tuple((source_pts or (0, 40)) if path == source_path else hint_pts),
        ),
    )


def _pgm_sequence(directory, prefix, frames):
    paths = []
    for index, values in enumerate(frames):
        path = directory / f"{prefix}_{index:06d}.pgm"
        pixels = np.asarray(values, dtype=np.uint8)
        path.write_bytes(f"P5\n{pixels.shape[1]} {pixels.shape[0]}\n255\n".encode() + pixels.tobytes())
        paths.append(path)
    return paths


def _pam_sequence(directory, prefix, frames):
    for index, rgba in enumerate(frames):
        path = directory / f"{prefix}_{index:06d}.pam"
        pixels = np.asarray(rgba, dtype=np.uint8)
        header = (
            f"P7\nWIDTH {pixels.shape[1]}\nHEIGHT {pixels.shape[0]}\n"
            "DEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n"
        )
        path.write_bytes(header.encode() + pixels.tobytes())


def _ppm_sequence(directory, prefix, frames):
    for index, rgb in enumerate(frames):
        path = directory / f"{prefix}_{index:06d}.ppm"
        pixels = np.asarray(rgb, dtype=np.uint8)
        path.write_bytes(f"P6\n{pixels.shape[1]} {pixels.shape[0]}\n255\n".encode() + pixels.tobytes())


def _make_gray_movie(directory, prefix, frames, *, rate="25"):
    ffmpeg = find_ffmpeg()
    if not ffmpeg:
        pytest.skip("FFmpeg is not installed")
    _pgm_sequence(directory, prefix, frames)
    output = directory / f"{prefix}.mkv"
    subprocess.run([
        ffmpeg, "-hide_banner", "-loglevel", "error", "-nostdin", "-y",
        "-framerate", rate, "-i", str(directory / f"{prefix}_%06d.pgm"),
        "-frames:v", str(len(frames)), "-c:v", "ffv1", "-pix_fmt", "gray",
        "-color_range", "pc", str(output),
    ], check=True, capture_output=True, timeout=8)
    return output


def _make_yuv420_movie(directory, prefix, frames):
    ffmpeg = find_ffmpeg()
    if not ffmpeg:
        pytest.skip("FFmpeg is not installed")
    raw_path = directory / f"{prefix}.yuv"
    with raw_path.open("wb") as handle:
        for values in frames:
            pixels = np.asarray(values, dtype=np.uint8)
            assert pixels.shape == (2, 4)
            handle.write(pixels.tobytes())
            handle.write(bytes([128, 128] * 2))
    output = directory / f"{prefix}.mkv"
    subprocess.run([
        ffmpeg, "-hide_banner", "-loglevel", "error", "-nostdin", "-y",
        "-f", "rawvideo", "-pixel_format", "yuv420p", "-video_size", "4x2",
        "-framerate", "25", "-i", str(raw_path), "-frames:v", str(len(frames)),
        "-c:v", "ffv1", "-pix_fmt", "yuv420p", "-color_range", "tv", str(output),
    ], check=True, capture_output=True, timeout=8)
    return output


def _make_qtrle_movie(directory, prefix, frames, *, alpha, rate="25", timescale=25000):
    ffmpeg = find_ffmpeg()
    if not ffmpeg:
        pytest.skip("FFmpeg is not installed")
    if alpha:
        _pam_sequence(directory, prefix, frames)
        source_pattern = f"{prefix}_%06d.pam"
        pixel_format = "argb"
    else:
        _ppm_sequence(directory, prefix, frames)
        source_pattern = f"{prefix}_%06d.ppm"
        pixel_format = "rgb24"
    output = directory / f"{prefix}.mov"
    subprocess.run([
        ffmpeg, "-hide_banner", "-loglevel", "error", "-nostdin", "-y",
        "-framerate", rate, "-i", str(directory / source_pattern),
        "-frames:v", str(len(frames)), "-c:v", "qtrle", "-pix_fmt", pixel_format,
        "-video_track_timescale", str(timescale), str(output),
    ], check=True, capture_output=True, timeout=8)
    return output


def _canonical_from_movie(tmp_path, movie, count, monkeypatch):
    clip_root = tmp_path / f"clip-{movie.stem}"
    clip_root.mkdir()
    monkeypatch.setattr(source_extraction, "detect_hwaccel", lambda _ffmpeg=None: [])
    monkeypatch.setattr(
        source_extraction, "_recompress_to_dwab",
        lambda out_dir, on_progress=None, cancel_event=None:
            source_extraction._recompress_sequential(out_dir, on_progress, cancel_event),
    )
    source = normalize_source_movie(str(movie), str(clip_root))
    assert source is not None and source.frame_count == count
    marker = json.loads((Path(source.frames_dir) / ".source_normalization.json").read_text())
    assert set(marker) == {"version", "complete", "source_path", "source_sha256", "metadata"}
    assert marker["version"] == 2 and marker["complete"] is True
    assert set(marker["metadata"]) == {
        "source_path", "fps", "width", "height", "frame_count", "codec", "duration",
        "exr_vf", "source_probe", "source_sha256", "source_stat_identity",
        "frame_rate_validation",
    }
    assert len(marker["metadata"]["source_stat_identity"]) == 5
    assert all(type(value) is int for value in marker["metadata"]["source_stat_identity"])
    assert marker["metadata"]["frame_rate_validation"] == "per_frame_presentation_timestamps"
    assert marker["metadata"]["source_probe"]["timestamp_count"] == count
    sidecar = json.loads((clip_root / ".video_metadata.json").read_text())
    assert sidecar["source_sha256"] == source.source_sha256
    return clip_root, source


def _read_values(directory, stems):
    images = [cv2.imread(str(Path(directory) / f"{stem}.png"), cv2.IMREAD_UNCHANGED) for stem in stems]
    assert all(image is not None and image.ndim == 2 and image.dtype == np.uint8 for image in images)
    return [image.tolist() for image in images]


def _staging_dir(clip_root, kind="alpha"):
    destination = {"alpha": "AlphaHint", "mask": "VideoMamaMaskHint"}[kind]
    return next(path for path in clip_root.glob(f".{destination}.normalizing-*") if path.is_dir())


def _replace_bytes(path, payload, mtime_ns):
    replacement = path.with_name(path.name + ".replacement")
    replacement.write_bytes(payload)
    os.utime(replacement, ns=(mtime_ns, mtime_ns))
    os.replace(replacement, path)


def _change_identity_restore_bytes(path):
    original = path.read_bytes()
    original_stat = path.stat()
    _replace_bytes(path, bytes([original[0] ^ 1]) + original[1:], original_stat.st_mtime_ns)
    _replace_bytes(path, original, original_stat.st_mtime_ns)
    assert path.read_bytes() == original
    assert normalization._stat_identity(str(path)) != (
        original_stat.st_dev, original_stat.st_ino, original_stat.st_size,
        original_stat.st_mtime_ns, original_stat.st_ctime_ns,
    )


def test_actual_ffmpeg_gray_matte_preserves_decoded_samples(tmp_path, monkeypatch):
    samples = [
        [[0, 1, 16, 17], [128, 235, 254, 255]],
        [[255, 254, 235, 128], [17, 16, 1, 0]],
    ]
    source_movie = _make_gray_movie(tmp_path, "source", samples)
    hint_movie = _make_gray_movie(tmp_path, "hint", samples)
    clip_root, source = _canonical_from_movie(tmp_path, source_movie, 2, monkeypatch)
    original = hint_movie.read_bytes()

    result = normalization.normalize_hint_movie(
        str(hint_movie), str(clip_root), source, kind="alpha", channel="gray",
    )

    assert result.frame_stems == source.frame_stems
    assert result.frame_count == 2 and (result.width, result.height) == (4, 2)
    assert _read_values(result.frames_dir, result.frame_stems) == samples
    assert hint_movie.read_bytes() == original


def test_actual_ffmpeg_yuv420_limited_luma_maps_video_black_white(tmp_path, monkeypatch):
    frame = [[16, 235, 16, 235], [235, 16, 235, 16]]
    samples = [frame, frame]
    source_movie = _make_gray_movie(tmp_path, "source", samples)
    hint_movie = _make_yuv420_movie(tmp_path, "hint_yuv", samples)
    clip_root, source = _canonical_from_movie(tmp_path, source_movie, 2, monkeypatch)
    assert normalization._probe(str(hint_movie), "limited fixture")["color_range"] == "tv"

    result = normalization.normalize_hint_movie(
        str(hint_movie), str(clip_root), source, kind="mask", channel="gray",
    )

    expected = [[0, 255, 0, 255], [255, 0, 255, 0]]
    assert _read_values(result.frames_dir, result.frame_stems) == [expected, expected]


def test_actual_ffmpeg_alpha_plane_preserves_numeric_alpha(tmp_path, monkeypatch):
    alpha = [
        [[0, 1, 16, 17], [128, 235, 254, 255]],
        [[255, 254, 235, 128], [17, 16, 1, 0]],
    ]
    source_rgb = [np.full((2, 4, 3), 80 + index, dtype=np.uint8) for index in range(2)]
    source_movie = _make_qtrle_movie(tmp_path, "source", source_rgb, alpha=False)
    rgba = [np.dstack([np.full((2, 4, 3), 120, dtype=np.uint8), frame]) for frame in alpha]
    hint_movie = _make_qtrle_movie(tmp_path, "hint", rgba, alpha=True)
    original = hint_movie.read_bytes()
    clip_root, source = _canonical_from_movie(tmp_path, source_movie, 2, monkeypatch)

    result = normalization.normalize_hint_movie(
        str(hint_movie), str(clip_root), source, kind="mask", channel="alpha",
    )

    assert _read_values(result.frames_dir, result.frame_stems) == alpha
    assert hint_movie.read_bytes() == original


@pytest.mark.parametrize(
    ("rate", "timescale"),
    [("25", 25000), ("30000/1001", 30000)],
)
def test_actual_source_normalizer_aligns_different_rational_timebases(
    tmp_path, monkeypatch, rate, timescale,
):
    alpha = [
        [[0, 1, 16, 17], [128, 235, 254, 255]],
        [[255, 254, 235, 128], [17, 16, 1, 0]],
    ]
    source_frames = [np.full((2, 4), 64 + index, dtype=np.uint8) for index in range(2)]
    source_movie = _make_gray_movie(tmp_path, "canonical", source_frames, rate=rate)
    source_root = tmp_path / "actual-source-clip"
    source_root.mkdir()
    monkeypatch.setattr(source_extraction, "detect_hwaccel", lambda _ffmpeg=None: [])
    monkeypatch.setattr(
        source_extraction, "_recompress_to_dwab",
        lambda out_dir, on_progress=None, cancel_event=None:
            source_extraction._recompress_sequential(out_dir, on_progress, cancel_event),
    )
    source = normalize_source_movie(str(source_movie), str(source_root))
    assert source is not None and source.frame_count == 2
    assert json.loads((Path(source.frames_dir) / ".source_normalization.json").read_text())["version"] == 2

    rgba = [
        np.dstack([np.full((2, 4, 3), 120, dtype=np.uint8), frame])
        for frame in alpha
    ]
    hint_movie = _make_qtrle_movie(
        tmp_path, "different_timebase", rgba, alpha=True, rate=rate, timescale=timescale,
    )
    source_info = normalization._probe(source_movie.as_posix(), "canonical fixture")
    hint_info = normalization._probe(hint_movie.as_posix(), "hint fixture")
    assert source_info["time_base"] != hint_info["time_base"]

    result = normalization.normalize_hint_movie(
        str(hint_movie), str(source_root), source, kind="alpha", channel="alpha",
    )
    assert result is not None and result.frame_stems == source.frame_stems
    assert _read_values(result.frames_dir, result.frame_stems) == alpha


def test_real_hint_resume_at_30000_1001_uses_frame_ordinals(tmp_path, monkeypatch):
    count = 8
    alpha = [
        np.full((2, 4), (index * 31) % 256, dtype=np.uint8)
        for index in range(count)
    ]
    source_frames = [np.full((2, 4), 90, dtype=np.uint8) for _ in range(count)]
    source_movie = _make_gray_movie(tmp_path, "resume_source", source_frames, rate="30000/1001")
    clip_root = tmp_path / "resume_clip"
    clip_root.mkdir()
    monkeypatch.setattr(source_extraction, "detect_hwaccel", lambda _ffmpeg=None: [])
    monkeypatch.setattr(
        source_extraction, "_recompress_to_dwab",
        lambda out_dir, on_progress=None, cancel_event=None:
            source_extraction._recompress_sequential(out_dir, on_progress, cancel_event),
    )
    source = normalize_source_movie(str(source_movie), str(clip_root))
    assert source is not None and source.frame_count == count
    rgba = [
        np.dstack([np.full((2, 4, 3), 100, dtype=np.uint8), frame])
        for frame in alpha
    ]
    hint_movie = _make_qtrle_movie(
        tmp_path, "resume_hint", rgba, alpha=True, rate="30000/1001", timescale=30000,
    )

    real_extract = normalization._extract_frames
    starts = []
    cancel = threading.Event()

    def extract_one_then_cancel(path, frames_dir, *, start, count, cancel_event, **kwargs):
        starts.append(start)
        if start == 0:
            assert real_extract(
                path, frames_dir, filter_name=kwargs["filter_name"], start=0, count=1,
                cancel_event=None, on_progress=None,
            )
            cancel.set()
            return False
        return real_extract(
            path, frames_dir, filter_name=kwargs["filter_name"], start=start, count=count,
            cancel_event=cancel_event, on_progress=kwargs["on_progress"],
        )

    monkeypatch.setattr(normalization, "_extract_frames", extract_one_then_cancel)
    assert normalization.normalize_hint_movie(
        str(hint_movie), str(clip_root), source, kind="alpha", channel="alpha",
        cancel_event=cancel,
    ) is None
    staging = _staging_dir(clip_root)
    partial_count = len(list(staging.glob("frame_*.png")))
    assert partial_count == 1

    def record_resume(path, frames_dir, *, start, count, filter_name, cancel_event, on_progress):
        starts.append(start)
        return real_extract(
            path, frames_dir, filter_name=filter_name, start=start, count=count,
            cancel_event=cancel_event, on_progress=on_progress,
        )

    monkeypatch.setattr(normalization, "_extract_frames", record_resume)
    result = normalization.normalize_hint_movie(
        str(hint_movie), str(clip_root), source, kind="alpha", channel="alpha",
    )
    assert starts == [0, 1]
    assert result is not None and result.frame_count == count
    assert _read_values(result.frames_dir, result.frame_stems) == [frame.tolist() for frame in alpha]


def test_actual_h264_timestamp_probe_ignores_sei_text_and_reads_json_frames(tmp_path):
    ffmpeg = find_ffmpeg()
    if not ffmpeg:
        pytest.skip("FFmpeg is not installed")
    movie = tmp_path / "sei.mp4"
    command = [
        ffmpeg, "-hide_banner", "-loglevel", "error", "-nostdin",
        "-f", "lavfi", "-i", "testsrc2=size=32x32:rate=25",
        "-frames:v", "6", "-c:v", "libx264", "-pix_fmt", "yuv420p",
        "-color_range", "tv", str(movie),
    ]
    result = subprocess.run(command, capture_output=True, text=True, timeout=8)
    if result.returncode and "Unknown encoder 'libx264'" in result.stderr:
        pytest.skip("FFmpeg build does not include libx264")
    assert result.returncode == 0, result.stderr[-1000:]
    info = normalization._probe(str(movie), "H.264 fixture")
    timeline = normalization._timeline(str(movie), "H.264 fixture", 6, None, info)
    assert timeline is not None and len(timeline[2]) == 6


def test_canceled_hint_extraction_resumes_from_last_complete_frame(tmp_path, monkeypatch):
    clip_root, source, info = _unit_source(tmp_path, rate="30000/1001", time_base="1/30000")
    hint = tmp_path / "hint.mov"
    hint.write_bytes(b"unchanged hint")
    _unit_probes(monkeypatch, source, info, hint_pts=(0, 1001), source_pts=(0, 1001))
    cancel = threading.Event()
    starts = []

    def cancel_after_first(_path, frames_dir, *, start, **_kwargs):
        starts.append(start)
        _png(Path(frames_dir) / "frame_000000.png", [[0, 1, 16, 17], [128, 235, 254, 255]])
        cancel.set()
        return False

    monkeypatch.setattr(normalization, "_extract_frames", cancel_after_first)
    assert normalization.normalize_hint_movie(
        str(hint), str(clip_root), source, kind="alpha", channel="gray", cancel_event=cancel,
    ) is None
    staging = _staging_dir(clip_root)
    assert (staging / "frame_000000.png").is_file()
    assert not (clip_root / "AlphaHint").exists()

    def finish(_path, frames_dir, *, start, **_kwargs):
        starts.append(start)
        _png(Path(frames_dir) / "frame_000001.png", [[255, 254, 235, 128], [17, 16, 1, 0]])
        return True

    monkeypatch.setattr(normalization, "_extract_frames", finish)
    result = normalization.normalize_hint_movie(
        str(hint), str(clip_root), source, kind="alpha", channel="gray",
    )
    assert starts == [0, 1]
    assert result.frame_count == 2


def test_extraction_failure_keeps_hidden_work_and_original_hint(tmp_path, monkeypatch):
    clip_root, source, info = _unit_source(tmp_path)
    hint = tmp_path / "hint.mov"
    hint.write_bytes(b"unchanged hint")
    original = hint.read_bytes()
    _unit_probes(monkeypatch, source, info)

    def fail(*_args, **_kwargs):
        raise normalization.HintNormalizationError("synthetic ffmpeg failure")

    monkeypatch.setattr(normalization, "_extract_frames", fail)
    with pytest.raises(normalization.HintNormalizationError, match="synthetic ffmpeg failure"):
        normalization.normalize_hint_movie(str(hint), str(clip_root), source, kind="alpha", channel="gray")
    assert not (clip_root / "AlphaHint").exists()
    assert _staging_dir(clip_root).is_dir()
    assert hint.read_bytes() == original


@pytest.mark.parametrize("failure", ["callback", "cancel"])
def test_extraction_callback_failure_or_cancel_terminates_and_reaps_writer(tmp_path, monkeypatch, failure):
    frames_dir = tmp_path / "staging"
    frames_dir.mkdir()
    stopped = []

    class Writer:
        pid = 43210
        returncode = None

        def poll(self):
            return self.returncode

        def wait(self, timeout=None):
            self.returncode = -15
            return self.returncode

    writer = Writer()
    monkeypatch.setattr(normalization, "find_ffmpeg", lambda: "/usr/bin/ffmpeg")
    monkeypatch.setattr(normalization.subprocess, "Popen", lambda *_a, **_k: writer)
    monkeypatch.setattr(
        normalization, "stop_process_tree",
        lambda proc, timeout: (stopped.append((proc.pid, timeout)), setattr(proc, "returncode", -15)),
    )
    cancel = threading.Event()
    callback = None
    if failure == "callback":
        def callback(*_args):
            raise RuntimeError("progress callback failed")
    else:
        cancel.set()

    if failure == "callback":
        with pytest.raises(RuntimeError, match="progress callback failed"):
            normalization._extract_frames(
                "hint.mov", str(frames_dir), filter_name="format=gray", start=0, count=1,
                cancel_event=cancel, on_progress=callback,
            )
    else:
        assert normalization._extract_frames(
            "hint.mov", str(frames_dir), filter_name="format=gray", start=0, count=1,
            cancel_event=cancel, on_progress=None,
        ) is False
    assert stopped == [(writer.pid, 3)]
    assert writer.returncode == -15


@pytest.mark.parametrize("changed_input", ["source", "hint"])
def test_final_progress_callback_input_swap_is_caught_before_publication(tmp_path, monkeypatch, changed_input):
    clip_root, source, info = _unit_source(tmp_path)
    hint = tmp_path / "hint.mov"
    hint.write_bytes(b"hint movie")
    _unit_probes(monkeypatch, source, info)

    def swap_after_extraction(_path, frames_dir, *, start, on_progress, **_kwargs):
        for index in range(start, 2):
            _png(Path(frames_dir) / f"frame_{index:06d}.png", [[index] * 4, [index] * 4])
        on_progress("extract", 2, 2)
        return True

    def final_callback(phase, _current, _total):
        if phase != "extract":
            return
        changed_path = Path(source.source_path if changed_input == "source" else hint)
        old_stat = changed_path.stat()
        payload = changed_path.read_bytes()
        changed_path.write_bytes(bytes([payload[0] ^ 1]) + payload[1:])
        os.utime(changed_path, ns=(old_stat.st_atime_ns, old_stat.st_mtime_ns))

    monkeypatch.setattr(normalization, "_extract_frames", swap_after_extraction)
    with pytest.raises(normalization.HintNormalizationError, match="changed during"):
        normalization.normalize_hint_movie(
            str(hint), str(clip_root), source, kind="alpha", channel="gray",
            on_progress=final_callback,
        )
    assert not (clip_root / "AlphaHint").exists()
    assert _staging_dir(clip_root).is_dir()
    assert not list(_staging_dir(clip_root).glob("frame_*.png"))
    assert json.loads((_staging_dir(clip_root) / ".hint_normalization.json").read_text())["complete"] is False


def test_same_size_restored_mtime_hint_mutation_cannot_poison_retry(tmp_path, monkeypatch):
    clip_root, source, info = _unit_source(tmp_path)
    hint = tmp_path / "hint.mov"
    hint.write_bytes(bytes([10]))
    original_stat = hint.stat()
    _unit_probes(monkeypatch, source, info)

    def mutate_during_final_callback(_path, writer_dir, *, start, on_progress, **_kwargs):
        assert start == 0
        old_stat = hint.stat()
        hint.write_bytes(bytes([200]))
        os.utime(hint, ns=(old_stat.st_atime_ns, old_stat.st_mtime_ns))
        _png(Path(writer_dir) / "frame_000000.png", [[200] * 4, [200] * 4])
        _png(Path(writer_dir) / "frame_000001.png", [[200] * 4, [200] * 4])
        on_progress("extract", 2, 2)
        return True

    monkeypatch.setattr(normalization, "_extract_frames", mutate_during_final_callback)
    with pytest.raises(normalization.HintNormalizationError, match="Hint movie changed"):
        normalization.normalize_hint_movie(str(hint), str(clip_root), source, kind="alpha", channel="gray")
    staging = _staging_dir(clip_root)
    assert not list(staging.glob("frame_*.png"))
    assert json.loads((staging / ".hint_normalization.json").read_text())["complete"] is False

    hint.write_bytes(bytes([10]))
    os.utime(hint, ns=(original_stat.st_atime_ns, original_stat.st_mtime_ns))

    def extract_current_hint(_path, writer_dir, *, start, count, **_kwargs):
        value = hint.read_bytes()[0]
        for index in range(start, count):
            _png(Path(writer_dir) / f"frame_{index:06d}.png", [[value] * 4, [value] * 4])
        return True

    monkeypatch.setattr(normalization, "_extract_frames", extract_current_hint)
    result = normalization.normalize_hint_movie(str(hint), str(clip_root), source, kind="alpha", channel="gray")
    assert result is not None
    assert _read_values(result.frames_dir, result.frame_stems) == [[[10] * 4, [10] * 4] for _ in range(2)]


@pytest.mark.parametrize("changed_input", ["source", "hint"])
def test_mutate_restore_during_decode_invalidates_prefix_and_preserves_good_hint(
    tmp_path, monkeypatch, changed_input,
):
    clip_root, source, info = _unit_source(tmp_path)
    hint = tmp_path / "hint.mov"
    hint.write_bytes(bytes([10]))
    _unit_probes(monkeypatch, source, info)
    old_good = clip_root / "AlphaHint"
    old_good.mkdir()
    keep = old_good / "frame_000000.png"
    keep.write_bytes(b"previous good AlphaHint")
    changed_path = Path(source.source_path if changed_input == "source" else hint)

    def mutate_restore_and_decode(_path, writer_dir, *, start, count, **_kwargs):
        _change_identity_restore_bytes(changed_path)
        for index in range(start, count):
            _png(Path(writer_dir) / f"frame_{index:06d}.png", [[200] * 4, [200] * 4])
        return True

    monkeypatch.setattr(normalization, "_extract_frames", mutate_restore_and_decode)
    with pytest.raises(normalization.HintNormalizationError, match="stat identity changed"):
        normalization.normalize_hint_movie(str(hint), str(clip_root), source, kind="mask", channel="gray")

    staging = _staging_dir(clip_root, "mask")
    assert not list(staging.glob("frame_*.png"))
    assert json.loads((staging / ".hint_normalization.json").read_text())["complete"] is False
    assert not (clip_root / "VideoMamaMaskHint").exists()
    assert keep.read_bytes() == b"previous good AlphaHint"


@pytest.mark.parametrize("changed_input", ["source", "hint"])
def test_cancelled_prefix_is_discarded_when_input_stat_changes_before_resume(
    tmp_path, monkeypatch, changed_input,
):
    clip_root, source, info = _unit_source(tmp_path)
    hint = tmp_path / "hint.mov"
    hint.write_bytes(bytes([10]))
    _unit_probes(monkeypatch, source, info)
    old_good = clip_root / "AlphaHint"
    old_good.mkdir()
    keep = old_good / "frame_000000.png"
    keep.write_bytes(b"previous good AlphaHint")
    changed_path = Path(source.source_path if changed_input == "source" else hint)
    cancel = threading.Event()
    starts = []

    def cancel_after_prefix(_path, writer_dir, *, start, count, **_kwargs):
        starts.append(start)
        for index in range(start, 1):
            _png(Path(writer_dir) / f"frame_{index:06d}.png", [[10] * 4, [10] * 4])
        cancel.set()
        return False

    monkeypatch.setattr(normalization, "_extract_frames", cancel_after_prefix)
    assert normalization.normalize_hint_movie(
        str(hint), str(clip_root), source, kind="mask", channel="gray", cancel_event=cancel,
    ) is None
    staging = _staging_dir(clip_root, "mask")
    assert len(list(staging.glob("frame_*.png"))) == 1

    _change_identity_restore_bytes(changed_path)

    def finish_from_current_input(_path, writer_dir, *, start, count, **_kwargs):
        starts.append(start)
        for index in range(start, count):
            _png(Path(writer_dir) / f"frame_{index:06d}.png", [[10] * 4, [10] * 4])
        return True

    monkeypatch.setattr(normalization, "_extract_frames", finish_from_current_input)
    result = normalization.normalize_hint_movie(str(hint), str(clip_root), source, kind="mask", channel="gray")
    assert starts == [0, 0]
    assert result is not None and _read_values(result.frames_dir, result.frame_stems) == [
        [[10] * 4, [10] * 4], [[10] * 4, [10] * 4],
    ]
    assert keep.read_bytes() == b"previous good AlphaHint"


def test_public_staging_symlink_cannot_redirect_private_writer(tmp_path, monkeypatch):
    clip_root, source, info = _unit_source(tmp_path)
    hint = tmp_path / "hint.mov"
    hint.write_bytes(b"hint")
    victim = tmp_path / "victim.png"
    victim.write_bytes(b"do not overwrite")
    _unit_probes(monkeypatch, source, info)

    def inject_public_staging_symlink(_path, writer_dir, *, start, **_kwargs):
        staging = _staging_dir(clip_root)
        assert os.stat(writer_dir).st_mode & 0o777 == 0o700
        assert os.path.realpath(writer_dir) != os.path.realpath(staging)
        os.symlink(victim, staging / "frame_000000.png")
        _png(Path(writer_dir) / f"frame_{start:06d}.png", [[1] * 4, [1] * 4])
        _png(Path(writer_dir) / f"frame_{start + 1:06d}.png", [[2] * 4, [2] * 4])
        return True

    monkeypatch.setattr(normalization, "_extract_frames", inject_public_staging_symlink)
    with pytest.raises(normalization.HintNormalizationError, match="without replacement"):
        normalization.normalize_hint_movie(str(hint), str(clip_root), source, kind="alpha", channel="gray")
    assert victim.read_bytes() == b"do not overwrite"
    assert not (clip_root / "AlphaHint").exists()


def test_v1_source_marker_is_rejected(tmp_path, monkeypatch):
    clip_root, source, info = _unit_source(tmp_path)
    hint = tmp_path / "hint.mov"
    hint.write_bytes(b"hint")
    marker_path = Path(source.frames_dir) / ".source_normalization.json"
    marker = json.loads(marker_path.read_text())
    marker["version"] = 1
    marker_path.write_text(json.dumps(marker))
    monkeypatch.setattr(normalization, "_extract_frames", lambda *_a, **_k: pytest.fail("must reject v1"))
    with pytest.raises(normalization.HintNormalizationError, match="source marker"):
        normalization.normalize_hint_movie(str(hint), str(clip_root), source, kind="alpha", channel="gray")


def test_v2_source_marker_without_optional_stat_identity_is_accepted(tmp_path, monkeypatch):
    clip_root, source, info = _unit_source(tmp_path)
    marker_path = Path(source.frames_dir) / ".source_normalization.json"
    marker = json.loads(marker_path.read_text())
    marker["metadata"].pop("source_stat_identity")
    marker_path.write_text(json.dumps(marker), encoding="utf-8")
    hint = tmp_path / "hint.mov"
    hint.write_bytes(bytes([10]))
    _unit_probes(monkeypatch, source, info)

    def extract(_path, writer_dir, *, start, count, **_kwargs):
        for index in range(start, count):
            _png(Path(writer_dir) / f"frame_{index:06d}.png", [[10] * 4, [10] * 4])
        return True

    monkeypatch.setattr(normalization, "_extract_frames", extract)
    result = normalization.normalize_hint_movie(str(hint), str(clip_root), source, kind="alpha", channel="gray")
    assert result is not None and result.frame_count == source.frame_count
    assert _read_values(result.frames_dir, result.frame_stems) == [[[10] * 4, [10] * 4] for _ in range(2)]


def test_fingerprint_lock_is_exclusive(tmp_path):
    lock_path = str(tmp_path / "hint.lock")
    with normalization._exclusive_lock(lock_path):
        with pytest.raises(normalization.HintNormalizationError, match="Another normalization"):
            with normalization._exclusive_lock(lock_path):
                pass


def test_existing_hint_sequence_is_never_replaced(tmp_path, monkeypatch):
    clip_root, source, info = _unit_source(tmp_path)
    hint = tmp_path / "hint.mov"
    hint.write_bytes(b"hint")
    _unit_probes(monkeypatch, source, info)
    existing = clip_root / "AlphaHint"
    existing.mkdir()
    good_frame = existing / "frame_000000.png"
    good_frame.write_bytes(b"previous good hint")
    monkeypatch.setattr(normalization, "_extract_frames", lambda *_a, **_k: pytest.fail("must not extract"))

    with pytest.raises(normalization.HintNormalizationError, match="preserved and not replaced"):
        normalization.normalize_hint_movie(str(hint), str(clip_root), source, kind="alpha", channel="gray")
    assert good_frame.read_bytes() == b"previous good hint"


def test_publication_race_preserves_winning_hint_directory(tmp_path, monkeypatch):
    clip_root, source, info = _unit_source(tmp_path)
    hint = tmp_path / "hint.mov"
    hint.write_bytes(b"hint")
    _unit_probes(monkeypatch, source, info)
    winner = clip_root / "AlphaHint"

    def publish_winner_during_extract(_path, frames_dir, *, start, **_kwargs):
        for index in range(start, 2):
            _png(Path(frames_dir) / f"frame_{index:06d}.png", [[index] * 4, [index] * 4])
        winner.mkdir()
        (winner / "winner.txt").write_bytes(b"winner")
        return True

    monkeypatch.setattr(normalization, "_extract_frames", publish_winner_during_extract)
    with pytest.raises(normalization.HintNormalizationError, match="without replacing existing data"):
        normalization.normalize_hint_movie(str(hint), str(clip_root), source, kind="alpha", channel="gray")
    assert (winner / "winner.txt").read_bytes() == b"winner"
    assert _staging_dir(clip_root).is_dir()


@pytest.mark.parametrize("mismatch", ["count", "geometry", "fps", "time_base", "vfr", "stems"])
def test_alignment_mismatches_are_rejected_before_extraction(tmp_path, monkeypatch, mismatch):
    clip_root, source, source_info = _unit_source(tmp_path)
    hint = tmp_path / "hint.mov"
    hint.write_bytes(b"hint")
    hint_info = dict(source_info)
    hint_pts = (0, 40)
    if mismatch == "count":
        hint_pts = (0,)
    elif mismatch == "geometry":
        hint_info["width"] = 5
    elif mismatch == "fps":
        hint_info["r_frame_rate"] = hint_info["avg_frame_rate"] = "24/1"
        hint_info["time_base"] = "1/1200"
        hint_pts = (0, 50)
    elif mismatch == "time_base":
        hint_info["time_base"] = "1/2000"
        hint_pts = (10, 90)
    elif mismatch == "vfr":
        hint_info["avg_frame_rate"] = "30/1"
    elif mismatch == "stems":
        source = replace(source, frame_stems=("wrong_000000", "wrong_000001"))
    _unit_probes(monkeypatch, source, source_info, hint_info, hint_pts)
    monkeypatch.setattr(normalization, "_extract_frames", lambda *_a, **_k: pytest.fail("must reject before extraction"))

    with pytest.raises(normalization.HintNormalizationError):
        normalization.normalize_hint_movie(str(hint), str(clip_root), source, kind="alpha", channel="gray")
    assert not (clip_root / "AlphaHint").exists()


def test_equal_advertised_rates_with_irregular_timestamps_are_rejected(tmp_path, monkeypatch):
    clip_root, source, info = _unit_source(tmp_path, frame_count=6)
    hint = tmp_path / "vfr.mov"
    hint.write_bytes(b"hint")
    source_pts = (0, 40, 80, 120, 160, 200)
    # Advertised 25 fps matches, but decoded frame timestamps contain the same VFR gap found in source review.
    hint_pts = (0, 40, 100, 120, 160, 200)
    _unit_probes(monkeypatch, source, info, hint_pts=hint_pts, source_pts=source_pts)
    monkeypatch.setattr(normalization, "_extract_frames", lambda *_a, **_k: pytest.fail("must reject VFR"))

    with pytest.raises(normalization.HintNormalizationError, match="variable or ambiguous frame timestamps"):
        normalization.normalize_hint_movie(str(hint), str(clip_root), source, kind="alpha", channel="gray")


def test_rgb_video_cannot_be_silently_treated_as_gray(tmp_path, monkeypatch):
    clip_root, source, source_info = _unit_source(tmp_path)
    hint = tmp_path / "hint.mov"
    hint.write_bytes(b"hint")
    hint_info = dict(source_info, pix_fmt="rgb24")
    _unit_probes(monkeypatch, source, source_info, hint_info)
    monkeypatch.setattr(normalization, "_extract_frames", lambda *_a, **_k: pytest.fail("must reject RGB"))

    with pytest.raises(normalization.HintNormalizationError, match="does not support pixel format"):
        normalization.normalize_hint_movie(str(hint), str(clip_root), source, kind="alpha", channel="gray")


def test_ambiguous_ranges_and_non_8bit_formats_are_refused():
    with pytest.raises(normalization.HintNormalizationError, match="ambiguous color range"):
        normalization._extract_filter("gray", "gray", "unknown")
    with pytest.raises(normalization.HintNormalizationError, match="ambiguous"):
        normalization._extract_filter("gray", "yuv420p", "unknown")
    with pytest.raises(normalization.HintNormalizationError, match="does not support pixel format"):
        normalization._extract_filter("gray", "yuv420p10le", "tv")
    with pytest.raises(normalization.HintNormalizationError, match="8-bit alpha"):
        normalization._extract_filter("alpha", "rgba64le", "pc")
