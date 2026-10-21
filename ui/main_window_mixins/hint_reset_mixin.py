"""Reset Hint: delete generated AlphaHint / SAM masks so another method can run."""
from __future__ import annotations

import logging

from PySide6.QtWidgets import QCheckBox, QMessageBox

from backend import ClipState
from backend.hint_assets import clear_hint_assets, has_hint_assets
from ui.main_window_mixins import _tr

logger = logging.getLogger(__name__)


class HintResetMixin:
    """Guarded Reset Hint workflow (button state, confirmation, deletion, refresh)."""

    # ── Guard ──

    def _hint_mutation_allowed(self) -> bool:
        """False while any GPU job runs/is pending/is cancelling or a chain is active."""
        worker = getattr(self, "_gpu_worker", None)
        if worker is not None and (worker.is_busy() or worker.current_job_id is not None):
            return False  # running, or cancel still completing
        queue = getattr(getattr(self, "_service", None), "job_queue", None)
        if queue is not None and (queue.pending_count > 0 or queue.running_count > 0):
            return False
        if getattr(self, "_pipeline_steps", None):
            return False
        model = getattr(self, "_clip_model", None)
        for clip in getattr(model, "clips", None) or []:
            if clip.is_processing:
                return False
        return True

    def _refresh_hint_button_state(self, *_args) -> None:
        clip = getattr(self, "_current_clip", None)
        allowed = self._hint_mutation_allowed()
        self._param_panel.set_reset_hint_enabled(
            clip is not None and allowed and has_hint_assets(clip)
        )
        self._param_panel.set_track_enabled(allowed)

    # ── Reset ──

    def _on_reset_hint(self) -> None:
        clip = self._current_clip
        if clip is None or not self._hint_mutation_allowed():
            self._refresh_hint_button_state()
            return

        box = QMessageBox(self)
        box.setIcon(QMessageBox.Question)
        box.setWindowTitle(_tr("Reset Hint"))
        box.setText(_tr("Delete AlphaHint and SAM masks for '%s'?") % clip.name)
        box.setInformativeText(_tr(
            "Keyed outputs, holdout strokes and settings are kept. "
            "Annotations are kept unless you tick the box below. "
            "If the clip already has complete outputs it stays Complete."
        ))
        cb = QCheckBox(_tr("Also clear annotations"))
        cb.setChecked(False)
        box.setCheckBox(cb)
        reset_btn = box.addButton(_tr("Reset"), QMessageBox.DestructiveRole)
        cancel_btn = box.addButton(_tr("Cancel"), QMessageBox.RejectRole)
        box.setDefaultButton(cancel_btn)
        box.exec()
        if box.clickedButton() is not reset_btn:
            return
        clear_annotations = cb.isChecked()

        # State may have changed while the modal was open
        if self._current_clip is not clip or not self._hint_mutation_allowed():
            QMessageBox.warning(
                self, _tr("Reset Hint"),
                _tr("The selection or processing state changed. Reset was cancelled."),
            )
            self._refresh_hint_button_state()
            return

        self._stop_playback_for_reset()

        error = None
        try:
            clear_hint_assets(clip)
        except OSError as e:
            error = e
            logger.warning("Reset hint failed for %s: %s", clip.name, e)

        if clear_annotations:
            self._clear_annotations_for_reset(clip)

        self._refresh_after_hint_change(clip)

        if error is not None:
            QMessageBox.warning(
                self, _tr("Reset Hint"),
                _tr("Could not delete all hint files: %s") % error,
            )

    def _stop_playback_for_reset(self) -> None:
        viewer = getattr(self, "_dual_viewer", None)
        stop = getattr(viewer, "stop_playback", None)
        if stop is None:
            stop = getattr(getattr(viewer, "_scrubber", None), "stop_playback", None)
        if callable(stop):
            stop()

    def _clear_annotations_for_reset(self, clip) -> None:
        """Clear SAM prompts in the shared model and each viewport's private model.

        Done before any reload/save so a stale in-memory model cannot rewrite
        annotations.json. Holdout strokes are deliberately untouched.
        """
        viewer = self._dual_viewer
        models = []
        for vp in (viewer.input_viewer, viewer.output_viewer):
            model = vp.annotation_model
            if model not in models:
                models.append(model)
        for model in models:
            model.clear()
        for model in models:
            model.save(clip.root_path)  # empty -> removes annotations.json
        for vp in (viewer.input_viewer, viewer.output_viewer):
            split = getattr(vp, "_split_view", None)
            if split is not None:
                split.update()

    def _refresh_after_hint_change(self, clip) -> None:
        """Rescan from disk and refresh model/viewer/buttons (keeps frame/zoom)."""
        try:
            clip.mask_asset = None
            clip.alpha_asset = None
            clip.find_assets()
        except Exception:
            logger.exception("find_assets failed after hint reset")
        self._clip_model.update_clip_state(clip.name, clip.state)
        if self._current_clip is not clip:
            return
        viewer = self._dual_viewer
        viewer.refresh_generated_assets()
        for name in ("_refresh_export_thumbnail",):
            fn = getattr(self, name, None)
            if callable(fn):
                fn(clip)
        self._update_annotation_info()
        panel = self._param_panel
        pre = clip.state in (ClipState.RAW, ClipState.MASKED)
        panel.set_applevision_enabled(pre)
        panel.set_gvm_enabled(pre)
        panel.set_birefnet_enabled(pre)
        panel.set_chroma_key_enabled(pre)
        has_mask = self._clip_has_videomama_ready_mask(clip)
        panel.set_videomama_enabled(has_mask)
        panel.set_matanyone2_enabled(has_mask)
        panel.set_import_alpha_enabled(
            clip.state in (ClipState.RAW, ClipState.MASKED, ClipState.READY)
        )
        self._refresh_button_state()
        self._refresh_hint_button_state()
