"""Hint keyframe ticks on the FrameScrubber marker overlay."""
from __future__ import annotations

from PySide6.QtCore import QEvent, QPoint, QPointF, Qt
from PySide6.QtGui import QColor, QMouseEvent
from PySide6.QtTest import QTest
from PySide6.QtWidgets import QApplication

from ui.widgets.frame_scrubber import FrameScrubber
from ui.widgets.timeline_widgets import MarkerOverlay

_APP = QApplication.instance() or QApplication([])

CYAN = (0, 229, 255)


def _make(total=101, width=800):
    s = FrameScrubber()
    s.resize(width, 80)
    s.set_range(total)
    s.show()
    _APP.processEvents()
    return s


def _is_cyan(img, x, y):
    c = QColor(img.pixel(x, y))
    return (c.red(), c.green(), c.blue()) == CYAN


def _tick_xs(s):
    ov = s._marker_overlay
    img = ov.grab().toImage()
    y = MarkerOverlay.KEYFRAME_SIZE + 1
    return [x for x in range(img.width()) if _is_cyan(img, x, y)], ov


def test_set_keyframes_sorts_dedups_filters():
    s = _make(10)
    try:
        s.set_keyframes([5, 2, 5, -1, 10, 99, 3, True, 1.5, "4"])
        assert s._keyframes == [2, 3, 5]
        s.set_keyframes(None)
        assert s._keyframes == []
    finally:
        s.deleteLater()


def test_ticks_painted_at_first_mid_last_without_in_out():
    s = _make(101)
    try:
        s.set_keyframes([0, 50, 100])
        _APP.processEvents()
        xs, ov = _tick_xs(s)
        w, n = ov.width(), 101
        for f in (0, 50, 100):
            x = int(f * w / n)
            assert x in xs, (f, x)
        # nothing between first and mid tick far from any tick
        assert not any(10 < x < int(50 * w / n) - 10 for x in xs)
    finally:
        s.deleteLater()


def test_ticks_render_with_in_out_and_keep_markers():
    s = _make(101)
    try:
        s.set_in_out(10, 60)
        s.set_keyframes([0, 30, 100])
        _APP.processEvents()
        xs, ov = _tick_xs(s)
        assert int(30 * ov.width() / 101) in xs
        assert s.get_in_out() == (10, 60)
    finally:
        s.deleteLater()


def test_resize_repositions_ticks():
    s = _make(101, 800)
    try:
        s.set_keyframes([50])
        _APP.processEvents()
        xs1, ov = _tick_xs(s)
        s.resize(400, 80)
        _APP.processEvents()
        xs2, ov = _tick_xs(s)
        assert int(50 * ov.width() / 101) in xs2
        assert max(xs2) < max(xs1)
    finally:
        s.deleteLater()


def test_empty_and_zero_range_safe():
    s = _make(0)
    try:
        s.set_keyframes([0, 1])
        assert s._keyframes == []
        s.grab()
        s.set_range(5)
        s.set_keyframes([])
        xs, _ = _tick_xs(s)
        assert xs == []
        MarkerOverlay().grab()
    finally:
        s.deleteLater()


def test_short_range_one_frame():
    s = _make(1)
    try:
        s.set_keyframes([0, 1])
        assert s._keyframes == [0]
        _APP.processEvents()
        xs, _ = _tick_xs(s)
        assert 0 in xs
    finally:
        s.deleteLater()


def test_set_range_total_change_clears_but_same_total_keeps():
    s = _make(20)
    try:
        s.set_keyframes([3, 7])
        s.set_range(20)
        assert s._keyframes == [3, 7]
        s.set_range(30)
        assert s._keyframes == []
        assert s._marker_overlay._keyframes == []
        _APP.processEvents()
        xs, _ = _tick_xs(s)
        assert xs == []
    finally:
        s.deleteLater()


def test_ticks_are_mouse_transparent_and_tooltip_legend():
    s = _make(101)
    try:
        base = s._slider.toolTip()
        s.set_keyframes([10])
        assert s._marker_overlay.testAttribute(Qt.WA_TransparentForMouseEvents)
        assert "keyframe" in s._slider.toolTip().lower()
        s.set_keyframes([])
        assert s._slider.toolTip() == base
    finally:
        s.deleteLater()


def _move(w, x, y):
    ev = QMouseEvent(QEvent.MouseMove, QPointF(x, y), QPointF(x, y),
                     Qt.NoButton, Qt.LeftButton, Qt.NoModifier)
    QApplication.sendEvent(w, ev)


def test_real_drag_across_ticks_emits_intermediate_frames():
    s = _make(101)
    try:
        s.set_keyframes([30, 50, 70])
        _APP.processEvents()
        sl = s._slider
        seen = []
        sl.valueChanged.connect(seen.append)
        y = sl.height() // 2
        QTest.mousePress(sl, Qt.LeftButton, Qt.NoModifier, QPoint(int(sl.width() * 0.2), y))
        assert sl.isSliderDown()
        for frac in (0.35, 0.5, 0.65, 0.8):
            _move(sl, int(sl.width() * frac), y)
        mid = list(seen)
        QTest.mouseRelease(sl, Qt.LeftButton, Qt.NoModifier,
                           QPoint(int(sl.width() * 0.8), y))
        assert len(mid) >= 4
        assert any(30 < v < 50 for v in mid) and any(50 < v < 70 for v in mid)
        assert not sl.isSliderDown()
        assert s._keyframes == [30, 50, 70]
    finally:
        s.deleteLater()


def test_in_out_marker_drag_still_works_with_keyframes():
    s = _make(101)
    try:
        s.set_in_out(20, 80)
        s.set_keyframes([20, 50, 80])
        _APP.processEvents()
        ov = s._marker_overlay
        got = []
        ov.in_point_dragged.connect(got.append)
        ov._become_interactive()
        x_in = ov._marker_x("in")
        y = ov.height() // 2
        QTest.mousePress(ov, Qt.LeftButton, Qt.NoModifier, QPoint(x_in, y))
        assert ov._dragging == "in"
        _move(ov, int(ov.width() * 0.3), y)
        QTest.mouseRelease(ov, Qt.LeftButton, Qt.NoModifier,
                           QPoint(int(ov.width() * 0.3), y))
        assert got and 25 <= got[-1] <= 35
    finally:
        s.deleteLater()
