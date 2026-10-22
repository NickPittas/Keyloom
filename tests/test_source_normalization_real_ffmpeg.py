"""Small CPU-only FFmpeg regressions for source movie normalization."""
from __future__ import annotations

import json
import multiprocessing
from multiprocessing.process import BaseProcess
import os
import shutil
import subprocess
import threading
from fractions import Fraction
from pathlib import Path

import numpy as np
import pytest

from backend.ffmpeg_tools.discovery import require_ffmpeg_install
from backend.ffmpeg_tools import extraction
from backend.ffmpeg_tools.probe import (
    probe_video,
    probe_video_timestamps,
    validate_constant_frame_rate_timestamps,
)
from backend.frame_io import cv2, imread_unicode
from backend.service import source_normalization as normalization


def _ffmpeg() -> str:
    result = require_ffmpeg_install(require_probe=True)
    assert result.ffmpeg_path and result.ffprobe_path
    return result.ffmpeg_path


def _run(args: list[str]) -> None:
    result = subprocess.run(args, capture_output=True, text=True, timeout=8)
    assert result.returncode == 0, result.stderr[-1000:]


def _make_cfr(path: Path, *, rate: str = "25", frames: int = 6, size: str = "32x32") -> None:
    _run([
        _ffmpeg(), "-hide_banner", "-loglevel", "error",
        "-f", "lavfi", "-i", f"testsrc2=size={size}:rate={rate}",
        "-frames:v", str(frames), "-c:v", "ffv1", "-pix_fmt", "yuv420p",
        "-y", str(path),
    ])


def _allow_software_decode(monkeypatch) -> None:
    monkeypatch.setattr(extraction, "detect_hwaccel", lambda _ffmpeg=None: [])
    monkeypatch.setattr(
        extraction, "_recompress_to_dwab",
        lambda out_dir, on_progress=None, cancel_event=None:
            extraction._recompress_sequential(out_dir, on_progress, cancel_event),
    )


def test_real_30000_1001_resume_preserves_decoded_frame_identity(tmp_path, monkeypatch):
    _allow_software_decode(monkeypatch)
    source = tmp_path / "rational.mkv"
    _make_cfr(source, rate="30000/1001", frames=12)
    info = probe_video(str(source))
    assert Fraction(info["r_frame_rate"]) == Fraction(30000, 1001)
    timeline = probe_video_timestamps(str(source), info["stream_index"], info["time_base"])
    assert timeline is not None and len(timeline.pts) == 12

    full_dir = tmp_path / "full"
    resumed_dir = tmp_path / "resumed"
    full_dir.mkdir()
    resumed_dir.mkdir()
    expected = extraction.extract_frames(
        str(source), str(full_dir), expected_frame_count=12,
    )
    assert expected == 12
    for index in range(7):
        shutil.copyfile(full_dir / f"frame_{index:06d}.exr",
                        resumed_dir / f"frame_{index:06d}.exr")

    actual = extraction.extract_frames(
        str(source), str(resumed_dir), expected_frame_count=12,
    )
    assert actual == 12
    for index in range(12):
        expected_frame = imread_unicode(str(full_dir / f"frame_{index:06d}.exr"), cv2.IMREAD_UNCHANGED)
        actual_frame = imread_unicode(str(resumed_dir / f"frame_{index:06d}.exr"), cv2.IMREAD_UNCHANGED)
        assert expected_frame is not None and actual_frame is not None
        np.testing.assert_allclose(actual_frame, expected_frame, atol=0.002, rtol=0)


def test_real_mkv_uses_timestamp_count_when_nb_frames_is_estimated(tmp_path, monkeypatch):
    _allow_software_decode(monkeypatch)
    source = tmp_path / "estimated.mkv"
    _make_cfr(source, rate="30000/1001", frames=12)
    info = probe_video(str(source))
    timeline = probe_video_timestamps(str(source), info["stream_index"], info["time_base"])
    assert timeline is not None
    assert len(timeline.pts) == 12
    estimated_info = dict(info, frame_count=11, frame_count_exact=False)
    monkeypatch.setattr(normalization, "probe_video", lambda _path: dict(estimated_info))

    clip_root = tmp_path / "clip"
    clip_root.mkdir()
    result = normalization.normalize_source_movie(str(source), str(clip_root))
    assert result is not None and result.frame_count == 12
    assert len(result.frame_stems) == 12
    metadata = json.loads((clip_root / ".video_metadata.json").read_text())
    assert metadata["source_probe"]["frame_count"] == 12
    assert metadata["source_probe"]["timestamp_count"] == 12
    assert metadata["source_probe"]["frame_count_exact"] is True


