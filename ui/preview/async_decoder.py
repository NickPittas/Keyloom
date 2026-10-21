"""Async frame decoder: bounded foreground/prefetch scheduler on a preview pool.

Workers only decode and emit a queued signal; every scheduling decision and
every cache insertion happens in GUI-thread slots (race-free). A single
module-level in-flight cap bounds how many full-resolution decodes exist at
once across all decoders.

Legacy ``request_decode`` / ``frame_decoded`` keep working (latest-wins,
no FrameCache); new callers use ``request_foreground`` / ``prefetch`` and
``frame_ready``.
"""
from __future__ import annotations

import dataclasses
import logging
import os
import sys
import threading
import weakref
from dataclasses import dataclass

import cv2
import numpy as np
from PySide6.QtCore import QObject, QRunnable, QThreadPool, Qt, Signal, Slot

from backend.frame_io import open_video

from .display_transform import (
    _numpy_to_qimage,
    _reduce,
    _transform_linear_rgb,
    decode_frame,
    decode_video_frame,
)
from .frame_cache import LEGACY_CACHE_BYTES, FrameCache
from .frame_index import ViewMode

logger = logging.getLogger(__name__)

MAX_INFLIGHT_DECODES = 12   # ~92 MB per 4K float frame: bound concurrent frames
MAX_PREFETCH_QUEUE = 64
LEGACY_LANE = "legacy"


def preview_thread_count() -> int:
    """Outer preview worker count: small on Windows, affinity CPUs (<=32) elsewhere."""
    if sys.platform == "win32":
        return 2
    try:
        n = len(os.sched_getaffinity(0))
    except (AttributeError, OSError):
        n = os.cpu_count() or 4
    return max(1, min(32, n))


_POOL: QThreadPool | None = None
_POOL_LOCK = threading.Lock()


def preview_pool() -> QThreadPool:
    """Shared preview-owned pool (never the Qt global pool)."""
    global _POOL
    with _POOL_LOCK:
        if _POOL is None:
            pool = QThreadPool()
            pool.setMaxThreadCount(preview_thread_count())
            _POOL = pool
        return _POOL


def max_inflight() -> int:
    return max(1, min(preview_thread_count(), MAX_INFLIGHT_DECODES))


# Slots prefetch may never take, so a user seek never queues behind a full
# prefetch backlog (two panes x two lanes).
FOREGROUND_RESERVE = 4


def max_prefetch_inflight() -> int:
    return max(1, max_inflight() - FOREGROUND_RESERVE)


class _Gate:
    """Counts submitted-but-not-finished tasks. Released by the worker."""

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._count = 0

    def try_acquire(self, limit: int) -> bool:
        with self._lock:
            if self._count >= limit:
                return False
            self._count += 1
            return True

    def release(self) -> None:
        with self._lock:
            self._count -= 1

    @property
    def count(self) -> int:
        with self._lock:
            return self._count


_GATE = _Gate()
_DECODERS: "weakref.WeakSet[AsyncDecoder]" = weakref.WeakSet()


@dataclass(frozen=True)
class DecodeRequest:
    lane: str
    stem_index: int
    mode: ViewMode
    path: str
    video_path: str | None = None
    video_frame_index: int = 0
    input_exr_is_linear: bool = False
    target_size: tuple[int, int] | None = None
    generation: int | None = None  # stamped with cache.generation when None


def _revision(path: str) -> tuple[int, int]:
    try:
        st = os.stat(path)
        return (st.st_mtime_ns, st.st_size)
    except OSError:
        return (0, 0)


def request_key(req: DecodeRequest) -> tuple:
    """Cache identity: source, revision, frame, transform mode, INPUT interp, size."""
    if req.video_path:
        src = req.video_path
        mode = ViewMode.INPUT
        frame = req.video_frame_index
    else:
        src = req.path
        mode = req.mode
        frame = -1
    return (src, _revision(src), frame, mode.value,
            int(req.input_exr_is_linear), req.target_size)


