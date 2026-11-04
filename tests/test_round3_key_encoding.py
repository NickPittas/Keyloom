"""Round 3 T8: parallel final Key encoding gives the old sequential files, in order, with cleanup."""
from __future__ import annotations

import itertools
import threading
import time

import cv2
import numpy as np
import pytest

from backend.service import key_job
from backend.service.key_job import _Renderer
from tests.test_key_job import A, B, STEMS, FakeEngine, env, run, svc  # noqa: F401
from tests.test_key_replace import contents, first_result, replace_job  # noqa: F401

ALL = dict(fg_enabled=True, matte_enabled=True, comp_enabled=True, processed_enabled=True)


@pytest.fixture(autouse=True)
def workers(monkeypatch):
    monkeypatch.setattr(key_job, "ENCODE_WORKERS", 3)


def leftovers(root):
    return [p for p in contents(root) if "/.ck-" in p or ".tmp." in p or key_job.LOCK_NAME in p]


def reference_spy(monkeypatch, tmp_path):
    """Render every item twice: the old sequential path (enc=None) into refs/, the new one as usual."""
    real, refs, count = key_job._render, {}, itertools.count()

    def spy(svc_, product, fmt, src, tmp, cfg, owned, enc=None):
        ref = str(tmp_path / "refs" / f"{next(count)}.{fmt}")
        (tmp_path / "refs").mkdir(exist_ok=True)
        real(svc_, product, fmt, src, ref, cfg, [], None)
        refs[tmp] = (ref, fmt)
        return real(svc_, product, fmt, src, tmp, cfg, owned, enc)
    monkeypatch.setattr(key_job, "_render", spy)
    return refs


@pytest.mark.parametrize("mode,compression,formats", [
    ("new", "piz", {}),
    ("new", "dwab", dict(fg_format="png", matte_format="png", comp_format="exr", processed_format="exr")),
    ("replace", "zip", {}),
])
def test_all_products_equal_old_sequential_path(svc, env, monkeypatch, tmp_path, mode, compression, formats):  # noqa: F811
    if mode == "replace":
        first_result(svc, env)
    refs = reference_spy(monkeypatch, tmp_path)
    job = env.job(mode=mode, compression=compression, cfg={} if mode == "replace" else {**ALL, **formats},
                  **({"job_id": "job-new00001"} if mode == "replace" else {}))
    _, done = run(svc, job)
    assert done["outcome"] == "complete" and done["published"] == B - A + 1
    assert leftovers(env.out) == []
    files = {p: (p.endswith(".png"), p) for ps in job["outputs"].values() for p in ps}
    assert len(files) == len(job['outputs']) * (B - A + 1) and len(refs) >= len(files)
    got = {}
    for ref, fmt in refs.values():  # one reference per (product, frame); same source -> same bytes
        got.setdefault(open(ref, "rb").read(), 0)
    for final in files:
        data = open(final, "rb").read()
        if final.endswith(".png"):
            assert data in got  # PNG: byte-identical to the sequential encoder
        else:
            px = cv2.imread(final, cv2.IMREAD_UNCHANGED)
            assert any(fmt == "exr" and np.array_equal(px, cv2.imread(r, cv2.IMREAD_UNCHANGED))
                       for r, fmt in refs.values())


def test_out_of_order_completion_publishes_in_frame_order(svc, env, monkeypatch):  # noqa: F811
    real = key_job._render

    def slow_first(svc_, product, fmt, src, tmp, cfg, owned, enc=None):
        if "_000000" in src:
            time.sleep(0.6)  # frame 0 finishes after frames 1 and 2
        return real(svc_, product, fmt, src, tmp, cfg, owned, enc)
    monkeypatch.setattr(key_job, "_render", slow_first)
    events, done = run(svc, env.job())
    assert done["outcome"] == "complete" and done["published"] == 3
    assert [e["done"] for e in events if e.get("phase") == "publish"] == [1, 2, 3]
    stems = [s for e in events if e.get("event") == "published" for s in e["stems"]]
    assert stems == STEMS[A:B + 1]


def test_failed_frame_keeps_an_ordered_prefix_and_no_temp_files(svc, env, monkeypatch):  # noqa: F811
    real, n = key_job._render, []

    def flaky(svc_, product, fmt, src, tmp, cfg, owned, enc=None):
        n.append(1)
        if len(n) == 5:
            raise OSError("disk full")
        return real(svc_, product, fmt, src, tmp, cfg, owned, enc)
    monkeypatch.setattr(key_job, "_render", flaky)
    _, done = run(svc, env.job())
    assert done["outcome"] == "failed" and done["error_kind"] == "disk"
    present = [(env.out / "Fg" / f"{s}.exr").exists() for s in STEMS[A:B + 1]]
    assert present == sorted(present, reverse=True)  # only a prefix of frames was published
    assert not all(present) and leftovers(env.out) == []


