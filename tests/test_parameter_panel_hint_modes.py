import os

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

import pytest

pytest.importorskip("PySide6", reason="PySide6 not installed")
from PySide6.QtWidgets import QApplication

from backend import InferenceParams
from ui.widgets.parameter_panel import ParameterPanel

_APP = None


def _panel() -> ParameterPanel:
    global _APP
    _APP = QApplication.instance() or QApplication([])
    return ParameterPanel()


def _ctx(p, **kw):
    d = dict(has_clip=True, frame_available=True, is_marked=False, count=0)
    d.update(kw)
    p.set_hint_keyframe_context(**d)


def test_entries_and_default():
    p = _panel()
    assert [p._hint_mode.itemData(i) for i in range(3)] == ["none", "full", "keyframes"]
    assert p._hint_mode.itemText(0) == "None"
    assert p._hint_mode.itemText(1) == "Full AlphaHint"
    assert p.get_params().hint_mode == "none"
    assert not hasattr(p, "_use_alpha_hint")


@pytest.mark.parametrize("n", [0, 1, 7])
def test_keyframe_counts(n):
    p = _panel()
    _ctx(p, count=n)
    assert p._hint_mode.itemText(2) == f"Keyframes ({n})"
    item = p._hint_mode.model().item(2)
    assert item.isEnabled() == (n > 0)
    assert bool(item.toolTip()) == (n == 0)


@pytest.mark.parametrize("mode", ["none", "full", "keyframes"])
def test_roundtrip_no_emit(mode):
    p = _panel()
    fired = []
    p.params_changed.connect(lambda: fired.append(1))
    prm = InferenceParams(hint_mode=mode, temporal_window="auto", carry_hint=False)
    p.set_params(prm)
    assert p.get_params() == prm
    assert fired == []


def test_hint_quality_and_carry_independent():
    p = _panel()
    assert not p._hint_quality.isEnabled()
    for mode, en in (("full", True), ("keyframes", True), ("none", False)):
        p.set_params(InferenceParams(hint_mode=mode, carry_hint=True))
        assert p._hint_quality.isEnabled() == en
        assert p._carry_hint_quality.isEnabled()
    p.set_params(InferenceParams(hint_mode="none", carry_hint=False))
    assert not p._carry_hint_quality.isEnabled()
    assert "Hann" in p._carry_hint.toolTip()


def test_unavailable_keyframes_restore_stays_selected():
    p = _panel()
    _ctx(p, count=0)
    p.set_params(InferenceParams(hint_mode="keyframes"))
    assert p.get_params().hint_mode == "keyframes"
    assert p._hint_mode.currentData() == "keyframes"
    assert p._hint_mode.property("invalid") is True
    _ctx(p, count=2)
    assert p._hint_mode.property("invalid") is False
    assert p.get_params().hint_mode == "keyframes"


def test_mark_button_label_signal_and_gates():
    p = _panel()
    got = []
    p.hint_keyframe_toggle_requested.connect(lambda: got.append(1))
    assert not p._hint_keyframe_btn.isEnabled()  # no clip
    _ctx(p, has_clip=False)
    assert not p._hint_keyframe_btn.isEnabled()
    _ctx(p)
    assert p._hint_keyframe_btn.text() == "Mark keyframe"
    p._hint_keyframe_btn.click()
    assert got == [1]
    p.set_hint_keyframe_mutation_enabled(False)
    assert not p._hint_keyframe_btn.isEnabled()
    p.set_hint_keyframe_mutation_enabled(True)
    assert p._hint_keyframe_btn.isEnabled()
    _ctx(p, frame_available=False)
    assert not p._hint_keyframe_btn.isEnabled()
    _ctx(p, frame_available=False, is_marked=True, count=1)
    assert p._hint_keyframe_btn.text() == "Unmark keyframe"
    assert p._hint_keyframe_btn.isEnabled()
    p.set_hint_keyframe_mutation_enabled(False)
    assert not p._hint_keyframe_btn.isEnabled()
    _ctx(p, error="boom")
    assert p._hint_keyframe_btn.toolTip() == "boom"


def test_context_and_reset_do_not_emit():
    p = _panel()
    fired = []
    p.params_changed.connect(lambda: fired.append(1))
    _ctx(p, count=3)
    p.set_hint_keyframe_mutation_enabled(False)
    p.set_params(InferenceParams())
    assert fired == []
