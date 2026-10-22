"""Task 22: async colour/temporal feedback wired into a real MainWindow (offscreen)."""
from __future__ import annotations

import os

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")
# Headless runs have no GPU, so deferred startup diagnostics would open a
# modal dialog nobody can dismiss (same opt-out scripts/ci_smoke.py uses).
os.environ.setdefault("CORRIDORKEY_SKIP_STARTUP_DIAGNOSTICS", "1")

import threading
import time

import cv2
import numpy as np
import pytest

pytest.importorskip("PySide6", reason="PySide6 not installed")
from PySide6.QtWidgets import QApplication

from backend.clip_state import ClipAsset, ClipEntry, ClipState
from backend.input_colour import InputColourSample
import ui.workers.inference_context_worker as icw
from ui.main_window import MainWindow
from ui.recent_sessions import RecentSessionsStore

_APP = QApplication.instance() or QApplication([])


def _pump(cond, timeout=5.0):
    end = time.time() + timeout
    while time.time() < end:
        _APP.processEvents()
        if cond():
            return True
        time.sleep(0.005)
    return cond()


def _make_clip(tmp_path, name):
    root = tmp_path / name
    (root / "Input").mkdir(parents=True)
    for i in range(3):
        img = np.full((8, 8, 3), 40 + i, dtype=np.uint8)
        assert cv2.imwrite(str(root / "Input" / f"frame_{i:05d}.png"), img)
    return ClipEntry(name=name, root_path=str(root), state=ClipState.RAW,
                     input_asset=ClipAsset(str(root / "Input"), "sequence"))


def _ctx(clip_root, peak=None, w=1920, h=1080, count=100, budget=100.0, error=None):
    sample = None
    if peak is not None or error:
        sample = InputColourSample(peak, (0, 1), (w, h), count, error)
    return icw.InferenceContext(str(clip_root), (), w, h, count, budget, sample, None)


class _Probe:
    """Replaces the worker's blocking builder; gated per clip root, records threads."""

    def __init__(self):
        self.results = {}    # root -> InferenceContext
        self.gates = {}      # root -> threading.Event
        self.threads = []
        self.calls = []

    def __call__(self, clip):
        root = str(clip.root_path)
        self.threads.append(threading.get_ident())
        self.calls.append(root)
        gate = self.gates.get(root)
        if gate is not None:
            assert gate.wait(10)
        return self.results[root]


@pytest.fixture
def probe(monkeypatch):
    p = _Probe()
    monkeypatch.setattr(icw, "build_inference_context", p)
    return p


@pytest.fixture
def win(tmp_path):
    w = MainWindow(store=RecentSessionsStore(str(tmp_path / "recent.json")))
    w._thumbs = []
    w._refresh_input_thumbnail = lambda clip, input_is_linear=None: w._thumbs.append(
        (clip.name, input_is_linear))
    yield w
    w._gpu_monitor.stop()
    w._close_inference_feedback()
    w._skip_shutdown_cleanup = True
    w.close()
    _APP.processEvents()


def _select(w, *clips):
    w._clip_model.set_clips(list(clips))
    w._on_clip_selected(clips[0])


def _notice_visible(w):
    return not w._param_panel._colour_notice.isHidden()


def test_probe_runs_off_gui_thread_and_applies_after_release(win, probe, tmp_path):
    clip = _make_clip(tmp_path, "a")
    probe.results[clip.root_path] = _ctx(clip.root_path, peak=0.95)
    gate = probe.gates[clip.root_path] = threading.Event()
    win._clip_input_is_linear["a"] = True
    _select(win, clip)
    # pending: unavailable estimates, no notice
    assert not _notice_visible(win)
    assert "GiB" not in win._param_panel._temporal_window.itemText(3)
    gate.set()
    assert _pump(lambda: _notice_visible(win))
    assert probe.threads and all(t != threading.get_ident() for t in probe.threads)
    assert "GiB" in win._param_panel._temporal_window.itemText(3)


