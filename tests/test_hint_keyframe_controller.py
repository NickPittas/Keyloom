"""HintKeyframeMixin: mark/unmark presented frame, persistence, guards."""
from __future__ import annotations

import os
from types import SimpleNamespace

import pytest
from PySide6.QtWidgets import QApplication, QWidget

from backend.clip_state import ClipAsset, ClipEntry
from backend.hint_keyframes import KEYFRAMES_FILENAME, load_hint_keyframes
from ui.main_window_mixins import hint_keyframe_mixin as mod
from ui.main_window_mixins.hint_keyframe_mixin import HintKeyframeMixin
from ui.preview.frame_index import ViewMode
from ui.widgets.frame_scrubber import FrameScrubber

_APP = QApplication.instance() or QApplication([])


class FakePanel:
    def __init__(self):
        self.ctx = None
        self.enabled = None

    def set_hint_keyframe_context(self, *, has_clip, frame_available, is_marked, count,
                                  error=None):
        self.ctx = dict(has_clip=has_clip, frame_available=frame_available,
                        is_marked=is_marked, count=count, error=error)

    def set_hint_keyframe_mutation_enabled(self, enabled):
        self.enabled = enabled


class FakeViewport:
    def __init__(self, idx=0):
        self.current_stem_index = idx


class FakeViewer:
    """input/output viewers + scrubber; stop_playback lands both on the scrubber frame."""

    def __init__(self):
        self.input_viewer = FakeViewport(0)
        self.output_viewer = FakeViewport(0)
        self.current_output_mode = ViewMode.COMP
        self._scrubber = FrameScrubber()
        self._scrubber.set_range(6)
        self.stopped = 0
        self.land_on = None  # simulated landing frame after stop

    def stop_playback(self):
        self.stopped += 1
        if self.land_on is not None:
            self.input_viewer.current_stem_index = self.land_on
            self.output_viewer.current_stem_index = self.land_on


class Host(HintKeyframeMixin, QWidget):
    def __init__(self, clip):
        super().__init__()
        self._param_panel = FakePanel()
        self._current_clip = clip
        self._dual_viewer = FakeViewer()
        self.allowed = True
        self.after_check = None  # callable run inside the busy check

    def _hint_mutation_allowed(self):
        if self.after_check is not None:
            self.after_check()
        return self.allowed


@pytest.fixture
def warnings(monkeypatch):
    calls = []
    monkeypatch.setattr(mod, "QMessageBox", SimpleNamespace(
        warning=lambda parent, title, text: calls.append(text)))
    return calls


def _make_clip(tmp_path, n=6, alpha_frames=None, name="clip"):
    root = tmp_path / name
    (root / "Input").mkdir(parents=True)
    (root / "AlphaHint").mkdir()
    for i in range(n):
        (root / "Input" / f"frame_{i:04d}.png").write_bytes(b"x")
    for i in (range(n) if alpha_frames is None else alpha_frames):
        (root / "AlphaHint" / f"frame_{i:04d}.png").write_bytes(b"x")
    return ClipEntry(
        name=name, root_path=str(root),
        input_asset=ClipAsset(str(root / "Input"), "sequence"),
        alpha_asset=ClipAsset(str(root / "AlphaHint"), "sequence"))


def _set_frame(host, idx):
    host._dual_viewer.input_viewer.current_stem_index = idx
    host._dual_viewer.output_viewer.current_stem_index = idx


def _stems(clip):
    return [os.path.splitext(f)[0] for f in clip.input_asset.get_frame_files()]


def test_mark_first_middle_last_and_ticks(tmp_path, warnings):
    clip = _make_clip(tmp_path)
    host = Host(clip)
    for i in (0, 3, 5):
        _set_frame(host, i)
        host._on_toggle_hint_keyframe()
    assert clip.hint_keyframes == (0, 3, 5)
    assert tuple(load_hint_keyframes(clip.root_path, _stems(clip))) == (0, 3, 5)
    assert host._dual_viewer._scrubber._keyframes == [0, 3, 5]
    assert host._param_panel.ctx["count"] == 3 and host._param_panel.ctx["is_marked"]
    assert not warnings


