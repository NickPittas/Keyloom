import os

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

import numpy as np
import pytest

pytest.importorskip("PySide6", reason="PySide6 not installed")
from ui.main_window import MainWindow
from ui.main_window_mixins import inference_mixin


class _DummyDualViewer:
    def __init__(self):
        self.previews: list[object] = []

    def show_reprocess_preview(self, qimage) -> None:
        self.previews.append(qimage)


class _Clip:
    name = "shot"


def _window(current_name="shot"):
    window = MainWindow.__new__(MainWindow)
    window._dual_viewer = _DummyDualViewer()
    window._clip_model = type("M", (), {"clips": [_Clip()]})()
    window._current_clip = type("C", (), {"name": current_name})()
    window._sam2_preview_qimage = lambda f, m: "qimg"
    return window


def _result():
    return {
        "kind": "sam2_preview",
        "clip_name": "shot",
        "frame_rgb": np.zeros((4, 4, 3), dtype=np.uint8),
        "mask": np.zeros((4, 4), dtype=np.float32),
        "frame_index": 0,
        "fill": 0.5,
    }


def test_keyed_frame_result_is_ignored():
    """v1 keyed-frame dicts (comp/fg/alpha) no longer produce a preview."""
    window = _window()
    MainWindow._on_sam2_preview_result(
        window, "job-1", {"comp": np.zeros((4, 4, 3), dtype=np.float32)},
    )
    assert window._dual_viewer.previews == []


def test_non_dict_result_is_ignored():
    window = _window()
    MainWindow._on_sam2_preview_result(window, "job-1", None)
    assert window._dual_viewer.previews == []


def test_sam2_preview_for_other_clip_is_ignored():
    window = _window(current_name="other")
    MainWindow._on_sam2_preview_result(window, "job-2", _result())
    assert window._dual_viewer.previews == []


def test_sam2_preview_shows_and_continues_on_yes(monkeypatch):
    window = _window()
    submitted = []
    window._submit_sam2_track_job = submitted.append
    monkeypatch.setattr(
        inference_mixin.QMessageBox, "question",
        staticmethod(lambda *a, **k: inference_mixin.QMessageBox.Yes),
    )
    MainWindow._on_sam2_preview_result(window, "job-3", _result())
    assert window._dual_viewer.previews == ["qimg"]
    assert len(submitted) == 1


def test_sam2_preview_no_keeps_refining(monkeypatch):
    window = _window()
    submitted = []
    messages = []
    window._submit_sam2_track_job = submitted.append
    window._status_bar = type("S", (), {"set_message": lambda s, m: messages.append(m)})()
    monkeypatch.setattr(
        inference_mixin.QMessageBox, "question",
        staticmethod(lambda *a, **k: inference_mixin.QMessageBox.No),
    )
    MainWindow._on_sam2_preview_result(window, "job-4", _result())
    assert submitted == []
    assert len(messages) == 1