def test_linear_exr_below_one_offers_display_coded_and_click_switches_once(win, probe, tmp_path):
    clip = _make_clip(tmp_path, "a")
    probe.results[clip.root_path] = _ctx(clip.root_path, peak=0.95)
    win._clip_input_is_linear["a"] = True
    _select(win, clip)
    assert _pump(lambda: _notice_visible(win))
    panel = win._param_panel
    assert panel._colour_action.text() == "Switch to sRGB"
    changed = []
    panel.params_changed.connect(lambda: changed.append(1))
    flags = []
    win._dual_viewer.set_input_exr_is_linear = flags.append
    del win._thumbs[:]
    panel._colour_action.click()
    assert panel.get_params().input_is_linear is False
    assert win._clip_input_is_linear["a"] is False
    assert flags == [False]
    assert win._thumbs == [("a", False)]
    assert changed == []  # programmatic switch, no double emission
    assert not _notice_visible(win)
    # survives session reopen
    data = win._build_session_data()
    assert data["clip_input_is_linear"]["a"] is False
    win._clip_input_is_linear = {}
    win._apply_session_data({"clip_input_is_linear": data["clip_input_is_linear"],
                             "selected_clip": "a"})
    assert win._clip_input_is_linear["a"] is False
    assert panel.get_params().input_is_linear is False


def test_display_coded_exr_above_one_offers_linear(win, probe, tmp_path):
    clip = _make_clip(tmp_path, "a")
    probe.results[clip.root_path] = _ctx(clip.root_path, peak=4.0)
    win._clip_input_is_linear["a"] = False
    _select(win, clip)
    assert _pump(lambda: _notice_visible(win))
    assert win._param_panel._colour_action.text() == "Switch to Linear"
    win._param_panel._colour_action.click()
    assert win._param_panel.get_params().input_is_linear is True
    assert win._clip_input_is_linear["a"] is True
    assert ("a", True) in win._thumbs


def test_no_sample_or_failed_sample_gives_no_notice_and_no_switch(win, probe, tmp_path):
    clip = _make_clip(tmp_path, "a")
    probe.results[clip.root_path] = _ctx(clip.root_path)  # PNG/video: no sample
    win._clip_input_is_linear["a"] = True
    _select(win, clip)
    assert _pump(lambda: "GiB" in win._param_panel._temporal_window.itemText(3))
    assert not _notice_visible(win)
    probe.results[clip.root_path] = _ctx(clip.root_path, peak=None, error="boom")
    win._on_clip_selected(clip)
    assert _pump(lambda: len(probe.calls) == 2)
    _pump(lambda: False, 0.2)
    assert not _notice_visible(win)
    assert win._param_panel.get_params().input_is_linear is True


def test_user_edit_during_probe_is_not_overridden(win, probe, tmp_path):
    clip = _make_clip(tmp_path, "a")
    probe.results[clip.root_path] = _ctx(clip.root_path, peak=0.9)
    gate = probe.gates[clip.root_path] = threading.Event()
    win._clip_input_is_linear["a"] = True
    _select(win, clip)
    win._param_panel._color_space.setCurrentIndex(0)  # user picks sRGB while probing
    assert win._clip_input_is_linear["a"] is False
    gate.set()
    assert _pump(lambda: "GiB" in win._param_panel._temporal_window.itemText(3))
    assert not _notice_visible(win)  # current (sRGB) interpretation re-evaluated
    assert win._param_panel.get_params().input_is_linear is False
    assert win._clip_input_is_linear["a"] is False


def test_stale_clip_completion_cannot_change_other_clip(win, probe, tmp_path):
    a, b = _make_clip(tmp_path, "a"), _make_clip(tmp_path, "b")
    probe.results[a.root_path] = _ctx(a.root_path, peak=0.5, w=4096, h=2160, count=7)
    probe.results[b.root_path] = _ctx(b.root_path, w=640, h=360, count=33)
    gate_a = probe.gates[a.root_path] = threading.Event()
    win._clip_input_is_linear.update({"a": True, "b": True})
    win._clip_model.set_clips([a, b])
    win._on_clip_selected(a)
    assert _pump(lambda: probe.calls == [a.root_path])
    win._on_clip_selected(b)
    gate_a.set()
    assert _pump(lambda: win._inference_ctx is not None)
    assert win._inference_ctx.root_path == b.root_path
    assert not _notice_visible(win)
    assert win._param_panel._tctx["width"] == 640
    assert win._clip_input_is_linear == {"a": True, "b": True}