def test_unmark_last_removes_sidecar(tmp_path, warnings):
    clip = _make_clip(tmp_path)
    host = Host(clip)
    _set_frame(host, 2)
    host._on_toggle_hint_keyframe()
    host._on_toggle_hint_keyframe()
    assert clip.hint_keyframes == ()
    assert not os.path.lexists(os.path.join(clip.root_path, KEYFRAMES_FILENAME))
    assert host._dual_viewer._scrubber._keyframes == []


def test_scrub_gesture_uses_landed_frame_not_future_target(tmp_path, warnings):
    clip = _make_clip(tmp_path)
    host = Host(clip)
    _set_frame(host, 1)                 # shown frame
    host._dual_viewer._scrubber.set_frame(4)   # future/clock target
    host._dual_viewer.land_on = 2       # exact frame playback lands on after stop
    host._on_toggle_hint_keyframe()
    assert host._dual_viewer.stopped == 1
    assert clip.hint_keyframes == (2,)


def test_input_only_view_uses_input_viewer(tmp_path, warnings):
    clip = _make_clip(tmp_path)
    host = Host(clip)
    v = host._dual_viewer
    v.input_viewer.current_stem_index = 4
    v.output_viewer.current_stem_index = 1
    assert host._current_presented_hint_index() == 1
    v.current_output_mode = ViewMode.INPUT
    assert host._current_presented_hint_index() == 4
    v.input_viewer.current_stem_index = -1
    assert host._current_presented_hint_index() is None


def test_missing_matte_mark_blocked_unmark_allowed(tmp_path, warnings):
    clip = _make_clip(tmp_path, alpha_frames=[0, 1])
    host = Host(clip)
    _set_frame(host, 4)
    host._refresh_hint_keyframe_tools()
    assert host._param_panel.ctx["frame_available"] is False
    assert "frame_0004" in host._param_panel.ctx["error"]
    host._on_toggle_hint_keyframe()
    assert clip.hint_keyframes == () and len(warnings) == 1 and "frame_0004" in warnings[0]
    # marked frame whose matte was later deleted may still be unmarked
    _set_frame(host, 1)
    host._on_toggle_hint_keyframe()
    os.remove(os.path.join(clip.root_path, "AlphaHint", "frame_0001.png"))
    host._refresh_hint_keyframe_tools()
    assert host._param_panel.ctx["is_marked"] and host._param_panel.ctx["count"] == 1
    host._on_toggle_hint_keyframe()
    assert clip.hint_keyframes == ()


def test_no_alpha_asset_unmark_still_possible(tmp_path, warnings):
    clip = _make_clip(tmp_path)
    host = Host(clip)
    _set_frame(host, 2)
    host._on_toggle_hint_keyframe()
    clip.alpha_asset = None
    host._refresh_hint_keyframe_tools()
    assert host._param_panel.ctx["frame_available"] is True  # marked → unmark allowed
    host._on_toggle_hint_keyframe()
    assert clip.hint_keyframes == ()
    host._refresh_hint_keyframe_tools()
    assert host._param_panel.ctx["frame_available"] is False


def test_video_hint_needs_cached_frame_count(tmp_path, warnings, monkeypatch):
    clip = _make_clip(tmp_path)
    probed = []
    monkeypatch.setattr(ClipAsset, "get_dimensions", lambda self: probed.append(1))
    clip.alpha_asset = SimpleNamespace(asset_type="video", path="x.mp4", frame_count=0)
    host = Host(clip)
    _set_frame(host, 1)
    host._refresh_hint_keyframe_tools()
    assert host._param_panel.ctx["frame_available"] is False
    assert "frame count" in host._param_panel.ctx["error"]
    clip.alpha_asset.frame_count = 3
    _set_frame(host, 4)
    host._refresh_hint_keyframe_tools()
    assert host._param_panel.ctx["frame_available"] is False
    _set_frame(host, 2)
    host._refresh_hint_keyframe_tools()
    assert host._param_panel.ctx["frame_available"] is True
    assert not probed


