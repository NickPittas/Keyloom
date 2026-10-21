"""PreviewViewport async split/live preview and standalone playback (Task 10)."""
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
from ui.widgets.preview_viewport import PreviewViewport

_APP = QApplication.instance() or QApplication([])
_MAIN_THREAD = threading.get_ident()
SRC = (160, 90)


def _index(n):
    stems = [f"f{i:03d}" for i in range(n)]
    modes = (ViewMode.INPUT, ViewMode.COMP)
    availability = {m: set(stems) for m in modes}
    files = {(m, s): f"/fake/{m.value}/{s}.png" for m in modes for s in stems}
    return FrameIndex(stems=stems, availability=availability, stem_files=files)


class _Env:
    def __init__(self):
        self.calls = []
        self.block = {}
        self._lock = threading.Lock()
        self.t = [0]

    def decode(self, path, mode, *, input_exr_is_linear=False, target_size=None):
        with self._lock:
            self.calls.append((path, mode, threading.get_ident()))
        ev = self.block.get(path)
        if ev is not None:
            ev.wait(5)
        idx = int(re.search(r"f(\d+)", os.path.basename(path)).group(1))
        img = QImage(SRC[0], SRC[1], QImage.Format_RGB32)
        img.fill(QColor(idx, 0 if mode == ViewMode.COMP else 128, 0))
        img.setText("source_width", str(SRC[0]))
        img.setText("source_height", str(SRC[1]))
        return img

    def blocker(self, mode, idx):
        ev = threading.Event()
        self.block[f"/fake/{mode}/f{idx:03d}.png"] = ev
        return ev


@pytest.fixture
def env(monkeypatch):
    e = _Env()
    monkeypatch.setattr(async_decoder, "decode_frame", e.decode)
    e.vps = []
    yield e
    for ev in e.block.values():
        ev.set()
    for vp in e.vps:
        vp.shutdown()
    async_decoder.preview_pool().waitForDone(5000)
    _APP.processEvents()
    for vp in e.vps:
        if shiboken6.isValid(vp):
            shiboken6.delete(vp)


def _wait(cond, timeout=5.0):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        _APP.processEvents()
        if cond():
            return True
        time.sleep(0.005)
    _APP.processEvents()
    return cond()


def _red(vp, split=False):
    sv = vp._split_view
    img = sv._right_image if split else sv._single_image
    return None if img is None else QColor(img.pixel(0, 0)).red()


def _make(env, n=10, blocks=(), scrubber=True, index=None):
    for m, i in blocks:
        env.blocker(m, i)
    vp = PreviewViewport(show_scrubber=scrubber)
    vp.resize(400, 330)
    vp.show()
    _APP.processEvents()
    env.vps.append(vp)
    idx = index or _index(n)
    vp._build_frame_index = lambda clip, _i=idx: _i
    if scrubber:
        vp._scrubber._clock = lambda: env.t[0]
        vp._scrubber._loop_enabled = lambda: True
    vp.set_clip(ClipEntry(name="c", root_path="/tmp/ck_pp", state=ClipState.RAW))
    vp.set_view_mode("Comp")
    return vp


def _tick(env, vp, n):
    env.t[0] = int(n * 1000 / 24) + 1
    vp._scrubber._playback_tick()


def test_standalone_scrubber_uses_presenter_not_legacy_emit(env):
    vp = _make(env)
    assert vp._scrubber._legacy_playback_emit is False
    assert vp._presenter is not None


def test_split_left_decodes_off_gui_thread_and_sides_are_correct(env):
    vp = _make(env)
    vp.set_split_mode(True)
    vp.navigate_to_frame(3)
    assert _wait(lambda: _red(vp, split=True) == 3)
    left = vp._split_view._left_image
    assert QColor(left.pixel(0, 0)).green() == 128  # INPUT on the left
    assert QColor(vp._split_view._right_image.pixel(0, 0)).green() == 0
    assert all(c[2] != _MAIN_THREAD for c in env.calls)


def test_split_ready_requires_both_lanes(env):
    vp = _make(env, blocks=[("Input", 5)])
    vp.set_split_mode(True)
    vp.prepare_frame(5)
    assert _wait(lambda: vp._decoder.is_cached(
        vp._make_request("primary", 5, ViewMode.COMP, vp._target_size())))
    assert not vp.has_cached_frame(5)  # split-left lane still decoding
    assert vp.present_cached_frame(5) is False
    env.block[f"/fake/Input/f005.png"].set()
    assert _wait(lambda: vp.has_cached_frame(5))