def test_estimates_follow_resolution_count_and_low_flag_without_second_probe(win, probe, tmp_path):
    a, b = _make_clip(tmp_path, "a"), _make_clip(tmp_path, "b")
    probe.results[a.root_path] = _ctx(a.root_path, w=1920, h=1080, count=100)
    probe.results[b.root_path] = _ctx(b.root_path, w=3840, h=2160, count=100)
    win._clip_model.set_clips([a, b])
    win._on_clip_selected(a)
    panel = win._param_panel
    assert _pump(lambda: win._inference_ctx is not None)
    text_a = panel._temporal_window.itemText(1)
    win._on_clip_selected(b)
    assert "GiB" not in panel._temporal_window.itemText(1)  # cleared immediately
    assert _pump(lambda: win._inference_ctx is not None)
    text_b = panel._temporal_window.itemText(1)
    assert text_a != text_b
    calls = len(probe.calls)
    panel._vram_profile.setCurrentIndex(1)  # Low VRAM gesture
    panel._temporal_window.setCurrentIndex(1)
    _pump(lambda: False, 0.1)
    assert len(probe.calls) == calls
    assert panel._temporal_window.itemText(1) != "" and "GiB" in panel._temporal_window.itemText(1)


def test_over_budget_is_red_but_run_stays_enabled(win, probe, tmp_path):
    clip = _make_clip(tmp_path, "a")
    probe.results[clip.root_path] = _ctx(clip.root_path, w=3840, h=2160, count=200, budget=0.5)
    _select(win, clip)
    panel = win._param_panel
    assert _pump(lambda: win._inference_ctx is not None)
    panel._temporal_window.setCurrentIndex(1)
    assert not panel._temporal_warning.isHidden()
    assert "e5484d" in panel._temporal_warning.styleSheet()
    win._on_params_changed()
    assert win._status_bar._run_btn.isEnabled()


def test_unavailable_context_clears_old_state(win, probe, tmp_path):
    a, b = _make_clip(tmp_path, "a"), _make_clip(tmp_path, "b")
    probe.results[a.root_path] = _ctx(a.root_path, peak=0.9)
    probe.results[b.root_path] = icw.InferenceContext(
        b.root_path, (), None, None, 0, None, None, "source metadata unavailable")
    win._clip_input_is_linear.update({"a": True, "b": True})
    win._clip_model.set_clips([a, b])
    win._on_clip_selected(a)
    assert _pump(lambda: _notice_visible(win))
    win._on_clip_selected(b)
    assert _pump(lambda: len(probe.calls) == 2)
    _pump(lambda: False, 0.2)
    assert not _notice_visible(win)
    assert win._inference_ctx is None
    assert "GiB" not in win._param_panel._temporal_window.itemText(3)


def test_clear_on_placeholder_and_return_to_welcome(win, probe, tmp_path):
    clip = _make_clip(tmp_path, "a")
    probe.results[clip.root_path] = _ctx(clip.root_path, peak=0.9)
    win._clip_input_is_linear["a"] = True
    _select(win, clip)
    assert _pump(lambda: _notice_visible(win))
    win._clip_model.set_clips([])  # clip removed -> placeholder
    assert win._current_clip is None
    assert not _notice_visible(win) and win._inference_ctx is None
    assert "GiB" not in win._param_panel._temporal_window.itemText(3)
    _select(win, clip)
    assert _pump(lambda: _notice_visible(win))
    win._return_to_welcome()
    assert not _notice_visible(win) and win._inference_ctx is None


def test_close_while_pending_ignores_completion(win, probe, tmp_path):
    clip = _make_clip(tmp_path, "a")
    probe.results[clip.root_path] = _ctx(clip.root_path, peak=0.9)
    gate = probe.gates[clip.root_path] = threading.Event()
    win._clip_input_is_linear["a"] = True
    _select(win, clip)
    assert _pump(lambda: probe.calls == [clip.root_path])
    win._close_inference_feedback()
    gate.set()
    _pump(lambda: False, 0.3)
    assert win._inference_ctx is None
    assert not _notice_visible(win)