class _SequentialVideoReader:
    """One cv2.VideoCapture, used by at most one task at a time.

    Reads forward without seeking when the requested index is the next one.
    """

    def __init__(self, video_path: str) -> None:
        self.video_path = video_path
        self.lock = threading.Lock()
        self._cap = None
        self._next = 0
        self._closed = False

    def _release_cap(self) -> None:
        cap, self._cap = self._cap, None
        self._next = 0
        if cap is not None:
            try:
                cap.release()
            except Exception:
                pass

    def close(self) -> None:
        self._closed = True
        if self.lock.acquire(blocking=False):
            try:
                self._release_cap()
            finally:
                self.lock.release()
        # else: the running task releases on completion (sees _closed)

    def read(self, index: int, input_exr_is_linear: bool, target_size):
        """QImage, or None on failure; NotImplemented if the reader is busy."""
        if not self.lock.acquire(blocking=False):
            return NotImplemented
        try:
            if self._closed:
                return NotImplemented
            if self._cap is None:
                cap = open_video(self.video_path)
                if not cap.isOpened():
                    cap.release()
                    return None
                self._cap, self._next = cap, 0
            if index != self._next:
                self._cap.set(cv2.CAP_PROP_POS_FRAMES, index)
            ok, frame = self._cap.read()
            if not ok or frame is None:
                self._release_cap()
                return None
            self._next = index + 1
            source_size = (frame.shape[1], frame.shape[0])
            frame = _reduce(frame, target_size)
            if input_exr_is_linear:
                return _transform_linear_rgb(
                    frame.astype(np.float32) / 255.0, ViewMode.INPUT,
                    input_exr_is_linear=True, source_size=source_size,
                )
            return _numpy_to_qimage(
                cv2.cvtColor(frame, cv2.COLOR_BGR2RGB), source_size=source_size)
        except Exception as exc:
            logger.warning("Sequential video read failed: %s", exc)
            self._release_cap()
            return None
        finally:
            if self._closed:
                self._release_cap()
            self.lock.release()


class _DecodeSignals(QObject):
    finished = Signal(int, object)  # task_id, QImage|None


class _DecodeTask(QRunnable):
    """Decode one frame in the preview pool; only decodes and emits."""

    def __init__(self, task_id: int, req: DecodeRequest,
                 reader: _SequentialVideoReader | None = None):
        super().__init__()
        self.signals = _DecodeSignals()
        self._task_id = task_id
        self._req = req
        self._reader = reader
        self.setAutoDelete(True)

    def _decode(self):
        r = self._req
        if r.video_path:
            if self._reader is not None:
                img = self._reader.read(
                    r.video_frame_index, r.input_exr_is_linear, r.target_size)
                if img is not NotImplemented:
                    return img
            return decode_video_frame(
                r.video_path, r.video_frame_index,
                input_exr_is_linear=r.input_exr_is_linear,
                target_size=r.target_size)
        return decode_frame(
            r.path, r.mode, input_exr_is_linear=r.input_exr_is_linear,
            target_size=r.target_size)

    def run(self) -> None:
        img = None
        try:
            try:
                img = self._decode()
            except Exception as e:
                logger.warning("Async decode failed: %s", e)
                img = None
        finally:
            _GATE.release()
        try:
            self.signals.finished.emit(self._task_id, img)
        except RuntimeError:
            pass  # signal source deleted — task outlived its decoder


@dataclass
class _Entry:
    req: DecodeRequest
    key: tuple
    signals: _DecodeSignals
    legacy_id: int = 0


