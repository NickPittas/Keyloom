from __future__ import annotations

import os
import logging

import cv2
import numpy as np
from PySide6.QtWidgets import QMessageBox
from PySide6.QtGui import QImage
from PySide6.QtCore import QTimer

from . import _tr

from backend import ClipEntry
from backend.hint_assets import clear_hint_assets

logger = logging.getLogger(__name__)


class AnnotationMixin:
    """Annotation painting, SAM2 tracking, and mask management for MainWindow."""

    def _is_chroma_key_active(self) -> bool:
        """True when the Chroma Key section is expanded (button checked)."""
        return self._param_panel._chroma_key_btn.isChecked()

    def _toggle_annotation_fg(self) -> None:
        """Hotkey 1: toggle foreground brush. Routes to holdout if chroma key on."""
        iv = self._dual_viewer.input_viewer
        ov = self._dual_viewer.output_viewer
        if iv.annotation_mode == "fg":
            iv.set_annotation_mode(None)
            ov.set_annotation_mode(None)
            self._dual_viewer.set_holdout_active(False)
        else:
            self._deactivate_eyedropper_if_active()
            iv.set_annotation_mode("fg")
            ov.set_annotation_mode("fg")
            self._dual_viewer.set_holdout_active(self._is_chroma_key_active())

    def _toggle_annotation_bg(self) -> None:
        """Hotkey 2: toggle background brush. Routes to holdout if chroma key on."""
        iv = self._dual_viewer.input_viewer
        ov = self._dual_viewer.output_viewer
        if iv.annotation_mode == "bg":
            iv.set_annotation_mode(None)
            ov.set_annotation_mode(None)
            self._dual_viewer.set_holdout_active(False)
        else:
            self._deactivate_eyedropper_if_active()
            iv.set_annotation_mode("bg")
            ov.set_annotation_mode("bg")
            self._dual_viewer.set_holdout_active(self._is_chroma_key_active())

    def _deactivate_eyedropper_if_active(self) -> None:
        """Turn off the eyedropper if it's currently on."""
        ed_btn = self._param_panel._eyedropper_btn
        if ed_btn.isChecked():
            ed_btn.setChecked(False)

    def _cycle_fg_color(self) -> None:
        """Hotkey C: cycle foreground annotation color (green/blue)."""
        from ui.widgets.annotation_overlay import cycle_fg_color
        name = cycle_fg_color()
        self._dual_viewer.input_viewer.update()
        self._show_toast(_tr("Foreground color: %s") % name)

    def _auto_save_annotations(self) -> None:
        """Auto-save annotation strokes to disk after changes."""
        if self._current_clip is not None:
            iv = self._dual_viewer.input_viewer
            iv.annotation_model.save(self._current_clip.root_path)
            manifest_path = os.path.join(
                self._current_clip.root_path,
                ".corridorkey_mask_manifest.json",
            )
            if os.path.isfile(manifest_path):
                os.remove(manifest_path)
            _has_mask = self._clip_has_videomama_ready_mask(self._current_clip)
            self._param_panel.set_videomama_enabled(_has_mask)
            self._param_panel.set_matanyone2_enabled(_has_mask)

    def _auto_save_holdout(self) -> None:
        """Auto-save holdout strokes to disk after changes."""
        if self._current_clip is not None:
            iv = self._dual_viewer.input_viewer
            iv.holdout_model.save(self._current_clip.root_path)

    def _clip_has_videomama_ready_mask(self, clip: ClipEntry | None) -> bool:
        """True when the clip has a dense mask track that alpha generators can consume."""
        if clip is None or clip.mask_asset is None:
            return False
        # mask_asset is set only when VideoMamaMaskHint/ has actual frames —
        # that's sufficient to enable alpha generators regardless of manifest.
        return True

    def _undo_annotation(self) -> None:
        """Ctrl+Z: undo last stroke. Routes by chroma key mode."""
        iv = self._dual_viewer.input_viewer
        if not iv.annotation_mode:
            return
        if self._is_chroma_key_active():
            if iv.holdout_model.undo(0):
                iv._split_view.update()
                self._auto_save_holdout()
                self._schedule_chroma_key_preview()
        elif iv.current_stem_index >= 0:
            if iv.annotation_model.undo(iv.current_stem_index):
                iv._split_view.update()
                self._auto_save_annotations()

    def _on_track_masks(self) -> None:
        """Preview SAM2 on the annotated frame, then confirm full tracking."""
        from ui.workers.gpu_job_worker import create_job_snapshot
        from backend import JobType

        clip = self._current_clip
        if clip is None:
            return
        # Any new request supersedes a deferred full-track continuation
        self._pending_sam2_track = None
        if not self._hint_mutation_allowed():
            self._refresh_hint_tools()
            return
        self._auto_save_annotations()
        iv = self._dual_viewer.input_viewer
        model = iv.annotation_model
        if not model.has_annotations():
            QMessageBox.information(
                self, _tr("No Paint Strokes"),
                _tr("Paint green (1) and red (2) strokes on frames first."),
            )
            return

        if not self._warn_mps_slow("SAM2 Track Mask"):
            return

        job = create_job_snapshot(clip, self._param_panel.get_params(), job_type=JobType.SAM2_PREVIEW)
        job.params["_frame_index"] = max(0, self._dual_viewer.current_stem_index)
        if not self._service.job_queue.submit(job):
            return
        self._sam2_preview_job_id = job.id

        self._start_worker_if_needed(job.id, job_label="Track Preview")

    def _submit_sam2_track_job(self, clip: ClipEntry) -> bool:
        """Queue the full SAM2 tracking job after preview confirmation."""
        from ui.workers.gpu_job_worker import create_job_snapshot
        from backend import JobType

        if self._current_clip is not clip:
            return False
        if not self._hint_mutation_allowed():
            # The accepted preview job may not be terminal yet (result signal
            # arrives before the worker finishes). Defer, never drop or overlap.
            self._defer_sam2_track(clip)
            return False
        self._pending_sam2_track = None

        alpha_dir = os.path.join(clip.root_path, "AlphaHint")
        if clip.alpha_asset is not None or os.path.isdir(alpha_dir):
            reply = QMessageBox.question(
                self, _tr("Replace Existing Alpha?"),
                _tr("This clip already has an AlphaHint (from GVM, chroma key or a previous run).\n\n"
                    "Tracking a new mask sequence will replace that alpha hint.\n\n"
                    "Remove existing AlphaHint and proceed?"),
            )
            if reply != QMessageBox.Yes:
                return False
            if self._current_clip is not clip or not self._hint_mutation_allowed():
                self._refresh_hint_tools()
                return False
            try:
                clear_hint_assets(clip, clear_alpha=True, clear_masks=False)
            except OSError as e:
                logger.warning("Could not remove AlphaHint for %s: %s", clip.name, e)
                QMessageBox.warning(
                    self, _tr("Replace Existing Alpha?"),
                    _tr("Could not delete the existing AlphaHint: %s") % e,
                )
                self._refresh_hint_tools()
                return False
            self._rescan_after_alpha_clear(clip)
            self._refresh_button_state()
            self._refresh_hint_tools()
            logger.info("Removed existing AlphaHint/ before SAM2 tracking")

        job = create_job_snapshot(clip, self._param_panel.get_params(), job_type=JobType.SAM2_TRACK)
        if not self._service.job_queue.submit(job):
            return False

        clip.set_processing(True)
        self._start_worker_if_needed(job.id, job_label="Track Mask")
        return True

    _SAM2_DEFER_MS = 200
    _SAM2_DEFER_MAX = 150  # ~30 s

    def _defer_sam2_track(self, clip: ClipEntry) -> None:
        """Remember an accepted full track and retry once the queue is idle."""
        self._pending_sam2_track = (clip, 0)
        QTimer.singleShot(self._SAM2_DEFER_MS, self._retry_pending_sam2_track)

    def _retry_pending_sam2_track(self) -> None:
        pending = getattr(self, "_pending_sam2_track", None)
        if not pending:
            return
        clip, tries = pending
        if self._current_clip is not clip:
            self._pending_sam2_track = None  # stale: clip changed
            return
        if not self._hint_mutation_allowed():
            if tries + 1 >= self._SAM2_DEFER_MAX:
                self._pending_sam2_track = None
                self._refresh_hint_tools()
                QMessageBox.warning(
                    self, _tr("Track Mask"),
                    _tr("Another job is still running. Run Track Mask again when it finishes."),
                )
                return
            self._pending_sam2_track = (clip, tries + 1)
            QTimer.singleShot(self._SAM2_DEFER_MS, self._retry_pending_sam2_track)
            return
        self._pending_sam2_track = None
        self._submit_sam2_track_job(clip)

    @staticmethod
    def _sam2_preview_qimage(frame_rgb: np.ndarray, mask: np.ndarray) -> QImage:
        """Render a contour-only SAM2 preview for the output viewer."""
        overlay = cv2.cvtColor(frame_rgb, cv2.COLOR_RGB2BGR)
        contours, _ = cv2.findContours(
            (mask > 0).astype(np.uint8),
            cv2.RETR_EXTERNAL,
            cv2.CHAIN_APPROX_SIMPLE,
        )
        cv2.drawContours(overlay, contours, -1, (0, 230, 255), 4)
        rgb = cv2.cvtColor(overlay, cv2.COLOR_BGR2RGB)
        h, w = rgb.shape[:2]
        return QImage(rgb.data, w, h, w * 3, QImage.Format_RGB888).copy()

    def _confirm_clear_annotations(self) -> None:
        """Ctrl+C: clear strokes. Routes to holdout or SAM2 by chroma key mode."""
        if self._is_chroma_key_active():
            self._confirm_clear_holdout()
        else:
            self._confirm_clear_sam2_annotations()

    def _confirm_clear_holdout(self) -> None:
        """Clear all holdout mask strokes for this clip."""
        iv = self._dual_viewer.input_viewer
        model = iv.holdout_model
        if not model.has_annotations():
            return
        reply = QMessageBox.question(
            self, _tr("Clear Holdout Strokes"),
            _tr("Clear all holdout mask strokes for this clip?"),
            QMessageBox.Yes | QMessageBox.No,
        )
        if reply == QMessageBox.Yes:
            model.clear()
            self._auto_save_holdout()
            iv._split_view.update()
            self._schedule_chroma_key_preview()

    def _confirm_clear_sam2_annotations(self) -> None:
        """Ctrl+C: choose to clear SAM2 annotations on this frame, entire clip, or cancel."""
        iv = self._dual_viewer.input_viewer
        model = iv.annotation_model
        if not model or not model.has_annotations():
            return

        stem_idx = iv._split_view._annotation_stem_idx
        has_frame = model.has_annotations(stem_idx)

        box = QMessageBox(self)
        box.setWindowTitle(_tr("Clear Paint Strokes"))
        box.setText(_tr("What would you like to clear?"))
        frame_btn = box.addButton(_tr("This Frame"), QMessageBox.AcceptRole)
        clip_btn = box.addButton(_tr("Entire Clip"), QMessageBox.DestructiveRole)
        box.addButton(QMessageBox.Cancel)

        if not has_frame:
            frame_btn.setEnabled(False)

        box.exec()
        clicked = box.clickedButton()

        if clicked == frame_btn:
            model.clear(stem_idx)
            iv._split_view.update()
            self._update_annotation_info()
        elif clicked == clip_btn:
            self._on_clear_annotations()

    def _on_clear_annotations(self) -> None:
        """Clear all annotations on the current clip and remove tracked masks."""
        clip = self._current_clip
        if not self._hint_mutation_allowed():
            self._refresh_hint_tools()
            return
        error = None
        if clip is not None:
            # Masks + manifest only: AlphaHint and holdout strokes are kept
            try:
                clear_hint_assets(clip, clear_alpha=False, clear_masks=True)
            except OSError as e:
                error = e
                logger.warning("Could not remove mask hints for %s: %s", clip.name, e)
            self._clear_annotations_for_reset(clip)
        else:
            self._dual_viewer.input_viewer.clear_annotations()
        self._update_annotation_info()
        if clip is not None:
            clip.mask_asset = None
            try:
                clip.find_assets()
            except Exception:
                logger.exception("find_assets failed after clearing annotations")
            self._clip_model.update_clip_state(clip.name, clip.state)
            self._refresh_hint_tools()
        if error is not None:
            QMessageBox.warning(
                self, _tr("Clear Annotations"),
                _tr("Could not delete all mask files: %s") % error,
            )

    def _update_annotation_info(self) -> None:
        """Update parameter panel with current annotation count and scrubber markers."""
        iv = self._dual_viewer.input_viewer
        model = iv.annotation_model
        fi = iv._frame_index
        total = fi.frame_count if fi else 0
        self._param_panel.set_annotation_info(
            model.annotated_frame_count(), total
        )
        # Push annotation coverage to the scrubber timeline
        if fi and total > 0:
            annotated = [model.has_annotations(i) for i in range(total)]
            self._dual_viewer._scrubber.set_annotation_markers(annotated)
        else:
            self._dual_viewer._scrubber.set_annotation_markers([])
