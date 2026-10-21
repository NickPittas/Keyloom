"""Tests for the preview thread policy (pool sizing, OpenCV threads in app)."""

from __future__ import annotations

import sys
import types

from PySide6.QtCore import QThreadPool
from PySide6.QtWidgets import QApplication

from ui import app
from ui.preview import async_decoder

_APP = QApplication.instance() or QApplication([])


def test_pool_size_uses_affinity_capped_at_32(monkeypatch):
    monkeypatch.setattr(async_decoder.sys, "platform", "linux")
    monkeypatch.setattr(async_decoder.os, "sched_getaffinity",
                        lambda _pid: set(range(64)), raising=False)
    assert async_decoder.preview_thread_count() == 32
    monkeypatch.setattr(async_decoder.os, "sched_getaffinity",
                        lambda _pid: set(range(6)), raising=False)
    assert async_decoder.preview_thread_count() == 6


def test_pool_size_without_affinity_falls_back_to_cpu_count(monkeypatch):
    monkeypatch.setattr(async_decoder.sys, "platform", "linux")

    def _raise(_pid):
        raise OSError

    monkeypatch.setattr(async_decoder.os, "sched_getaffinity", _raise, raising=False)
    monkeypatch.setattr(async_decoder.os, "cpu_count", lambda: 8)
    assert async_decoder.preview_thread_count() == 8


def test_windows_keeps_small_pool(monkeypatch):
    monkeypatch.setattr(async_decoder.sys, "platform", "win32")
    assert async_decoder.preview_thread_count() == 2


def test_inflight_cap_never_exceeds_pool_or_12(monkeypatch):
    monkeypatch.setattr(async_decoder, "preview_thread_count", lambda: 32)
    assert async_decoder.max_inflight() == 12
    monkeypatch.setattr(async_decoder, "preview_thread_count", lambda: 2)
    assert async_decoder.max_inflight() == 2


def test_preview_pool_is_separate_from_global_pool():
    before = QThreadPool.globalInstance().maxThreadCount()
    pool = async_decoder.preview_pool()
    assert pool is not QThreadPool.globalInstance()
    assert QThreadPool.globalInstance().maxThreadCount() == before


def _fake_cv2(monkeypatch):
    calls: list[int] = []
    fake = types.SimpleNamespace(setNumThreads=calls.append)
    monkeypatch.setitem(sys.modules, "cv2", fake)
    return calls


def test_linux_sets_opencv_single_thread_once(monkeypatch):
    calls = _fake_cv2(monkeypatch)
    monkeypatch.setattr(app.sys, "platform", "linux")
    app._configure_runtime_backends()
    assert calls == [1]


def test_non_linux_posix_leaves_opencv_untouched(monkeypatch):
    calls = _fake_cv2(monkeypatch)
    monkeypatch.setattr(app.sys, "platform", "darwin")
    app._configure_runtime_backends()
    assert calls == []
