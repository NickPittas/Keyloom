"""PreviewViewport cached background navigation (Tasks 5+9)."""
import os

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

import re
import threading
import time

import pytest

pytest.importorskip("PySide6", reason="PySide6 not installed")

import shiboken6
from PySide6.QtCore import QPointF
from PySide6.QtGui import QColor, QImage
from PySide6.QtWidgets import QApplication

from backend.clip_state import ClipEntry, ClipState
from ui.preview import async_decoder
from ui.preview.frame_index import FrameIndex, ViewMode
from ui.widgets import preview_viewport as pv
from ui.widgets.preview_viewport import PreviewViewport

_APP = QApplication.instance() or QApplication([])
_MAIN_THREAD = threading.get_ident()
SRC = (640, 360)


def _stem(i):
    return f"f{i:03d}"


def _index(n, modes=(ViewMode.INPUT, ViewMode.COMP), missing=()):
    stems = [_stem(i) for i in range(n)]
    availability = {m: set() for m in modes}
    files = {}
    for m in modes:
        for i, s in enumerate(stems):
            if (m, i) in missing:
                continue
            availability[m].add(s)
            files[(m, s)] = f"/fake/{m.value}/{s}.png"
    return FrameIndex(stems=stems, availability=availability, stem_files=files)


class _Env:
    """Fake decoder backend: records calls, can block specific paths."""

    def __init__(self):
        self.calls = []          # (path, mode, target, thread_ident)
        self.block = {}          # path -> threading.Event
        self._lock = threading.Lock()

    def decode(self, path, mode, *, input_exr_is_linear=False, target_size=None):
        with self._lock:
            self.calls.append((path, mode, target_size, threading.get_ident()))
        ev = self.block.get(path)
        if ev is not None:
            ev.wait(5)
        idx = int(re.search(r"f(\d+)", path).group(1))
        if target_size is None:
            w, h = SRC
        else:
            sc = min(target_size[0] / SRC[0], target_size[1] / SRC[1], 1.0)
            w, h = max(1, round(SRC[0] * sc)), max(1, round(SRC[1] * sc))
        img = QImage(w, h, QImage.Format_RGB32)
        img.fill(QColor(idx % 256, 0 if mode == ViewMode.COMP else 128, 0))
        img.setText("source_width", str(SRC[0]))
        img.setText("source_height", str(SRC[1]))
        return img

    def count(self, pred=lambda c: True):
        with self._lock:
            return sum(1 for c in self.calls if pred(c))

    def release_all(self):
        for ev in self.block.values():
            ev.set()


@pytest.fixture
def env(monkeypatch):
    e = _Env()
    monkeypatch.setattr(async_decoder, "decode_frame", e.decode)
    viewports = []
    e.viewports = viewports
    yield e
    e.release_all()
    for vp in viewports:
        vp.shutdown()
    async_decoder.preview_pool().waitForDone(5000)
    _APP.processEvents()
    for vp in viewports:
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


def _color_index(vp):
    img = vp._split_view._single_image
    return None if img is None else QColor(img.pixel(0, 0)).red()


def _make(env, n=10, name="clip", **index_kw):
    vp = PreviewViewport(show_scrubber=False)
    vp.resize(400, 330)
    vp.show()
    _APP.processEvents()
    env.viewports.append(vp)
    index = _index(n, **index_kw)
    vp._build_frame_index = lambda clip, _i=index: _i
    clip = ClipEntry(name=name, root_path=f"/tmp/ck_nav_{name}", state=ClipState.RAW)
    vp.set_clip(clip)
    vp.set_view_mode("Comp")
    return vp, clip, index


def _settled(env, vp, idx):
    assert _wait(lambda: _color_index(vp) == idx)


def test_navigation_decodes_off_gui_thread_and_requests_canvas_size(env):
    vp, _clip, _idx = _make(env)
    vp.navigate_to_frame(3)
    _settled(env, vp, 3)
    canvas = vp._split_view.canvas_physical_size()
    fg = [c for c in env.calls if c[0].endswith("f003.png") and c[1] == ViewMode.COMP]
    assert fg and all(c[3] != _MAIN_THREAD for c in env.calls)
    assert fg[0][2] == canvas


def test_cached_frame_presents_synchronously(env):
    vp, _clip, _idx = _make(env)
    presented = []
    vp.frame_presented.connect(presented.append)
    vp.navigate_to_frame(2)
    _settled(env, vp, 2)
    vp.navigate_to_frame(4)
    _settled(env, vp, 4)
    n_calls = env.count()
    presented.clear()
    vp.navigate_to_frame(2)  # no event processing: must already be painted
    assert _color_index(vp) == 2
    assert presented == [2]
    assert env.count(lambda c: c[0].endswith("f002.png") and c[1] == ViewMode.COMP) == 1


def test_delayed_old_index_result_does_not_paint(env):
    vp, _clip, _idx = _make(env)
    env.block["/fake/Comp/f005.png"] = threading.Event()
    vp.navigate_to_frame(5)
    assert _wait(lambda: env.count(lambda c: c[0].endswith("f005.png")) >= 1)
    vp.navigate_to_frame(6)
    _settled(env, vp, 6)
    env.release_all()
    async_decoder.preview_pool().waitForDone(5000)
    _APP.processEvents()
    assert _color_index(vp) == 6
    assert vp._split_view._annotation_stem_idx == 6


