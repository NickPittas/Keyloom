"""Center panel — frame preview viewport.

Phase 3: Composites SplitViewWidget, ViewModeBar, FrameScrubber, and
AsyncDecoder for a professional preview experience.

Architecture (Codex findings incorporated):
- QImage as internal currency, not QPixmap (guaranteed CPU-only)
- Stem-based navigation via FrameIndex (no index misalignment)
- Async frame decoding with request coalescing
- Worker preview gated: only applied if viewing same clip + COMP mode + latest frame
- Frame list snapshot built once per clip selection (atomic)
"""
from __future__ import annotations

import os
import logging
from dataclasses import dataclass

from PySide6.QtWidgets import QWidget, QVBoxLayout, QHBoxLayout, QLabel, QPushButton
from PySide6.QtCore import Qt, QTimer, Signal, Slot
from PySide6.QtGui import QImage

from backend import ClipEntry
from ui.preview.frame_index import FrameIndex, ViewMode, build_frame_index
from ui.preview.display_transform import clear_cache, image_source_size
from ui.preview.async_decoder import AsyncDecoder, DecodeRequest
from ui.preview.frame_cache import FrameCache, PREVIEW_CACHE_BYTES
from ui.preview.playback import choose_ready_frame
from ui.widgets.split_view import SplitViewWidget
from ui.widgets.frame_scrubber import FrameScrubber
from ui.widgets.view_mode_bar import ViewModeBar
from ui.widgets.annotation_overlay import AnnotationModel

logger = logging.getLogger(__name__)

PREFETCH_BEHIND = 8
PREFETCH_AHEAD = 32
LANE_PRIMARY = "primary"
LANE_SPLIT = "split_input"
_RESIZE_SETTLE_MS = 150
_RASTER_TOLERANCE_PX = 2.0


@dataclass(frozen=True)
class _Expect:
    """The one frame a lane is currently waiting to present."""
    stem: int
    mode_value: str          # DECODED mode (INPUT when falling back)
    target_size: tuple[int, int] | None


class PlaybackPresenter:
    """Presents the newest frame ready in *every* viewer, never waiting.

    The scrubber clock only says which frame *should* be showing (the target).
    The presenter prepares the target in each viewer and shows the newest
    frame at/before the target (and not behind the last one shown) that every
    viewer already has cached, so slow decodes drop frames instead of
    stalling or building a slow-motion backlog.  Pure GUI-thread logic.
    """

    SCAN_LIMIT = 96  # max frames below the target probed per attempt

    def __init__(self, viewers, scrubber, on_presented) -> None:
        self._viewers = list(viewers)
        self._scrubber = scrubber
        self._on_presented = on_presented
        self._active = False
        self._target: int | None = None
        self._cycle: int | None = None
        self._last: int | None = None
        self._busy = False

    @property
    def active(self) -> bool:
        return self._active

    @property
    def last_presented(self) -> int | None:
        return self._last

    def start(self) -> None:
        self._active = True
        self._target = None
        self._cycle = None
        self._last = self._scrubber.current_frame()

    def stop(self) -> bool:
        """End the playback context. Returns whether one was active."""
        was = self._active
        self._active = False
        self._target = None
        self._cycle = None
        self._last = None
        for v in self._viewers:
            v.clear_playback_hints()
        return was

    def seek(self, index: int) -> None:
        """User navigation during playback: forget the old clock target."""
        self._last = index
        self._target = None

    def on_target(self, index: int, cycle: int) -> None:
        if not self._active:
            return
        if self._cycle is not None and cycle != self._cycle:
            self._last = None  # loop wrap: frames behind are fair game again
        self._cycle = cycle
        self._target = index
        lo, hi = self._scrubber.play_range()
        loop = (lo, hi) if self._scrubber.loop_enabled() else None
        for v in self._viewers:
            v.set_playback_hints(loop)
            v.prepare_frame(index)
        self._present()

    def on_ready(self, index: int) -> None:
        """A viewer finished a frame; show it now if it is the best candidate."""
        if not self._active or self._target is None or index > self._target:
            return
        if self._last is not None and index <= self._last:
            return
        self._present()

    def _show_on_scrubber(self, index: int) -> None:
        """Move the slider to the shown frame without re-anchoring the clock.

        ``FrameScrubber.set_frame`` re-anchors playback at the new value, which
        would shift the clock back to the (older) presented frame and cause
        slow motion; the clock must keep following wall time.
        """
        self._scrubber.show_presented_frame(index)

    def _present(self) -> None:
        if self._busy or self._target is None:
            return
        self._busy = True
        try:
            target = self._target
            lo, hi = self._scrubber.play_range()
            floor = lo if self._last is None else max(lo, self._last)
            stop_at = max(floor, target - self.SCAN_LIMIT)
            best = None
            for i in range(min(target, hi), stop_at - 1, -1):
                if all(v.has_cached_frame(i) for v in self._viewers):
                    best = i
                    break
            choose = choose_ready_frame(
                target, lo, hi, self._last, [] if best is None else [best])
            if choose is None or choose == self._last:
                return  # keep the last shown pair; never wait
            shown = [v.present_cached_frame(choose) for v in self._viewers]
            if not all(shown):
                return
            self._last = choose
            self._show_on_scrubber(choose)
            self._on_presented(choose)
        finally:
            self._busy = False


