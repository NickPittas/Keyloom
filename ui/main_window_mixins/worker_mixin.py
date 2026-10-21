from __future__ import annotations

import dataclasses
import logging

from PySide6.QtWidgets import QMessageBox
from PySide6.QtCore import Slot

from . import _tr
from .hint_reset_mixin import refresh_hint_tools_if_available

from backend import ClipState, JobType
from ui.workers.gpu_job_worker import create_job_snapshot
from ui.workers.job_helpers import clip_frame_range, create_inference_job, reset_complete_state

logger = logging.getLogger(__name__)


class WorkerMixin:
    """GPU worker signal handlers for MainWindow."""

    @Slot(str, str, int, int)
    def _on_worker_progress(self, job_id: str, clip_name: str, current: int, total: int, fps: float = 0.0) -> None:
        if self._cancel_requested_job_id == job_id:
            self._queue_panel.refresh()
            return

        # Set active_job_id only on first progress of a new job (not every event)
        if self._active_job_id != job_id:
            # Only update if this is genuinely a new running job
            current_job = self._service.job_queue.current_job
            if current_job and current_job.id == job_id:
                self._active_job_id = job_id

        if job_id == self._active_job_id:
            self._status_bar.update_progress(current, total, fps)

        if self._current_clip and self._current_clip.name == clip_name:
            self._schedule_live_asset_refresh(clip_name, current, total)

        self._queue_panel.refresh()

        # Batch pipeline dialog update
        if hasattr(self, '_on_batch_progress'):
            self._on_batch_progress(job_id, clip_name, current, total, fps)

    def _schedule_live_asset_refresh(self, clip_name: str, current: int, total: int) -> None:
        """Coalesce progress-driven asset refreshes for the selected clip.

        This keeps the feedback live without rescanning on every frame tick or
        touching any GPU-side work.
        """
        if current <= 0 or total <= 0:
            return

        step = max(1, total // 50)
        should_refresh = current <= 3 or current >= total or total <= 30 or current % step == 0
        if not should_refresh:
            return

        self._pending_live_asset_refresh_clip = clip_name
        if not self._live_asset_refresh_timer.isActive():
            self._live_asset_refresh_timer.start()

    @Slot()
    def _refresh_selected_clip_live_assets(self) -> None:
        """Refresh the selected clip's coverage/modes without resetting navigation."""
        clip_name = self._pending_live_asset_refresh_clip
        self._pending_live_asset_refresh_clip = None

        if clip_name is None or self._current_clip is None:
            return
        if self._current_clip.name != clip_name:
            return

        self._dual_viewer.refresh_generated_assets()

    @Slot(str, str)
    def _on_worker_status(self, job_id: str, message: str) -> None:
        """Phase status from long-running jobs (e.g. VideoMaMa loading phases).

        Always update — status signals fire during loading phases before
        progress signals arrive, so _active_job_id may not match yet.
        Also set _active_job_id if not already set.
        """
        if self._cancel_requested_job_id == job_id:
            return
        if self._active_job_id is None:
            self._active_job_id = job_id
        from ui.workers.gpu_job_worker import PHASE_PREFIX
        if message.startswith(PHASE_PREFIX):
            self._status_bar.set_progress_phase(message[len(PHASE_PREFIX):])
            return
        from ui.state_labels import backend_status_text
        self._status_bar.set_phase(backend_status_text(message))

    @Slot(str, str, int, str)
    def _on_worker_preview(self, job_id: str, clip_name: str, frame_index: int, path: str) -> None:
        if self._cancel_requested_job_id == job_id:
            return
        # Only update preview if this is the active job
        if job_id == self._active_job_id:
            self._dual_viewer.load_preview_from_file(path, clip_name, frame_index)

    @Slot(str, str, str)
    def _on_worker_clip_finished(self, job_id: str, clip_name: str, job_type: str) -> None:
        self._force_stop_armed = False
        self._status_bar.set_stop_button_mode(force=False)
        # Map job type to the state the finished step should leave the clip in.
        # INFERENCE is resolved from disk below (only a full-range manifest is COMPLETE).
        is_inference = job_type == JobType.INFERENCE.value
        if job_type == JobType.SAM2_TRACK.value:
            target_state = ClipState.MASKED
        elif job_type in (JobType.GVM_ALPHA.value, JobType.VIDEOMAMA_ALPHA.value,
                          JobType.MATANYONE2_ALPHA.value, JobType.BIREFNET_ALPHA.value,
                          JobType.CHROMA_KEY_ALPHA.value, JobType.APPLEVISION_ALPHA.value):
            target_state = ClipState.READY
        elif is_inference:
            target_state = None
        else:
            target_state = ClipState.COMPLETE

        # Clear processing lock and rescan assets (re-resolves state from disk)
        for clip in self._clip_model.clips:
            if clip.name == clip_name:
                clip.set_processing(False)
                try:
                    clip.find_assets()
                except Exception:
                    pass
                if is_inference:
                    # find_assets() resolved COMPLETE only for a full-range manifest;
                    # a partial-range run leaves RAW/MASKED/READY.
                    target_state = clip.state
                else:
                    clip.state = target_state
                self._clip_model.update_clip_state(clip_name, clip.state)
                break
        if target_state is None:
            self._clip_model.update_clip_state(clip_name, ClipState.RAW)
            target_state = ClipState.RAW

        finished_clip = next((c for c in self._clip_model.clips if c.name == clip_name), None)
        if finished_clip is not None:
            self._refresh_input_thumbnail(finished_clip)
            self._refresh_export_thumbnail(finished_clip)

        # Reload full-res frame on the output viewer. During rendering the
        # preview system sends downscaled 960px images; without this the
        # viewer keeps showing the low-res preview after the job finishes.
        if self._current_clip and self._current_clip.name == clip_name:
            self._dual_viewer.refresh_generated_assets()

        # Pipeline auto-chain: queue the next stage, if any.
        steps = self._pipeline_steps.get(clip_name)
        if steps:
            next_step = steps.pop(0)
            queued_next = False
            for clip in self._clip_model.clips:
                if clip.name != clip_name:
                    continue
                if next_step == JobType.INFERENCE:
                    s = self._pipeline_settings.get(clip_name)
                    if s is not None:
                        job = create_inference_job(
                            clip, s["params"], s["output_config"], frame_range=s["frame_range"],
                        )
                    else:
                        # Legacy chain: always follows a hint generator, so the hint is used.
                        job = create_inference_job(
                            clip,
                            dataclasses.replace(self._param_panel.get_params(), use_alpha_hint=True),
                            self._param_panel.get_output_config(),
                            frame_range=clip_frame_range(clip),
                        )
                    reset_complete_state(clip)
                else:
                    job = create_job_snapshot(clip, job_type=next_step)
                clip.set_processing(True)
                if self._service.job_queue.submit(job):
                    queued_next = True
                    logger.info(
                        "Pipeline auto-chain: queued %s for %s",
                        next_step.value,
                        clip_name,
                    )
                else:
                    clip.set_processing(False)
                break
            if not queued_next:
                logger.warning("Pipeline auto-chain failed to queue next step for %s", clip_name)
                self._pipeline_steps.pop(clip_name, None)
                self._pipeline_settings.pop(clip_name, None)
        if not self._pipeline_steps.get(clip_name):
            self._pipeline_steps.pop(clip_name, None)
            self._pipeline_settings.pop(clip_name, None)

        # Stop timer; only exit running state if no more jobs are pending
        has_more = self._service.job_queue.has_pending or self._pipeline_steps
        self._status_bar.stop_job_timer()
        if not has_more:
            self._status_bar.set_running(False)
        else:
            # Reset progress for next job — show descriptive label
            self._status_bar.reset_progress()
            next_job = self._service.job_queue.next_job()
            from ui.state_labels import job_type_display_name
            if next_job:
                next_label = job_type_display_name(next_job.job_type)
            else:
                next_label = job_type_display_name(None)
            self._status_bar.start_job_timer(label=next_label)

        from ui.sounds.audio_manager import UIAudio
        if target_state == ClipState.MASKED:
            UIAudio.mask_done()
            self._status_bar.set_message(_tr("Track Mask complete for %s") % clip_name)
        elif target_state == ClipState.READY:
            UIAudio.mask_done()
            type_label = {
                JobType.GVM_ALPHA.value: "GVM Auto",
                JobType.BIREFNET_ALPHA.value: "BiRefNet",
                JobType.VIDEOMAMA_ALPHA.value: "VideoMaMa",
                JobType.MATANYONE2_ALPHA.value: "MatAnyone2",
            }.get(job_type, _tr("Alpha"))
            # Show alpha coverage count
            alpha_info = ""
            for c in self._clip_model.clips:
                if c.name == clip_name and c.alpha_asset and c.input_asset:
                    alpha_info = f" ({c.alpha_asset.frame_count}/{c.input_asset.frame_count} alpha frames)"
                    break
            self._status_bar.set_message(
                _tr("%s complete for %s%s -- Ready to Run Inference") % (type_label, clip_name, alpha_info)
            )
        elif target_state == ClipState.COMPLETE and is_inference:
            UIAudio.inference_done()
            self._status_bar.set_message(_tr("Inference complete: %s") % clip_name)
        elif is_inference:
            self._status_bar.set_message(_tr("Partial-range inference finished: %s") % clip_name)

        # Refresh views
        self._pending_live_asset_refresh_clip = None
        self._live_asset_refresh_timer.stop()
        self._queue_panel.refresh()
        self._io_tray.refresh()

        # If selected clip, reload preview to show new assets
        if self._current_clip and self._current_clip.name == clip_name:
            self._sync_selected_clip_view(self._current_clip)
            self._refresh_button_state()
            refresh_hint_tools_if_available(self)

        logger.info(f"Clip finished ({job_type}): {clip_name} -> {target_state.value}")

        # Batch pipeline dialog update
        if hasattr(self, '_on_batch_clip_finished'):
            self._on_batch_clip_finished(job_id, clip_name, job_type)

    @Slot(str, str)
    def _on_worker_warning(self, job_id: str, message: str) -> None:
        if message.startswith("Cancelled:"):
            self._cancel_requested_job_id = None
            self._active_job_id = None
            self._force_stop_armed = False
            self._status_bar.set_stop_button_mode(force=False)
            # Job was cancelled — clear processing lock on the clip
            clip_name = message.removeprefix("Cancelled:").strip()
            self._pipeline_steps.pop(clip_name, None)
            self._pipeline_settings.pop(clip_name, None)
            for clip in self._clip_model.clips:
                if clip.name == clip_name:
                    clip.set_processing(False)
                    # Refresh viewer to show any partial output from before cancel
                    if self._current_clip and self._current_clip.name == clip_name:
                        clip.find_assets()
                        self._sync_selected_clip_view(clip)
                        self._refresh_button_state()
                    break
            refresh_hint_tools_if_available(self)
            self._status_bar.stop_job_timer()
            self._status_bar.set_running(False)
            self._status_bar.set_message(_tr("Cancelled: %s") % clip_name)
            self._pending_live_asset_refresh_clip = None
            self._live_asset_refresh_timer.stop()
            self._queue_panel.refresh()
            logger.info(f"Job cancelled: {clip_name}")
        else:
            self._status_bar.add_warning(message)
            logger.warning(f"Worker warning: {message}")

    @Slot(str, str, str)
    def _on_worker_error(self, job_id: str, clip_name: str, error_msg: str) -> None:
        if self._cancel_requested_job_id == job_id:
            self._cancel_requested_job_id = None
            self._active_job_id = None
        self._force_stop_armed = False
        self._status_bar.set_stop_button_mode(force=False)
        self._status_bar.stop_job_timer()
        self._status_bar.set_running(False)
        self._pending_live_asset_refresh_clip = None
        self._live_asset_refresh_timer.stop()
        self._pipeline_steps.pop(clip_name, None)
        self._pipeline_settings.pop(clip_name, None)
        self._clip_model.update_clip_state(clip_name, ClipState.ERROR)
        # Clear processing lock
        for clip in self._clip_model.clips:
            if clip.name == clip_name:
                clip.set_processing(False)
                # Refresh viewer to show any partial output
                if self._current_clip and self._current_clip.name == clip_name:
                    clip.find_assets()
                    self._sync_selected_clip_view(clip)
                break
        refresh_hint_tools_if_available(self)
        self._queue_panel.refresh()
        logger.error(f"Worker error for {clip_name}: {error_msg}")
        from ui.sounds.audio_manager import UIAudio
        UIAudio.error()

        # Try to match a known error pattern for actionable diagnostics
        from ui.widgets.diagnostic_dialog import match_diagnostic, DiagnosticDialog
        diag = match_diagnostic(error_msg)
        if diag:
            dlg = DiagnosticDialog(
                diag, error_msg,
                gpu_info=self._service.get_vram_info(),
                recent_errors=self._debug_console.recent_errors()
                if hasattr(self, '_debug_console') else None,
                parent=self,
            )
            dlg.exec()
        else:
            QMessageBox.critical(self, _tr("Processing Error"), _tr("Clip: %s\n\n%s") % (clip_name, error_msg))

        # Batch pipeline dialog update
        if hasattr(self, '_on_batch_clip_error'):
            self._on_batch_clip_error(job_id, clip_name, error_msg)

    @Slot()
    def _on_queue_empty(self) -> None:
        if self._service.job_queue.has_pending:
            return
        self._cancel_requested_job_id = None
        self._force_stop_armed = False
        self._status_bar.set_stop_button_mode(force=False)
        self._status_bar.set_running(False)
        self._status_bar.stop_job_timer()
        self._active_job_id = None
        self._pending_live_asset_refresh_clip = None
        self._live_asset_refresh_timer.stop()
        self._pipeline_steps.clear()
        self._pipeline_settings.clear()
        self._queue_panel.refresh()
        # current_job_id is cleared before queue_empty fires: re-enable hint tools now
        refresh_hint_tools_if_available(self)
        # NOTE: Do NOT call unload_engines() here — engines stay loaded so the
        # next whole-clip job reuses them.  Model switching is already handled
        # by _ensure_model() when a different model type is needed.
        logger.info("All jobs completed, queue idle")
