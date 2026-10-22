"""Task 18: keyframe wiring through DualViewerPanel / MainWindow / ClipEntry lifecycle."""
from __future__ import annotations

import os

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

import cv2
import numpy as np
import pytest

pytest.importorskip("PySide6", reason="PySide6 not installed")
from PySide6.QtWidgets import QApplication, QWidget

from backend.clip_state import ClipAsset, ClipEntry, ClipState
from backend.hint_keyframes import KEYFRAMES_FILENAME, save_hint_keyframes
from ui.main_window import MainWindow
from ui.main_window_mixins.hint_keyframe_mixin import HintKeyframeMixin
from ui.preview.frame_index import ViewMode
from ui.widgets.dual_viewer import DualViewerPanel

_APP = QApplication.instance() or QApplication([])


def _make_clip(tmp_path, name="clip", n=4):
    root = tmp_path / name
    (root / "Input").mkdir(parents=True)
    (root / "AlphaHint").mkdir()
    for i in range(n):
        img = np.full((8, 8, 3), 40 + i, dtype=np.uint8)
        assert cv2.imwrite(str(root / "Input" / f"frame_{i:05d}.png"), img)
        assert cv2.imwrite(str(root / "AlphaHint" / f"frame_{i:05d}.png"), img[:, :, 0])
    return ClipEntry(
        name=name, root_path=str(root), state=ClipState.RAW,
        input_asset=ClipAsset(str(root / "Input"), "sequence"),
        alpha_asset=ClipAsset(str(root / "AlphaHint"), "sequence"))


def _pump(cond, timeout=5.0):
    import time
    end = time.time() + timeout
    while time.time() < end:
        _APP.processEvents()
        if cond():
            return True
        time.sleep(0.01)
    return cond()


class _Panel:
    def __init__(self):
        self.ctx = None
        self.enabled = None

    def set_hint_keyframe_context(self, *, has_clip, frame_available, is_marked, count,
                                  error=None):
        self.ctx = dict(has_clip=has_clip, frame_available=frame_available,
                        is_marked=is_marked, count=count, error=error)

    def set_hint_keyframe_mutation_enabled(self, enabled):
        self.enabled = enabled


class _Host(HintKeyframeMixin, QWidget):
    def __init__(self, viewer):
        super().__init__()
        self._param_panel = _Panel()
        self._dual_viewer = viewer
        self._current_clip = None

    def _hint_mutation_allowed(self):
        return True


# ── viewer API ──

def test_presented_index_is_painted_frame_not_requested():
    v = DualViewerPanel()
    assert v.input_viewer.presented_stem_index == -1
    assert v.current_presented_source_index == -1
    v.output_viewer._current_stem_idx = 3   # requested
    v.output_viewer._shown_stem = 1         # painted (slow decode)
    assert v.output_viewer.current_stem_index == 3
    assert v.current_presented_source_index == 1


def test_input_view_uses_input_viewer_index():
    v = DualViewerPanel()
    v.input_viewer._shown_stem = 2
    v.output_viewer._shown_stem = 0
    v.output_viewer._current_mode = ViewMode.COMP
    assert v.current_presented_source_index == 0
    v.output_viewer._current_mode = ViewMode.INPUT
    assert v.current_presented_source_index == 2


def test_real_paint_sets_presented_index(tmp_path):
    clip = _make_clip(tmp_path)
    v = DualViewerPanel()
    v.set_clip(clip)
    v.output_viewer.navigate_to_frame(2)
    assert _pump(lambda: v.output_viewer.presented_stem_index == 2)
    assert v.current_presented_source_index == 2


def test_keyframe_ticks_forward_and_clear_on_clip_change_same_count(tmp_path):
    a, b = _make_clip(tmp_path, "a"), _make_clip(tmp_path, "b")
    v = DualViewerPanel()
    v.set_clip(a)
    v.set_keyframes([1, 3])
    assert v._scrubber._keyframes == [1, 3]
    v.set_clip(b)                       # same frame count
    assert v._scrubber._keyframes == []
    v.set_keyframes([2])
    v.show_placeholder("none")
    assert v._scrubber._keyframes == []


def test_stop_playback_wraps_scrubber(tmp_path):
    v = DualViewerPanel()
    calls = []
    v._scrubber.stop_playback = lambda: calls.append(1)
    v.stop_playback()
    assert calls == [1]


# ── window wiring ──

