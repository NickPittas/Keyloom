import os
import threading
from types import SimpleNamespace

import pytest
from PySide6.QtCore import QCoreApplication, QThreadPool
from PySide6.QtWidgets import QApplication

import ui.workers.inference_context_worker as w
from backend.input_colour import InputColourSample
from ui.workers.inference_context_worker import (
    InferenceContext, InferenceContextController, build_inference_context)


@pytest.fixture(scope="module")
def app():
    return QApplication.instance() or QApplication([])


class Asset:
    def __init__(self, path, log, gate=None, dims=(64, 32), asset_type="sequence",
                 files=()):
        self.path, self.log, self.gate = path, log, gate
        self.asset_type = asset_type
        self._dimensions = None
        self._dims, self._files = dims, list(files)
        self.frame_count = len(self._files)

    def __deepcopy__(self, memo):  # share spy log/gate; copy the state
        a = Asset(self.path, self.log, self.gate, self._dims, self.asset_type,
                  self._files)
        a._dimensions = self._dimensions
        return a

    def _rec(self, what):
        self.log.append((what, threading.current_thread() is
                         QCoreApplication.instance().thread()))

    def get_frame_files(self):
        self._rec("files")
        return self._files

    def get_dimensions(self):
        self._rec("dims")
        if self.gate:
            self.gate.wait(5)
        return self._dims


def clip(root, asset):
    return SimpleNamespace(root_path=str(root), input_asset=asset)


def spin(app, cond, n=2000):
    import time
    end = time.time() + 10
    while not cond() and time.time() < end:
        app.processEvents()
        time.sleep(0.002)
    assert cond()


@pytest.fixture
def seq(tmp_path):
    d = tmp_path / "in"
    d.mkdir()
    names = []
    for i in range(3):
        (d / f"f{i}.exr").write_bytes(b"x" * (i + 1))
        names.append(f"f{i}.exr")
    return d, names


@pytest.fixture
def fake_sample(monkeypatch):
    log = []

    def sample(paths):
        log.append(threading.current_thread() is QCoreApplication.instance().thread())
        return InputColourSample(0.5, (0,), (64, 32), len(paths))
    monkeypatch.setattr(w, "sample_input_colour", sample)
    monkeypatch.setattr(w, "query_vram_budget_gib", lambda: 12.0)
    return log


def test_all_io_off_gui_thread(app, seq, fake_sample):
    d, names = seq
    log = []
    c = InferenceContextController()
    got = []
    c.ready.connect(got.append)
    c.request(clip(d.parent, Asset(str(d), log, files=names)))
    assert log == []  # nothing on request
    spin(app, lambda: got)
    ctx = got[0]
    assert isinstance(ctx, InferenceContext) and ctx.error is None
    assert (ctx.width, ctx.height, ctx.frame_count, ctx.budget_gib) == (64, 32, 3, 12.0)
    assert ctx.colour_sample.peak == 0.5
    assert log and all(not on_gui for _, on_gui in log)
    assert fake_sample == [False]
    assert len(ctx.source_revision) == 3
    c.close()


def test_stale_same_root_and_slow_old_superseded(app, seq, fake_sample):
    d, names = seq
    log, gate = [], threading.Event()
    c = InferenceContextController()
    got = []
    c.ready.connect(got.append)
    c.request(clip(d.parent, Asset(str(d), log, gate=gate, dims=(1, 1), files=names)))
    spin(app, lambda: ("dims", False) in log)
    c.request(clip(d.parent, Asset(str(d), log, dims=(2, 2), files=names)))
    c.request(clip(d.parent, Asset(str(d), log, dims=(3, 3), files=names)))  # coalesced
    gate.set()
    spin(app, lambda: got)
    spin(app, lambda: not c._running)
    app.processEvents()
    assert [(g.width) for g in got] == [3]
    c.close()


def test_overwrite_changes_revision(app, seq, fake_sample):
    d, names = seq
    a = clip(d.parent, Asset(str(d), [], files=names))
    first = build_inference_context(a)
    (d / "f1.exr").write_bytes(b"longer-content")
    second = build_inference_context(a)
    assert first.source_revision != second.source_revision


def test_close_ignores_completion(app, seq, fake_sample):
    d, names = seq
    gate, log = threading.Event(), []
    c = InferenceContextController()
    got = []
    c.ready.connect(got.append)
    c.request(clip(d.parent, Asset(str(d), log, gate=gate, files=names)))
    spin(app, lambda: ("dims", False) in log)
    c.close()
    c.request(clip(d.parent, Asset(str(d), log, files=names)))
    gate.set()
    spin(app, lambda: not c._running)
    app.processEvents()
    assert got == []


def test_unavailable_paths(app, tmp_path, monkeypatch):
    monkeypatch.setattr(w, "query_vram_budget_gib", lambda: None)
    assert build_inference_context(clip(tmp_path, None)).error == "no input"

    class Boom(Asset):
        def get_dimensions(self):
            raise RuntimeError("probe failed")
    ctx = build_inference_context(clip(tmp_path, Boom(str(tmp_path), [])))
    assert ctx.error and ctx.budget_gib is None

    def bad(_):
        raise OSError("decode")
    monkeypatch.setattr(w, "sample_input_colour", bad)
    d = tmp_path / "s"
    d.mkdir()
    (d / "a.exr").write_bytes(b"1")
    ctx = build_inference_context(clip(tmp_path, Asset(str(d), [], files=["a.exr"])))
    assert ctx.colour_sample is None and ctx.error is None and ctx.width == 64


def test_fake_pool_pending_coalescing(app, seq, fake_sample):
    d, names = seq

    class Pool:
        def __init__(self):
            self.started = []

        def start(self, t):
            self.started.append(t)

    pool = Pool()
    c = InferenceContextController(pool=pool)
    got = []
    c.ready.connect(got.append)
    for _ in range(3):
        c.request(clip(d.parent, Asset(str(d), [], files=names)))
    assert len(pool.started) == 1 and c._pending is not None
    pool.started[0].run()
    spin(app, lambda: len(pool.started) == 2)
    assert got == []  # first result stale
    pool.started[1].run()
    spin(app, lambda: got)
    assert len(got) == 1
    c.invalidate()
    c.close()
