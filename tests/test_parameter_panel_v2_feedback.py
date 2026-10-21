"""Panel feedback: temporal window estimates + input colour notice."""
import sys
from types import SimpleNamespace

import pytest

pytest.importorskip("PySide6")
from PySide6.QtWidgets import QApplication

from backend import InferenceParams
from ui.widgets.parameter_panel import ParameterPanel

_app = QApplication.instance() or QApplication([])
IDS = ["auto", "80/40", "48/24", "24/12", "8/4", "1/1"]


@pytest.fixture
def panel():
    p = ParameterPanel()
    yield p
    p.deleteLater()


def S(peak, error=None):
    return SimpleNamespace(peak=peak, error=error)


def test_construction_makes_no_estimate(monkeypatch):
    import backend.v2_temporal as vt
    monkeypatch.setattr(vt, "estimate_temporal_window",
                        lambda *a, **k: pytest.fail("estimated without context"))
    p = ParameterPanel()
    p.set_params(InferenceParams(temporal_window="8/4"))
    p.get_params()


def test_ids_and_roundtrip(panel):
    assert [panel._temporal_window.itemData(i) for i in range(6)] == IDS
    hits = []
    panel.params_changed.connect(lambda: hits.append(1))
    for pid in IDS:
        for vram in ("auto", "low"):
            panel.set_params(InferenceParams(temporal_window=pid, vram_profile=vram))
            prm = panel.get_params()
            assert (prm.temporal_window, prm.vram_profile) == (pid, vram)
    assert not hits
    panel._temporal_window.setCurrentIndex(4)
    panel._temporal_window.setCurrentIndex(0)
    assert panel.get_params().temporal_window == "auto"


def test_estimates_and_stable_ids(panel):
    panel.set_temporal_context(width=1920, height=1080, frame_count=200, budget_gib=100.0)
    panel._temporal_window.setCurrentIndex(3)
    t = [panel._temporal_window.itemText(i) for i in range(6)]
    assert all("GiB" in x for x in t[1:])
    assert t[0].startswith("Auto (")
    assert panel.get_params().temporal_window == "24/12"
    before = t[3]
    panel._vram_profile.setCurrentIndex(1)
    assert panel._temporal_window.itemText(3) != before
    assert panel.get_params().temporal_window == "24/12"
    assert panel._temporal_window.itemData(3) == "24/12"


def test_over_budget_warning_and_unavailable(panel):
    panel.set_temporal_context(width=3840, height=2160, frame_count=200, budget_gib=0.5)
    panel._temporal_window.setCurrentIndex(1)
    assert not panel._temporal_warning.isHidden()
    assert "e5484d" in panel._temporal_warning.styleSheet()
    panel.set_temporal_context(width=None, height=None, frame_count=0, budget_gib=None)
    assert panel._temporal_warning.isHidden()
    assert [panel._temporal_window.itemText(i) for i in range(6)] == [
        "Auto", "80/40", "48/24", "24/12", "8/4", "1/1"]


def test_short_clip_floor_caveat(panel):
    panel.set_temporal_context(width=1920, height=1080, frame_count=2, budget_gib=100.0)
    panel._temporal_window.setCurrentIndex(1)
    assert panel._temporal_warning.toolTip()


def test_colour_notice_thresholds_and_action(panel):
    got = []
    panel.input_colour_switch_requested.connect(got.append)
    panel.set_input_colour_notice(S(0.9), True)
    assert not panel._colour_notice.isHidden() and not panel._colour_action.isHidden()
    assert "may be" in panel._colour_notice.text()
    panel.set_input_is_linear(True)
    panel._colour_action.click()
    assert got == [False]
    assert panel.get_params().input_is_linear is True  # panel not mutated
    panel.set_input_colour_notice(S(1.0), True)
    assert not panel._colour_notice.isHidden()
    panel.set_input_colour_notice(S(1.0), False)
    assert panel._colour_notice.isHidden()
    panel.set_input_colour_notice(S(3.0), False)
    assert not panel._colour_notice.isHidden()
    panel._colour_action.click()
    assert got == [False, True]
    panel.set_input_is_linear(True)  # stored sample now consistent
    assert panel._colour_notice.isHidden()
    panel.set_input_colour_notice(S(3.0), False)
    panel.set_input_colour_notice(S(None, "boom"), False)
    assert panel._colour_notice.isHidden() and panel._colour_action.isHidden()
    panel.set_input_colour_notice(S(3.0), False)
    panel.set_input_colour_notice(None, False)
    assert panel._colour_notice.isHidden()


def test_no_cuda_probe(monkeypatch):
    import backend.v2_temporal as vt
    monkeypatch.setattr(vt, "query_vram_budget_gib",
                        lambda: pytest.fail("probed CUDA"))
    p = ParameterPanel()
    p.set_params(InferenceParams(temporal_window="8/4"))
    p.get_params()
    p.set_temporal_context(width=1280, height=720, frame_count=50, budget_gib=8.0)
