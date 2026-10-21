"""ExrSequenceWriter (EZ-CorridorKey2): failure-safe queue/shutdown."""
from __future__ import annotations

import os
import threading

os.environ.setdefault("OPENCV_IO_ENABLE_OPENEXR", "1")

import cv2
import numpy as np
import pytest

from CorridorKeyV2.corridorkey import video_io
from CorridorKeyV2.corridorkey.video_io import ExrSequenceWriter


def _run(fn, timeout=10):
    box = {}

    def target():
        try:
            box["r"] = fn()
        except Exception as exc:  # noqa: BLE001
            box["e"] = exc

    t = threading.Thread(target=target, daemon=True)
    t.start()
    t.join(timeout)
    assert not t.is_alive(), "deadlock"
    if "e" in box:
        raise box["e"]
    return box.get("r")


def _batch(n=1, c=3):
    return np.full((n, 4, 5, c), 0.5, np.float32)


@pytest.mark.parametrize("c", [1, 3])
def test_roundtrip(tmp_path, c):
    w = ExrSequenceWriter(str(tmp_path), 24, (5, 4), channels=c, dtype="float")
    data = np.random.rand(2, 4, 5, c).astype(np.float32)
    data[0, 0, 0, :] = 1e6
    w.write_many(data)
    w.release()
    w.release()
    for i in range(2):
        img = cv2.imread(str(tmp_path / f"frame_{i:06d}.exr"), cv2.IMREAD_UNCHANGED)
        assert img.shape[:2] == (4, 5)
        np.testing.assert_allclose(img.reshape(4, 5, c), data[i], rtol=1e-6)


def test_half_clamps(tmp_path):
    w = ExrSequenceWriter(str(tmp_path), 24, (5, 4), channels=1)
    w.write_many(np.full((1, 4, 5, 1), 1e9, np.float32))
    w.release()
    img = cv2.imread(str(tmp_path / "frame_000000.exr"), cv2.IMREAD_UNCHANGED)
    assert np.isfinite(img).all() and img.max() == 65504.0


def test_validation_errors(tmp_path):
    w = ExrSequenceWriter(str(tmp_path), 24, (5, 4), channels=3)
    with pytest.raises(ValueError):
        w.write_many(_batch(c=1))
    with pytest.raises(ValueError):
        w.write_many(_batch().astype(np.float64))
    w.release()


@pytest.mark.parametrize("fail_after", [0, 2])
def test_failure_never_blocks(tmp_path, monkeypatch, fail_after):
    real = cv2.imwrite
    calls = {"n": 0}

    def fake(path, img, params):
        calls["n"] += 1
        if calls["n"] > fail_after:
            return False
        return real(path, img, params)

    monkeypatch.setattr(video_io.cv2, "imwrite", fake)
    w = ExrSequenceWriter(str(tmp_path), 24, (5, 4), channels=3, maxsize=2)

    def produce():
        with pytest.raises(RuntimeError, match="imwrite failed"):
            for _ in range(50):
                w.write_many(_batch(2))

    _run(produce)
    with pytest.raises(RuntimeError, match="imwrite failed"):
        _run(lambda: w.write_many(_batch()))
    with pytest.raises(RuntimeError, match="imwrite failed"):
        _run(w.release)
    _run(w.release)  # second call: no-op, no re-raise


def _write_all(path, workers):
    w = ExrSequenceWriter(str(path), 24, (5, 4), channels=3, dtype="float",
                          workers=workers)
    for b in range(3):
        data = np.stack([np.full((4, 5, 3), b * 5 + k, np.float32)
                         for k in range(5)])
        w.write_many(data)
    w.release()
    return w


def _read(path, i):
    return cv2.imread(str(path / f"frame_{i:06d}.exr"), cv2.IMREAD_UNCHANGED)


def test_pool_writes_all_frames_in_order(tmp_path):
    w = _write_all(tmp_path, 4)
    assert len(w.threads) == 4 and not any(t.is_alive() for t in w.threads)
    assert sorted(p.name for p in tmp_path.iterdir()) == [
        f"frame_{i:06d}.exr" for i in range(15)]
    for i in range(15):
        assert (_read(tmp_path, i) == i).all()


def test_pool_matches_single_worker(tmp_path):
    a, b = tmp_path / "a", tmp_path / "b"
    _write_all(a, 1)
    _write_all(b, 4)
    for i in range(15):
        np.testing.assert_array_equal(_read(a, i), _read(b, i))


def test_worker_error_propagates_without_hang(tmp_path):
    w = ExrSequenceWriter(str(tmp_path), 24, (5, 4), channels=3, workers=3)
    real = w._write_item

    def bad(idx0, arr):
        if idx0 == 3:
            raise RuntimeError("boom")
        real(idx0, arr)

    w._write_item = bad

    def produce():
        with pytest.raises(RuntimeError, match="boom"):
            for _ in range(50):
                w.write_many(_batch(2))

    _run(produce)
    with pytest.raises(RuntimeError, match="boom"):
        _run(w.release)
    assert not any(t.is_alive() for t in w.threads)


def test_workers_validation_and_default(tmp_path):
    with pytest.raises(ValueError):
        ExrSequenceWriter(str(tmp_path), 24, (5, 4), workers=0)
    w = ExrSequenceWriter(str(tmp_path), 24, (5, 4))
    w.release()
    assert 1 <= w.workers <= 8
