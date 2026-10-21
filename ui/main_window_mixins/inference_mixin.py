from __future__ import annotations

import dataclasses
import logging

import numpy as np
from PySide6.QtWidgets import QMessageBox
from PySide6.QtCore import Slot, QThread

from . import _tr
from .hint_reset_mixin import refresh_hint_tools_if_available

from backend import (
    ClipState, JobType,
    PipelineRoute, classify_pipeline_route,
)
from backend.clip_state import can_run_inference
from ui.preview.frame_index import ViewMode
from ui.workers.gpu_job_worker import create_job_snapshot
from ui.workers.job_helpers import (
    clip_frame_range, create_inference_job, reset_complete_state,
)

logger = logging.getLogger(__name__)


class InferenceMixin:
    """Inference control, parameter changes, and GPU job submission for MainWindow."""

    @Slot()
    def _on_params_changed(self) -> None:
        """Handle parameter change — keep colour-space state and thumbnails in sync."""
        self._remember_current_clip_input_color_space()
        self._dual_viewer.set_input_exr_is_linear(
            self._param_panel.get_params().input_is_linear
        )
        if self._current_clip is not None:
            self._refresh_input_thumbnail(
                self._current_clip,
                input_is_linear=self._param_panel.get_params().input_is_linear,
            )

    @Slot(str)
    def _on_output_mode_changed(self, mode_value: str) -> None:
        """Keep export-strip thumbnails aligned with the current output viewer mode."""
        try:
            ViewMode(mode_value)
        except ValueError:
            return

        for clip in self._clip_model.clips:
            if clip.state == ClipState.COMPLETE:
                self._refresh_export_thumbnail(clip)

    @Slot(str)
    def _on_screen_color_changed(self, color: str) -> None:
        """Update the UI accent color when BG Color changes.

        Reloads the QSS with the appropriate accent palette and
        updates the brand mark text color.
        """
        from ui.theme import load_stylesheet, ACCENT_COLORS

        # Resolve "auto" to detected color for the current clip
        effective = color
        if color == "auto" and self._current_clip is not None:
            effective = getattr(self._current_clip, '_screen_color_cache', 'green')
        if effective not in ("green", "blue"):
            effective = "green"

        # Swap QSS accent colors
        from PySide6.QtWidgets import QApplication
        app = QApplication.instance()
        if app:
            app.setStyleSheet(load_stylesheet(screen_color=effective))

        # Update brand mark text — hyphen and KEY carry the accent
        palette = ACCENT_COLORS[effective]
        self._current_accent = palette["accent"]
        self._brand_label.setText(
            f'<span style="color:#FFF203;">EZ</span>'
            f'<span style="color:{palette["accent"]};">-</span>'
            f'<span style="color:#FFF203;">CORRIDOR</span>'
            f'<span style="color:{palette["accent"]};">KEY</span>'
        )

    @Slot(str, object)
    def _on_sam2_preview_result(self, job_id: str, result: object) -> None:
        """Handle a SAM2 Track Mask preview result."""
        if not isinstance(result, dict):
            return
        if job_id != getattr(self, "_sam2_preview_job_id", None):
            return

        if result.get("kind") == "sam2_preview":
            clip_name = result.get("clip_name")
            if not isinstance(clip_name, str):
                return
            clip = next((c for c in self._clip_model.clips if c.name == clip_name), None)
            if clip is None or self._current_clip is None or self._current_clip.name != clip_name:
                return

            frame_rgb = result.get("frame_rgb")
            mask = result.get("mask")
            if not isinstance(frame_rgb, np.ndarray) or not isinstance(mask, np.ndarray):
                return

            qimg = self._sam2_preview_qimage(frame_rgb, mask)
            self._dual_viewer.show_reprocess_preview(qimg)

            frame_number = int(result.get("frame_index", 0)) + 1
            fill_pct = float(result.get("fill", 0.0)) * 100.0
            reply = QMessageBox.question(
                self,
                _tr("Track Mask Preview"),
                _tr("SAM2 preview on frame %d covers %.1f%% of the frame.\n\n"
                    "If this looks right, continue with full Track Mask.\n"
                    "If not, keep painting corrections on this frame and run Track Mask again.") % (frame_number, fill_pct),
                QMessageBox.Yes | QMessageBox.No,
                QMessageBox.Yes,
            )
            if reply == QMessageBox.Yes:
                self._submit_sam2_track_job(clip)
            else:
                self._status_bar.set_message(_tr("Track preview ready. Refine paint strokes and run Track Mask again."))
            return

    # ── Inference Control ──

    @Slot()
    def _on_run_inference(self) -> None:
        # If multiple clips are selected, dispatch to pipeline
        if self._io_tray.selected_count() > 1:
            self._on_run_pipeline()
            return

        if self._current_clip is None:
            return

        clip = self._current_clip
        params = self._param_panel.get_params()
        if not can_run_inference(clip, params.use_alpha_hint):
            if params.use_alpha_hint and clip.alpha_asset is None:
                text = _tr(
                    "Clip '%s' has no AlphaHint. Generate one (GVM, BiRefNet, "
                    "VideoMaMa, MatAnyone2…) or turn off 'Use AlphaHint'."
                ) % clip.name
            else:
                text = _tr("Clip '%s' in %s state and cannot be keyed.") % (
                    clip.name, clip.state.value,
                )
            QMessageBox.warning(self, _tr("Not Ready"), text)
            return

        # Dense AlphaHint required: warn if it does not cover all input frames
        if params.use_alpha_hint and clip.alpha_asset and clip.input_asset:
            alpha_count = clip.alpha_asset.frame_count
            input_count = clip.input_asset.frame_count
            if 0 < alpha_count < input_count:
                msg = QMessageBox(self)
                msg.setIcon(QMessageBox.Warning)
                msg.setWindowTitle(_tr("Incomplete Alpha"))
                msg.setText(
                    _tr("Alpha hints cover %d of %d frames.\n\n"
                        "CorridorKey needs an AlphaHint for every frame. Re-run GVM to\n"
                        "regenerate all alpha frames, key without AlphaHint, or cancel.")
                    % (alpha_count, input_count)
                )
                btn_rerun = msg.addButton(_tr("Re-run GVM"), QMessageBox.ActionRole)
                btn_nohint = msg.addButton(_tr("Key without AlphaHint"), QMessageBox.ActionRole)
                msg.addButton(QMessageBox.Cancel)
                msg.exec()
                clicked = msg.clickedButton()
                if clicked == btn_rerun:
                    # Transition back to RAW and submit GVM job
                    clip.transition_to(ClipState.RAW)
                    self._clip_model.update_clip_state(clip.name, ClipState.RAW)
                    gvm_job = create_job_snapshot(clip, job_type=JobType.GVM_ALPHA)
                    if self._service.job_queue.submit(gvm_job):
                        clip.set_processing(True)
                        self._start_worker_if_needed(
                            gvm_job.id, job_label="GVM Auto",
                        )
                    return
                if clicked == btn_nohint:
                    params = dataclasses.replace(params, use_alpha_hint=False)
                else:
                    return  # Cancel

        reset_complete_state(clip)

        output_config = self._param_panel.get_output_config()
        job = create_inference_job(
            clip, params, output_config, frame_range=clip_frame_range(clip),
        )

        if not self._service.job_queue.submit(job):
            QMessageBox.information(self, _tr("Duplicate"), _tr("'%s' is already queued.") % clip.name)
            return

        self._start_worker_if_needed(job.id)

    @Slot()
    def _on_resume_inference(self) -> None:
        """Resume inference — skip already-processed frames, process full clip."""
        if self._current_clip is None:
            return

        clip = self._current_clip
        params = self._param_panel.get_params()
        if not can_run_inference(clip, params.use_alpha_hint):
            return

        reset_complete_state(clip)

        output_config = self._param_panel.get_output_config()
        # Resume always processes full clip — no in/out range
        job = create_inference_job(clip, params, output_config, resume=True)

        if not self._service.job_queue.submit(job):
            QMessageBox.information(self, _tr("Duplicate"), _tr("'%s' is already queued.") % clip.name)
            return

        self._start_worker_if_needed(job.id)

    def _refresh_button_state(self) -> None:
        """Update run/resume button state based on current clip."""
        clip = self._current_clip
        batch_count = self._io_tray.selected_count()
        if clip is None:
            self._status_bar.update_button_state(
                can_run=False, has_partial=False, has_in_out=False,
            )
            refresh_hint_tools_if_available(self)
            return
        can_run = can_run_inference(clip, self._param_panel.get_params().use_alpha_hint)
        self._status_bar.update_button_state(
            can_run=can_run,
            has_partial=clip.completed_frame_count() > 0,
            has_in_out=clip.in_out_range is not None,
            batch_count=batch_count if batch_count > 1 else 0,
        )
        refresh_hint_tools_if_available(self)

    @Slot()
    def _on_run_all_ready(self) -> None:
        """Queue every runnable (not COMPLETE) clip for inference.

        With 'Use AlphaHint' on, only READY clips with an AlphaHint qualify;
        with it off, any clip that has input frames does.
        """
        params = self._param_panel.get_params()
        ready_clips = [
            c for c in self._clip_model.clips
            if can_run_inference(c, params.use_alpha_hint)
            and c.state != ClipState.COMPLETE
        ]
        if not ready_clips:
            QMessageBox.information(self, _tr("No Clips"), _tr("No clips ready to process."))
            return

        output_config = self._param_panel.get_output_config()

        queued = 0
        first_job_id = None
        for clip in ready_clips:
            job = create_inference_job(
                clip, params, output_config, frame_range=clip_frame_range(clip),
            )
            if self._service.job_queue.submit(job):
                queued += 1
                if first_job_id is None:
                    first_job_id = job.id

        if queued > 0:
            # next_job() only peeks the queue head (job_queue never pops there);
            # use the first job we submitted instead.
            self._start_worker_if_needed(first_job_id)
            logger.info(f"Batch queued: {queued} clips")

    def _on_run_pipeline(self) -> None:
        """Full pipeline: classify each selected clip and queue appropriate jobs.

        Routes per clip (see PipelineRoute):
        - INFERENCE_ONLY: queue inference directly
        - GVM_PIPELINE: queue GVM, auto-chain inference on completion
        - VIDEOMAMA_PIPELINE: track dense masks, queue VideoMaMa, auto-chain inference
        - VIDEOMAMA_INFERENCE: queue VideoMaMa, auto-chain inference
        - SKIP: skip clips in EXTRACTING/ERROR state
        """
        selected = self._io_tray.get_selected_clips()
        if not selected:
            return
        self._auto_save_annotations()

        params = self._param_panel.get_params()
        output_config = self._param_panel.get_output_config()

        # Classify all clips
        routes: dict[str, PipelineRoute] = {}
        for clip in selected:
            route = classify_pipeline_route(clip, params.use_alpha_hint)
            if route != PipelineRoute.SKIP:
                routes[clip.name] = route

        if not routes:
            QMessageBox.information(
                self, _tr("Nothing to Process"),
                _tr("No selected clips are in a processable state."),
            )
            return

        self._pipeline_steps.clear()
        self._pipeline_settings.clear()

        queued = 0
        first_job_id = None

        for clip in selected:
            route = routes.get(clip.name)
            if route is None:
                continue
            job = None
            next_steps: list[JobType] = []

            if route == PipelineRoute.GVM_PIPELINE:
                job = create_job_snapshot(clip, job_type=JobType.GVM_ALPHA)
                next_steps = [JobType.INFERENCE]
            elif route == PipelineRoute.VIDEOMAMA_PIPELINE:
                job = create_job_snapshot(clip, params, job_type=JobType.SAM2_TRACK)
                next_steps = [JobType.VIDEOMAMA_ALPHA, JobType.INFERENCE]
            elif route == PipelineRoute.VIDEOMAMA_INFERENCE:
                job = create_job_snapshot(clip, job_type=JobType.VIDEOMAMA_ALPHA)
                next_steps = [JobType.INFERENCE]
            elif route == PipelineRoute.INFERENCE_ONLY:
                reset_complete_state(clip)
                job = create_inference_job(
                    clip, params, output_config, frame_range=clip_frame_range(clip),
                )

            if job is None:
                continue
            if self._service.job_queue.submit(job):
                clip.set_processing(True)
                if next_steps:
                    self._pipeline_steps[clip.name] = next_steps
                    self._pipeline_settings[clip.name] = {
                        "params": dataclasses.replace(params, use_alpha_hint=True),
                        "output_config": output_config,
                        "frame_range": clip_frame_range(clip),
                    }
                if first_job_id is None:
                    first_job_id = job.id
                queued += 1

        if queued > 0:
            self._start_worker_if_needed(first_job_id, job_label="Pipeline")
            gvm_n = sum(1 for r in routes.values() if r == PipelineRoute.GVM_PIPELINE)
            track_n = sum(1 for r in routes.values() if r == PipelineRoute.VIDEOMAMA_PIPELINE)
            vm_n = sum(1 for r in routes.values() if r == PipelineRoute.VIDEOMAMA_INFERENCE)
            inf_n = sum(1 for r in routes.values() if r == PipelineRoute.INFERENCE_ONLY)
            logger.info(
                f"Pipeline queued: {gvm_n} GVM + {track_n} Track Mask + {vm_n} VideoMaMa + "
                f"{inf_n} inference = {queued} initial jobs "
                f"(+{sum(len(v) for v in self._pipeline_steps.values())} auto-chain pending)"
            )


    def _start_worker_if_needed(
        self,
        first_job_id: str | None = None,
        job_label: str = "Inference",
    ) -> None:
        """Ensure GPU worker is running and show queue panel."""
        if first_job_id and self._active_job_id is None:
            self._active_job_id = first_job_id

        if not self._gpu_worker.isRunning():
            self._gpu_worker.start(QThread.HighPriority)
        else:
            self._gpu_worker.wake()

        self._force_stop_armed = False
        self._status_bar.set_running(True)
        self._status_bar.set_stop_button_mode(force=False)
        self._status_bar.reset_progress()
        self._status_bar.start_job_timer(label=job_label)
        self._queue_panel.refresh()
        refresh_hint_tools_if_available(self)  # job submitted: disable hint tools
        if self._queue_panel._collapsed:
            self._queue_panel.toggle_collapsed()  # auto-expand when job starts