def test_delayed_old_mode_result_does_not_paint(env):
    vp, _clip, _idx = _make(env)
    vp.navigate_to_frame(2)
    _settled(env, vp, 2)
    env.block["/fake/Comp/f003.png"] = threading.Event()
    vp.navigate_to_frame(3)
    assert _wait(lambda: env.count(lambda c: c[0].endswith("Comp/f003.png")) >= 1)
    vp.set_view_mode("Input")
    assert _wait(lambda: vp._split_view._single_image is not None
                 and QColor(vp._split_view._single_image.pixel(0, 0)).green() == 128)
    env.release_all()
    async_decoder.preview_pool().waitForDone(5000)
    _APP.processEvents()
    img = vp._split_view._single_image
    assert QColor(img.pixel(0, 0)).green() == 128 and QColor(img.pixel(0, 0)).red() == 3


def test_clip_switch_invalidates_and_drops_old_results(env):
    vp, _clip, _idx = _make(env)
    env.block["/fake/Comp/f002.png"] = threading.Event()
    vp.navigate_to_frame(2)
    assert _wait(lambda: env.count(lambda c: c[0].endswith("Comp/f002.png")) >= 1)
    gen = vp._frame_cache.generation
    vp.set_clip(ClipEntry(name="other", root_path="/tmp/ck_nav_other", state=ClipState.RAW))
    assert vp._frame_cache.generation > gen
    assert vp._frame_cache.bytes_used == 0 or len(vp._frame_cache) <= 1
    env.release_all()
    async_decoder.preview_pool().waitForDone(5000)
    _APP.processEvents()
    # only the new clip's frame 0 may be shown; the stale f002 never paints
    assert _color_index(vp) in (None, 0)


def test_invalidate_cache_clears_and_bumps_generation(env):
    vp, _clip, _idx = _make(env)
    vp.navigate_to_frame(1)
    _settled(env, vp, 1)
    gen = vp._frame_cache.generation
    vp.invalidate_cache()
    assert vp._frame_cache.generation == gen + 1
    assert len(vp._frame_cache) == 0
    assert not vp.has_cached_frame(1)


def test_two_viewports_have_separate_caches(env):
    a, _c, _i = _make(env, name="a")
    b, _c2, _i2 = _make(env, name="b")
    assert a._frame_cache is not b._frame_cache
    assert a._decoder.cache is a._frame_cache
    a.navigate_to_frame(1)
    _settled(env, a, 1)
    b.navigate_to_frame(1)
    _settled(env, b, 1)
    gen_b = b._frame_cache.generation
    n_b = len(b._frame_cache)
    a.invalidate_cache()
    assert len(a._frame_cache) == 0
    assert b._frame_cache.generation == gen_b and len(b._frame_cache) == n_b


def _decoded_indices(env, mode=ViewMode.COMP):
    with env._lock:
        return {int(re.search(r"f(\d+)", c[0]).group(1)) for c in env.calls if c[1] == mode}


def test_prefetch_window_around_index_and_clamping(env):
    vp, _clip, _idx = _make(env, n=100)
    vp.navigate_to_frame(50)
    want = set(range(50 - pv.PREFETCH_BEHIND, 50 + pv.PREFETCH_AHEAD + 1))
    assert _wait(lambda: want <= _decoded_indices(env))
    # anything else decoded is only the initial clip-load window (0..32)
    assert _decoded_indices(env) - want <= set(range(0, pv.PREFETCH_AHEAD + 1))
    assert not any(i > 82 for i in _decoded_indices(env))

    env.calls.clear()
    vp.navigate_to_frame(95)
    want = set(range(95 - pv.PREFETCH_BEHIND, 100))
    assert _wait(lambda: want <= _decoded_indices(env))
    assert max(_decoded_indices(env)) == 99


def test_prefetch_order_is_ahead_first_by_distance(env):
    vp, _clip, _idx = _make(env, n=100)
    vp._requested_mode = ViewMode.COMP
    reqs = vp._window_requests(50, (320, 240))
    stems = [r.stem_index for r in reqs]
    assert stems[: pv.PREFETCH_AHEAD] == list(range(51, 51 + pv.PREFETCH_AHEAD))
    assert stems[pv.PREFETCH_AHEAD:] == [49 - d for d in range(pv.PREFETCH_BEHIND)]


