"""Tests for ui.preview.async_decoder (bounded foreground/prefetch scheduler)."""

from __future__ import annotations

import threading
import time

import cv2
import numpy as np
import pytest
from PySide6.QtCore import QCoreApplication, QThreadPool
from PySide6.QtGui import QImage
from PySide6.QtWidgets import QApplication

from ui.preview import async_decoder
from ui.preview.async_decoder import AsyncDecoder, DecodeRequest
from ui.preview.frame_cache import FrameCache
from ui.preview.frame_index import ViewMode

_APP = QApplication.instance() or QApplication([])


def _img(w: int = 4, h: int = 4) -> QImage:
    img = QImage(w, h, QImage.Format.Format_RGB888)
    img.fill(0)
    return img


def _wait(cond, timeout: float = 5.0) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        QCoreApplication.processEvents()
        if cond():
            return True
        time.sleep(0.002)
    QCoreApplication.processEvents()
    return cond()


def _req(i: int, lane: str = "primary", **kw) -> DecodeRequest:
    return DecodeRequest(lane, i, ViewMode.FG, f"/nonexistent/frame_{i:06d}.exr", **kw)


class _Fake:
    """Blockable fake decode_frame recording threads, order and concurrency."""

    def __init__(self) -> None:
        self.gate = threading.Event()
        self.lock = threading.Lock()
        self.calls: list[str] = []
        self.threads: set[int] = set()
        self.active = 0
        self.peak = 0

    def __call__(self, path, mode, *, input_exr_is_linear=False, target_size=None):
        with self.lock:
            self.calls.append(path)
            self.threads.add(threading.get_ident())
            self.active += 1
            self.peak = max(self.peak, self.active)
        try:
            assert self.gate.wait(5)
            return _img()
        finally:
            with self.lock:
                self.active -= 1


@pytest.fixture
def fake(monkeypatch):
    f = _Fake()
    monkeypatch.setattr(async_decoder, "decode_frame", f)
    decoders: list[AsyncDecoder] = []
    f.make = lambda cache=None: decoders.append(AsyncDecoder(None, cache)) or decoders[-1]
    yield f
    f.gate.set()
    assert async_decoder.preview_pool().waitForDone(5000)
    QCoreApplication.processEvents()
    for d in decoders:
        d.close()
    assert async_decoder._GATE.count == 0


def _drain(dec: AsyncDecoder) -> None:
    assert _wait(lambda: dec.inflight_count() == 0)


def test_preview_pool_is_not_global_and_sized():
    pool = async_decoder.preview_pool()
    assert pool is not QThreadPool.globalInstance()
    assert pool is async_decoder.preview_pool()
    assert pool.maxThreadCount() == async_decoder.preview_thread_count()
    assert 1 <= pool.maxThreadCount() <= 32


def test_global_pool_not_modified():
    before = QThreadPool.globalInstance().maxThreadCount()
    dec = AsyncDecoder()
    dec.close()
    assert QThreadPool.globalInstance().maxThreadCount() == before


def test_inflight_cap_respected(fake, monkeypatch):
    monkeypatch.setattr(async_decoder, "MAX_INFLIGHT_DECODES", 3)
    dec = fake.make(FrameCache(10 * 1024 * 1024))
    cap = async_decoder.max_inflight()
    assert cap == min(3, async_decoder.preview_thread_count())
    for i in range(100):
        dec.request_foreground(_req(i, lane=f"lane{i % 5}"))
    dec.prefetch([_req(1000 + i) for i in range(100)])
    assert _wait(lambda: fake.active == cap)
    assert dec.inflight_count() == cap
    fake.gate.set()
    _drain(dec)
    assert fake.peak <= cap


def test_latest_wins_per_lane(fake, monkeypatch):
    monkeypatch.setattr(async_decoder, "MAX_INFLIGHT_DECODES", 1)
    dec = fake.make()
    got: list[int] = []
    dec.frame_ready.connect(lambda lane, i, m, img, g: got.append(i))
    dec.request_foreground(_req(0))
    assert _wait(lambda: len(fake.calls) == 1)
    for i in range(1, 200):
        dec.request_foreground(_req(i))
    fake.gate.set()
    _drain(dec)
    assert [p[-10:-4] for p in fake.calls] == ["000000", "000199"]
    assert got == [0, 199]