class AsyncDecoder(QObject):
    """Latest-wins foreground per lane + bounded prefetch, GUI-thread owned."""

    frame_decoded = Signal(int, str, object)  # legacy: stem_index, mode_value, QImage|None
    # lane, stem_index, mode_value, QImage|None, generation
    frame_ready = Signal(str, int, str, object, int)

    def __init__(self, parent=None, cache: FrameCache | None = None):
        super().__init__(parent)
        self._cache = cache if cache is not None else FrameCache(LEGACY_CACHE_BYTES)
        self._fg: dict[str, tuple[DecodeRequest, tuple]] = {}
        self._prefetch: list[tuple[DecodeRequest, tuple]] = []
        self._inflight: dict[int, _Entry] = {}
        self._inflight_keys: dict[tuple, int] = {}
        self._readers: dict[tuple[str, str], _SequentialVideoReader] = {}
        self._next_task_id = 0
        self._legacy_id = 0
        self._closed = False
        _DECODERS.add(self)

    # ── public API ──────────────────────────────────────────────

    @property
    def cache(self) -> FrameCache:
        return self._cache

    def inflight_count(self) -> int:
        return len(self._inflight)

    def is_cached(self, req: DecodeRequest) -> bool:
        return self._cache.contains(request_key(req))

    def cached_image(self, req: DecodeRequest):
        return self._cache.get(request_key(req))

    def request_foreground(self, req: DecodeRequest) -> None:
        """Latest-wins per lane. Cached frames are emitted synchronously (GUI thread)."""
        if self._closed:
            return
        req = self._stamp(req)
        key = request_key(req)
        img = self._cache.get(key)
        if img is not None:
            self._fg.pop(req.lane, None)
            self.frame_ready.emit(req.lane, req.stem_index, req.mode.value,
                                  img, req.generation)
            return
        if (req.lane, key) in self._inflight_keys:
            # Already running (e.g. as prefetch): its result emits frame_ready.
            self._fg.pop(req.lane, None)
            return
        self._fg[req.lane] = (req, key)
        _pump_all()

    def prefetch(self, requests: list[DecodeRequest]) -> None:
        """Replace the prefetch queue (priority = given order, bounded)."""
        if self._closed:
            return
        queue: list[tuple[DecodeRequest, tuple]] = []
        seen: set[tuple] = set()
        for req in requests:
            if len(queue) >= MAX_PREFETCH_QUEUE:
                break
            req = self._stamp(req)
            key = request_key(req)
            if key in seen or (req.lane, key) in self._inflight_keys or self._cache.contains(key):
                continue
            seen.add(key)
            queue.append((req, key))
        self._prefetch = queue
        _pump_all()

    def cancel_pending(self) -> None:
        self._fg.clear()
        self._prefetch.clear()

    def invalidate(self) -> None:
        """New generation: drop unstarted work; in-flight old results are discarded."""
        self._cache.clear()
        self.cancel_pending()
        self._close_readers()

    def close(self) -> None:
        self._closed = True
        self.cancel_pending()
        self._inflight.clear()
        self._inflight_keys.clear()
        self._close_readers()
        _DECODERS.discard(self)

    def request_decode(self, path: str, mode: ViewMode, stem_index: int,
                       video_path: str | None = None,
                       video_frame_index: int = 0,
                       input_exr_is_linear: bool = False) -> None:
        """Legacy API: latest request wins; result via ``frame_decoded``."""
        if self._closed:
            return
        self._legacy_id += 1
        req = DecodeRequest(
            LEGACY_LANE, stem_index, mode, path, video_path, video_frame_index,
            input_exr_is_linear, None, self._cache.generation)
        # Unique key so legacy work is never deduplicated against cached frames.
        self._fg[LEGACY_LANE] = (req, ("legacy", self._legacy_id))
        _pump_all()

    # ── internals (GUI thread) ──────────────────────────────────

    def _stamp(self, req: DecodeRequest) -> DecodeRequest:
        if req.generation is None:
            return dataclasses.replace(req, generation=self._cache.generation)
        return req

    def _close_readers(self) -> None:
        readers, self._readers = self._readers, {}
        for r in readers.values():
            r.close()

    def _reader_for(self, req: DecodeRequest) -> _SequentialVideoReader | None:
        if not req.video_path:
            return None
        rk = (req.lane, req.video_path)
        reader = self._readers.get(rk)
        if reader is None:
            for ok in [k for k in self._readers if k[0] == req.lane]:
                self._readers.pop(ok).close()  # lane switched video
            reader = self._readers[rk] = _SequentialVideoReader(req.video_path)
        return reader

    def _next_request(self):
        """Pop next dispatchable (req, key): foreground first, then prefetch."""
        gen = self._cache.generation
        for lane in list(self._fg):
            req, key = self._fg.pop(lane)
            if req.lane != LEGACY_LANE and (req.generation != gen
                                            or (req.lane, key) in self._inflight_keys):
                continue
            return req, key
        while self._prefetch:
            req, key = self._prefetch.pop(0)
            if (req.generation != gen or (req.lane, key) in self._inflight_keys
                    or self._cache.contains(key)):
                continue
            return req, key
        return None

    def _pump(self) -> None:
        while not self._closed and (self._fg or self._prefetch):
            limit = max_inflight() if self._fg else max_prefetch_inflight()
            if not _GATE.try_acquire(limit):
                return
            nxt = self._next_request()
            if nxt is None:
                _GATE.release()
                return
            req, key = nxt
            self._next_task_id += 1
            tid = self._next_task_id
            task = _DecodeTask(tid, req, self._reader_for(req))
            self._inflight[tid] = _Entry(
                req, key, task.signals,
                self._legacy_id if req.lane == LEGACY_LANE else 0)
            self._inflight_keys[(req.lane, key)] = tid
            task.signals.finished.connect(
                self._on_task_finished, Qt.ConnectionType.QueuedConnection)
            preview_pool().start(task)

    @Slot(int, object)
    def _on_task_finished(self, task_id: int, qimage: object) -> None:
        entry = self._inflight.pop(task_id, None)
        if entry is not None:
            self._inflight_keys.pop((entry.req.lane, entry.key), None)
            req = entry.req
            if req.lane == LEGACY_LANE:
                if entry.legacy_id == self._legacy_id:
                    self.frame_decoded.emit(req.stem_index, req.mode.value, qimage)
            elif req.generation == self._cache.generation:
                if qimage is not None:
                    self._cache.put(entry.key, qimage, generation=req.generation,
                                    frame_index=req.stem_index)
                self.frame_ready.emit(req.lane, req.stem_index, req.mode.value,
                                      qimage, req.generation)
        _pump_all()


def _pump_all() -> None:
    for dec in list(_DECODERS):
        try:
            dec._pump()
        except RuntimeError:
            _DECODERS.discard(dec)  # underlying QObject deleted
