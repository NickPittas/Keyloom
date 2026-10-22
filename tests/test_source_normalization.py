import json
import threading
from fractions import Fraction

import numpy as np
import pytest

from backend.service import source_normalization as normalization
from backend.ffmpeg_tools.probe import (
    VideoTimestamps,
    validate_constant_frame_rate_timestamps,
)


def _inputs(tmp_path, monkeypatch):
    source = tmp_path / "movie.mov"
    source.write_bytes(b"original movie bytes")
    clip_root = tmp_path / "clip"
    clip_root.mkdir()
    info = {
        "fps": 24.0,
        "r_frame_rate": "24/1",
        "avg_frame_rate": "24/1",
        "time_base": "1/24",
        "stream_index": 0,
        "width": 4,
        "height": 3,
        "frame_count": 2,
        "frame_count_exact": True,
        "codec": "prores",
        "duration": 2 / 24,
        "pix_fmt": "gbrp",
        "color_space": "",
        "color_primaries": "",
        "color_transfer": "",
        "color_range": "",
        "chroma_location": "",
        "bits_per_raw_sample": 0,
    }
    monkeypatch.setattr(normalization, "probe_video", lambda _path: dict(info))
    monkeypatch.setattr(
        normalization, "probe_video_timestamps",
        lambda _path, stream_index, time_base, **_kwargs: VideoTimestamps(
            stream_index, Fraction(time_base), (0, 1),
        ),
    )
    monkeypatch.setattr(
        normalization,
        "imread_unicode",
        lambda _path, _flags: np.zeros((3, 4, 3), dtype=np.float16),
    )
    return source, clip_root, info


def _write_frames(directory, count=2):
    from pathlib import Path

    for index in range(count):
        (Path(directory) / f"frame_{index:06d}.exr").write_bytes(b"frame")
    (Path(directory) / ".dwab_done").write_text("done", encoding="utf-8")
    return count


def test_cfr_timestamp_validation_handles_quantized_and_equivalent_timebases():
    rate = Fraction(30000, 1001)
    validate_constant_frame_rate_timestamps(rate, Fraction(1, 1000), (0, 33, 67, 100, 133, 167))
    validate_constant_frame_rate_timestamps(rate, Fraction(1, 30000), (0, 1001, 2002, 3003, 4004))
    rounded_ticks = tuple(round(index * Fraction(1001, 30)) for index in range(61))
    validate_constant_frame_rate_timestamps(rate, Fraction(1, 1000), rounded_ticks)
    drifting_ticks = tuple(
        33 * index if index <= 30 else 990 + 34 * (index - 30)
        for index in range(61)
    )
    with pytest.raises(ValueError, match="Variable or unsupported"):
        validate_constant_frame_rate_timestamps(rate, Fraction(1, 1000), drifting_ticks)
    with pytest.raises(ValueError, match="Variable or unsupported"):
        validate_constant_frame_rate_timestamps(Fraction(25, 1), Fraction(1, 1000), (0, 40, 100, 120))


def test_normalizes_publishes_and_reuses_source_sequence(tmp_path, monkeypatch):
    source, clip_root, _info = _inputs(tmp_path, monkeypatch)
    sidecar = clip_root / ".video_metadata.json"
    sidecar.write_text(json.dumps({
        "custom": {"keep": "value"},
        "source_probe": {"vendor_field": 17},
    }), encoding="utf-8")
    extractions = []

    def extract(_source, out_dir, **_kwargs):
        extractions.append(out_dir)
        return _write_frames(out_dir)

    monkeypatch.setattr(normalization, "extract_frames", extract)
    phases = []
    result = normalization.normalize_source_movie(
        str(source), str(clip_root), on_progress=lambda phase, _now, _total: phases.append(phase)
    )

    assert result is not None
    assert result.frames_dir == str(clip_root / "Frames")
    assert result.frame_stems == ("frame_000000", "frame_000001")
    assert all(path.startswith(result.frames_dir) for path in result.frame_paths)
    assert result.frame_count == 2 and result.fps == 24.0 and (result.width, result.height) == (4, 3)
    assert (source.read_bytes() == b"original movie bytes")
    marker = json.loads((clip_root / "Frames" / ".source_normalization.json").read_text(encoding="utf-8"))
    assert marker["version"] == 2 and marker["complete"] is True
    assert marker["source_sha256"] == result.source_sha256
    assert marker["metadata"]["source_probe"]["timestamp_count"] == 2
    assert len(marker["metadata"]["source_stat_identity"]) == 5
    saved = json.loads(sidecar.read_text(encoding="utf-8"))
    assert saved["custom"]["keep"] == "value"
    assert saved["source_probe"]["vendor_field"] == 17
    assert ".dwab_done" in {path.name for path in (clip_root / "Frames").iterdir()}
    assert not list(clip_root.glob(".Frames.normalizing-*"))
    assert "fingerprint" in phases and "validate" in phases

    reused = normalization.normalize_source_movie(str(source), str(clip_root))
    assert reused == result
    assert len(extractions) == 1