def test_lanes_are_independent(fake, monkeypatch):
    monkeypatch.setattr(async_decoder, "MAX_INFLIGHT_DECODES", 1)
    dec = fake.make()
    got: list[tuple[str, int]] = []
    dec.frame_ready.connect(lambda lane, i, m, img, g: got.append((lane, i)))
    dec.request_foreground(_req(0, "primary"))
    assert _wait(lambda: len(fake.calls) == 1)
    dec.request_foreground(_req(1, "primary"))
    dec.request_foreground(_req(2, "primary"))
    dec.request_foreground(_req(7, "split_input"))
    fake.gate.set()
    _drain(dec)
    assert sorted(got) == [("primary", 0), ("primary", 2), ("split_input", 7)]


def test_foreground_before_prefetch_and_no_duplicates(fake, monkeypatch):
    monkeypatch.setattr(async_decoder, "MAX_INFLIGHT_DECODES", 1)
    dec = fake.make()
    dec.request_foreground(_req(0))
    assert _wait(lambda: len(fake.calls) == 1)
    dec.prefetch([_req(1), _req(2), _req(3), _req(0)])  # 0 is in flight
    dec.request_foreground(_req(9))
    dec.prefetch([_req(1), _req(2), _req(1)])  # replaces queue, dedupes
    fake.gate.set()
    _drain(dec)
    order = [int(p[-10:-4]) for p in fake.calls]
    assert order == [0, 9, 1, 2]


def test_prefetch_queue_bounded(fake, monkeypatch):
    monkeypatch.setattr(async_decoder, "MAX_INFLIGHT_DECODES", 1)
    dec = fake.make()
    dec.request_foreground(_req(0))
    assert _wait(lambda: len(fake.calls) == 1)
    dec.prefetch([_req(i) for i in range(1, 500)])
    fake.gate.set()
    _drain(dec)
    assert len(fake.calls) == 1 + async_decoder.MAX_PREFETCH_QUEUE
    assert dec.cache.contains(async_decoder.request_key(_req(64)))
    assert not dec.cache.contains(async_decoder.request_key(_req(65)))


def test_skips_cached_and_cached_foreground_is_synchronous(fake):
    fake.gate.set()
    dec = fake.make()
    got: list[int] = []
    dec.frame_ready.connect(lambda lane, i, m, img, g: got.append(i))
    dec.request_foreground(_req(1))
    _drain(dec)
    assert got == [1] and dec.is_cached(_req(1))
    assert dec.cached_image(_req(1)) is not None
    dec.request_foreground(_req(1))
    assert got == [1, 1]  # synchronous, no QCoreApplication spin
    n = len(fake.calls)
    dec.prefetch([_req(1)])
    _drain(dec)
    assert len(fake.calls) == n


def test_invalidate_discards_inflight_and_drops_pending(fake, monkeypatch):
    monkeypatch.setattr(async_decoder, "MAX_INFLIGHT_DECODES", 1)
    dec = fake.make()
    got: list[int] = []
    dec.frame_ready.connect(lambda lane, i, m, img, g: got.append(i))
    gen0 = dec.cache.generation
    dec.request_foreground(_req(0))
    assert _wait(lambda: len(fake.calls) == 1)
    dec.prefetch([_req(1), _req(2)])
    dec.invalidate()
    assert dec.cache.generation == gen0 + 1
    fake.gate.set()
    _drain(dec)
    assert len(fake.calls) == 1
    assert got == [] and len(dec.cache) == 0
    # new generation still works
    dec.request_foreground(_req(3))
    assert _wait(lambda: got == [3])


def test_cancel_pending_keeps_inflight(fake, monkeypatch):
    monkeypatch.setattr(async_decoder, "MAX_INFLIGHT_DECODES", 1)
    dec = fake.make()
    got: list[int] = []
    dec.frame_ready.connect(lambda lane, i, m, img, g: got.append(i))
    dec.request_foreground(_req(0))
    assert _wait(lambda: len(fake.calls) == 1)
    dec.request_foreground(_req(1, "split_input"))
    dec.prefetch([_req(2)])
    dec.cancel_pending()
    fake.gate.set()
    _drain(dec)
    assert got == [0]


def test_threading_decode_on_worker_completion_on_gui(fake):
    fake.gate.set()
    dec = fake.make()
    gui = threading.get_ident()
    ready_threads: list[int] = []
    legacy_threads: list[int] = []
    legacy: list[tuple] = []
    dec.frame_ready.connect(
        lambda *a: ready_threads.append(threading.get_ident()))

    def on_legacy(i, m, img):
        legacy_threads.append(threading.get_ident())
        legacy.append((i, m, img is not None))

    dec.frame_decoded.connect(on_legacy)
    dec.request_foreground(_req(1))
    dec.request_decode("/nonexistent/legacy.exr", ViewMode.COMP, 5)
    assert _wait(lambda: ready_threads and legacy)
    assert ready_threads == [gui] and legacy_threads == [gui]
    assert legacy == [(5, "Comp", True)]
    assert gui not in fake.threads and fake.threads