def test_cancel_mid_render_joins_workers_and_leaves_no_temp_files(svc, env, monkeypatch):  # noqa: F811
    cancel, real, n = threading.Event(), key_job._render, []

    def cancelling(svc_, product, fmt, src, tmp, cfg, owned, enc=None):
        n.append(1)
        if len(n) == 3:
            cancel.set()
        return real(svc_, product, fmt, src, tmp, cfg, owned, enc)
    monkeypatch.setattr(key_job, "_render", cancelling)
    _, done = run(svc, env.job(), cancel=cancel)
    assert done["outcome"] == "cancelled" and leftovers(env.out) == []


def test_replace_failure_and_resume_after_failure(svc, env, monkeypatch):  # noqa: F811
    old = first_result(svc, env)
    real, n = key_job._render, []

    def flaky(svc_, product, fmt, src, tmp, cfg, owned, enc=None):
        n.append(1)
        if len(n) == 4:
            raise OSError("disk full")
        return real(svc_, product, fmt, src, tmp, cfg, owned, enc)
    monkeypatch.setattr(key_job, "_render", flaky)
    FakeEngine.hdr = 3.0
    _, done = run(svc, replace_job(env))
    assert done["outcome"] == "failed" and contents(env.out) == old
    monkeypatch.setattr(key_job, "_render", real)

    # resume: a fresh job that failed midway, then the same range again with mode=resume
    new_env = type(env)(env.tmp / "second")
    n.clear()
    monkeypatch.setattr(key_job, "_render", flaky)
    _, done = run(svc, new_env.job(job_id="job-cccccccc"))
    assert done["outcome"] == "failed"
    monkeypatch.setattr(key_job, "_render", real)
    _, done = run(svc, new_env.job(job_id="job-dddddddd", mode="resume"))
    assert done["outcome"] == "complete" and done["published"] + done["skipped"] == 3
    assert len(list(new_env.out.rglob("*.exr"))) == 6 and leftovers(new_env.out) == []


def test_unicode_output_folder_exr_through_processes(svc, env, tmp_path):  # noqa: F811
    env.out = tmp_path / "ü ñ 日本" / "v001"
    _, done = run(svc, env.job())
    assert done["outcome"] == "complete" and len(list(env.out.rglob("*.exr"))) == 6
    assert leftovers(env.out) == []


def test_process_and_thread_paths_write_the_same_exr(tmp_path):
    img = np.random.default_rng(1).random((8, 6, 4), dtype=np.float32) * 1000
    a, b = tmp_path / "a.exr", tmp_path / "ü.exr"
    key_job._write_exr_float32(str(a), img, "piz")
    enc = _Renderer(None, None, processes=True)
    try:
        b.write_bytes(b"")
        enc.write_exr(str(b), img, "piz")
    finally:
        enc.close()
    assert np.array_equal(cv2.imread(str(a), cv2.IMREAD_UNCHANGED), cv2.imread(str(b), cv2.IMREAD_UNCHANGED))


def test_budget_backpressure_and_oversize(monkeypatch):
    monkeypatch.setattr("backend.frame_encoding.default_budget_bytes", lambda: 100)
    enc = _Renderer(None, None, processes=False)
    try:
        enc.reserve(60)
        got = []
        t = threading.Thread(target=lambda: (enc.reserve(60), got.append(1)))
        t.start()
        time.sleep(0.3)
        assert not got  # second frame waits for the first to finish
        enc.release(60)
        t.join(2)
        assert got == [1]
        with pytest.raises(key_job.CorridorKeyError, match="101 bytes"):
            enc.reserve(101)
    finally:
        enc.close()


def test_runtime_exr_writers_share_a_byte_budget_and_write_unicode_paths(tmp_path):
    from CorridorKeyV2.corridorkey.video_io import ExrSequenceWriter, FrameByteBudget
    budget = FrameByteBudget(1)  # smaller than one frame: frames still go through one at a time
    folder = tmp_path / "日本ü"
    w1 = ExrSequenceWriter(str(folder / "a"), 24, (5, 4), channels=3, budget=budget, workers=2)
    w2 = ExrSequenceWriter(str(folder / "b"), 24, (5, 4), channels=1, budget=budget, workers=2)
    for k in range(3):
        w1.write_many(np.full((2, 4, 5, 3), k, np.float32))
        w2.write_many(np.full((2, 4, 5, 1), k, np.float32))
    w1.release()
    w2.release()
    assert budget.used == 0
    assert len(list((folder / "a").glob("*.exr"))) == 6 and len(list((folder / "b").glob("*.exr"))) == 6