def test_sync_selected_clip_view_refreshes_keyframe_tools():
    w = MainWindow.__new__(MainWindow)
    seen = []

    class _V:
        def set_clip(self, clip): seen.append("set_clip")

    w._dual_viewer = _V()
    w._refresh_export_thumbnail = lambda c: seen.append("thumb")
    w._refresh_hint_keyframe_tools = lambda *a: seen.append("refresh")
    clip = ClipEntry(name="x", root_path="/tmp/x", state=ClipState.RAW, input_asset=None)
    MainWindow._sync_selected_clip_view(w, clip)
    assert seen == ["set_clip", "thumb", "refresh"]


def test_mixin_and_slots_exist_on_main_window():
    assert issubclass(MainWindow, HintKeyframeMixin)


def test_frame_presented_refreshes_without_stop_or_decode(tmp_path):
    clip = _make_clip(tmp_path)
    save_hint_keyframes(clip.root_path, [1], ["frame_00000", "frame_00001",
                                              "frame_00002", "frame_00003"])
    clip.find_assets()
    v = DualViewerPanel()
    host = _Host(v)
    host._current_clip = clip
    stops = []
    v._scrubber.stop_playback = lambda: stops.append(1)
    v.output_viewer.frame_presented.connect(host._refresh_hint_keyframe_tools)
    v.input_viewer.frame_presented.connect(host._refresh_hint_keyframe_tools)
    v.set_clip(clip)
    host._refresh_hint_keyframe_tools()
    assert host._param_panel.ctx["count"] == 1
    assert v._scrubber._keyframes == [1]
    del stops[:]  # set_clip legitimately ends playback context
    v.output_viewer.navigate_to_frame(1)
    assert _pump(lambda: host._param_panel.ctx["is_marked"])
    v.output_viewer.navigate_to_frame(2)
    assert _pump(lambda: not host._param_panel.ctx["is_marked"]
                 and v.current_presented_source_index == 2)
    assert host._param_panel.ctx["frame_available"] is True
    assert stops == []


def test_requested_vs_shown_marks_use_shown(tmp_path):
    clip = _make_clip(tmp_path)
    clip.hint_keyframes = (1,)
    v = DualViewerPanel()
    host = _Host(v)
    host._current_clip = clip
    v.output_viewer._current_stem_idx = 3   # requested, still decoding
    v.output_viewer._shown_stem = 1
    host._refresh_hint_keyframe_tools()
    assert host._param_panel.ctx["is_marked"] is True
    v.output_viewer._shown_stem = -1        # before first paint
    host._refresh_hint_keyframe_tools()
    assert host._param_panel.ctx["frame_available"] is False
    assert host._param_panel.ctx["error"]


def test_placeholder_clears_context_and_ticks():
    v = DualViewerPanel()
    host = _Host(v)
    v._scrubber.set_range(4)
    v.set_keyframes([1])
    host._current_clip = None
    host._refresh_hint_keyframe_tools()
    assert host._param_panel.ctx["has_clip"] is False
    assert host._param_panel.ctx["count"] == 0
    assert v._scrubber._keyframes == []


# ── ClipEntry reload lifecycle ──

def test_switch_clips_and_rescan_keep_only_own_marks_and_dont_write(tmp_path):
    a, b = _make_clip(tmp_path, "a"), _make_clip(tmp_path, "b")
    stems = [f"frame_{i:05d}" for i in range(4)]
    save_hint_keyframes(a.root_path, [0, 2], stems)
    sa = os.path.join(a.root_path, KEYFRAMES_FILENAME)
    before = (open(sa, "rb").read(), os.stat(sa).st_mtime_ns)
    a.find_assets()
    b.find_assets()
    assert a.hint_keyframes == (0, 2) and b.hint_keyframes == ()
    host = _Host(DualViewerPanel())
    host._current_clip = b
    host._refresh_hint_keyframe_tools()
    assert host._param_panel.ctx["count"] == 0
    host._current_clip = a
    host._refresh_hint_keyframe_tools()
    assert host._param_panel.ctx["count"] == 2
    # rescan / recreated entry: loads, never saves
    a.find_assets()
    fresh = ClipEntry(name="a", root_path=a.root_path, state=ClipState.RAW)
    fresh.find_assets()
    assert fresh.hint_keyframes == (0, 2)
    assert (open(sa, "rb").read(), os.stat(sa).st_mtime_ns) == before
    assert not os.path.exists(os.path.join(b.root_path, KEYFRAMES_FILENAME))


