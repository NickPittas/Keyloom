"""GPU worker — consumes jobs from GPUJobQueue, strictly one at a time.

Design decisions:
- ONE dedicated QThread runs every job inline, serially (CorridorKey 2.0
  runs whole clips; there is no pool and no concurrent GPU work)
- Emits signals AFTER releasing any locks (no deadlock risk)
- Progress carries the current phase's fraction; file previews are saved
  only from "publish" phase ticks (frames actually written to disk)
- Preview saved to temp file — QPixmap created on main thread only
- All signals carry job.id (stable, assigned at creation time)
- Job params are FROZEN SNAPSHOTS: paths + params dict, not live references
- Cancellation is cooperative: stop() only flags the job; the service raises
  JobCancelledError at its next check. stop() never blocks.
"""
from __future__ import annotations

import os
import logging
import tempfile

import cv2
import numpy as np
from PySide6.QtCore import QCoreApplication, QThread, Signal, QMutex, QWaitCondition

from backend import (
    CorridorKeyService,
    ClipEntry,
    ClipState,
    GPUJob,
    GPUJobQueue,
    InferenceParams,
    JobType,
)
from backend.job_queue import JobStatus
from backend.frame_io import imread_unicode, imwrite_unicode
from backend.errors import JobCancelledError, CorridorKeyError

# Re-export create_job_snapshot for backward compatibility
from ui.workers.job_helpers import create_job_snapshot

logger = logging.getLogger(__name__)

# status_update messages starting with this prefix carry a *progress phase*
# label (shown next to the frame counter), not a loading-phase message.
PHASE_PREFIX = "\x00phase:"


def _phase_label(phase: str) -> str:
    """User-facing label for a service progress phase (None if unknown)."""
    tr = QCoreApplication.translate
    return {
        "rvm": tr("GPUJobWorker", "Video matting (RVM)"),
        "vfm": tr("GPUJobWorker", "Foreground model (VFM)"),
        "stage1": tr("GPUJobWorker", "Stage 1 - windows"),
        "stage2": tr("GPUJobWorker", "Stage 2 - refine"),
        "publish": tr("GPUJobWorker", "Writing frames"),
    }.get(phase, "")