class PreviewViewport(QWidget):
    """Center panel frame preview with split view, scrubber, and mode switching.

    When used inside DualViewerPanel, pass show_scrubber=False to hide the
    internal scrubber (the dual viewer provides a shared one).
    """

    frame_changed = Signal(int)  # current stem index
    view_mode_changed = Signal(str)
    frame_ready_for_index = Signal(int)  # a primary-lane frame for this index is cached
    frame_presented = Signal(int)        # this index was painted on the primary lane

    # Class-level defaults keep stub-built instances (tests) working.
    _presenter: "PlaybackPresenter | None" = None
    _playback_hint: bool = False
    _loop_range: "tuple[int, int] | None" = None

    def __init__(self, show_scrubber: bool = True, parent=None):
        super().__init__(parent)
        self.setMinimumSize(200, 150)

        # State
        self._clip: ClipEntry | None = None
        self._clip_name: str = ""
        self._frame_index: FrameIndex | None = None
        self._current_stem_idx: int = -1
        self._current_mode: ViewMode = ViewMode.COMP
        self._locked_mode: ViewMode | None = None
        self._input_exr_is_linear: bool = False
        self._requested_mode: ViewMode = ViewMode.COMP  # mode asked for (may differ from decoded)
        self._expect: dict[str, _Expect] = {}
        self._source_dims: tuple[int, int] | None = None   # from decoded images
        self._shown_raster: tuple[int, int] | None = None  # primary raster actually painted
        self._shown_stem: int = -1
        self._direction: int = 1
        self._playback_hint = False
        self._loop_range = None
        self._presenter = None

        # One bounded cache per viewport; async decoder fills it off-thread.
        # ``frame_decoded`` stays as a *re-emitted* primary-lane signal (the
        # dual viewer's wipe listens to it); this widget never connects it.
        self._frame_cache = FrameCache(PREVIEW_CACHE_BYTES)
        self._decoder = AsyncDecoder(self, cache=self._frame_cache)
        self._decoder.frame_ready.connect(self._on_frame_ready)

        self._resize_timer = QTimer(self)
        self._resize_timer.setSingleShot(True)
        self._resize_timer.setInterval(_RESIZE_SETTLE_MS)
        self._resize_timer.timeout.connect(self._on_canvas_settled)

        # Build layout
        layout = QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(0)

        # Top bar: view modes (left) + clip info (right)
        self._top_bar = QWidget()
        self._top_bar.setFixedHeight(30)
        self._top_bar.setStyleSheet("background: #0E0D00;")
        top_bar = QHBoxLayout(self._top_bar)
        top_bar.setContentsMargins(0, 0, 0, 0)
        top_bar.setSpacing(0)

        self._mode_bar = ViewModeBar()
        self._mode_bar.mode_changed.connect(self._on_mode_changed)
        top_bar.addWidget(self._mode_bar)

        self._clip_info = QLabel("")
        self._clip_info.setAlignment(Qt.AlignRight | Qt.AlignVCenter)
        self._clip_info.setStyleSheet(
            "color: #808070; font-size: 11px; padding-right: 8px; background: #0E0D00;"
        )
        top_bar.addWidget(self._clip_info)

        # A/B wipe button — hidden by default, shown via add_ab_button()
        self._ab_button: QPushButton | None = None

        layout.addWidget(self._top_bar)

        # Annotation model (shared across frames, persists during scrubbing)
        self._annotation_model = AnnotationModel()
        # Holdout model (chroma key forced regions, frame-independent)
        self._holdout_model = AnnotationModel(filename="holdout_strokes.json")

        # Split view widget (center, fills space)
        self._split_view = SplitViewWidget()
        self._split_view.zoom_changed.connect(self._on_zoom_changed)
        self._split_view.canvas_resized.connect(self._on_canvas_resized)
        self._split_view.set_annotation_model(self._annotation_model)
        self._split_view.set_holdout_model(self._holdout_model)
        layout.addWidget(self._split_view, 1)

        # Bottom bar: scrubber + zoom indicator (optional)
        self._has_scrubber = show_scrubber
        if show_scrubber:
            bottom = QHBoxLayout()
            bottom.setContentsMargins(0, 0, 0, 0)
            bottom.setSpacing(4)

            self._scrubber = FrameScrubber()
            self._scrubber.frame_changed.connect(self._on_scrubber_frame)
            self._scrubber._legacy_playback_emit = False
            self._presenter = PlaybackPresenter(
                [self], self._scrubber, self.frame_changed.emit)
            self._scrubber.playback_requested.connect(self._presenter.on_target)
            self._scrubber.playback_state_changed.connect(self._on_playback_state)
            self.frame_ready_for_index.connect(self._presenter.on_ready)
            bottom.addWidget(self._scrubber, 1)

            self._zoom_label = QLabel("100%")
            self._zoom_label.setFixedWidth(50)
            self._zoom_label.setAlignment(Qt.AlignCenter)
            self._zoom_label.setStyleSheet("color: #808070; font-size: 10px;")
            bottom.addWidget(self._zoom_label)

            layout.addLayout(bottom)
        else:
            self._scrubber = None
            self._zoom_label = None

    @property
    def current_stem_index(self) -> int:
        """Current stem index for reprocess targeting."""
        return self._current_stem_idx

    # ── Public API ──

    def lock_mode(self, mode: ViewMode) -> None:
        """Lock this viewport to a specific view mode and hide the mode buttons.

        Used by DualViewerPanel to lock the input viewer to INPUT mode.
        The top bar stays visible (clip info label) so both viewers align.
        """
        self._locked_mode = mode
        self._current_mode = mode
        self._mode_bar.hide()

    def set_input_exr_is_linear(self, enabled: bool) -> None:
        """Set how INPUT-mode source frames should be interpreted for display."""
        if self._input_exr_is_linear == enabled:
            return
        self._input_exr_is_linear = enabled

        if self._current_stem_idx < 0:
            return

        if self._current_mode == ViewMode.INPUT:
            self._request_frame(self._current_stem_idx, self._current_mode)
        if self._split_view.split_enabled:
            self._load_split_images()

    def invalidate_cache(self) -> None:
        """Drop every cached/in-flight frame (hint reset, replaced content).

        The displayed image is left alone; callers re-navigate to refresh.
        """
        self._decoder.invalidate()
        self._expect.clear()

    def set_playback_hints(self, loop_range: tuple[int, int] | None) -> None:
        """Playing: cache eviction follows the playhead; ``loop_range`` wraps."""
        self._playback_hint = True
        self._loop_range = loop_range

    def clear_playback_hints(self) -> None:
        self._playback_hint = False
        self._loop_range = None
        self._frame_cache.set_playhead(None)

    def shutdown(self) -> None:
        """Stop background work for this viewport (idempotent)."""
        self._resize_timer.stop()
        self._decoder.close()

    def closeEvent(self, event) -> None:
        self.shutdown()
        super().closeEvent(event)

    # ── Eyedropper API ──

    def set_eyedropper_mode(self, enabled: bool) -> None:
        """Toggle eyedropper color sampling on this viewport."""
        self._split_view.set_eyedropper_mode(enabled)

    def set_eyedropper_source(self, image: QImage | None) -> None:
        """Set the input frame image for eyedropper sampling."""
        self._split_view.set_eyedropper_source(image)

    # ── Annotation API ──

    def set_annotation_mode(self, mode: str | None) -> None:
        """Set annotation mode: 'fg', 'bg', or None to disable."""
        self._split_view.set_annotation_mode(mode)

    @property
    def annotation_mode(self) -> str | None:
        return self._split_view.annotation_mode

    @property
    def annotation_model(self) -> AnnotationModel:
        return self._annotation_model

    @property
    def holdout_model(self) -> AnnotationModel:
        return self._holdout_model

    def set_holdout_active(self, active: bool) -> None:
        self._split_view.set_holdout_active(active)

    def clear_holdout(self) -> None:
        """Clear all holdout strokes for the current clip."""
        self._holdout_model.clear()
        if self._clip is not None:
            self._holdout_model.save(self._clip.root_path)
        self._split_view.update()

    def clear_annotations(self) -> None:
        """Clear all annotations for the current clip."""
        self._annotation_model.clear()
        if self._clip is not None:
            self._annotation_model.save(self._clip.root_path)
        self._split_view.update()

    def set_clip(self, clip: ClipEntry) -> None:
        """Load a clip and build its frame index.

        Called when user selects a clip in the browser.
        Builds frame index atomically (Codex: snapshot, don't re-read per scrub).
        EXTRACTING clips show a placeholder — no frames to scrub yet.
        """
        from backend import ClipState

        # Save annotations and holdout for previous clip before switching
        if self._clip is not None:
            self._annotation_model.save(self._clip.root_path)
            self._holdout_model.save(self._clip.root_path)
        self._clip = clip
        self._clip_name = clip.name
        # Preserve the caller-managed input interpretation when reloading a clip.
        # MainWindow reapplies the remembered per-clip override after set_clip();
        # resetting here causes tray clicks and clear actions to snap back to the
        # auto-detected default before the user asked for it.
        clear_cache()
        self._reset_frame_state()
        self._annotation_model.load(clip.root_path)
        self._holdout_model.load(clip.root_path)

        # EXTRACTING clips: show placeholder, no frame index
        if clip.state == ClipState.EXTRACTING:
            self._frame_index = None
            self._current_stem_idx = -1
            if self._scrubber:
                self._scrubber.set_range(0)
            self._update_clip_info(clip)
            # Clear any cached images from previous clip
            self._split_view._single_image = None
            self._split_view._left_image = None
            self._split_view._right_image = None
            self._split_view.set_placeholder(
                self.tr("Extracting frames...\n%s") % clip.name
            )
            return

        # Build frame index
        self._frame_index = self._build_frame_index(clip)

        # Configure mode bar (unless locked)
        if self._locked_mode is None:
            available = self._frame_index.available_modes()
            self._mode_bar.set_available_modes(available)
            self._current_mode = self._mode_bar.current_mode()
        else:
            self._current_mode = self._locked_mode

        # Configure scrubber (if we have one)
        if self._scrubber:
            self._scrubber.set_range(self._frame_index.frame_count)

        # Update clip info label
        self._update_clip_info(clip)

        if self._frame_index.frame_count > 0:
            self._navigate_to(0)
        else:
            from ui.state_labels import state_display_name
            self._split_view.set_placeholder(
                self.tr("Selected: %s\nState: %s") % (clip.name, state_display_name(clip.state))
            )

    def refresh_available_assets(self) -> None:
        """Refresh mode availability and frame coverage without resetting navigation.

        Used by live progress updates while a job is writing frames. This keeps the
        current stem stable instead of calling set_clip(), which would jump back to frame 0.
        """
        if self._clip is None:
            return

        old_stem: str | None = None
        if self._frame_index and 0 <= self._current_stem_idx < self._frame_index.frame_count:
            old_stem = self._frame_index.stems[self._current_stem_idx]

        self._frame_index = self._build_frame_index(self._clip)

        if self._locked_mode is None:
            self._mode_bar.set_available_modes(self._frame_index.available_modes())
            self._current_mode = self._mode_bar.current_mode()
        else:
            self._current_mode = self._locked_mode

        if self._scrubber:
            self._scrubber.set_range(self._frame_index.frame_count)

        self._update_clip_info(self._clip)

        if self._frame_index.frame_count <= 0:
            self._current_stem_idx = -1
            self._split_view.set_annotation_stem_index(-1)
            return

        if old_stem and old_stem in self._frame_index.stems:
            new_idx = self._frame_index.stems.index(old_stem)
        elif self._current_stem_idx >= 0:
            new_idx = min(self._current_stem_idx, self._frame_index.frame_count - 1)
        else:
            new_idx = 0

        self._current_stem_idx = new_idx
        self._split_view.set_annotation_stem_index(new_idx)
        if self._scrubber:
            self._scrubber.set_frame(new_idx)
        # Re-push per-stem availability because set_available_modes
        # reset it to "all clip-available" and the current stem may
        # actually only have a subset.
        self._push_stem_availability(new_idx)

    def load_preview_from_file(self, file_path: str, clip_name: str, frame_index: int) -> None:
        """Load a worker preview image.

        Gated: only applied if viewing the same clip AND at the latest
        frame (Codex: don't override user browsing).  Always rebuilds
        FrameIndex so mode buttons enable as new outputs appear.
        """
        if clip_name != self._clip_name:
            return

        # Don't interrupt annotation mode with heavy I/O (frame index rebuild + decode)
        if self._split_view.is_annotating:
            return

        # Rebuild frame index so newly written outputs are discoverable
        if self._clip:
            self._frame_index = self._build_frame_index(self._clip)
            # Enable mode buttons as new output types appear during inference
            self._mode_bar.set_available_modes(self._frame_index.available_modes())
            # Re-evaluate per-stem availability at the viewer's current
            # position so buttons toggle correctly as frames stream in.
            self._push_stem_availability(self._current_stem_idx)

        # Only update the displayed image if in COMP mode at the latest frame
        if self._current_mode != ViewMode.COMP:
            return
        if self._playback_hint:
            return  # never hijack playback

        if self._frame_index and self._current_stem_idx >= 0:
            at_latest = self._current_stem_idx >= self._frame_index.frame_count - 2
            if not at_latest:
                return

        if not os.path.isfile(file_path) or not self._frame_index:
            return

        # Decode off the GUI thread; latest-wins, same-clip guarded by _expect.
        # The cache key carries the file revision, so a reused temp path with
        # new content is never served stale.
        stem = self._frame_index.frame_count - 1
        if stem < 0:
            return
        if self._scrubber:
            self._scrubber.set_range(self._frame_index.frame_count)
            self._scrubber.set_frame(stem)
        self._current_stem_idx = stem
        target = self._target_size()
        req = DecodeRequest(
            LANE_PRIMARY, stem, ViewMode.COMP, file_path, None, 0, False, target)
        self._requested_mode = ViewMode.COMP
        self._expect[LANE_PRIMARY] = _Expect(stem, ViewMode.COMP.value, target)
        self._decoder.request_foreground(req)

    def _build_frame_index(self, clip: ClipEntry) -> FrameIndex:
        """Build a fresh FrameIndex snapshot for the current clip assets."""
        asset_type = clip.input_asset.asset_type if clip.input_asset else "sequence"
        video_path = clip.input_asset.path if (clip.input_asset and asset_type == "video") else None
        seq_dir = clip.input_asset.path if (clip.input_asset and asset_type == "sequence") else None
        # Resolve output_dir via ClipEntry so per-clip and global "Default
        # Output Directory" overrides are honored. Falls back to
        # ``{clip_root}/Output`` when no override is set.
        try:
            output_dir = clip.output_dir
        except Exception:
            output_dir = None
        return build_frame_index(
            clip.root_path, asset_type, video_path=video_path,
            input_sequence_dir=seq_dir, output_dir=output_dir,
        )

    def show_placeholder(self, text: str = "No clip selected") -> None:
        """Show placeholder text."""
        self._split_view.set_placeholder(text)
        if self._scrubber:
            self._scrubber.set_range(0)
        self._clip_info.setText("")
        self._clip = None
        self._clip_name = ""
        self._frame_index = None
        self._current_stem_idx = -1
        self._reset_frame_state()

    def _reset_frame_state(self) -> None:
        """Clip switch / placeholder: new cache generation, nothing pending."""
        if self._presenter is not None:
            self._presenter.stop()
            if self._scrubber is not None:
                self._scrubber.stop_playback()
        self._playback_hint = False
        self._loop_range = None
        self._decoder.invalidate()
        self._expect.clear()
        self._source_dims = None
        self._shown_raster = None
        self._shown_stem = -1

    def hide_clip_info(self) -> None:
        """Hide the clip info label (used on right panel to avoid duplication)."""
        self._clip_info.hide()

    def show_clip_info(self) -> None:
        """Show the clip info label."""
        self._clip_info.show()

    def add_ab_button(self) -> QPushButton:
        """Add an A/B wipe toggle button at the right edge of the top bar.

        Returns the button so the parent can connect its signal.
        """
        btn = QPushButton("A/B")
        btn.setCheckable(True)
        btn.setFixedHeight(24)
        btn.setFixedWidth(50)
        btn.setToolTip(
            self.tr(
                "Toggle A/B wipe comparison (hotkey: A)\n\n"
                "Overlays input (A) and current output (B) in one viewer\n"
                "with a diagonal divider line.\n\n"
                "Drag the center handle to slide the line.\n"
                "Drag above or below the handle to rotate the angle.\n"
                "Scroll wheel to slide the line (Shift+scroll for fine-grain).\n"
                "Middle-click the line to reset to default."
            )
        )
        btn.setStyleSheet(
            "QPushButton { background-color: #1A1900; color: #808070; "
            "font-size: 10px; padding: 2px 6px; border: 1px solid #2A2910; "
            "margin-right: 8px; }"
            "QPushButton:checked { background-color: #FFF203; color: #000000; "
            "font-weight: 700; border: none; margin-right: 8px; }"
            "QPushButton:hover { border-color: #454430; color: #E0E0E0; }"
        )
        self._top_bar.layout().addWidget(btn)
        self._ab_button = btn
        return btn

    def navigate_to_frame(self, stem_index: int) -> None:
        """Public method for external scrubber to drive navigation."""
        self._navigate_to(stem_index)

    def _update_clip_info(self, clip: ClipEntry) -> None:
        """Update the clip info label with frame count, dimensions, and state."""
        parts = []
        asset = clip.input_asset
        if asset:
            parts.append(f"{asset.frame_count} frames")
            # Prefer dimensions over the asset_type string — "1920x1080" is
            # far more useful than a fixed "sequence"/"video" label.
            # Never read pixels on the GUI thread: use metadata already known
            # on the asset or the source size of a decoded frame.
            dims = self._source_dims or getattr(asset, "_dimensions", None)
            if dims:
                parts.append(f"{dims[0]}x{dims[1]}")
        from ui.state_labels import state_display_name
        parts.append(state_display_name(clip.state))
        self._clip_info.setText("  \u00B7  ".join(parts))  # middle dot separator

    def set_split_mode(self, enabled: bool) -> None:
        """Toggle split view on/off."""
        self._split_view.set_split_enabled(enabled)
        if self._frame_index and self._current_stem_idx >= 0:
            if enabled:
                self._request_frame(self._current_stem_idx, self._current_mode)
                self._load_split_images()
            else:
                self._expect.pop(LANE_SPLIT, None)

    def reset_zoom(self) -> None:
        """Reset zoom to fit."""
        self._split_view.reset_zoom()

    def show_reprocess_preview(self, qimage: QImage) -> None:
        """Show a live reprocess result image in the viewport."""
        if self._split_view.split_enabled:
            self._split_view.set_right_image(qimage)
        else:
            self._split_view.set_image(qimage)

    # ── Navigation ──

    def _navigate_to(self, stem_index: int) -> None:
        """Navigate to a stem index and load the frame for current mode."""
        if not self._frame_index or stem_index < 0:
            return
        stem_index = min(stem_index, self._frame_index.frame_count - 1)
        if self._current_stem_idx >= 0 and stem_index != self._current_stem_idx:
            self._direction = 1 if stem_index > self._current_stem_idx else -1
        self._current_stem_idx = stem_index
        # Annotation stem follows the *shown* frame (set in _on_frame_ready).
        if self._scrubber:
            self._scrubber.set_frame(stem_index)
        self.frame_changed.emit(stem_index)

        # Disable mode buttons that don't have a frame at this stem so the
        # user can't click into an empty state. The currently selected
        # mode stays enabled — if it has no frame here, _request_frame
        # falls back to showing the input frame.
        self._push_stem_availability(stem_index)

        self._request_frame(stem_index, self._current_mode)

        # If split view, also load input frame for left side
        if self._split_view.split_enabled:
            self._load_split_images()

    def _push_stem_availability(self, stem_index: int) -> None:
        """Tell the mode bar which modes have a frame at ``stem_index``.

        Safe to call even if ``stem_index`` is out of range — computes an
        empty set in that case. Keeps the mode bar in sync with whatever
        the current frame index believes about availability at this stem.
        """
        if not self._frame_index or stem_index < 0:
            self._mode_bar.set_current_stem_availability(set())
            return
        present = {
            mode for mode in ViewMode
            if self._frame_index.has_frame(mode, stem_index)
        }
        self._mode_bar.set_current_stem_availability(present)

    # ── Request building ──

    def _resolve(self, stem_index: int, mode: ViewMode):
        """(decoded_mode, path, video_path, video_frame) or None.

        Falls back to INPUT for the same stem when ``mode`` has no frame.
        """
        fi = self._frame_index
        if fi is None:
            return None
        if fi.is_video_mode(mode):
            video_path = fi.video_modes.get(mode)
            return (mode, "", video_path, stem_index) if video_path else None
        path = fi.get_path(mode, stem_index)
        if path:
            return (mode, path, None, 0)
        if mode != ViewMode.INPUT:
            if fi.is_video_mode(ViewMode.INPUT):
                video_path = fi.video_modes.get(ViewMode.INPUT)
                if video_path:
                    return (ViewMode.INPUT, "", video_path, stem_index)
            input_path = fi.get_path(ViewMode.INPUT, stem_index)
            if input_path:
                return (ViewMode.INPUT, input_path, None, 0)
        return None

    def _make_request(self, lane: str, stem_index: int, mode: ViewMode,
                      target_size) -> DecodeRequest | None:
        if lane == LANE_SPLIT:
            mode = ViewMode.INPUT
        resolved = self._resolve(stem_index, mode)
        if resolved is None:
            return None
        decoded_mode, path, video_path, vframe = resolved
        return DecodeRequest(
            lane, stem_index, decoded_mode, path, video_path, vframe,
            self._input_exr_is_linear if decoded_mode == ViewMode.INPUT else False,
            target_size,
        )

    def _canvas_size(self) -> tuple[int, int]:
        return self._split_view.canvas_physical_size()

    def _fit_scale(self) -> float | None:
        if self._source_dims is None:
            return None
        sw, sh = self._source_dims
        cw, ch = self._canvas_size()
        return min(cw / sw, ch / sh)

    def _wants_full(self) -> bool:
        """True when zoom makes the canvas-fit raster too coarse."""
        fit = self._fit_scale()
        if fit is None:
            return False
        zoom = self._split_view.view_transform()[0]
        if zoom <= 1.0:
            return False
        sw = self._source_dims[0]
        fit_raster_w = sw * min(fit, 1.0)
        return fit_raster_w < sw and sw * fit * zoom > fit_raster_w + _RASTER_TOLERANCE_PX

    def _target_size(self):
        return None if self._wants_full() else self._canvas_size()

    def _request_frame(self, stem_index: int, mode: ViewMode,
                       prefetch: bool = True) -> None:
        """Request async decode of a frame (cache hit presents synchronously).

        If the requested mode has no frame at this stem, silently falls
        back to the input frame for the same stem so the viewport always
        shows something sensible. ``self._current_mode`` (and the mode bar)
        keep the requested mode — when the user scrubs back into a range
        where it has data, the correct frame resumes automatically.
        """
        if not self._frame_index:
            return
        self._requested_mode = mode
        req = self._make_request(LANE_PRIMARY, stem_index, mode, self._target_size())
        if req is None:
            self._expect.pop(LANE_PRIMARY, None)
            # Truly nothing to show for this stem (no input either).
            self._split_view.set_placeholder(
                self.tr("No frame available for stem %d") % stem_index
            )
            return
        self._expect[LANE_PRIMARY] = _Expect(
            stem_index, req.mode.value, req.target_size)
        if self._playback_hint:
            self._frame_cache.set_playhead(stem_index, 1, self._loop_range)
        else:
            self._frame_cache.set_playhead(None)
        self._decoder.request_foreground(req)
        if prefetch:
            self._schedule_prefetch(stem_index)

    def _load_split_images(self) -> None:
        """Request the input frame for the split-left lane (never decoded here)."""
        if not self._frame_index or self._current_stem_idx < 0:
            return
        idx = self._current_stem_idx
        req = self._make_request(LANE_SPLIT, idx, ViewMode.INPUT, self._target_size())
        if req is None:
            self._expect.pop(LANE_SPLIT, None)
            return
        self._expect[LANE_SPLIT] = _Expect(idx, req.mode.value, req.target_size)
        self._decoder.request_foreground(req)

    def _window_requests(self, center: int, target) -> list[DecodeRequest]:
        n = self._frame_index.frame_count if self._frame_index else 0
        ahead = [center + d for d in range(1, PREFETCH_AHEAD + 1)]
        behind = [center - d for d in range(1, PREFETCH_BEHIND + 1)]
        order = behind + ahead if self._direction < 0 else ahead + behind
        lanes = [LANE_PRIMARY]
        if self._split_view.split_enabled:
            lanes.append(LANE_SPLIT)
        out: list[DecodeRequest] = []
        for idx in order:
            if not 0 <= idx < n:
                continue
            for lane in lanes:
                req = self._make_request(lane, idx, self._requested_mode, target)
                if req is not None:
                    out.append(req)
        return out

    def _schedule_prefetch(self, center: int) -> None:
        self._decoder.prefetch(self._window_requests(center, self._target_size()))

    # ── Cached-frame API (Tasks 10/11) ──

    def _lane_requests(self, index: int) -> list[DecodeRequest] | None:
        if not self._frame_index or not 0 <= index < self._frame_index.frame_count:
            return None
        target = self._target_size()
        prim = self._make_request(LANE_PRIMARY, index, self._current_mode, target)
        if prim is None:
            return None
        reqs = [prim]
        if self._split_view.split_enabled:
            split = self._make_request(LANE_SPLIT, index, ViewMode.INPUT, target)
            if split is not None:
                reqs.append(split)
        return reqs

    def has_cached_frame(self, index: int) -> bool:
        reqs = self._lane_requests(index)
        return bool(reqs) and all(self._decoder.is_cached(r) for r in reqs)

    def prepare_frame(self, index: int) -> None:
        """Warm ``index`` (priority 0 prefetch) without changing the display."""
        reqs = self._lane_requests(index)
        if reqs is None:
            return
        self._requested_mode = self._current_mode
        self._decoder.prefetch(reqs + self._window_requests(index, reqs[0].target_size))

    def present_cached_frame(self, index: int) -> bool:
        """Paint ``index`` synchronously if every needed lane is cached."""
        if not self.has_cached_frame(index):
            return False
        if (not self._playback_hint and self._current_stem_idx >= 0
                and index != self._current_stem_idx):
            self._direction = 1 if index > self._current_stem_idx else -1
        self._current_stem_idx = index
        self._push_stem_availability(index)
        # While playing, the presenter's prepare_frame owns the prefetch window.
        self._request_frame(index, self._current_mode,
                            prefetch=not self._playback_hint)
        if self._split_view.split_enabled:
            self._load_split_images()
        return True

    # ── Signal Handlers ──

    @Slot(str)
    def set_view_mode(self, mode_value: str) -> None:
        """Programmatically switch view mode (from hotkey or external call)."""
        try:
            mode = ViewMode(mode_value)
        except ValueError:
            return
        btn = self._mode_bar._buttons.get(mode)
        if btn and btn.isEnabled():
            btn.setChecked(True)
            self._on_mode_changed(mode_value)

    def _on_mode_changed(self, mode_value: str) -> None:
        """Handle view mode switch."""
        try:
            self._current_mode = ViewMode(mode_value)
        except ValueError:
            return
        # Ask the mode bar to re-render all buttons. It preserves the
        # per-stem dim state it already knows about, so switching modes
        # doesn't clobber the dim-on-missing-frame visual.
        for m in self._mode_bar._buttons:
            self._mode_bar._refresh_button_style(m)
        self.view_mode_changed.emit(mode_value)

        if self._current_stem_idx >= 0:
            self._request_frame(self._current_stem_idx, self._current_mode)

    @Slot(int)
    def _on_scrubber_frame(self, stem_index: int) -> None:
        """Handle scrubber navigation (strict latest-wins)."""
        if self._presenter is not None:
            self._presenter.seek(stem_index)
        self._navigate_to(stem_index)

    @Slot(bool)
    def _on_playback_state(self, playing: bool) -> None:
        if self._presenter is None:
            return
        if playing:
            self._presenter.start()
        elif self._presenter.stop() and self._scrubber is not None:
            # Paused: exact frame via a normal latest-wins request.
            self._navigate_to(self._scrubber.current_frame())

    @Slot(str, int, str, object, int)
    def _on_frame_ready(self, lane: str, stem_index: int, mode_value: str,
                        qimage: object, generation: int) -> None:
        """Paint a decoded frame iff it is exactly what this lane is waiting for."""
        if generation != self._frame_cache.generation:
            return
        if not isinstance(qimage, QImage):
            return
        if lane == LANE_PRIMARY or self._split_view.split_enabled:
            self.frame_ready_for_index.emit(stem_index)
        exp = self._expect.get(lane)
        if exp is None or exp.stem != stem_index or exp.mode_value != mode_value:
            return  # prefetch / superseded: cache only
        sw, sh = image_source_size(qimage)
        if exp.target_size is None and (qimage.width(), qimage.height()) != (sw, sh):
            return  # reduced raster arriving after a full-res request
        if lane == LANE_SPLIT:
            if self._split_view.split_enabled:
                self._split_view.set_left_image(qimage)
            return
        if lane != LANE_PRIMARY:
            return
        if self._split_view.split_enabled:
            self._split_view.set_right_image(qimage)
        else:
            self._split_view.set_image(qimage)
        first_dims = self._source_dims is None
        self._source_dims = (sw, sh)
        self._shown_raster = (qimage.width(), qimage.height())
        self._shown_stem = stem_index
        self._split_view.set_annotation_stem_index(stem_index)
        if first_dims and self._clip is not None:
            self._update_clip_info(self._clip)
        self._decoder.frame_decoded.emit(stem_index, mode_value, qimage)
        self.frame_presented.emit(stem_index)

    def _rerequest_current(self) -> None:
        if self._frame_index and self._current_stem_idx >= 0:
            self._request_frame(self._current_stem_idx, self._requested_mode)
            if self._split_view.split_enabled:
                self._load_split_images()

    def _on_canvas_resized(self, _w: int, _h: int) -> None:
        self._resize_timer.start()

    def _on_canvas_settled(self) -> None:
        """After a resize, re-request only if the shown raster is now too coarse."""
        fit = self._fit_scale()
        if fit is None or self._shown_raster is None:
            return
        sw = self._source_dims[0]
        zoom = self._split_view.view_transform()[0]
        need = min(sw * fit * max(zoom, 1.0), sw)
        if self._shown_raster[0] < need - _RASTER_TOLERANCE_PX:
            self._rerequest_current()

    @Slot(float)
    def _on_zoom_changed(self, zoom: float) -> None:
        if self._zoom_label:
            self._zoom_label.setText(f"{int(zoom * 100)}%")
        # Zoom-in past the fit raster: one async full-resolution request; the
        # current image stays up until it arrives. Pan-only changes and later
        # zooms are no-ops (the lane's target is already None).
        exp = self._expect.get(LANE_PRIMARY)
        if exp is not None and exp.target_size is not None and self._wants_full():
            self._rerequest_current()
