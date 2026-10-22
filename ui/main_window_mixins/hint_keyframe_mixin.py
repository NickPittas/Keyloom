"""Sparse hint keyframes: mark/unmark the presented source frame, persist, refresh UI.

No pixel decoding or video probing happens here (GUI thread): matte presence is an
exact filename lookup, video hints rely on an already-cached frame count.
"""
from __future__ import annotations

import logging
import os

from PySide6.QtWidgets import QMessageBox

from backend.hint_keyframes import HintKeyframesError, save_hint_keyframes
from backend.service.v2_inputs import V2InputError, _resolve_selected
from ui.main_window_mixins import _tr

logger = logging.getLogger(__name__)


class HintKeyframeMixin:
    """Mark-keyframe controller. Needs ``_hint_mutation_allowed`` (HintResetMixin)."""

    # ── Helpers ──

    def _hint_keyframe_viewer(self):
        return getattr(self, "_dual_viewer", None)

    def _current_presented_hint_index(self) -> int | None:
        """Source index of the frame the user is looking at, None if unknown.

        INPUT-only view (output viewer in INPUT mode) presents through the input
        viewer; every other view presents through the output viewer. The viewers'
        presented index is the painted frame, never the playback clock target
        (playback is stopped before a mutation lands on the exact frame).
        """
        viewer = self._hint_keyframe_viewer()
        idx = getattr(viewer, "current_presented_source_index", None)
        if isinstance(idx, bool) or not isinstance(idx, int) or idx < 0:
            return None
        return idx

    @staticmethod
    def _hint_keyframe_source_stems(clip) -> tuple[list[str] | None, str | None]:
        """Cached ordered source stems (listing only), or (None, reason)."""
        asset = getattr(clip, "input_asset", None)
        if asset is None:
            return None, _tr("The clip has no source frames.")
        if asset.asset_type == "video":
            count = int(getattr(asset, "frame_count", 0) or 0)
            if count <= 0:
                return None, _tr("The source video frame count is not known yet.")
            return [f"{i:05d}" for i in range(count)], None
        try:
            files = asset.get_frame_files()
        except OSError as exc:
            return None, _tr("Could not list the source frames: %s") % exc
        if not files:
            return None, _tr("The clip has no source frames.")
        return [os.path.splitext(f)[0] for f in files], None

    def _hint_keyframe_matte_problem(self, clip, stems, index: int) -> str | None:
        """None if the exact selected AlphaHint exists for ``index``; else the reason."""
        stem = stems[index]
        asset = getattr(clip, "alpha_asset", None)
        if asset is None:
            return _tr("No AlphaHint for frame %d ('%s'): generate or import a matte first.") % (
                index, stem)
        if asset.asset_type == "video":
            count = int(getattr(asset, "frame_count", 0) or 0)
            if count <= 0:
                return _tr(
                    "The AlphaHint video's frame count is not known yet, so frame %d "
                    "('%s') cannot be checked here.") % (index, stem)
            if index >= count:
                return _tr("The AlphaHint video has %d frames; frame %d ('%s') is missing.") % (
                    count, index, stem)
            return None
        try:
            _resolve_selected(asset.get_frame_files(), stems, [index])
        except V2InputError as exc:
            return str(exc)
        except OSError as exc:
            return _tr("Could not list the AlphaHint frames: %s") % exc
        return None

    def _stop_playback_for_keyframe(self) -> None:
        viewer = self._hint_keyframe_viewer()
        stop = getattr(viewer, "stop_playback", None)
        if stop is None:
            stop = getattr(getattr(viewer, "_scrubber", None), "stop_playback", None)
        if callable(stop):
            stop()

    def _set_keyframe_ticks(self, indices) -> None:
        viewer = self._hint_keyframe_viewer()
        setter = getattr(viewer, "set_keyframes", None)
        if setter is None:
            setter = getattr(getattr(viewer, "_scrubber", None), "set_keyframes", None)
        if callable(setter):
            setter(list(indices))

    # ── Refresh ──

    def _refresh_hint_keyframe_tools(self, *_args) -> None:
        """Push count / marked / availability / ticks for the selected clip."""
        panel = getattr(self, "_param_panel", None)
        if not hasattr(panel, "set_hint_keyframe_context"):
            return  # partially built window / minimal test double
        clip = getattr(self, "_current_clip", None)
        if clip is None:
            panel.set_hint_keyframe_context(
                has_clip=False, frame_available=False, is_marked=False, count=0, error=None)
            panel.set_hint_keyframe_mutation_enabled(False)
            self._set_keyframe_ticks(())
            return
        clip_error = getattr(clip, "hint_keyframes_error", None)
        marks = () if clip_error else tuple(clip.hint_keyframes)
        idx = self._current_presented_hint_index()
        is_marked = idx is not None and idx in marks
        available = False
        error = clip_error
        if not clip_error and not is_marked:
            if idx is None:
                error = _tr("No frame is shown.")
            else:
                stems, why = self._hint_keyframe_source_stems(clip)
                if stems is None:
                    error = why
                elif idx >= len(stems):
                    error = _tr("The current frame is outside the source frames.")
                else:
                    error = self._hint_keyframe_matte_problem(clip, stems, idx)
                    available = error is None
        elif is_marked:
            available = True
        panel.set_hint_keyframe_context(
            has_clip=True, frame_available=available and not clip_error,
            is_marked=is_marked, count=len(marks), error=error)
        panel.set_hint_keyframe_mutation_enabled(
            not clip_error and self._hint_mutation_allowed())
        self._set_keyframe_ticks(marks)

    # ── Toggle ──

    def _on_toggle_hint_keyframe(self) -> None:
        clip = getattr(self, "_current_clip", None)
        if clip is None or not self._hint_mutation_allowed():
            self._refresh_hint_keyframe_tools()
            return
        if getattr(clip, "hint_keyframes_error", None):
            self._refresh_hint_keyframe_tools()
            return

        self._stop_playback_for_keyframe()
        # Re-read after stopping: the scrubber lands on the exact shown frame.
        idx = self._current_presented_hint_index()
        root = clip.root_path
        stems, why = self._hint_keyframe_source_stems(clip)
        if idx is None or stems is None or idx >= len(stems):
            self._warn(why or _tr("No frame is shown."))
            self._refresh_hint_keyframe_tools()
            return

        marks = set(clip.hint_keyframes)
        if idx in marks:
            marks.discard(idx)
        else:
            problem = self._hint_keyframe_matte_problem(clip, stems, idx)
            if problem is not None:
                self._warn(problem)
                self._refresh_hint_keyframe_tools()
                return
            marks.add(idx)

        # Immediately before saving: selection, root, frame and busy state unchanged?
        idle = self._hint_mutation_allowed()
        if (not idle
                or getattr(self, "_current_clip", None) is not clip
                or clip.root_path != root
                or self._current_presented_hint_index() != idx):
            self._warn(_tr("The selection or processing state changed. Keyframe was not saved."))
            self._refresh_hint_keyframe_tools()
            return

        try:
            save_hint_keyframes(root, sorted(marks), stems)
        except (OSError, HintKeyframesError) as exc:
            logger.warning("Saving hint keyframes failed for %s: %s", clip.name, exc)
            self._warn(_tr("Could not save keyframes: %s") % exc)
            self._refresh_hint_keyframe_tools()
            return
        clip.hint_keyframes = tuple(sorted(marks))
        self._refresh_hint_keyframe_tools()

    def _warn(self, text: str) -> None:
        QMessageBox.warning(self, _tr("Hint keyframe"), text)
