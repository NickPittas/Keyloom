"""Off-thread inference-context probe (source metadata, EXR peak, VRAM budget).

The GUI only deep-copies the lightweight clip and queues a task on a private
single-thread pool. All listing/stat/dimension/video/raw-EXR work happens in
the worker; results are scalars plus a source-revision token (no arrays,
QPixmap or model). A monotonically increasing token drops stale completions,
and at most one task runs with the latest pending request coalesced.
"""
from __future__ import annotations

import copy
import logging
import os
from dataclasses import dataclass

from PySide6.QtCore import QObject, QRunnable, QThreadPool, Qt, Signal

from backend.input_colour import InputColourSample, sample_input_colour
from backend.v2_temporal import query_vram_budget_gib

logger = logging.getLogger(__name__)


@dataclass(frozen=True)
class InferenceContext:
    root_path: str
    source_revision: tuple
    width: int | None
    height: int | None
    frame_count: int
    budget_gib: float | None
    colour_sample: InputColourSample | None
    error: str | None = None


def _stat_entry(path: str) -> tuple:
    try:
        st = os.stat(path)
        return (path, st.st_mtime_ns, st.st_size)
    except OSError:
        return (path, None, None)


def build_inference_context(clip) -> InferenceContext:
    """Blocking; call only off the GUI thread. Never raises."""
    root = str(getattr(clip, "root_path", "") or "")
    try:
        asset = getattr(clip, "input_asset", None)
        if asset is None:
            return InferenceContext(root, (), None, None, 0, None, None, "no input")
        try:
            budget = query_vram_budget_gib()
        except Exception:
            budget = None
        # The copy may carry stale cached probe results; force a fresh probe.
        if hasattr(asset, "_dimensions"):
            asset._dimensions = None
        is_video = getattr(asset, "asset_type", "sequence") == "video"
        if is_video:
            paths = [asset.path]
            if hasattr(asset, "_calculate_length"):
                asset._calculate_length()
            count = int(asset.frame_count or 0)
        else:
            paths = [os.path.join(asset.path, f) for f in asset.get_frame_files()]
            count = len(paths)
        revision = tuple(_stat_entry(p) for p in paths)
        dims = asset.get_dimensions()
        colour = None
        if (not is_video and paths and paths[0].lower().endswith(".exr")):
            try:
                colour = sample_input_colour(paths)
            except Exception as exc:  # sample failure -> unavailable
                logger.debug("colour sample failed: %s", exc)
                colour = None
        if dims is None and colour is not None and colour.dimensions:
            dims = colour.dimensions
        error = None
        if count <= 0 or not dims:
            error = "source metadata unavailable"
        w, h = (int(dims[0]), int(dims[1])) if dims else (None, None)
        return InferenceContext(root, revision, w, h, count, budget, colour, error)
    except Exception as exc:
        logger.debug("inference context failed for %s: %s", root, exc)
        return InferenceContext(root, (), None, None, 0, None, None, f"{exc}")


class _ContextSignals(QObject):
    finished = Signal(int, object)  # token, InferenceContext


class _ContextTask(QRunnable):
    def __init__(self, token: int, clip):
        super().__init__()
        self.signals = _ContextSignals()
        self.token = token
        self._clip = clip
        self.setAutoDelete(False)  # controller retains until finished

    def run(self) -> None:
        try:
            ctx = build_inference_context(self._clip)
        except Exception as exc:  # defensive; builder already catches
            ctx = InferenceContext("", (), None, None, 0, None, None, str(exc))
        self._clip = None
        self.signals.finished.emit(self.token, ctx)


class InferenceContextController(QObject):
    ready = Signal(object)  # InferenceContext

    def __init__(self, parent=None, pool: QThreadPool | None = None):
        super().__init__(parent)
        if pool is None:
            pool = QThreadPool(self)
            pool.setMaxThreadCount(1)
        self._pool = pool
        self._token = 0
        self._closed = False
        self._running: dict[int, _ContextTask] = {}
        self._pending: tuple[int, object] | None = None

    def request(self, clip) -> None:
        if self._closed:
            return
        self._token += 1
        try:
            snapshot = copy.deepcopy(clip)
        except Exception:
            logger.debug("clip snapshot failed", exc_info=True)
            snapshot = None
        self._pending = (self._token, snapshot)
        self._pump()

    def invalidate(self) -> None:
        self._token += 1
        self._pending = None

    def close(self) -> None:
        self._closed = True
        self._token += 1
        self._pending = None

    def _pump(self) -> None:
        if self._closed or self._running or self._pending is None:
            return
        token, clip = self._pending
        self._pending = None
        task = _ContextTask(token, clip)
        task.signals.finished.connect(self._on_finished, Qt.QueuedConnection)
        self._running[id(task)] = task
        task.signals.setProperty("_task_id", id(task))
        self._pool.start(task)

    def _on_finished(self, token: int, ctx) -> None:
        sender = self.sender()
        key = sender.property("_task_id") if sender is not None else None
        if key is not None:
            self._running.pop(key, None)
        else:
            self._running.clear()
        if self._closed:
            return
        current = token == self._token
        self._pump()
        if current:
            self.ready.emit(ctx)