def test_legacy_request_decode_latest_wins(fake, monkeypatch):
    monkeypatch.setattr(async_decoder, "MAX_INFLIGHT_DECODES", 1)
    dec = fake.make()
    got: list[int] = []
    dec.frame_decoded.connect(lambda i, m, img: got.append(i))
    for i in range(5):
        dec.request_decode(f"/nonexistent/{i}.exr", ViewMode.FG, i)
        if i == 0:
            assert _wait(lambda: len(fake.calls) == 1)
    fake.gate.set()
    _drain(dec)
    assert got == [4]


def test_decode_failure_emits_none_and_is_not_cached(monkeypatch):
    def boom(*a, **k):
        raise ValueError("bad")

    monkeypatch.setattr(async_decoder, "decode_frame", boom)
    dec = AsyncDecoder()
    got: list = []
    dec.frame_ready.connect(lambda lane, i, m, img, g: got.append(img))
    dec.request_foreground(_req(1))
    assert _wait(lambda: got == [None])
    assert not dec.is_cached(_req(1))
    assert async_decoder.preview_pool().waitForDone(5000)
    dec.close()


def test_close_with_inflight_is_safe(fake):
    dec = fake.make()
    got: list = []
    dec.frame_ready.connect(lambda *a: got.append(a))
    dec.request_foreground(_req(1))
    assert _wait(lambda: len(fake.calls) == 1)
    dec.close()
    dec.request_foreground(_req(2))  # ignored after close
    fake.gate.set()
    assert async_decoder.preview_pool().waitForDone(5000)
    QCoreApplication.processEvents()
    assert got == [] and len(fake.calls) == 1


# ── sequential video reader ──────────────────────────────────────


@pytest.fixture
def video(tmp_path):
    path = str(tmp_path / "clip.avi")
    w = cv2.VideoWriter(path, cv2.VideoWriter_fourcc(*"MJPG"), 24, (64, 48))
    assert w.isOpened()
    for i in range(12):
        w.write(np.full((48, 64, 3), i * 20, np.uint8))
    w.release()
    return path


def _vreq(i: int, video_path: str, lane: str = "primary") -> DecodeRequest:
    return DecodeRequest(lane, i, ViewMode.INPUT, "", video_path, i)


def _gray(img: QImage) -> int:
    return img.pixelColor(10, 10).red()


def test_sequential_video_reader_reads_forward_then_reseeks(video):
    dec = AsyncDecoder()
    got: dict[int, QImage] = {}
    dec.frame_ready.connect(lambda lane, i, m, img, g: got.__setitem__(i, img))
    try:
        for i in (0, 1, 2, 3, 8, 9, 4):  # serial; 8 and 4 force a re-seek
            dec.request_foreground(_vreq(i, video))
            assert _wait(lambda i=i: i in got), i
            assert got[i] is not None
            assert abs(_gray(got[i]) - i * 20) <= 8, i
        reader = dec._readers[("primary", video)]
        assert reader._next == 5
        dec.invalidate()
        assert dec._readers == {}
        assert reader._cap is None and reader._closed
    finally:
        assert async_decoder.preview_pool().waitForDone(5000)
        dec.close()


def test_video_reader_one_capture_per_lane_and_closed_on_close(video):
    dec = AsyncDecoder()
    got: list[int] = []
    dec.frame_ready.connect(lambda lane, i, m, img, g: got.append(i))
    dec.request_foreground(_vreq(0, video, "primary"))
    dec.request_foreground(_vreq(0, video, "split_input"))
    assert _wait(lambda: len(got) == 2)
    assert set(dec._readers) == {("primary", video), ("split_input", video)}
    readers = list(dec._readers.values())
    assert async_decoder.preview_pool().waitForDone(5000)
    dec.close()
    assert all(r._cap is None for r in readers)


def test_busy_video_reader_falls_back_to_independent_decode(monkeypatch, video):
    calls: list[int] = []
    monkeypatch.setattr(
        async_decoder, "decode_video_frame",
        lambda vp, idx, **k: calls.append(idx) or _img())
    dec = AsyncDecoder()
    got: list[int] = []
    dec.frame_ready.connect(lambda lane, i, m, img, g: got.append(i))
    reader = dec._reader_for(_vreq(3, video))
    assert reader.lock.acquire(blocking=False)
    try:
        dec.request_foreground(_vreq(3, video))
        assert _wait(lambda: got == [3])
        assert calls == [3]
    finally:
        reader.lock.release()
        assert async_decoder.preview_pool().waitForDone(5000)
        dec.close()
