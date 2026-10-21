"""Drag-scrubbing behaviour of _FatSlider."""
from __future__ import annotations

from PySide6.QtCore import QEvent, QPoint, QPointF, Qt
from PySide6.QtGui import QMouseEvent
from PySide6.QtTest import QTest
from PySide6.QtWidgets import QApplication

from ui.widgets.timeline_widgets import _FatSlider

_APP = QApplication.instance() or QApplication([])


def _make():
    w = _FatSlider(Qt.Horizontal)
    w.setRange(0, 100)
    w.resize(600, 30)
    w.show()
    return w


def _move(w, x):
    ev = QMouseEvent(QEvent.MouseMove, QPointF(x, 10), QPointF(x, 10),
                     Qt.NoButton, Qt.LeftButton, Qt.NoModifier)
    QApplication.sendEvent(w, ev)


def test_drag_moves_value_and_release_finalizes():
    w = _make()
    try:
        QTest.mousePress(w, Qt.LeftButton, Qt.NoModifier, QPoint(50, 10))
        v0 = w.value()
        assert w.isSliderDown()
        _move(w, 300)
        v1 = w.value()
        _move(w, 550)
        v2 = w.value()
        assert v0 < v1 < v2
        QTest.mouseRelease(w, Qt.LeftButton, Qt.NoModifier, QPoint(550, 10))
        assert not w.isSliderDown()
        _move(w, 100)
        assert w.value() == v2
    finally:
        w.deleteLater()


def test_drag_clamps_outside_widget():
    w = _make()
    try:
        QTest.mousePress(w, Qt.LeftButton, Qt.NoModifier, QPoint(300, 10))
        _move(w, 5000)
        assert w.value() == 100
        _move(w, -5000)
        assert w.value() == 0
        QTest.mouseRelease(w, Qt.LeftButton, Qt.NoModifier, QPoint(-5000, 10))
        assert not w.isSliderDown()
    finally:
        w.deleteLater()


def test_disabled_does_not_seek():
    w = _make()
    try:
        w.setEnabled(False)
        QTest.mousePress(w, Qt.LeftButton, Qt.NoModifier, QPoint(300, 10))
        _move(w, 500)
        assert w.value() == 0
        assert not w.isSliderDown()
    finally:
        w.deleteLater()


def _handle_center(w):
    from PySide6.QtWidgets import QStyle, QStyleOptionSlider
    opt = QStyleOptionSlider()
    w.initStyleOption(opt)
    r = w.style().subControlRect(QStyle.CC_Slider, opt, QStyle.SC_SliderHandle, w)
    return r.center()


def test_value_changed_emits_intermediate_before_release():
    w = _make()
    try:
        seen = []
        w.valueChanged.connect(seen.append)
        QTest.mousePress(w, Qt.LeftButton, Qt.NoModifier, QPoint(50, 10))
        _move(w, 200)
        _move(w, 400)
        before_release = list(seen)
        QTest.mouseRelease(w, Qt.LeftButton, Qt.NoModifier, QPoint(400, 10))
        assert len(before_release) >= 3
        assert before_release == sorted(set(before_release))
        assert before_release[-1] > before_release[0]
    finally:
        w.deleteLater()


def test_drag_starting_on_handle():
    w = _make()
    try:
        w.setValue(50)
        hc = _handle_center(w)
        QTest.mousePress(w, Qt.LeftButton, Qt.NoModifier, hc)
        assert w.isSliderDown()
        _move(w, 500)
        assert w.value() > 50
        _move(w, 100)
        assert w.value() < 50
        QTest.mouseRelease(w, Qt.LeftButton, Qt.NoModifier, QPoint(100, 10))
        assert not w.isSliderDown()
    finally:
        w.deleteLater()


def test_move_without_button_does_not_change_value():
    w = _make()
    try:
        w.setValue(30)
        ev = QMouseEvent(QEvent.MouseMove, QPointF(500, 10), QPointF(500, 10),
                         Qt.NoButton, Qt.NoButton, Qt.NoModifier)
        QApplication.sendEvent(w, ev)
        assert w.value() == 30
        # after a completed gesture, hover moves must not change value either
        QTest.mousePress(w, Qt.LeftButton, Qt.NoModifier, QPoint(300, 10))
        QTest.mouseRelease(w, Qt.LeftButton, Qt.NoModifier, QPoint(300, 10))
        v = w.value()
        QApplication.sendEvent(w, ev)
        assert w.value() == v
    finally:
        w.deleteLater()


def test_move_with_stale_scrub_but_no_button_ignored():
    w = _make()
    try:
        QTest.mousePress(w, Qt.LeftButton, Qt.NoModifier, QPoint(300, 10))
        v = w.value()
        ev = QMouseEvent(QEvent.MouseMove, QPointF(550, 10), QPointF(550, 10),
                         Qt.NoButton, Qt.NoButton, Qt.NoModifier)
        QApplication.sendEvent(w, ev)
        assert w.value() == v
        QTest.mouseRelease(w, Qt.LeftButton, Qt.NoModifier, QPoint(300, 10))
    finally:
        w.deleteLater()


def test_right_button_does_not_scrub():
    w = _make()
    try:
        w.setValue(10)
        QTest.mousePress(w, Qt.RightButton, Qt.NoModifier, QPoint(500, 10))
        assert not w.isSliderDown()
        assert w.value() == 10
        ev = QMouseEvent(QEvent.MouseMove, QPointF(550, 10), QPointF(550, 10),
                         Qt.NoButton, Qt.RightButton, Qt.NoModifier)
        QApplication.sendEvent(w, ev)
        assert w.value() == 10
        QTest.mouseRelease(w, Qt.RightButton, Qt.NoModifier, QPoint(550, 10))
        assert w.value() == 10
    finally:
        w.deleteLater()


def test_composite_scrubber_drag_away_from_markers():
    from ui.widgets.frame_scrubber import FrameScrubber
    s = FrameScrubber()
    try:
        s.resize(800, 80)
        s.set_range(101)
        s.set_in_out(5, 10)  # markers far left; drag happens on the right
        s.show()
        _APP.processEvents()
        sl = s._slider
        seen = []
        sl.valueChanged.connect(seen.append)
        y = sl.height() // 2
        x0 = int(sl.width() * 0.5)
        QTest.mousePress(sl, Qt.LeftButton, Qt.NoModifier, QPoint(x0, y))
        _move(sl, int(sl.width() * 0.7))
        _move(sl, int(sl.width() * 0.9))
        mid = list(seen)
        QTest.mouseRelease(sl, Qt.LeftButton, Qt.NoModifier, QPoint(int(sl.width() * 0.9), y))
        assert len(mid) >= 2 and mid[-1] > mid[0] > 5
        assert sl.value() >= 85
        assert not sl.isSliderDown()
    finally:
        s.deleteLater()