def test_zoom_in_requests_full_resolution_once(env):
    vp, _clip, _idx = _make(env, n=3)
    vp.navigate_to_frame(1)
    _settled(env, vp, 1)
    assert vp._shown_raster[0] < SRC[0]
    full = lambda c: c[0].endswith("Comp/f001.png") and c[2] is None
    assert env.count(full) == 0
    sv = vp._split_view
    sv.set_view_transform(2.0, QPointF(0, 0))
    sv.set_view_transform(3.0, QPointF(0, 0))
    sv.set_view_transform(3.0, QPointF(10, 5))  # pan only
    sv.set_view_transform(4.0, QPointF(10, 5))
    assert _wait(lambda: vp._shown_raster == SRC)
    assert env.count(full) == 1
    # navigating while zoomed requests full-res directly (no fit decode first)
    env.calls.clear()
    vp.navigate_to_frame(2)
    assert _wait(lambda: vp._shown_raster == SRC and _color_index(vp) == 2)
    assert env.count(lambda c: c[0].endswith("Comp/f002.png") and c[2] is not None) == 0


def test_zoom_keeps_old_image_until_full_res_arrives(env):
    vp, _clip, _idx = _make(env, n=3)
    vp.navigate_to_frame(1)
    _settled(env, vp, 1)
    fit_img = vp._split_view._single_image
    env.block["/fake/Comp/f001.png"] = threading.Event()
    vp._split_view.set_view_transform(2.0, QPointF(0, 0))
    assert _wait(lambda: env.count(lambda c: c[0].endswith("f001.png") and c[2] is None) == 1)
    assert vp._split_view._single_image is fit_img
    env.release_all()
    assert _wait(lambda: vp._shown_raster == SRC)


def test_input_fallback_keeps_requested_mode(env):
    vp, _clip, _idx = _make(env, missing={(ViewMode.COMP, 2)})
    vp.navigate_to_frame(2)
    assert _wait(lambda: _color_index(vp) == 2)
    img = vp._split_view._single_image
    assert QColor(img.pixel(0, 0)).green() == 128  # INPUT image displayed
    assert vp._current_mode == ViewMode.COMP
    assert vp._requested_mode == ViewMode.COMP
    assert any(c[0] == "/fake/Input/f002.png" for c in env.calls)


def test_annotation_stem_follows_shown_frame(env):
    vp, _clip, _idx = _make(env, n=80)
    vp.navigate_to_frame(2)
    _settled(env, vp, 2)
    env.block["/fake/Comp/f070.png"] = threading.Event()
    vp.navigate_to_frame(70)
    assert _wait(lambda: env.count(lambda c: c[0].endswith("f070.png")) >= 1)
    assert vp.current_stem_index == 70
    assert vp._split_view._annotation_stem_idx == 2  # still showing frame 2
    env.release_all()
    assert _wait(lambda: vp._split_view._annotation_stem_idx == 70)


def test_prepare_has_present_cached_frame_api(env):
    vp, _clip, _idx = _make(env, n=80)
    vp.navigate_to_frame(1)
    _settled(env, vp, 1)
    ready, presented = [], []
    vp.frame_ready_for_index.connect(ready.append)
    vp.frame_presented.connect(presented.append)
    assert vp.present_cached_frame(60) is False
    assert _color_index(vp) == 1 and vp.current_stem_index == 1
    vp.prepare_frame(60)
    assert _wait(lambda: vp.has_cached_frame(60))
    assert 60 in ready
    assert _color_index(vp) == 1  # display unchanged
    assert vp.current_stem_index == 1
    presented.clear()
    assert vp.present_cached_frame(60) is True
    assert _color_index(vp) == 60 and presented == [60]
    assert vp.current_stem_index == 60


def test_split_left_lane_is_async_and_shares_cache(env):
    vp, _clip, _idx = _make(env)
    vp.set_split_mode(True)
    vp.navigate_to_frame(3)
    sv = vp._split_view
    assert _wait(lambda: sv._left_image is not None and sv._right_image is not None
                 and QColor(sv._left_image.pixel(0, 0)).red() == 3
                 and QColor(sv._right_image.pixel(0, 0)).red() == 3)
    assert QColor(sv._left_image.pixel(0, 0)).green() == 128   # INPUT on the left
    assert QColor(sv._right_image.pixel(0, 0)).green() == 0    # COMP on the right
    assert all(c[3] != _MAIN_THREAD for c in env.calls)
    assert vp.has_cached_frame(3)


def test_frame_decoded_reemitted_for_wipe_listeners(env):
    vp, _clip, _idx = _make(env)
    seen = []
    vp._decoder.frame_decoded.connect(lambda s, m, img: seen.append((s, m)))
    vp.navigate_to_frame(4)
    _settled(env, vp, 4)
    assert (4, ViewMode.COMP.value) in seen


class _ExplodingAsset:
    frame_count = 12
    asset_type = "sequence"
    _dimensions = None

    def get_dimensions(self):
        raise AssertionError("GUI thread must not read pixels for dimensions")


def test_clip_info_does_not_read_pixels_and_uses_decoded_source_size(env):
    vp = PreviewViewport(show_scrubber=False)
    env.viewports.append(vp)
    index = _index(3)
    vp._build_frame_index = lambda clip: index
    clip = ClipEntry(name="info", root_path="/tmp/ck_nav_info", state=ClipState.RAW,
                     input_asset=_ExplodingAsset())
    vp.set_clip(clip)
    assert "12 frames" in vp._clip_info.text()
    assert "640x360" not in vp._clip_info.text()
    assert _wait(lambda: "640x360" in vp._clip_info.text())