@pytest.mark.parametrize("busy", ["flag", "pending", "cancelling"])
def test_busy_blocks_toggle_and_disables_button(tmp_path, warnings, busy):
    clip = _make_clip(tmp_path)
    host = Host(clip)
    host.allowed = False  # running / pending / cancelling / chain all map to this guard
    _set_frame(host, 1)
    host._on_toggle_hint_keyframe()
    assert clip.hint_keyframes == ()
    assert host._dual_viewer.stopped == 0
    assert host._param_panel.enabled is False
    assert not os.path.lexists(os.path.join(clip.root_path, KEYFRAMES_FILENAME))


def test_idle_enables_button(tmp_path):
    host = Host(_make_clip(tmp_path))
    host._refresh_hint_keyframe_tools()
    assert host._param_panel.enabled is True


def test_worker_becomes_busy_right_before_save(tmp_path, warnings):
    clip = _make_clip(tmp_path)
    host = Host(clip)
    _set_frame(host, 1)
    calls = {"n": 0}

    def flip():
        calls["n"] += 1
        host.allowed = calls["n"] < 2   # idle at entry, busy at the pre-save recheck

    host.after_check = flip
    host._on_toggle_hint_keyframe()
    assert clip.hint_keyframes == ()
    assert not os.path.lexists(os.path.join(clip.root_path, KEYFRAMES_FILENAME))
    assert len(warnings) == 1


def test_selection_changed_before_save(tmp_path, warnings):
    clip = _make_clip(tmp_path)
    other = _make_clip(tmp_path, name="other")
    host = Host(clip)
    _set_frame(host, 1)
    host.after_check = lambda: setattr(host, "_current_clip", other)
    host._on_toggle_hint_keyframe()
    assert clip.hint_keyframes == () and other.hint_keyframes == ()
    assert not os.path.lexists(os.path.join(clip.root_path, KEYFRAMES_FILENAME))


def test_frame_changed_before_save(tmp_path, warnings):
    clip = _make_clip(tmp_path)
    host = Host(clip)
    _set_frame(host, 1)
    calls = {"n": 0}

    def move():
        calls["n"] += 1
        if calls["n"] == 2:
            _set_frame(host, 3)

    host.after_check = move
    host._on_toggle_hint_keyframe()
    assert clip.hint_keyframes == ()


def test_unicode_root_and_different_clip_roots(tmp_path, warnings):
    a = _make_clip(tmp_path, name="клип 日本語")
    b = _make_clip(tmp_path, name="other")
    host = Host(a)
    _set_frame(host, 2)
    host._on_toggle_hint_keyframe()
    host._current_clip = b
    host._on_toggle_hint_keyframe()
    assert a.hint_keyframes == (2,) and b.hint_keyframes == (2,)
    assert os.path.isfile(os.path.join(a.root_path, KEYFRAMES_FILENAME))
    assert os.path.isfile(os.path.join(b.root_path, KEYFRAMES_FILENAME))


def test_failed_save_keeps_tuple_and_warns(tmp_path, warnings, monkeypatch):
    clip = _make_clip(tmp_path)
    host = Host(clip)
    _set_frame(host, 1)

    def boom(*a, **k):
        raise OSError("disk full")

    monkeypatch.setattr(mod, "save_hint_keyframes", boom)
    host._on_toggle_hint_keyframe()
    assert clip.hint_keyframes == ()
    assert len(warnings) == 1 and "disk full" in warnings[0]


def test_clip_sidecar_error_is_shown_and_blocks_mutation(tmp_path, warnings):
    clip = _make_clip(tmp_path)
    clip.hint_keyframes_error = "Corrupt keyframe sidecar"
    host = Host(clip)
    _set_frame(host, 1)
    host._refresh_hint_keyframe_tools()
    ctx = host._param_panel.ctx
    assert ctx["error"] == "Corrupt keyframe sidecar"
    assert ctx["count"] == 0 and ctx["frame_available"] is False
    assert host._param_panel.enabled is False
    host._on_toggle_hint_keyframe()
    assert not os.path.lexists(os.path.join(clip.root_path, KEYFRAMES_FILENAME))
    assert clip.hint_keyframes_error == "Corrupt keyframe sidecar"


def test_no_clip_clears_ui(tmp_path):
    host = Host(None)
    host._dual_viewer._scrubber.set_keyframes([1, 2])
    host._refresh_hint_keyframe_tools()
    assert host._param_panel.ctx["has_clip"] is False
    assert host._dual_viewer._scrubber._keyframes == []
