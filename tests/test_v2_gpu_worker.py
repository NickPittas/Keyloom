"""GPUJobWorker: strictly serial, phase-gated preview, cooperative cancel/stop."""
from __future__ import annotations

import itertools
import os
import threading
import time

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

import pytest

pytest.importorskip("PySide6", reason="PySide6 not installed")
from PySide6.QtCore import QCoreApplication, Qt

from backend.errors import JobCancelledError
from backend.job_queue import GPUJob, GPUJobQueue, JobStatus, JobType
from ui.workers.gpu_job_worker import GPUJobWorker


@pytest.fixture(scope="module", autouse=True)
def _app():
    app = QCoreApplication.instance() or QCoreApplication([])
    yield app


class FakeService:
    def __init__(self, work=0.05):
        self.job_queue = GPUJobQueue()
        self.work = work
        self.lock = threading.Lock()
        self.active = 0
        self.max_active = 0
        self.spans: list[tuple[str, float, float]] = []
        self.started = threading.Event()
        self.release: threading.Event | None = None
        self.progress_calls: list[dict] = []

    def _run(self, name, job, on_progress=None):
        with self.lock:
            self.active += 1
            self.max_active = max(self.max_active, self.active)
        t0 = time.monotonic()
        self.started.set()
        try:
            end = t0 + self.work
            while time.monotonic() < end or (self.release and not self.release.is_set()):
                if job.is_cancelled:
                    raise JobCancelledError(job.clip_name, 0)
                time.sleep(0.005)
            if on_progress:
                for kw in self.progress_calls:
                    on_progress(job.clip_name, kw.pop("c", 1), kw.pop("t", 10), **kw)
        finally:
            with self.lock:
                self.active -= 1
            self.spans.append((name, t0, time.monotonic()))

    def run_inference(self, clip, params, job, on_progress=None, **kw):
        self._run("inference", job, on_progress)

    def run_gvm(self, clip, job, on_progress=None, **kw):
        self._run("gvm", job)


def _job(jt=JobType.INFERENCE, name="c"):
    return GPUJob(job_type=jt, clip_name=name,
                  params={"_clip_snapshot": object(), "_inference_params": object()})


def _run_worker(svc, jobs, timeout=5.0):
    w = GPUJobWorker(svc)
    events = {"clip": [], "warn": [], "err": [], "progress": []}
    w.clip_finished.connect(lambda j, c, t: events["clip"].append(j), type=Qt.DirectConnection)
    w.warning.connect(lambda j, m: events["warn"].append((j, m)), type=Qt.DirectConnection)
    w.error.connect(lambda j, c, m: events["err"].append(m), type=Qt.DirectConnection)
    for j in jobs:
        svc.job_queue.submit(j)
    w.start()
    return w, events


def _wait_idle(svc, w, timeout=5.0):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if not svc.job_queue.has_pending and not w.is_busy() and svc.job_queue.current_job is None:
            return
        time.sleep(0.01)
    raise AssertionError("worker did not go idle")


def _shutdown(w):
    w.stop()
    assert w.wait(3000)


def test_inference_jobs_strictly_serial():
    svc = FakeService()
    jobs = [_job(name=f"c{i}") for i in range(4)]
    w, ev = _run_worker(svc, jobs)
    _wait_idle(svc, w)
    _shutdown(w)
    assert svc.max_active == 1
    spans = sorted(svc.spans, key=lambda s: s[1])
    for a, b in itertools.pairwise(spans):
        assert a[2] <= b[1]
    assert len(ev["clip"]) == 4


def test_gvm_and_inference_never_overlap():
    svc = FakeService()
    jobs = [_job(JobType.INFERENCE, "a"), _job(JobType.GVM_ALPHA, "b"), _job(JobType.INFERENCE, "c")]
    w, _ev = _run_worker(svc, jobs)
    _wait_idle(svc, w)
    _shutdown(w)
    assert svc.max_active == 1
    assert [s[0] for s in sorted(svc.spans, key=lambda s: s[1])] == ["inference", "gvm", "inference"]


