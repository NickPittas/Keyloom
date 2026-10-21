"""Guard against PySide6 builds that leak None references (6.12.0 aborts the app)."""
import gc
import sys

from PySide6.QtWidgets import QApplication, QWidget

_APP = QApplication.instance() or QApplication([])


def test_repaint_and_resize_do_not_leak_none():
    w = QWidget()
    w.resize(64, 64)
    w.show()
    _APP.processEvents()
    gc.collect()
    before = sys.getrefcount(None)
    for _ in range(200):
        w.repaint()
        w.resize(64, 65)
        w.resize(64, 64)
    gc.collect()
    # A leaking build loses ~3 per iteration (600 total); allow small noise.
    assert sys.getrefcount(None) - before > -50
    w.close()
    w.deleteLater()
