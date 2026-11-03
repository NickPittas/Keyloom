import os
import threading
import time
import uuid

import cv2
import numpy as np
import pytest

from backend import frame_encoding as fe
from backend.frame_encoding import EncodingCancelled, EncodingError, FrameEncoder


def _img(n=16):
    return np.random.randint(0, 255, (n, n, 3), dtype=np.uint8)


def _stall_exr_job(path, img, compression):
    with open(os.path.join(path, f".{uuid.uuid4().hex}.exr"), "wb"):
        pass
    while True:
        time.sleep(1)


def test_png_threads_unicode_path_and_completed(tmp_path):
    paths = [str(tmp_path / f"främe_é_{i:04d}.png") for i in range(8)]
    with FrameEncoder(workers=3) as enc:
        for p in paths:
            enc.submit_image(p, _img())
    assert sorted(enc.completed) == sorted(paths)
    assert all(cv2.imdecode(np.fromfile(p, np.uint8), -1).shape == (16, 16, 3) for p in paths)


def test_exr_spawned_process(tmp_path):
    pytest.importorskip("OpenEXR")
    p = str(tmp_path / "främe_é.exr")
    with FrameEncoder(workers=1) as enc:
        enc.submit_exr(p, np.random.rand(8, 8, 4).astype(np.float32))
    assert os.path.getsize(p) > 0 and enc.completed == [p]


def test_first_error_propagates(tmp_path):
    enc = FrameEncoder(workers=1)
    enc.submit_image(str(tmp_path / "missing_dir" / "x.png"), _img())
    with pytest.raises(EncodingError):
        enc.join()


def test_frame_over_budget_reports_requirement(tmp_path):
    enc = FrameEncoder(workers=1, budget_bytes=100)
    with pytest.raises(EncodingError, match="needs"):
        enc.submit_image(str(tmp_path / "x.png"), _img(64))
    enc.cancel()


def test_backpressure_never_exceeds_budget(tmp_path, monkeypatch):
    seen, lock = [], threading.Lock()
    enc = FrameEncoder(workers=4, budget_bytes=16 * 16 * 3 * 2 * 2)  # two frames
    real = fe._write_image_job

    def slow(path, img, params):
        with lock:
            seen.append(enc._inflight)
        threading.Event().wait(0.02)
        return real(path, img, params)

    monkeypatch.setattr(fe, "_write_image_job", slow)
    with enc:
        for i in range(10):
            enc.submit_image(str(tmp_path / f"{i}.png"), _img())
    assert max(seen) <= enc.budget and len(enc.completed) == 10


def test_cancel_stops_submission_and_joins(tmp_path):
    enc = FrameEncoder(workers=1)
    enc.submit_image(str(tmp_path / "a.png"), _img())
    enc.cancel()
    assert enc._threads is None
    with pytest.raises(EncodingCancelled):
        enc.submit_image(str(tmp_path / "b.png"), _img())


def test_cancel_terminates_stalled_spawned_worker():
    import tempfile

    with tempfile.TemporaryDirectory() as folder:
        enc = FrameEncoder(workers=1)
        enc._procs = fe.ProcessPoolExecutor(
            1, mp_context=fe.multiprocessing.get_context("spawn"))
        future = enc._procs.submit(_stall_exr_job, folder, _img(), "dwab")
        enc._futures.append(future)
        deadline = time.monotonic() + 10
        while not enc._procs._processes and time.monotonic() < deadline:
            time.sleep(0.01)
        children = list(enc._procs._processes.values())
        assert children

        started = time.monotonic()
        enc.cancel()

        assert time.monotonic() - started < fe._SHUTDOWN_TIMEOUT * 2 + 1
        assert not list(__import__("pathlib").Path(folder).glob("frame-*.exr"))


def test_cancel_does_not_wait_for_stalled_png_thread(tmp_path, monkeypatch):
    release = threading.Event()
    started = threading.Event()

    def stalled_write(path, img, params):
        started.set()
        release.wait()
        return True

    monkeypatch.setattr(fe, "_SHUTDOWN_TIMEOUT", 0.1)
    monkeypatch.setattr(fe, "_write_image_job", stalled_write)
    enc = FrameEncoder(workers=1)
    enc.submit_image(str(tmp_path / "stalled.png"), _img())
    assert started.wait(2)
    try:
        before = time.monotonic()
        enc.cancel()
        assert time.monotonic() - before < 1
    finally:
        release.set()


def test_default_bounds(monkeypatch):
    assert 1 <= fe.default_workers() <= 4
    assert fe._BUDGET_CAP == 16 * 1024 * 1024 * 1024
    assert fe._BUDGET_FRACTION == 0.25
    assert fe.default_budget_bytes() <= fe._BUDGET_CAP
    monkeypatch.setattr(fe, "_available_bytes", lambda: 100 * 1024 * 1024)
    assert fe.default_budget_bytes() == 25 * 1024 * 1024
