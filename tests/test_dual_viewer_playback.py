"""DualViewerPanel ready-pair playback, FPS and wipe (Task 11)."""
import os

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

import re
import threading
import time

import pytest

pytest.importorskip("PySide6", reason="PySide6 not installed")

import shiboken6
from PySide6.QtGui import QColor, QImage
from PySide6.QtWidgets import QApplication

from backend.clip_state import ClipEntry, ClipState
from ui.preview import async_decoder
from ui.preview.frame_index import FrameIndex, ViewMode
from ui.widgets import dual_viewer as dv
from ui.widgets.dual_viewer import DualViewerPanel

_APP = QApplication.instance() or QApplication([])
_MAIN_THREAD = threading.get_ident()
SRC = (160, 90)
N = 10


def _index(n=N):
    stems = [f"f{i:03d}" for i in range(n)]
    modes = (ViewMode.INPUT, ViewMode.COMP)
    availability = {m: set(stems) for m in modes}
    files = {(m, s): f"/fake/{m.value}/{s}.png" for m in modes for s in stems}
    return FrameIndex(stems=stems, availability=availability, stem_files=files)


class _Env:
    def __init__(self):
        self.block = {}
        self.fps = {}
        self.fps_gate = threading.Event()
        self.fps_threads = []
        self.t = [0]
        self._lock = threading.Lock()

    def decode(self, path, mode, *, input_exr_is_linear=False, target_size=None):
        ev = self.block.get(path)
        if ev is not None:
            ev.wait(5)
        idx = int(re.search(r"f(\d+)", path).group(1))
        img = QImage(SRC[0], SRC[1], QImage.Format_RGB32)
        img.fill(QColor(idx, 0 if mode == ViewMode.COMP else 128, 0))
        img.setText("source_width", str(SRC[0]))
        img.setText("source_height", str(SRC[1]))
        return img

    def blocker(self, mode, idx):
        ev = threading.Event()
        self.block[f"/fake/{mode}/f{idx:03d}.png"] = ev
        return ev

    def resolve_fps(self, root, *, video_path=None, default=24.0):
        self.fps_threads.append(threading.get_ident())
        self.fps_gate.wait(5)
        return self.fps.get(os.path.basename(root), default)


@pytest.fixture
def env(monkeypatch):
    e = _Env()
    monkeypatch.setattr(async_decoder, "decode_frame", e.decode)
    monkeypatch.setattr(dv, "resolve_clip_fps", e.resolve_fps)
    e.panels = []
    yield e
    for ev in e.block.values():
        ev.set()
    e.fps_gate.set()
    for p in e.panels:
        p._input_viewer.shutdown()
        p._output_viewer.shutdown()
    async_decoder.preview_pool().waitForDone(5000)
    _APP.processEvents()
    for p in e.panels:
        if shiboken6.isValid(p):
            shiboken6.delete(p)


def _wait(cond, timeout=5.0):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        _APP.processEvents()
        if cond():
            return True
        time.sleep(0.005)
    _APP.processEvents()
    return cond()


def _clip(name):
    return ClipEntry(name=name, root_path=f"/tmp/ck_dual_{name}", state=ClipState.RAW)


def _make(env, name="a", blocks=()):
    evs = [env.blocker(m, i) for m, i in blocks]  # before any prefetch starts
    p = DualViewerPanel()
    p.resize(900, 400)
    p.show()
    _APP.processEvents()
    env.panels.append(p)
    idx = _index()
    p._input_viewer._build_frame_index = lambda clip, _i=idx: _i
    p._output_viewer._build_frame_index = lambda clip, _i=idx: _i
    p._output_viewer.set_view_mode("Comp")
    sc = p._scrubber
    sc._clock = lambda: env.t[0]
    sc._loop_enabled = lambda: True
    p.set_clip(_clip(name))
    p.shown = []
    p.frame_changed.connect(p.shown.append)
    p.evs = evs
    return p


def _at(env, n):  # clock value that lands on frame offset n at 24 fps
    return int(n * 1000 / 24) + 1


def _tick(env, p, n):
    env.t[0] = _at(env, n)
    p._scrubber._playback_tick()


def _all_cached(p):
    return all(p._input_viewer.has_cached_frame(i)
               and p._output_viewer.has_cached_frame(i) for i in range(N))


def _shown(p):
    return p._input_viewer._shown_stem, p._output_viewer._shown_stem


def _warm(p):
    for i in range(N):
        p._input_viewer.prepare_frame(i)
        p._output_viewer.prepare_frame(i)
        assert _wait(lambda i=i: p._input_viewer.has_cached_frame(i)
                     and p._output_viewer.has_cached_frame(i))


def test_scrubber_legacy_emit_disabled(env):
    p = _make(env)
    assert p._scrubber._legacy_playback_emit is False


def test_unequal_latency_never_shows_mismatched_stems(env):
    p = _make(env, blocks=[("Input", 3), ("Comp", 4)])
    p.toggle_playback()
    for n in range(1, 8):
        _tick(env, p, n)
        _wait(lambda: False, 0.03)
        a, b = _shown(p)
        assert a == b
    assert p.shown and all(s not in (3, 4) for s in p.shown)
    for ev in env.block.values():
        ev.set()


