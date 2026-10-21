"""Frame scrubber timeline with transport controls and coverage overlay.

Horizontal slider + frame counter + step buttons. User navigation emits
``frame_changed`` immediately; playback is clock-driven (FPS) and requests
frames through ``playback_requested``.

Helper widgets (CoverageBar, MarkerOverlay, _FatSlider) live in
``ui.widgets.timeline_widgets`` to keep this module focused on FrameScrubber.
"""
from __future__ import annotations

from PySide6.QtWidgets import QWidget, QHBoxLayout, QVBoxLayout, QLabel, QPushButton, QToolTip
import math

from PySide6.QtCore import Qt, Signal, QTimer, QEvent, QSettings, QElapsedTimer

from ui.widgets.timeline_widgets import _FatSlider, CoverageBar, MarkerOverlay


class FrameScrubber(QWidget):
    """Frame navigation scrubber with transport controls, coverage bar, and debounced output."""

    frame_changed = Signal(int)      # immediate user navigation, stem index
    playback_requested = Signal(int, int)  # (target index, loop cycle counter)
    playback_state_changed = Signal(bool)
    in_point_changed = Signal(int)   # in-point set at stem index
    out_point_changed = Signal(int)  # out-point set at stem index
    range_cleared = Signal()         # in/out range cleared

    def __init__(self, parent=None):
        super().__init__(parent)
        self.setFixedHeight(48)

        row = QHBoxLayout(self)
        row.setContentsMargins(8, 0, 8, 0)
        row.setSpacing(4)

        # Frame counter label (left)
        self._frame_label = QLabel("0 / 0")
        self._frame_label.setFixedWidth(90)
        self._frame_label.setAlignment(Qt.AlignCenter)
        self._frame_label.setStyleSheet("color: #808070; font-size: 11px;")
        row.addWidget(self._frame_label)

        # Transport controls
        btn_style = (
            "QPushButton { background: transparent; color: #808070; border: none; "
            "font-size: 14px; padding: 0 4px; font-family: sans-serif; }"
            "QPushButton:hover { color: #E0E0E0; }"
            "QPushButton:disabled { color: #3A3A30; }"
        )

        # Jump to start
        self._start_btn = QPushButton("\u25C0\u25C0")  # ◀◀
        self._start_btn.setFixedWidth(28)
        self._start_btn.setStyleSheet(btn_style)
        self._start_btn.setToolTip(self.tr("Go to first frame"))
        self._start_btn.clicked.connect(self._go_start)
        row.addWidget(self._start_btn)

        # Step back
        self._prev_btn = QPushButton("\u25C0")  # ◀
        self._prev_btn.setFixedWidth(24)
        self._prev_btn.setStyleSheet(btn_style)
        self._prev_btn.setToolTip(self.tr("Previous frame"))
        self._prev_btn.clicked.connect(self._step_back)
        row.addWidget(self._prev_btn)

        # Play/Pause toggle
        self._play_btn = QPushButton("\u25B6")  # ▶ (play icon)
        self._play_btn.setFixedWidth(24)
        self._play_btn.setStyleSheet(btn_style)
        self._play_btn.setToolTip(self.tr("Play / Pause (Space)"))
        self._play_btn.clicked.connect(self.toggle_playback)
        row.addWidget(self._play_btn)

        # ── Center column: coverage bar + slider ──
        self._center = QWidget()
        center_layout = QVBoxLayout(self._center)
        center_layout.setContentsMargins(0, 2, 0, 0)
        center_layout.setSpacing(0)

        # Coverage bar (top of center column — same width as slider)
        self._coverage_bar = CoverageBar()
        self._coverage_bar.setToolTip(
            self.tr(
                "Coverage bar — shows which frames have been processed.\n"
                "Green lane: painted frames (brush strokes).\n"
                "White lane: alpha hint coverage.\n"
                "Yellow lane: inference output coverage."
            )
        )
        center_layout.addWidget(self._coverage_bar)

        # Slider (bottom of center column) — tall hitbox for easy clicking
        self._slider = _FatSlider(Qt.Horizontal)
        self._slider.setMinimum(0)
        self._slider.setMaximum(0)
        self._slider.setEnabled(False)
        self._slider.setToolTip(self.tr("Scrub through frames. Scroll wheel or Left/Right to step."))
        self._slider.valueChanged.connect(self._on_slider_changed)
        center_layout.addWidget(self._slider)

        row.addWidget(self._center, 1)

        # Marker overlay — parented to center, sits on top
        self._marker_overlay = MarkerOverlay(self._center)
        self._marker_overlay.in_point_dragged.connect(self._on_in_dragged)
        self._marker_overlay.out_point_dragged.connect(self._on_out_dragged)
        self._marker_overlay.scrub_to_frame.connect(self._on_marker_scrub)
        self._marker_overlay.raise_()

        # Track mouse over slider/overlay to detect marker proximity + tooltips
        self._slider.setMouseTracking(True)
        self._slider.installEventFilter(self)
        self._marker_overlay.installEventFilter(self)
        self._center.installEventFilter(self)

        # Step forward
        self._next_btn = QPushButton("\u25B6")  # ▶
        self._next_btn.setFixedWidth(24)
        self._next_btn.setStyleSheet(btn_style)
        self._next_btn.setToolTip(self.tr("Next frame"))
        self._next_btn.clicked.connect(self._step_forward)
        row.addWidget(self._next_btn)

        # Jump to end
        self._end_btn = QPushButton("\u25B6\u25B6")  # ▶▶
        self._end_btn.setFixedWidth(28)
        self._end_btn.setStyleSheet(btn_style)
        self._end_btn.setToolTip(self.tr("Go to last frame"))
        self._end_btn.clicked.connect(self._go_end)
        row.addWidget(self._end_btn)

        # Playback clock: target frame derives from elapsed time, not tick count
        self._elapsed = QElapsedTimer()
        self._elapsed.start()
        self._clock = lambda: self._elapsed.elapsed()
        self._fps = 24.0
        self._playback_timer = QTimer(self)
        self._playback_timer.setTimerType(Qt.PreciseTimer)
        self._playback_timer.timeout.connect(self._playback_tick)
        self._playing = False
        self._anchor_index = 0
        self._anchor_ms = 0
        self._last_emitted: tuple[int, int] | None = None
        # Also emit frame_changed on each playback tick (legacy consumers).
        self._legacy_playback_emit = True

        self._total = 0
        self._suppress_signal = False
        self._in_point: int | None = None
        self._out_point: int | None = None

    def resizeEvent(self, event) -> None:
        """Keep marker overlay sized to the center column."""
        super().resizeEvent(event)
        self._marker_overlay.setGeometry(0, 0, self._center.width(), self._center.height())
        self._marker_overlay.raise_()

    def eventFilter(self, obj, event) -> bool:
        """Watch slider for mouse moves (marker proximity) and tooltip forwarding."""
        if obj is self._slider:
            if event.type() == QEvent.MouseMove:
                x = int(event.position().x())
                hit = self._marker_overlay._hit_marker(x)
                if hit and not self._marker_overlay._dragging:
                    self._marker_overlay._become_interactive()
        # Forward tooltip events from overlay or center to correct child
        if event.type() == QEvent.ToolTip and obj in (
            self._marker_overlay, self._center,
        ):
            local_y = int(event.pos().y())
            coverage_h = self._coverage_bar.height()
            if local_y < coverage_h:
                tip = self._coverage_bar.toolTip()
            else:
                tip = self._slider.toolTip()
            if tip:
                QToolTip.showText(event.globalPos(), tip, obj)
                return True
        return super().eventFilter(obj, event)

    def set_range(self, total_frames: int) -> None:
        """Configure scrubber for a clip with total_frames stems."""
        changed = total_frames != self._total
        if self._playing and changed:
            self._stop_playback()
        self._total = total_frames
        enabled = total_frames > 0
        self._slider.setEnabled(enabled)
        self._slider.setMaximum(max(0, total_frames - 1))
        self._slider.setTickInterval(max(1, total_frames // 20))
        self._start_btn.setEnabled(enabled)
        self._prev_btn.setEnabled(enabled)
        self._play_btn.setEnabled(enabled)
        self._next_btn.setEnabled(enabled)
        self._end_btn.setEnabled(enabled)
        self._update_label()
        if self._playing:
            self._reanchor()

    def set_frame(self, index: int) -> None:
        """Set current frame without emitting signal (external update)."""
        self._suppress_signal = True
        self._slider.setValue(index)
        self._suppress_signal = False
        self._update_label()
        if self._playing:
            self._reanchor()

    def set_coverage(self, alpha: list[bool], inference: list[bool]) -> None:
        """Update the coverage overlay lanes."""
        self._coverage_bar.set_coverage(alpha, inference)

    def set_annotation_markers(self, annotated: list[bool]) -> None:
        """Update the annotation marker lane on the coverage bar."""
        self._coverage_bar.set_annotation_markers(annotated)

    def current_frame(self) -> int:
        return self._slider.value()

    # ── In/Out Markers ──

    def set_in_point(self, index: int | None) -> None:
        """Set in-point at stem index. None to clear just the in-point."""
        self._in_point = index
        self._sync_markers()
        if index is not None:
            self.in_point_changed.emit(index)

    def set_out_point(self, index: int | None) -> None:
        """Set out-point at stem index. None to clear just the out-point."""
        self._out_point = index
        self._sync_markers()
        if index is not None:
            self.out_point_changed.emit(index)

    def set_in_out(self, in_point: int | None, out_point: int | None) -> None:
        """Set both markers at once (for restore from session)."""
        self._in_point = in_point
        self._out_point = out_point
        self._sync_markers()

    def clear_in_out(self) -> None:
        """Clear both markers."""
        self._in_point = None
        self._out_point = None
        self._sync_markers()
        self._range_refreshed()
        self.range_cleared.emit()

    def get_in_out(self) -> tuple[int | None, int | None]:
        """Return current (in_point, out_point)."""
        return self._in_point, self._out_point

    @property
    def has_range(self) -> bool:
        return self._in_point is not None and self._out_point is not None

    def _sync_markers(self) -> None:
        """Push current in/out state to marker overlay."""
        self._marker_overlay.set_in_out(self._in_point, self._out_point, self._total)
        self._range_refreshed()

    def _range_refreshed(self) -> None:
        """Range changed: keep playing, re-anchored inside the new range."""
        if self._playing:
            self._reanchor()

    def _on_in_dragged(self, frame: int) -> None:
        """Handle in-point drag from overlay."""
        self._in_point = frame
        self._range_refreshed()
        self.in_point_changed.emit(frame)

    def _on_out_dragged(self, frame: int) -> None:
        """Handle out-point drag from overlay."""
        self._out_point = frame
        self._range_refreshed()
        self.out_point_changed.emit(frame)

    def _on_marker_scrub(self, frame: int) -> None:
        """While dragging a marker, move the playhead and emit immediately."""
        self._slider.setValue(frame)

    def _on_slider_changed(self, value: int) -> None:
        self._update_label()
        if not self._suppress_signal:
            if self._playing:
                self._reanchor()
            self.frame_changed.emit(value)

    def _update_label(self) -> None:
        current = self._slider.value() + 1 if self._total > 0 else 0
        self._frame_label.setText(f"{current} / {self._total}")

    # Transport controls
    def _go_start(self) -> None:
        self._slider.setValue(0)

    def _step_back(self) -> None:
        self._slider.setValue(max(0, self._slider.value() - 1))

    def _step_forward(self) -> None:
        self._slider.setValue(min(self._slider.maximum(), self._slider.value() + 1))

    def _go_end(self) -> None:
        self._slider.setValue(self._slider.maximum())

    # ── Playback ──

    def toggle_playback(self) -> None:
        """Toggle play/pause state."""
        if self._playing:
            self._stop_playback()
        else:
            self._start_playback()

    def set_fps(self, fps: float) -> None:
        """Set playback rate; non-finite or non-positive falls back to 24."""
        try:
            value = float(fps)
        except (TypeError, ValueError):
            value = 0.0
        if not math.isfinite(value) or value <= 0:
            value = 24.0
        self._fps = value
        self._playback_timer.setInterval(max(4, int(1000 / value / 2)))
        if self._playing:
            self._reanchor()

    def is_playing(self) -> bool:
        return self._playing

    def stop_playback(self) -> None:
        if self._playing:
            self._stop_playback()

    def show_presented_frame(self, index: int) -> None:
        """Move slider/label silently WITHOUT re-anchoring the playback clock."""
        self._suppress_signal = True
        try:
            self._slider.setValue(index)
        finally:
            self._suppress_signal = False
        self._update_label()

    def play_range(self) -> tuple[int, int]:
        return self._play_range()

    def loop_enabled(self) -> bool:
        return self._loop_enabled()

    def _play_range(self) -> tuple[int, int]:
        hi_max = max(0, self._total - 1)
        if self._in_point is not None and self._out_point is not None:
            lo = max(0, min(self._in_point, self._out_point))
            hi = min(hi_max, max(self._in_point, self._out_point))
            return min(lo, hi), hi
        return 0, hi_max

    def _loop_enabled(self) -> bool:
        return QSettings().value("playback/loop", True, type=bool)

    def _reanchor(self) -> None:
        lo, hi = self._play_range()
        self._anchor_index = min(max(self._slider.value(), lo), hi)
        self._anchor_ms = self._clock()
        self._last_emitted = None

    def _start_playback(self) -> None:
        if self._total <= 1:
            return
        self._playing = True
        self._reanchor()
        self._play_btn.setText("\u275A\u275A")  # pause icon
        self._play_btn.setToolTip(self.tr("Pause (Space)"))
        self.set_fps(self._fps)
        self._playback_timer.start()
        self.playback_state_changed.emit(True)

    def _stop_playback(self) -> None:
        self._playing = False
        self._playback_timer.stop()
        self._play_btn.setText("\u25B6")  # play icon
        self._play_btn.setToolTip(self.tr("Play (Space)"))
        self.playback_state_changed.emit(False)

    def _playback_tick(self) -> None:
        """Move to the frame the clock says we should be at (skipping if late)."""
        if not self._playing:
            return
        lo, hi = self._play_range()
        span = hi - lo + 1
        elapsed = max(0, self._clock() - self._anchor_ms)
        advanced = int(elapsed * self._fps / 1000.0)
        raw = (self._anchor_index - lo) + advanced
        if self._loop_enabled():
            cycle = raw // span
            idx = lo + raw % span
            stop = False
        else:
            cycle = 0
            stop = raw >= span - 1 and advanced > 0
            idx = hi if raw >= span else lo + raw
        key = (idx, cycle)
        if key != self._last_emitted:
            self._last_emitted = key
            self._suppress_signal = True
            self._slider.setValue(idx)
            self._suppress_signal = False
            self._update_label()
            self.playback_requested.emit(idx, cycle)
            if self._legacy_playback_emit:
                self.frame_changed.emit(idx)
        if stop:
            self._stop_playback()

    def wheelEvent(self, event) -> None:
        """Scroll wheel scrubs frames: scroll up = forward, scroll down = back.

        Steps by 2% of total frames per tick (minimum 1 frame).
        """
        if self._total <= 0:
            return
        step = max(1, self._total * 2 // 100)
        delta = event.angleDelta().y()
        if delta > 0:
            self._slider.setValue(min(self._slider.maximum(), self._slider.value() + step))
        elif delta < 0:
            self._slider.setValue(max(0, self._slider.value() - step))
        event.accept()

    def keyPressEvent(self, event) -> None:
        """Space for play/pause, Left/Right for stepping, I/O for markers."""
        if event.key() == Qt.Key_Space:
            self.toggle_playback()
        elif event.key() == Qt.Key_Left:
            self._step_back()
        elif event.key() == Qt.Key_Right:
            self._step_forward()
        elif event.key() == Qt.Key_Home:
            self._go_start()
        elif event.key() == Qt.Key_End:
            self._go_end()
        elif event.key() == Qt.Key_I:
            self.set_in_point(self._slider.value())
        elif event.key() == Qt.Key_O:
            self.set_out_point(self._slider.value())
        else:
            super().keyPressEvent(event)
