"""Bounded frame encoder shared by every writer: capped workers + capped in-flight bytes.

PNG/JPEG (cv2 releases the GIL) use threads; EXR (OpenEXR holds the GIL) uses
*spawned* CPU-only processes, never a fork of a CUDA-loaded worker. The caller
(parent) owns names, manifests and publishing: submit with a final path, call
``join()`` to wait for all and raise the first error. Images are copied on
submit, so callers may reuse their buffers. The memory budget counts frame
copies only; it excludes each spawned child's roughly 150-250 MB baseline.
"""
from __future__ import annotations

import logging
import multiprocessing
import os
import threading
import uuid
from concurrent.futures import Future, ProcessPoolExecutor, ThreadPoolExecutor, wait
from typing import Optional

import numpy as np

logger = logging.getLogger(__name__)

_MAX_WORKERS = 4
_BUDGET_CAP = 16 * 1024 * 1024 * 1024
_BUDGET_FRACTION = 0.25
_SHUTDOWN_TIMEOUT = 2.0


class EncodingError(RuntimeError):
    """A frame failed to encode or cannot fit the memory budget."""


class EncodingCancelled(EncodingError):
    """Submission after cancel()."""


def _available_bytes() -> Optional[int]:
    """Available RAM, or Windows free commit when lower; None if unknown."""
    values = []
    try:
        import psutil
        values.append(int(psutil.virtual_memory().available))
    except Exception:
        pass
    try:
        from .ffmpeg_tools.extraction import _free_commit_bytes
        commit = _free_commit_bytes()
        if commit is not None:
            values.append(int(commit))
    except Exception:
        pass
    return min(values) if values else None


def default_budget_bytes() -> int:
    """min(16 GiB, 25% of available RAM/Windows commit headroom)."""
    avail = _available_bytes()
    return _BUDGET_CAP if avail is None else min(_BUDGET_CAP, int(avail * _BUDGET_FRACTION))


def default_workers() -> int:
    return max(1, min(_MAX_WORKERS, os.cpu_count() or 1))


# Child entry points: importable module-level, CPU-only, take paths + arrays.
def _write_exr_job(path: str, img: np.ndarray, compression: str) -> bool:
    from .frame_io import write_exr
    path = os.path.abspath(path)
    folder = os.path.dirname(path)
    temp_name = f".{uuid.uuid4().hex}.exr"
    try:
        os.chdir(folder)
        if not write_exr(temp_name, img, compression):
            return False
        os.replace(temp_name, path)
        return True
    finally:
        if os.path.exists(temp_name):
            os.unlink(temp_name)


def _write_image_job(path: str, img: np.ndarray, params: Optional[list]) -> bool:
    from .frame_io import imwrite_unicode
    return imwrite_unicode(path, img, params)


class FrameEncoder:
    """Bounded encoder. Use as a context manager; exit cancels on error and joins."""

    def __init__(self, workers: Optional[int] = None, budget_bytes: Optional[int] = None):
        self.workers = max(1, workers or default_workers())
        self.budget = budget_bytes if budget_bytes is not None else default_budget_bytes()
        self._cond = threading.Condition()
        self._inflight = 0          # bytes reserved by unfinished encodes
        self._futures: list[Future] = []
        self._error: Optional[BaseException] = None
        self._cancelled = False
        self._threads: Optional[ThreadPoolExecutor] = None
        self._procs: Optional[ProcessPoolExecutor] = None
        self.completed: list[str] = []   # finished paths, completion order

    # -- submission -------------------------------------------------------
    def submit_image(self, path: str, img: np.ndarray, params: Optional[list] = None) -> None:
        """PNG/JPEG (extension picks codec) on a thread."""
        if self._threads is None:
            self._threads = ThreadPoolExecutor(self.workers, thread_name_prefix="frame-enc")
        self._submit(self._threads, img, 2, _write_image_job, path, params)

    def submit_exr(self, path: str, img: np.ndarray, compression: str = "dwab") -> None:
        """EXR in a spawned process (input copy + pickle copy + half-float/encoded)."""
        if self._procs is None:
            self._procs = ProcessPoolExecutor(
                self.workers, mp_context=multiprocessing.get_context("spawn"))
        self._submit(self._procs, img, 3, _write_exr_job, path, compression)

    def _submit(self, pool, img, factor, fn, path, extra) -> None:
        cost = int(img.nbytes) * factor
        if cost > self.budget:
            raise EncodingError(
                f"one frame needs {cost} bytes of encoder memory but the budget is {self.budget}")
        with self._cond:
            while True:
                if self._cancelled:
                    raise EncodingCancelled("encoder cancelled")
                if self._error is not None:
                    raise EncodingError(f"encode failed: {self._error}") from self._error
                if self._inflight == 0 or self._inflight + cost <= self.budget:
                    break
                self._cond.wait(0.1)
            self._inflight += cost
        try:
            fut = pool.submit(fn, path, np.ascontiguousarray(img).copy(), extra)
        except BaseException:
            self._release(cost)
            raise
        self._futures.append(fut)
        fut.add_done_callback(lambda f: self._done(f, path, cost))

    def _release(self, cost: int) -> None:
        with self._cond:
            self._inflight -= cost
            self._cond.notify_all()

    def _done(self, fut: Future, path: str, cost: int) -> None:
        err = None
        if fut.cancelled():
            err = None
        elif fut.exception() is not None:
            err = fut.exception()
        elif not fut.result():
            err = EncodingError(f"failed to write {path}")
        with self._cond:
            if err is not None and self._error is None:
                self._error = err
            elif err is None and not fut.cancelled():
                self.completed.append(path)
            self._inflight -= cost
            self._cond.notify_all()

    # -- lifecycle --------------------------------------------------------
    def join(self) -> None:
        """Wait for every submitted encode; raise the first error."""
        for fut in list(self._futures):
            try:
                fut.result()
            except BaseException:
                pass
        self._shutdown()
        if self._error is not None:
            raise EncodingError(f"encode failed: {self._error}") from self._error

    def cancel(self) -> None:
        """Stop submission, drop queued work, and bound worker shutdown."""
        with self._cond:
            self._cancelled = True
            self._cond.notify_all()
        for fut in self._futures:
            fut.cancel()
        self._shutdown()

    def _shutdown(self) -> None:
        timed_out = False
        for pool in (self._threads, self._procs):
            if pool is not None:
                _, pending = wait(self._futures, timeout=_SHUTDOWN_TIMEOUT)
                timed_out |= bool(pending)
                if pool is self._procs and pending:
                    processes = list((pool._processes or {}).values())
                    pool.shutdown(wait=False, cancel_futures=True)
                    for process in processes:
                        if process.is_alive():
                            process.terminate()
                    for process in processes:
                        process.join(_SHUTDOWN_TIMEOUT)
                        if process.is_alive() and hasattr(process, "kill"):
                            process.kill()
                            process.join(_SHUTDOWN_TIMEOUT)
                elif pool is self._threads and pending:
                    pool.shutdown(wait=False, cancel_futures=True)
                    timed_out = True
                else:
                    pool.shutdown(wait=True, cancel_futures=True)
        if timed_out:
            logger.error("encoder shutdown timed out; terminated owned processes and abandoned stalled threads")
        self._threads = self._procs = None

    def __enter__(self) -> "FrameEncoder":
        return self

    def __exit__(self, exc_type, exc, tb) -> None:
        if exc_type is not None:
            self.cancel()
        else:
            self.join()