def test_cancel_keeps_partial_hidden_and_same_source_resumes(tmp_path, monkeypatch):
    source, clip_root, _info = _inputs(tmp_path, monkeypatch)
    cancel_event = threading.Event()
    staging_paths = []

    def extract(_source, out_dir, *, cancel_event, **_kwargs):
        staging_paths.append(out_dir)
        if len(staging_paths) == 1:
            from pathlib import Path

            (Path(out_dir) / "frame_000000.exr").write_bytes(b"partial")
            cancel_event.set()
            return 1
        return _write_frames(out_dir)

    monkeypatch.setattr(normalization, "extract_frames", extract)
    assert normalization.normalize_source_movie(
        str(source), str(clip_root), cancel_event=cancel_event
    ) is None
    assert not (clip_root / "Frames").exists()
    assert len(list(clip_root.glob(".Frames.normalizing-*/frame_*.exr"))) == 1

    cancel_event.clear()
    result = normalization.normalize_source_movie(
        str(source), str(clip_root), cancel_event=cancel_event
    )
    assert result is not None and result.frame_count == 2
    assert staging_paths[0] == staging_paths[1]
    assert not list(clip_root.glob(".Frames.normalizing-*"))


def test_rejects_ambiguous_frame_rate_without_writing(tmp_path, monkeypatch):
    source, clip_root, info = _inputs(tmp_path, monkeypatch)
    monkeypatch.setattr(
        normalization, "probe_video_timestamps",
        lambda _path, stream_index, time_base, **_kwargs: VideoTimestamps(
            stream_index, Fraction(time_base), (0, 1, 3),
        ),
    )
    monkeypatch.setattr(
        normalization, "extract_frames",
        lambda *_args, **_kwargs: pytest.fail("must reject before extraction"),
    )

    with pytest.raises(normalization.SourceNormalizationError, match="Variable or unsupported"):
        normalization.normalize_source_movie(str(source), str(clip_root))
    assert not (clip_root / "Frames").exists()
    assert not list(clip_root.glob(".Frames.normalizing-*"))


def test_geometry_mismatch_never_publishes_frames(tmp_path, monkeypatch):
    source, clip_root, _info = _inputs(tmp_path, monkeypatch)

    def extract(_source, out_dir, **_kwargs):
        return _write_frames(out_dir)

    monkeypatch.setattr(normalization, "extract_frames", extract)
    monkeypatch.setattr(
        normalization,
        "imread_unicode",
        lambda path, _flags: np.zeros((2, 4, 3), dtype=np.float16)
        if path.endswith("frame_000001.exr") else np.zeros((3, 4, 3), dtype=np.float16),
    )

    with pytest.raises(normalization.SourceNormalizationError, match="4x2"):
        normalization.normalize_source_movie(str(source), str(clip_root))
    assert not (clip_root / "Frames").exists()
    assert list(clip_root.glob(".Frames.normalizing-*/frame_*.exr"))


def test_exact_frame_count_mismatch_never_publishes_frames(tmp_path, monkeypatch):
    source, clip_root, _info = _inputs(tmp_path, monkeypatch)
    monkeypatch.setattr(
        normalization, "extract_frames",
        lambda _source, out_dir, **_kwargs: _write_frames(out_dir, count=1),
    )

    with pytest.raises(normalization.SourceNormalizationError, match="reports 2"):
        normalization.normalize_source_movie(str(source), str(clip_root))
    assert not (clip_root / "Frames").exists()
    assert list(clip_root.glob(".Frames.normalizing-*/frame_*.exr"))


def test_existing_frames_are_never_replaced(tmp_path, monkeypatch):
    source, clip_root, _info = _inputs(tmp_path, monkeypatch)
    frames = clip_root / "Frames"
    frames.mkdir()
    existing = frames / "keep.png"
    existing.write_bytes(b"existing good asset")
    monkeypatch.setattr(
        normalization, "extract_frames",
        lambda *_args, **_kwargs: pytest.fail("must not replace existing assets"),
    )

    with pytest.raises(normalization.SourceNormalizationError, match="left untouched"):
        normalization.normalize_source_movie(str(source), str(clip_root))
    assert existing.read_bytes() == b"existing good asset"