def test_cold_progress_then_warm_follows_clock(env):
    p = _make(env)
    p.toggle_playback()
    _tick(env, p, 1)
    assert _wait(lambda: p.shown)  # cold decode still advances
    _warm(p)
    p.shown.clear()
    for n in range(2, 8):
        _tick(env, p, n)
        assert p.shown[-1] == n  # warm: synchronous, exact clock frame
        assert _shown(p) == (n, n)
        assert p._scrubber.current_frame() == n


def test_stalled_pane_drops_frames_without_blocking(env):
    p = _make(env, blocks=[("Comp", 4), ("Comp", 5), ("Comp", 6)])
    evs = p.evs
    p.toggle_playback()
    for n in range(1, 7):
        _tick(env, p, n)
        _wait(lambda: False, 0.03)
    last = p.shown[-1]
    assert last < 4  # output stalled; pair retained, no wait
    for ev in evs:
        ev.set()
    _tick(env, p, 6)
    assert _wait(lambda: p.shown[-1] == 6)  # late frames present immediately
    assert _shown(p) == (6, 6)


def test_late_frame_presented_when_best_candidate_only(env):
    p = _make(env, blocks=[("Comp", 2)])
    ev = p.evs[0]
    p.toggle_playback()
    _tick(env, p, 5)
    assert _wait(lambda: p.shown and p.shown[-1] == 5)
    ev.set()
    assert _wait(lambda: not p._output_viewer._decoder.inflight_count())
    assert p.shown[-1] == 5  # frame 2 is behind the last pair: never shown


def test_loop_wrap_resets_last_presented(env):
    p = _make(env)
    _warm(p)
    p.toggle_playback()
    _tick(env, p, 9)
    assert p.shown[-1] == 9 and p._presenter.last_presented == 9
    _tick(env, p, 10)  # wraps to frame 0, cycle 1
    assert p.shown[-1] == 0 and p._presenter.last_presented == 0


def test_pause_requests_exact_frame_latest_wins(env):
    p = _make(env, blocks=[("Comp", 6)])
    p.toggle_playback()
    _tick(env, p, 6)
    _wait(lambda: False, 0.05)
    cur = p._scrubber.current_frame()
    p.shown.clear()
    p.toggle_playback()  # pause
    assert not p._scrubber.is_playing()
    assert p.shown == [cur]
    assert p._output_viewer._expect["primary"].stem == cur
    assert p._input_viewer._expect["primary"].stem == cur
    assert not p._presenter.active


def test_clip_switch_stops_playback_and_ignores_late_ready(env):
    p = _make(env, blocks=[("Comp", 6)])
    ev = p.evs[0]
    p.toggle_playback()
    _tick(env, p, 6)
    _wait(lambda: False, 0.05)
    p.set_clip(_clip("b"))
    assert not p._scrubber.is_playing()
    assert not p._presenter.active and p._presenter.last_presented is None
    p.shown.clear()
    ev.set()
    p._output_viewer.frame_ready_for_index.emit(7)
    p._input_viewer.frame_ready_for_index.emit(7)
    _wait(lambda: False, 0.1)
    assert p.shown == []


def test_fps_resolved_in_background_and_only_for_current_clip(env):
    env.fps = {"ck_dual_a": 60.0, "ck_dual_b": 30.0}
    p = _make(env, "a")  # resolve for a is gated in the pool
    assert p._scrubber._fps == 24.0
    p.set_clip(_clip("b"))
    env.fps_gate.set()
    async_decoder.preview_pool().waitForDone(5000)
    assert _wait(lambda: p._scrubber._fps == 30.0)
    assert p._scrubber._fps == 30.0  # a's 60 never applied
    assert env.fps_threads and all(t != _MAIN_THREAD for t in env.fps_threads)


def test_fps_result_of_replaced_clip_is_ignored_after_placeholder(env):
    env.fps = {"ck_dual_a": 60.0}
    p = _make(env, "a")
    p.show_placeholder("none")
    env.fps_gate.set()
    async_decoder.preview_pool().waitForDone(5000)
    _wait(lambda: False, 0.05)
    assert p._scrubber._fps == 24.0


def test_refresh_generated_assets_during_playback_keeps_playing(env):
    p = _make(env)
    p.set_in_out(2, 7)
    p._scrubber.set_frame(2)
    p.toggle_playback()
    _tick(env, p, 3)
    p.refresh_generated_assets()
    assert p._scrubber.is_playing()
    assert p.get_in_out() == (2, 7)
    assert p._presenter.active


def test_wipe_updates_only_from_presented_frames(env):
    p = _make(env)
    p._wipe_active = True
    calls = []
    p._load_wipe_images = lambda: calls.append(1)
    _wait(lambda: p._output_viewer._shown_stem == 0)
    calls.clear()
    # a raw decoder completion alone (legacy signal) must not refresh the wipe
    p._output_viewer._decoder.frame_decoded.emit(5, "Comp", QImage())
    assert calls == []
    p._on_scrubber_frame(4)
    assert _wait(lambda: p._output_viewer._shown_stem == 4)
    assert calls