def test_real_vfr_pts_rejected_even_when_advertised_rates_match(tmp_path):
    source = tmp_path / "vfr.mkv"
    _run([
        _ffmpeg(), "-hide_banner", "-loglevel", "error",
        "-f", "lavfi", "-i", "testsrc2=size=32x32:rate=25:duration=0.24",
        "-vf", "settb=expr=1/1000,setpts='if(eq(N,0),0,if(eq(N,1),40,if(eq(N,2),100,if(eq(N,3),120,if(eq(N,4),160,200)))))'",
        "-fps_mode:v", "passthrough", "-enc_time_base", "1/1000",
        "-c:v", "ffv1", "-y", str(source),
    ])
    info = probe_video(str(source))
    assert info["r_frame_rate"] == info["avg_frame_rate"] == "25/1"
    timeline = probe_video_timestamps(str(source), info["stream_index"], info["time_base"])
    assert timeline is not None and len(timeline.pts) == 6

    clip_root = tmp_path / "clip"
    clip_root.mkdir()
    with pytest.raises(normalization.SourceNormalizationError, match="Variable or unsupported"):
        normalization.normalize_source_movie(str(source), str(clip_root))
    assert not (clip_root / "Frames").exists()
    assert not list(clip_root.glob(".Frames.normalizing-*"))


def test_real_gradual_rounding_drift_is_rejected_by_full_timeline(tmp_path, monkeypatch):
    source = tmp_path / "drifting_cfr.mkv"
    _run([
        _ffmpeg(), "-hide_banner", "-loglevel", "error",
        "-f", "lavfi", "-i", "testsrc2=size=32x32:rate=30000/1001:duration=2.1",
        "-vf", "settb=expr=1/1000,setpts='if(lt(N,31),33*N,990+34*(N-30))'",
        "-frames:v", "61", "-fps_mode:v", "passthrough", "-enc_time_base", "1/1000",
        "-c:v", "ffv1", "-y", str(source),
    ])
    info = probe_video(str(source))
    timeline = probe_video_timestamps(str(source), info["stream_index"], info["time_base"])
    assert timeline is not None and len(timeline.pts) == 61
    assert timeline.time_base == Fraction(1, 1000)
    assert all(
        right - left in {33, 34}
        for left, right in zip(timeline.pts, timeline.pts[1:])
    )
    with pytest.raises(ValueError, match="Variable or unsupported"):
        validate_constant_frame_rate_timestamps(
            Fraction(30000, 1001), timeline.time_base, timeline.pts,
        )

    monkeypatch.setattr(normalization, "probe_video", lambda _path: {
        **info, "r_frame_rate": "30000/1001", "avg_frame_rate": "30000/1001",
        "fps": 30000 / 1001,
    })
    clip_root = tmp_path / "clip"
    clip_root.mkdir()
    with pytest.raises(normalization.SourceNormalizationError, match="Variable or unsupported"):
        normalization.normalize_source_movie(str(source), str(clip_root))
    assert not (clip_root / "Frames").exists()


def test_real_h264_frame_timestamps_use_json_output(tmp_path):
    source = tmp_path / "sei.mp4"
    try:
        _run([
            _ffmpeg(), "-hide_banner", "-loglevel", "error",
            "-f", "lavfi", "-i", "testsrc2=size=32x32:rate=25:duration=0.24",
            "-frames:v", "6", "-c:v", "libx264", "-pix_fmt", "yuv420p",
            "-y", str(source),
        ])
    except AssertionError as exc:
        if "Unknown encoder 'libx264'" in str(exc):
            pytest.skip("FFmpeg build does not include the libx264 encoder")
        raise
    info = probe_video(str(source))
    timeline = probe_video_timestamps(str(source), info["stream_index"], info["time_base"])
    assert timeline is not None and len(timeline.pts) == 6