class GPUJobWorker(QThread):
    """GPU worker thread: runs queued jobs strictly one at a time.

    Signals carry job.id (stable, assigned at submit time) so the UI
    can ignore stale signals from previous selections or cancelled jobs.
    """

    # Signals — all carry job_id as first arg for stale detection
    progress = Signal(str, str, int, int, float)  # job_id, clip_name, current, total, fps (current phase fraction)
    preview_ready = Signal(str, str, int, str) # job_id, clip_name, frame_index, temp_file_path
    clip_finished = Signal(str, str, str)       # job_id, clip_name, job_type_value
    warning = Signal(str, str)                 # job_id, message
    status_update = Signal(str, str)           # job_id, status_text (phase label for status bar)
    error = Signal(str, str, str)              # job_id, clip_name, error_message
    queue_empty = Signal()                     # all jobs done
    sam2_preview_result = Signal(str, object)  # job_id, result_dict (SAM2 prompt preview)

    def __init__(self, service: CorridorKeyService, parent=None):
        super().__init__(parent)
        self._service = service
        self._queue = service.job_queue
        self._running = False
        self._mutex = QMutex()
        self._condition = QWaitCondition()
        self._preview_interval = 5  # emit preview every N published frames
        self._preview_dir = tempfile.mkdtemp(prefix="corridorkey_preview_")
        self._current_job_id: str | None = None
        self._queue_empty_emitted = True

    @property
    def preview_dir(self) -> str:
        return self._preview_dir

    @property
    def current_job_id(self) -> str | None:
        """ID of the job currently executing, or None."""
        return self._current_job_id

    def is_busy(self) -> bool:
        """True while a job is executing."""
        return self._current_job_id is not None

    def wake(self) -> None:
        """Wake the worker to check for new jobs."""
        self._condition.wakeOne()

    def stop(self) -> None:
        """Request stop. Non-blocking: cancels queued/running jobs cooperatively."""
        self._running = False
        self._queue.cancel_all()
        self._condition.wakeOne()

    def run(self) -> None:
        """Main consumer loop — runs each job inline, one at a time."""
        self._running = True
        self._queue_empty_emitted = True  # Start idle — no spurious emit on launch
        logger.info("GPU worker started")

        while self._running:
            job = self._queue.next_job()

            if job is None:
                if not self._queue.has_pending and not self._queue_empty_emitted:
                    self._queue_empty_emitted = True
                    self.queue_empty.emit()

                self._mutex.lock()
                if self._running and not self._queue.has_pending:
                    self._condition.wait(self._mutex, 500)
                self._mutex.unlock()
                continue

            self._queue_empty_emitted = False  # Reset — will fire again when all done
            self._process_job(job)

        logger.info("GPU worker stopped")

    def _process_job(self, job: GPUJob) -> None:
        """Execute a single GPU job inline (marks start/complete)."""
        self._queue.start_job(job)
        self._current_job_id = job.id
        try:
            self._process_job_inner(job)
        finally:
            self._current_job_id = None
        if not self._queue.has_pending and not self._queue_empty_emitted:
            self._queue_empty_emitted = True
            self.queue_empty.emit()

    def _process_job_inner(self, job: GPUJob) -> None:
        """Execute a GPU job. Assumes start_job() has already been called."""
        job_id = job.id
        logger.info(f">>> PROCESS_JOB START [{job_id}]: type={job.job_type.value}, clip={job.clip_name}")

        try:
            if job.job_type == JobType.INFERENCE:
                self._run_inference(job)
            elif job.job_type == JobType.GVM_ALPHA:
                self._run_gvm(job)
            elif job.job_type == JobType.SAM2_PREVIEW:
                self._run_sam2_preview(job)
            elif job.job_type == JobType.SAM2_TRACK:
                self._run_sam2_track(job)
            elif job.job_type == JobType.VIDEOMAMA_ALPHA:
                self._run_videomama(job)
            elif job.job_type == JobType.MATANYONE2_ALPHA:
                self._run_matanyone2(job)
            elif job.job_type == JobType.BIREFNET_ALPHA:
                self._run_birefnet(job)
            elif job.job_type == JobType.CHROMA_KEY_ALPHA:
                self._run_chroma_key(job)
            elif job.job_type == JobType.APPLEVISION_ALPHA:
                self._run_applevision(job)
            elif job.job_type == JobType.PREVIEW_REPROCESS:
                self._run_preview_reprocess(job)
            else:
                self._queue.fail_job(job, f"Unknown job type: {job.job_type}")
                self.error.emit(job_id, job.clip_name, f"Unknown job type: {job.job_type}")
                return

            # If cancel was requested during a non-interruptible phase
            # (e.g. model load / CUDA teardown), honor it before marking the
            # job completed.
            if job.is_cancelled:
                self._queue.mark_cancelled(job)
                logger.info(f"Job cancelled after phase return [{job_id}]: {job.clip_name}")
                self.warning.emit(job_id, f"Cancelled: {job.clip_name}")
                return

            self._queue.complete_job(job)
            if job.job_type not in (JobType.PREVIEW_REPROCESS, JobType.SAM2_PREVIEW):
                self.clip_finished.emit(job_id, job.clip_name, job.job_type.value)

        except JobCancelledError:
            self._queue.mark_cancelled(job)
            logger.info(f"Job cancelled [{job_id}]: {job.clip_name}")
            self.warning.emit(job_id, f"Cancelled: {job.clip_name}")

        except CorridorKeyError as e:
            logger.error(f"Job failed [{job_id}]: {job.clip_name} — {e}")
            try:
                from backend.error_reporting import capture_stage_exception
                capture_stage_exception("inference", e)
            except Exception:
                pass
            self._queue.fail_job(job, str(e))
            self.error.emit(job_id, job.clip_name, str(e))

        except Exception as e:
            msg = f"Unexpected error: {e}"
            try:
                from backend.error_reporting import capture_stage_exception
                capture_stage_exception("inference", e)
            except Exception:
                pass
            self._queue.fail_job(job, msg)
            self.error.emit(job_id, job.clip_name, msg)
            logger.exception(msg)

    def _run_inference(self, job: GPUJob) -> None:
        """Run CorridorKey inference for a single clip."""
        clip = job.params.get("_clip_snapshot")
        params = job.params.get("_inference_params")
        skip_stems = job.params.get("_skip_stems", set())

        if clip is None or params is None:
            raise CorridorKeyError(f"Job [{job.id}] for '{job.clip_name}' missing clip or params snapshot")

        last_phase: list[str | None] = [None]

        def on_progress(clip_name: str, current: int, total: int, **kwargs) -> None:
            phase = kwargs.get("phase")
            if phase and phase != last_phase[0]:
                last_phase[0] = phase
                label = _phase_label(phase)
                if label:
                    self.status_update.emit(job.id, PHASE_PREFIX + label)
            self.progress.emit(job.id, clip_name, current, total, kwargs.get("fps", 0.0))

            # Only "publish" ticks mean frames hit disk; other phases
            # (rvm/vfm/stage1/stage2) never produce a file preview.
            if kwargs.get("phase") != "publish":
                return
            if current > 0 and (current % self._preview_interval == 0 or current == total):
                self._save_preview(job.id, clip, current)

        def on_warning(message: str) -> None:
            self.warning.emit(job.id, message)

        def on_status(message: str) -> None:
            self.status_update.emit(job.id, message)

        output_config = job.params.get("_output_config")
        frame_range = job.params.get("_frame_range")
        self._service.run_inference(
            clip=clip,
            params=params,
            job=job,
            on_progress=on_progress,
            on_warning=on_warning,
            on_status=on_status,
            skip_stems=skip_stems,
            output_config=output_config,
            frame_range=frame_range,
        )

    def _run_gvm(self, job: GPUJob) -> None:
        """Run GVM auto alpha generation."""
        clip = job.params.get("_clip_snapshot")
        if clip is None:
            raise CorridorKeyError(f"Job [{job.id}] for '{job.clip_name}' missing clip snapshot")

        def on_progress(clip_name: str, current: int, total: int, **kwargs) -> None:
            self.progress.emit(job.id, clip_name, current, total, kwargs.get("fps", 0.0))

        def on_warning(message: str) -> None:
            self.warning.emit(job.id, message)

        self._service.run_gvm(
            clip=clip,
            job=job,
            on_progress=on_progress,
            on_warning=on_warning,
        )

    def _run_chroma_key(self, job: GPUJob) -> None:
        """Run chroma key alpha hint generation (CPU-only, no GPU lock)."""
        clip = job.params.get("_clip_snapshot")
        if clip is None:
            raise CorridorKeyError(f"Job [{job.id}] for '{job.clip_name}' missing clip snapshot")

        chroma_params = job.params.get("_chroma_params", {})

        def on_progress(clip_name: str, current: int, total: int, **kwargs) -> None:
            self.progress.emit(job.id, clip_name, current, total, kwargs.get("fps", 0.0))

        def on_warning(message: str) -> None:
            self.warning.emit(job.id, message)

        self._service.run_chroma_key(
            clip=clip,
            chroma_params=chroma_params,
            job=job,
            on_progress=on_progress,
            on_warning=on_warning,
        )

    def _run_applevision(self, job: GPUJob) -> None:
        """Run Apple Vision foreground alpha hint generation (Neural Engine)."""
        clip = job.params.get("_clip_snapshot")
        if clip is None:
            raise CorridorKeyError(f"Job [{job.id}] for '{job.clip_name}' missing clip snapshot")

        def on_progress(clip_name: str, current: int, total: int, **kwargs) -> None:
            self.progress.emit(job.id, clip_name, current, total, kwargs.get("fps", 0.0))

        def on_warning(message: str) -> None:
            self.warning.emit(job.id, message)

        self._service.run_applevision(
            clip=clip,
            job=job,
            on_progress=on_progress,
            on_warning=on_warning,
        )

    def _run_sam2_track(self, job: GPUJob) -> None:
        """Run SAM2 prompt-to-mask tracking."""
        clip = job.params.get("_clip_snapshot")
        params = job.params.get("_inference_params")
        if clip is None:
            raise CorridorKeyError(f"Job [{job.id}] for '{job.clip_name}' missing clip snapshot")

        def on_progress(clip_name: str, current: int, total: int, **kwargs) -> None:
            self.progress.emit(job.id, clip_name, current, total, kwargs.get("fps", 0.0))

        def on_warning(message: str) -> None:
            self.warning.emit(job.id, message)

        def on_status(message: str) -> None:
            self.status_update.emit(job.id, message)

        self._service.run_sam2_track(
            clip=clip,
            input_is_linear=(params.input_is_linear if isinstance(params, InferenceParams) else None),
            job=job,
            on_progress=on_progress,
            on_warning=on_warning,
            on_status=on_status,
        )

    def _run_sam2_preview(self, job: GPUJob) -> None:
        """Run a one-frame SAM2 preview without writing tracked masks."""
        clip = job.params.get("_clip_snapshot")
        frame_index = job.params.get("_frame_index")
        params = job.params.get("_inference_params")
        if clip is None:
            raise CorridorKeyError(f"Job [{job.id}] for '{job.clip_name}' missing clip snapshot")

        def on_progress(clip_name: str, current: int, total: int, **kwargs) -> None:
            self.progress.emit(job.id, clip_name, current, total, kwargs.get("fps", 0.0))

        def on_warning(message: str) -> None:
            self.warning.emit(job.id, message)

        def on_status(message: str) -> None:
            self.status_update.emit(job.id, message)

        result = self._service.preview_sam2_prompt(
            clip=clip,
            preferred_frame_index=frame_index,
            input_is_linear=(params.input_is_linear if isinstance(params, InferenceParams) else None),
            job=job,
            on_progress=on_progress,
            on_warning=on_warning,
            on_status=on_status,
        )
        if result is not None:
            self.sam2_preview_result.emit(job.id, result)

    def _run_videomama(self, job: GPUJob) -> None:
        """Run VideoMaMa guided alpha generation."""
        clip = job.params.get("_clip_snapshot")
        chunk_size = job.params.get("_chunk_size", 16)

        if clip is None:
            raise CorridorKeyError(f"Job [{job.id}] for '{job.clip_name}' missing clip snapshot")

        def on_progress(clip_name: str, current: int, total: int, **kwargs) -> None:
            self.progress.emit(job.id, clip_name, current, total, kwargs.get("fps", 0.0))

        def on_warning(message: str) -> None:
            self.warning.emit(job.id, message)

        def on_status(message: str) -> None:
            self.status_update.emit(job.id, message)

        self._service.run_videomama(
            clip=clip,
            job=job,
            on_progress=on_progress,
            on_warning=on_warning,
            on_status=on_status,
            chunk_size=chunk_size,
        )

    def _run_matanyone2(self, job: GPUJob) -> None:
        """Run MatAnyone2 video matting alpha generation."""
        clip = job.params.get("_clip_snapshot")

        if clip is None:
            raise CorridorKeyError(f"Job [{job.id}] for '{job.clip_name}' missing clip snapshot")

        def on_progress(clip_name: str, current: int, total: int, **kwargs) -> None:
            self.progress.emit(job.id, clip_name, current, total, kwargs.get("fps", 0.0))

        def on_warning(message: str) -> None:
            self.warning.emit(job.id, message)

        def on_status(message: str) -> None:
            self.status_update.emit(job.id, message)

        self._service.run_matanyone2(
            clip=clip,
            job=job,
            on_progress=on_progress,
            on_warning=on_warning,
            on_status=on_status,
        )

    def _run_birefnet(self, job: GPUJob) -> None:
        """Run BiRefNet automatic alpha generation."""
        clip = job.params.get("_clip_snapshot")

        if clip is None:
            raise CorridorKeyError(f"Job [{job.id}] for '{job.clip_name}' missing clip snapshot")

        usage = job.params.get("_birefnet_usage", "Matting")

        def on_progress(clip_name: str, current: int, total: int, **kwargs) -> None:
            self.progress.emit(job.id, clip_name, current, total, kwargs.get("fps", 0.0))

        def on_warning(message: str) -> None:
            self.warning.emit(job.id, message)

        def on_status(message: str) -> None:
            self.status_update.emit(job.id, message)

        self._service.run_birefnet(
            clip=clip,
            usage=usage,
            job=job,
            on_progress=on_progress,
            on_warning=on_warning,
            on_status=on_status,
        )

    def _run_preview_reprocess(self, job: GPUJob) -> None:
        """Single-frame v1 re-key no longer exists in 2.0 — drop stale jobs."""
        logger.info("Ignoring obsolete PREVIEW_REPROCESS job [%s]", job.id)

    def _save_preview(self, job_id: str, clip: ClipEntry, frame_index: int) -> None:
        """Save a downscaled preview of the latest comp frame to temp dir.

        The main thread will load this as QPixmap — we never create
        QPixmap off the GUI thread (Codex finding).
        """
        try:
            from backend.natural_sort import natsorted
            out_dir = clip.output_dir
            comp_dir = None
            comp_files: list[str] = []
            for sub in ("Comp", "Processed"):
                cand = os.path.join(out_dir, sub)
                if os.path.isdir(cand):
                    files = natsorted(os.listdir(cand))
                    if files:
                        comp_dir, comp_files = cand, files
                        break
            if comp_dir is None:
                return

            latest = os.path.join(comp_dir, comp_files[-1])
            img = imread_unicode(latest)
            if img is None:
                return

            # Downscale for preview (max 960px wide)
            h, w = img.shape[:2]
            if w > 960:
                scale = 960 / w
                img = cv2.resize(img, (960, int(h * scale)), interpolation=cv2.INTER_AREA)

            preview_path = os.path.join(self._preview_dir, f"preview_{job_id}.png")
            imwrite_unicode(preview_path, img)

            self.preview_ready.emit(job_id, clip.name, frame_index, preview_path)
        except Exception as e:
            logger.debug(f"Preview save skipped: {e}")


__all__ = ["GPUJobWorker", "create_job_snapshot"]