def test_cold_decode_advances_despite_faster_clock(env):
    vp = _make(env, blocks=[("Comp", 2), ("Comp", 4), ("Comp", 5)])
    assert _wait(lambda: _red(vp) == 0)
    shown = []
    vp.frame_changed.connect(shown.append)
    vp._scrubber.toggle_playback()
    _tick(env, vp, 5)  # clock far ahead of the decoder
    assert _wait(lambda: shown and shown[-1] == 3)  # newest ready <= target
    assert _red(vp) == 3
    env.block["/fake/Comp/f004.png"].set()
    assert _wait(lambda: _red(vp) == 4)
    env.block["/fake/Comp/f005.png"].set()
    assert _wait(lambda: _red(vp) == 5)
    assert 2 not in shown  # blocked frame completing late is behind: dropped
    env.block["/fake/Comp/f002.png"].set()
    _wait(lambda: False, 0.05)
    assert _red(vp) == 5


def test_playback_clock_is_not_reanchored_by_presentation(env):
    vp = _make(env)
    vp._scrubber.toggle_playback()
    for n in range(1, 5):
        _tick(env, vp, n)
        assert _wait(lambda n=n: _red(vp) == n)
    assert vp._scrubber._anchor_ms == 0


def test_pause_and_seek_are_strict_latest_wins(env):
    vp = _make(env, blocks=[("Comp", 6), ("Comp", 8)])
    vp._scrubber.toggle_playback()
    _tick(env, vp, 6)
    _wait(lambda: False, 0.05)
    cur = vp._scrubber.current_frame()
    vp._scrubber.toggle_playback()  # pause: exact current frame, normal request
    assert vp._expect["primary"].stem == cur
    assert vp._playback_hint is False and vp._frame_cache._playhead is None
    # seek: only the newest request paints, even if an older one lands later
    vp._scrubber.frame_changed.emit(6)
    vp._scrubber.frame_changed.emit(7)
    assert _wait(lambda: _red(vp) == 7)
    env.block["/fake/Comp/f006.png"].set()
    _wait(lambda: False, 0.05)
    assert _red(vp) == 7


def test_playing_sets_playhead_hints_for_eviction(env):
    vp = _make(env)
    vp._scrubber.set_in_out(2, 7)
    vp._scrubber.toggle_playback()
    _tick(env, vp, 1)
    assert _wait(lambda: vp._presenter.last_presented is not None)
    assert vp._playback_hint and vp._loop_range == (2, 7)
    assert vp._frame_cache._loop == (2, 7)
    vp._scrubber.toggle_playback()
    assert vp._loop_range is None and vp._frame_cache._playhead is None


def test_temp_preview_decodes_off_thread_and_revision_invalidates(env, tmp_path):
    vp = _make(env)
    vp.navigate_to_frame(9)
    assert _wait(lambda: _red(vp) == 9)
    path = tmp_path / "f007.png"
    path.write_bytes(b"x" * 10)
    n0 = len(env.calls)
    vp.load_preview_from_file(str(path), "c", 9)
    assert _wait(lambda: _red(vp) == 7)
    assert env.calls[-1][2] != _MAIN_THREAD and len(env.calls) == n0 + 1
    path.write_bytes(b"y" * 25)  # reused temp name, new content
    os.utime(path, ns=(1, 2))
    vp.load_preview_from_file(str(path), "c", 9)
    assert _wait(lambda: len(env.calls) == n0 + 2)


def test_temp_preview_other_clip_and_annotation_mode_ignored(env, tmp_path):
    vp = _make(env)
    vp.navigate_to_frame(9)
    assert _wait(lambda: _red(vp) == 9)
    path = tmp_path / "f003.png"
    path.write_bytes(b"x")
    n0 = len(env.calls)
    vp.load_preview_from_file(str(path), "other", 9)
    vp.set_annotation_mode("fg")
    vp.load_preview_from_file(str(path), "c", 9)
    _wait(lambda: False, 0.1)
    assert len(env.calls) == n0 and _red(vp) == 9