def test_real_multiple_video_streams_extracts_the_probed_stream(tmp_path, monkeypatch):
    _allow_software_decode(monkeypatch)
    source = tmp_path / "two_video_streams.mkv"
    _run([
        _ffmpeg(), "-hide_banner", "-loglevel", "error",
        "-f", "lavfi", "-i", "color=c=red:s=16x16:r=25:d=0.24",
        "-f", "lavfi", "-i", "color=c=blue:s=32x32:r=25:d=0.24",
        "-map", "0:v:0", "-map", "1:v:0", "-c:v", "ffv1", "-y", str(source),
    ])
    info = probe_video(str(source))
    assert info["stream_index"] == 0 and info["width"] == 16
    second_stream = probe_video_timestamps(str(source), 1, info["time_base"])
    assert second_stream is not None and second_stream.stream_index == 1
    assert len(second_stream.pts) == 6

    clip_root = tmp_path / "clip"
    clip_root.mkdir()
    result = normalization.normalize_source_movie(str(source), str(clip_root))
    assert result is not None and (result.width, result.height) == (16, 16)
    metadata = json.loads((clip_root / ".video_metadata.json").read_text())
    assert metadata["source_probe"]["stream_index"] == 0


def test_real_audio_leading_stream_uses_absolute_video_index(tmp_path):
    source = tmp_path / "audio_then_video.mkv"
    _run([
        _ffmpeg(), "-hide_banner", "-loglevel", "error",
        "-f", "lavfi", "-i", "sine=frequency=1000:duration=0.24",
        "-f", "lavfi", "-i", "testsrc2=size=16x16:rate=25:duration=0.24",
        "-map", "0:a:0", "-map", "1:v:0", "-c:a", "pcm_s16le", "-c:v", "ffv1",
        "-y", str(source),
    ])
    info = probe_video(str(source))
    assert info["stream_index"] == 1
    timeline = probe_video_timestamps(str(source), info["stream_index"], info["time_base"])
    assert timeline is not None and timeline.stream_index == 1 and len(timeline.pts) == 6


def test_real_competing_normalizers_cannot_replace_winning_metadata(tmp_path, monkeypatch):
    _allow_software_decode(monkeypatch)
    winner_source = tmp_path / "winner.mkv"
    loser_source = tmp_path / "loser.mkv"
    _make_cfr(winner_source, rate="25", frames=4)
    _make_cfr(loser_source, rate="30", frames=4)
    clip_root = tmp_path / "clip"
    clip_root.mkdir()
    sidecar = clip_root / ".video_metadata.json"
    sidecar.write_text(json.dumps({"unknown": {"keep": True}}))
    loser_errors: list[Exception] = []
    attempted = False

    def compete(phase, _current, _total):
        nonlocal attempted
        if phase == "probe" and not attempted:
            attempted = True

            def run_loser():
                try:
                    normalization.normalize_source_movie(str(loser_source), str(clip_root))
                except Exception as exc:
                    loser_errors.append(exc)

            loser = threading.Thread(target=run_loser)
            loser.start()
            loser.join(timeout=2)
            assert not loser.is_alive()

    winner = normalization.normalize_source_movie(
        str(winner_source), str(clip_root), on_progress=compete,
    )
    assert winner is not None
    assert len(loser_errors) == 1 and "Another normalization owns" in str(loser_errors[0])
    metadata = json.loads(sidecar.read_text())
    assert metadata["source_path"] == str(winner_source.resolve())
    assert metadata["fps"] == 25.0
    assert metadata["unknown"]["keep"] is True


def test_real_source_swap_in_validation_callback_invalidates_completion(tmp_path, monkeypatch):
    _allow_software_decode(monkeypatch)
    source = tmp_path / "source.mkv"
    replacement = tmp_path / "replacement.mkv"
    _make_cfr(source, frames=4)
    _make_cfr(replacement, rate="30", frames=4)
    original_bytes = source.read_bytes()
    clip_root = tmp_path / "clip"
    clip_root.mkdir()
    swapped = False

    def swap_after_frame_validation(phase, current, total):
        nonlocal swapped
        if phase == "validate" and current == total and not swapped:
            swapped = True
            os.replace(replacement, source)

    with pytest.raises(normalization.SourceNormalizationError, match="Movie changed during normalization"):
        normalization.normalize_source_movie(
            str(source), str(clip_root), on_progress=swap_after_frame_validation,
        )
    assert not list(clip_root.glob(".Frames.normalizing-*"))
    assert not (clip_root / "Frames").exists()
    source.write_bytes(original_bytes)
    retried = normalization.normalize_source_movie(str(source), str(clip_root))
    assert retried is not None and retried.frame_count == 4
    assert (clip_root / "Frames" / ".source_normalization.json").is_file()


