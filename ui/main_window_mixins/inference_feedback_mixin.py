"""Async colour/temporal feedback for the selected clip.

Owns the background ``InferenceContextController`` and the latest accepted scalar
context. The GUI thread never decodes, stats or probes the source: it only asks
the worker for a context on source selection/reload and applies scalar results.
Estimates and the colour notice re-evaluate inside the panel from that cached
context when profile/window/interpretation change (no second worker request).
Nothing here switches the input interpretation automatically or gates Run.
"""
from __future__ import annotations

import logging
import os

from PySide6.QtCore import Slot

from ui.workers.inference_context_worker import InferenceContextController

logger = logging.getLogger(__name__)


def _norm(path) -> str:
    return os.path.normcase(os.path.abspath(str(path or "")))


class InferenceFeedbackMixin:
    """Selected-clip probe lifecycle + one-click colour-interpretation switch."""

    # ── Lifecycle ──

    def _init_inference_feedback(self) -> None:
        self._inference_ctx = None
        self._inference_ctx_root: str | None = None  # root the pending/accepted probe is for
        self._inference_ctx_controller = InferenceContextController(self)
        self._inference_ctx_controller.ready.connect(self._on_inference_context_ready)
        self._param_panel.input_colour_switch_requested.connect(
            self._on_input_colour_switch_requested)

    def _clear_inference_feedback(self) -> None:
        """Drop pending work and any old estimates/notice (never stale numbers)."""
        controller = getattr(self, "_inference_ctx_controller", None)
        if controller is None:
            return
        controller.invalidate()
        self._inference_ctx = None
        self._inference_ctx_root = None
        panel = getattr(self, "_param_panel", None)
        if panel is None:
            return
        panel.set_temporal_context(width=None, height=None, frame_count=0, budget_gib=None)
        panel.set_input_colour_notice(None, panel.get_params().input_is_linear)

    def _close_inference_feedback(self) -> None:
        """Stop accepting completions; pending worker signals are ignored safely."""
        controller = getattr(self, "_inference_ctx_controller", None)
        if controller is not None:
            controller.close()
        self._inference_ctx = None
        self._inference_ctx_root = None

    def _request_inference_feedback(self, clip) -> None:
        """Clear old feedback now, then probe the selected clip off-thread."""
        if getattr(self, "_inference_ctx_controller", None) is None:
            return
        self._clear_inference_feedback()
        if clip is None or getattr(clip, "input_asset", None) is None:
            return
        self._inference_ctx_root = _norm(clip.root_path)
        self._inference_ctx_controller.request(clip)

    # ── Completion ──

    @Slot(object)
    def _on_inference_context_ready(self, ctx) -> None:
        clip = getattr(self, "_current_clip", None)
        if clip is None or self._inference_ctx_root is None:
            return
        root = _norm(clip.root_path)
        if root != self._inference_ctx_root or _norm(ctx.root_path) != root:
            return  # superseded by another clip / project
        panel = self._param_panel
        linear = panel.get_params().input_is_linear  # current, not probe-start
        if ctx.error or not ctx.width or not ctx.height or ctx.frame_count <= 0:
            self._inference_ctx = None
            panel.set_temporal_context(width=None, height=None, frame_count=0, budget_gib=None)
            panel.set_input_colour_notice(None, linear)
            return
        self._inference_ctx = ctx
        panel.set_temporal_context(width=ctx.width, height=ctx.height,
                                   frame_count=ctx.frame_count, budget_gib=ctx.budget_gib)
        panel.set_input_colour_notice(ctx.colour_sample, linear)

    # ── One-click colour switch ──

    @Slot(bool)
    def _on_input_colour_switch_requested(self, linear: bool) -> None:
        """Route the notice action through the normal params-changed path once."""
        clip = getattr(self, "_current_clip", None)
        ctx = getattr(self, "_inference_ctx", None)
        if clip is None or ctx is None or _norm(ctx.root_path) != _norm(clip.root_path):
            return
        panel = self._param_panel
        if panel.get_params().input_is_linear == bool(linear):
            return  # user already chose this while the click was in flight
        panel.set_input_is_linear(bool(linear))  # silent; refreshes notice
        self._on_params_changed()  # memory, buttons, both viewers, thumbnail