def test_preview_only_on_publish_phase(monkeypatch):
    svc = FakeService(work=0)
    svc.progress_calls = [
        {"c": 5, "t": 10, "phase": "stage1"},
        {"c": 10, "t": 10, "phase": "stage2"},
        {"c": 5, "t": 10, "phase": "publish"},
        {"c": 7, "t": 10, "phase": "publish"},
        {"c": 10, "t": 10, "phase": "publish"},
    ]
    saved = []
    monkeypatch.setattr(GPUJobWorker, "_save_preview",
                        lambda self, jid, clip, idx: saved.append(idx))
    w = GPUJobWorker(svc)
    progress = []
    w.progress.connect(lambda *a: progress.append(a))
    job = _job()
    svc.job_queue.submit(job)
    svc.job_queue.start_job(job)
    w._process_job_inner(job)
    assert saved == [5, 10]
    assert len(progress) == 5  # every phase still reports progress


def test_cancel_raises_marks_cancelled_no_clip_finished():
    svc = FakeService(work=10)
    job = _job()
    w, ev = _run_worker(svc, [job])
    assert svc.started.wait(2)
    svc.job_queue.cancel_job(job)
    _wait_idle(svc, w)
    _shutdown(w)
    assert job.status == JobStatus.CANCELLED
    assert ev["clip"] == []
    assert any("Cancelled" in m for _, m in ev["warn"])


def test_cancel_requested_but_run_returns_is_not_success():
    svc = FakeService(work=0)
    job = _job()
    svc.run_inference = lambda **kw: job.request_cancel()
    w = GPUJobWorker(svc)
    done = []
    w.clip_finished.connect(lambda *a: done.append(a))
    svc.job_queue.submit(job)
    svc.job_queue.start_job(job)
    w._process_job_inner(job)
    assert job.status == JobStatus.CANCELLED
    assert done == []


def test_stop_is_nonblocking_and_job_finishes_cooperatively():
    svc = FakeService(work=0)
    svc.release = threading.Event()  # job runs until cancelled
    job = _job()
    w, ev = _run_worker(svc, [job])
    assert svc.started.wait(2)
    assert w.is_busy() and w.current_job_id == job.id
    t0 = time.monotonic()
    w.stop()
    assert time.monotonic() - t0 < 0.5
    assert w.wait(3000)
    assert job.status == JobStatus.CANCELLED
    assert not w.is_busy() and w.current_job_id is None
    assert ev["clip"] == []


def test_stale_preview_reprocess_job_does_not_crash():
    svc = FakeService()
    job = _job(JobType.PREVIEW_REPROCESS)
    w, ev = _run_worker(svc, [job])
    _wait_idle(svc, w)
    _shutdown(w)
    assert ev["err"] == []
    assert job.status == JobStatus.COMPLETED


def test_retired_api_absent():
    w = GPUJobWorker(FakeService())
    for name in ("set_max_workers", "max_workers", "reprocess_result", "_drain_pool", "_pool"):
        assert not hasattr(w, name)


def test_close_while_busy_asks_and_no_keeps_window(monkeypatch):
    import types

    from ui import main_window as mw

    saved = []
    ignored = []
    fake = types.SimpleNamespace(
        _skip_shutdown_cleanup=False,
        _gpu_worker=types.SimpleNamespace(is_busy=lambda: True, isRunning=lambda: True),
        _auto_save_annotations=lambda: saved.append(1),
        tr=lambda s: s,
    )
    box = types.SimpleNamespace(question=lambda *a, **k: 2, Yes=1, No=2)
    monkeypatch.setattr(mw, "QMessageBox", box)
    mw.MainWindow.closeEvent(fake, types.SimpleNamespace(ignore=lambda: ignored.append(1)))
    assert ignored == [1] and saved == []