@pytest.mark.skipif(os.name == "nt", reason="Windows ctime is creation time, not mutation time")
def test_real_transient_source_mutation_then_restore_is_detected(tmp_path, monkeypatch):
    _allow_software_decode(monkeypatch)
    source = tmp_path / "source.mkv"
    replacement = tmp_path / "replacement.mkv"
    _make_cfr(source, frames=4)
    _make_cfr(replacement, rate="30", frames=4)
    original_bytes = source.read_bytes()
    replacement_bytes = replacement.read_bytes()
    initial_stat = source.stat()
    clip_root = tmp_path / "clip"
    clip_root.mkdir()
    changed = False

    def mutate_and_restore(phase, current, total):
        nonlocal changed
        if phase == "validate" and current == total and not changed:
            changed = True
            source.write_bytes(replacement_bytes)
            source.write_bytes(original_bytes)
            os.utime(source, ns=(initial_stat.st_atime_ns, initial_stat.st_mtime_ns))

    with pytest.raises(normalization.SourceNormalizationError, match="Movie changed during normalization"):
        normalization.normalize_source_movie(
            str(source), str(clip_root), on_progress=mutate_and_restore,
        )
    assert source.read_bytes() == original_bytes
    assert source.stat().st_ctime_ns != initial_stat.st_ctime_ns
    assert not list(clip_root.glob(".Frames.normalizing-*"))
    assert not (clip_root / "Frames").exists()


def test_cancelled_partial_decode_is_discarded_if_source_changed_then_restored(tmp_path, monkeypatch):
    _allow_software_decode(monkeypatch)
    source = tmp_path / "source.mkv"
    replacement = tmp_path / "replacement.mkv"
    _make_cfr(source, frames=30)
    _make_cfr(replacement, rate="30", frames=30)
    original_bytes = source.read_bytes()
    replacement_bytes = replacement.read_bytes()
    clip_root = tmp_path / "clip"
    clip_root.mkdir()
    cancel = threading.Event()
    original_popen = extraction.subprocess.Popen
    extraction_processes = []
    slow_first_extraction = True

    def observe(cmd, **kwargs):
        nonlocal slow_first_extraction
        cmd = list(cmd)
        if Path(cmd[0]).name == "ffmpeg" and "-vf" in cmd:
            if slow_first_extraction:
                cmd.insert(cmd.index("-i"), "-re")
                slow_first_extraction = False
            extraction_processes.append(True)
        return original_popen(cmd, **kwargs)

    monkeypatch.setattr(extraction.subprocess, "Popen", observe)
    changed = False

    def change_then_cancel(phase, _current, _total):
        nonlocal changed
        if phase == "extract" and not changed:
            changed = True
            source.write_bytes(replacement_bytes)
            cancel.set()

    assert normalization.normalize_source_movie(
        str(source), str(clip_root), cancel_event=cancel, on_progress=change_then_cancel,
    ) is None
    stage = next(clip_root.glob(".Frames.normalizing-*"))
    assert list(stage.glob("frame_*.exr"))
    source.write_bytes(original_bytes)
    cancel.clear()

    retried = normalization.normalize_source_movie(str(source), str(clip_root))
    assert retried is not None and retried.frame_count == 30
    assert len(extraction_processes) == 2
    assert not list(clip_root.glob(".Frames.normalizing-*"))