def test_bad_sidecar_reports_error_not_deleted(tmp_path):
    a = _make_clip(tmp_path, "a")
    sa = os.path.join(a.root_path, KEYFRAMES_FILENAME)
    with open(sa, "w") as f:
        f.write("{not json")
    a.find_assets()
    assert a.hint_keyframes == () and a.hint_keyframes_error
    host = _Host(DualViewerPanel())
    host._current_clip = a
    host._refresh_hint_keyframe_tools()
    assert host._param_panel.ctx["error"]
    assert host._param_panel.enabled is False
    assert os.path.exists(sa)


def test_refresh_caches_listings_until_invalidated(tmp_path, monkeypatch):
    clip = _make_clip(tmp_path)
    host = _Host(None)
    host._current_clip = clip
    calls = []
    orig = ClipAsset.get_frame_files

    def counting(self):
        calls.append(self.path)
        return orig(self)

    monkeypatch.setattr(ClipAsset, "get_frame_files", counting)
    monkeypatch.setattr(host, "_current_presented_hint_index", lambda: host._idx)
    for i in (0, 1, 2, 3, 1, 2):
        host._idx = i
        host._refresh_hint_keyframe_tools()
        assert host._param_panel.ctx["frame_available"] is True
    assert len(calls) == 2  # source + AlphaHint, once

    # Matte removed on disk: cached UI is unchanged, but the mutation path is fresh.
    os.remove(os.path.join(clip.root_path, "AlphaHint", "frame_00002.png"))
    host._idx = 2
    host._refresh_hint_keyframe_tools()
    assert host._param_panel.ctx["frame_available"] is True
    assert len(calls) == 2
    assert host._hint_keyframe_matte_problem(
        clip, host._hint_keyframe_source_stems(clip)[0], 2) is not None

    # Reload/selection invalidation rescans and picks up the change.
    host._invalidate_hint_keyframe_context()
    host._refresh_hint_keyframe_tools()
    assert host._param_panel.ctx["frame_available"] is False
    n = len(calls)
    host._refresh_hint_keyframe_tools()
    assert len(calls) == n

    # Asset object replacement invalidates without an explicit call.
    clip.alpha_asset = ClipAsset(os.path.join(clip.root_path, "AlphaHint"), "sequence")
    host._refresh_hint_keyframe_tools()
    assert len(calls) > n

    # No selected clip clears the cache.
    host._current_clip = None
    host._refresh_hint_keyframe_tools()
    assert host._hint_kf_cache is None


def test_cached_oserror_listing_not_retried_until_invalidated(tmp_path, monkeypatch):
    clip = _make_clip(tmp_path)
    host = _Host(None)
    host._current_clip = clip
    calls = []
    orig = ClipAsset.get_frame_files

    def flaky(self):
        if self.path.endswith("AlphaHint"):
            calls.append(1)
            raise OSError("boom")
        return orig(self)

    monkeypatch.setattr(ClipAsset, "get_frame_files", flaky)
    monkeypatch.setattr(host, "_current_presented_hint_index", lambda: host._idx)
    for i in (0, 1, 2, 1):
        host._idx = i
        host._refresh_hint_keyframe_tools()
        assert host._param_panel.ctx["frame_available"] is False
        assert "boom" in host._param_panel.ctx["error"]
    assert len(calls) == 1
    host._invalidate_hint_keyframe_context()
    host._refresh_hint_keyframe_tools()
    assert len(calls) == 2


def test_toggle_after_cached_available_then_matte_deleted_does_not_save(tmp_path, monkeypatch):
    clip = _make_clip(tmp_path)
    host = _Host(None)
    host._current_clip = clip
    host._idx = 2
    warns = []
    host._warn = warns.append
    monkeypatch.setattr(host, "_current_presented_hint_index", lambda: host._idx)
    monkeypatch.setattr(host, "_stop_playback_for_keyframe", lambda: None)
    host._refresh_hint_keyframe_tools()
    assert host._param_panel.ctx["frame_available"] is True  # cached
    os.remove(os.path.join(clip.root_path, "AlphaHint", "frame_00002.png"))
    host._on_toggle_hint_keyframe()
    assert warns
    assert not os.path.exists(os.path.join(clip.root_path, KEYFRAMES_FILENAME))
    assert tuple(clip.hint_keyframes) == ()
