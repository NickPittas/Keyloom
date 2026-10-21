import json
import math

import pytest

from ui.preview import playback
from ui.preview.playback import choose_ready_frame, resolve_clip_fps


class FakeCap:
    def __init__(self, fps, opened=True):
        self.fps, self.opened, self.released = fps, opened, False

    def isOpened(self):
        return self.opened

    def get(self, prop):
        return self.fps

    def release(self):
        self.released = True


def _sidecar(tmp_path, fps):
    (tmp_path / ".video_metadata.json").write_text(json.dumps({"fps": fps}))


def test_sidecar_fractional(tmp_path):
    _sidecar(tmp_path, 23.976)
    assert resolve_clip_fps(str(tmp_path)) == 23.976


@pytest.mark.parametrize("bad", [None, 0, -5, "abc", float("nan"), float("inf")])
def test_bad_sidecar_falls_to_default(tmp_path, bad):
    (tmp_path / ".video_metadata.json").write_text(json.dumps({"fps": bad}))
    assert resolve_clip_fps(str(tmp_path)) == 24.0


def test_missing_and_malformed_sidecar(tmp_path):
    assert resolve_clip_fps(str(tmp_path), default=25.0) == 25.0
    (tmp_path / ".video_metadata.json").write_text("{nope")
    assert resolve_clip_fps(str(tmp_path)) == 24.0


def test_video_fallback_and_release(tmp_path, monkeypatch):
    cap = FakeCap(29.97)
    monkeypatch.setattr(playback, "open_video", lambda p: cap)
    assert resolve_clip_fps(str(tmp_path), video_path="x.mp4") == 29.97
    assert cap.released


@pytest.mark.parametrize("fps", [0, float("nan"), math.inf, -1, None])
def test_video_bad_fps_default(tmp_path, monkeypatch, fps):
    cap = FakeCap(fps)
    monkeypatch.setattr(playback, "open_video", lambda p: cap)
    assert resolve_clip_fps(str(tmp_path), video_path="x") == 24.0
    assert cap.released


def test_video_unopened_and_sidecar_priority(tmp_path, monkeypatch):
    cap = FakeCap(30, opened=False)
    monkeypatch.setattr(playback, "open_video", lambda p: cap)
    assert resolve_clip_fps(str(tmp_path), video_path="x") == 24.0
    assert cap.released
    _sidecar(tmp_path, 12)
    assert resolve_clip_fps(str(tmp_path), video_path="x") == 12.0


def test_choose_basic_and_gaps():
    assert choose_ready_frame(5, 0, 10, None, {1, 3, 4, 7}) == 4
    assert choose_ready_frame(5, 0, 10, None, [2, 3]) == 3
    assert choose_ready_frame(5, 0, 10, None, set()) is None


def test_choose_bounds():
    assert choose_ready_frame(20, 0, 10, None, {5, 10, 15}) == 10
    assert choose_ready_frame(5, 3, 10, None, {1, 2}) is None


def test_choose_never_backwards():
    assert choose_ready_frame(8, 0, 10, 6, {3, 5}) is None
    assert choose_ready_frame(8, 0, 10, 6, {3, 6}) == 6
    assert choose_ready_frame(8, 0, 10, 6, {3, 7}) == 7


def test_choose_wrap_last_beyond_target():
    assert choose_ready_frame(2, 0, 10, 9, {1, 2, 9}) == 2