def test_real_source_swap_while_reusing_does_not_rewrite_metadata(tmp_path, monkeypatch):
    _allow_software_decode(monkeypatch)
    source = tmp_path / "source.mkv"
    replacement = tmp_path / "replacement.mkv"
    _make_cfr(source, frames=4)
    _make_cfr(replacement, rate="30", frames=4)
    clip_root = tmp_path / "clip"
    clip_root.mkdir()
    original = normalization.normalize_source_movie(str(source), str(clip_root))
    assert original is not None
    sidecar = clip_root / ".video_metadata.json"
    original_metadata = sidecar.read_bytes()
    swapped = False

    def swap_after_validation(phase, current, total):
        nonlocal swapped
        if phase == "validate" and current == total and not swapped:
            swapped = True
            os.replace(replacement, source)

    with pytest.raises(normalization.SourceNormalizationError, match="Movie changed during normalization"):
        normalization.normalize_source_movie(
            str(source), str(clip_root), on_progress=swap_after_validation,
        )
    assert sidecar.read_bytes() == original_metadata
    assert (clip_root / "Frames" / ".source_normalization.json").is_file()


def test_real_committed_frames_recover_sidecar_after_publication_failure(tmp_path, monkeypatch):
    _allow_software_decode(monkeypatch)
    source = tmp_path / "recover.mkv"
    _make_cfr(source, frames=4)
    clip_root = tmp_path / "clip"
    clip_root.mkdir()
    sidecar = clip_root / ".video_metadata.json"
    sidecar.write_text(json.dumps({"unknown": "preserve"}))
    publish = normalization._publish_metadata
    calls = 0

    def fail_once(root, metadata):
        nonlocal calls
        calls += 1
        if calls == 1:
            raise OSError("simulated sidecar publication failure")
        publish(root, metadata)

    monkeypatch.setattr(normalization, "_publish_metadata", fail_once)
    with pytest.raises(OSError, match="simulated sidecar"):
        normalization.normalize_source_movie(str(source), str(clip_root))
    assert (clip_root / "Frames" / ".source_normalization.json").is_file()
    assert json.loads(sidecar.read_text()) == {"unknown": "preserve"}

    monkeypatch.setattr(normalization, "_publish_metadata", publish)
    recovered = normalization.normalize_source_movie(str(source), str(clip_root))
    assert recovered is not None and recovered.frame_count == 4
    saved = json.loads(sidecar.read_text())
    assert saved["unknown"] == "preserve"
    assert saved["source_path"] == str(source.resolve())


def test_real_ffmpeg_progress_callback_exception_stops_writer(tmp_path, monkeypatch):
    _allow_software_decode(monkeypatch)
    source = tmp_path / "slow.mkv"
    _make_cfr(source, frames=50)
    original_popen = extraction.subprocess.Popen
    processes = []
    callback_called = False

    def slow_real_ffmpeg(cmd, **kwargs):
        if Path(cmd[0]).name == "ffmpeg" and "-vf" in cmd:
            cmd = list(cmd)
            cmd.insert(cmd.index("-i"), "-re")
        proc = original_popen(cmd, **kwargs)
        if Path(cmd[0]).name == "ffmpeg" and "-vf" in cmd:
            processes.append(proc)
        return proc

    monkeypatch.setattr(extraction.subprocess, "Popen", slow_real_ffmpeg)
    out_dir = tmp_path / "frames"

    def fail_progress(_current, _total):
        nonlocal callback_called
        callback_called = True
        raise RuntimeError("progress callback failed")

    with pytest.raises(RuntimeError, match="progress callback failed"):
        extraction.extract_frames(str(source), str(out_dir), on_progress=fail_progress)
    assert callback_called and len(processes) == 1
    assert processes[0].poll() is not None


def test_recompress_cancellation_joins_all_real_workers(tmp_path, monkeypatch):
    monkeypatch.setattr(extraction.os, "cpu_count", lambda: 32)
    source = tmp_path / "pool.mkv"
    _make_cfr(source, frames=24, size="512x512")
    out_dir = tmp_path / "exrs"
    out_dir.mkdir()
    _run([
        _ffmpeg(), "-hide_banner", "-loglevel", "error",
        "-i", str(source), "-map", "0:0", "-frames:v", "24",
        "-vf", "format=gbrpf32le", "-compression", "3", "-format", "1",
        "-start_number", "0", str(out_dir / "frame_%06d.exr"), "-y",
    ])
    files = sorted(path.name for path in out_dir.glob("*.exr"))
    assert len(files) == 24
    before = {child.pid for child in multiprocessing.active_children()}
    cancel = threading.Event()

    def stop_after_first(_done, _total):
        cancel.set()

    failed = extraction._recompress_multiprocess(
        str(out_dir), files, len(files), str(out_dir / ".dwab_done"),
        on_progress=stop_after_first, cancel_event=cancel,
    )
    after = {child.pid for child in multiprocessing.active_children()}
    assert failed == []
    assert after == before
    assert not (out_dir / ".dwab_done").exists()

    def fail_progress(_done, _total):
        raise RuntimeError("recompress callback failed")

    with pytest.raises(RuntimeError, match="recompress callback failed"):
        extraction._recompress_multiprocess(
            str(out_dir), files, len(files), str(out_dir / ".dwab_done"),
            on_progress=fail_progress,
        )
    assert {child.pid for child in multiprocessing.active_children()} == before


def test_recompress_partial_worker_launch_failure_joins_started_worker(tmp_path, monkeypatch):
    monkeypatch.setattr(extraction.os, "cpu_count", lambda: 4)
    out_dir = tmp_path / "partial_launch"
    out_dir.mkdir()
    files = [f"frame_{index:06d}.exr" for index in range(4)]
    for name in files:
        (out_dir / name).write_bytes(b"tiny placeholder")
    before = {child.pid for child in multiprocessing.active_children()}
    started = []
    start_calls = 0
    original_start = BaseProcess.start

    def fail_second_start(process):
        nonlocal start_calls
        start_calls += 1
        if start_calls == 2:
            assert len(started) == 1 and started[0].is_alive()
            raise OSError(11, "Resource temporarily unavailable")
        result = original_start(process)
        started.append(process)
        return result

    monkeypatch.setattr(BaseProcess, "start", fail_second_start)
    with pytest.raises(OSError, match="Resource temporarily unavailable"):
        extraction._recompress_multiprocess(
            str(out_dir), files, len(files), str(out_dir / ".dwab_done"),
        )

    assert len(started) == 1
    assert not started[0].is_alive() and started[0].exitcode is not None
    assert {child.pid for child in multiprocessing.active_children()} == before


def test_recompress_subprocess_cancellation_joins_descendant_workers(tmp_path, monkeypatch):
    source = tmp_path / "pool_subprocess.mkv"
    _make_cfr(source, frames=2, size="256x256")
    out_dir = tmp_path / "subprocess_exrs"
    out_dir.mkdir()
    _run([
        _ffmpeg(), "-hide_banner", "-loglevel", "error",
        "-i", str(source), "-map", "0:0", "-frames:v", "2",
        "-vf", "format=gbrpf32le", "-compression", "3", "-format", "1",
        "-start_number", "0", str(out_dir / "frame_%06d.exr"), "-y",
    ])
    files = sorted(path.name for path in out_dir.glob("*.exr"))
    assert len(files) == 2
    original_popen = extraction.subprocess.Popen
    processes = []

    def observe(cmd, **kwargs):
        proc = original_popen(cmd, **kwargs)
        if len(cmd) > 1 and str(cmd[1]).endswith("_dwab_recompress.py"):
            processes.append(proc)
        return proc

    monkeypatch.setattr(extraction.subprocess, "Popen", observe)
    cancel = threading.Event()

    def stop_after_first(_done, _total):
        cancel.set()

    result = extraction._recompress_subprocess(
        str(out_dir), files, len(files), str(out_dir / ".dwab_done"),
        on_progress=stop_after_first, cancel_event=cancel,
    )
    assert result == [] and len(processes) == 1
    assert processes[0].poll() is not None
    assert not (out_dir / "_dwab_recompress.py").exists()
    if os.name != "nt":
        with pytest.raises(ProcessLookupError):
            os.killpg(processes[0].pid, 0)

    def fail_progress(_done, _total):
        raise RuntimeError("subprocess progress callback failed")

    with pytest.raises(RuntimeError, match="subprocess progress callback failed"):
        extraction._recompress_subprocess(
            str(out_dir), files, len(files), str(out_dir / ".dwab_done"),
            on_progress=fail_progress,
        )
    assert processes[-1].poll() is not None
    if os.name != "nt":
        with pytest.raises(ProcessLookupError):
            os.killpg(processes[-1].pid, 0)
