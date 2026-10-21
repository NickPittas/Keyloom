import os

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

import pytest

pytest.importorskip("PySide6", reason="PySide6 not installed")
from PySide6.QtWidgets import QApplication

from backend import InferenceParams
from ui.widgets.parameter_panel import ParameterPanel


def _app() -> QApplication:
    app = QApplication.instance()
    if app is None:
        app = QApplication([])
    return app


def test_color_space_tooltip_explains_preview_and_export_behavior():
    _app()
    panel = ParameterPanel()

    tooltip = panel._color_space.toolTip()

    assert "left INPUT viewer" in tooltip
    assert "future exports" in tooltip
    assert "does not rewrite those files on disk" in tooltip
    assert "rerun inference to save new outputs" in tooltip
    assert panel._color_space_label.toolTip() == tooltip


def test_set_input_is_linear_updates_combo_without_emitting_params_changed():
    _app()
    panel = ParameterPanel()
    fired: list[bool] = []
    panel.params_changed.connect(lambda: fired.append(True))

    panel.set_input_is_linear(True)

    assert panel.get_params().input_is_linear is True
    assert fired == []


def test_defaults_match_inference_params():
    _app()
    assert ParameterPanel().get_params() == InferenceParams()


def test_set_get_roundtrip_non_default():
    _app()
    panel = ParameterPanel()
    p = InferenceParams(
        input_is_linear=True, use_alpha_hint=True, hint_quality=0.7,
        carry_hint=False, carry_hint_quality=0.95, vram_profile="low",
        despill_strength=0.3,
    )
    panel.set_params(p)
    assert panel.get_params() == p


def test_quality_controls_enabled_state():
    _app()
    panel = ParameterPanel()
    assert not panel._hint_quality.isEnabled()
    assert panel._carry_hint_quality.isEnabled()
    panel._use_alpha_hint.setChecked(True)
    panel._carry_hint.setChecked(False)
    assert panel._hint_quality.isEnabled()
    assert not panel._carry_hint_quality.isEnabled()
    panel.set_params(InferenceParams())
    assert not panel._hint_quality.isEnabled()
    assert panel._carry_hint_quality.isEnabled()


def test_cutout_linear_disabled_with_tooltip():
    _app()
    panel = ParameterPanel()
    assert not panel._cutout_linear.isEnabled()
    assert "Not applicable" in panel._cutout_linear.toolTip()
    assert panel.get_params().cutout_linear is False


def test_removed_features_absent():
    _app()
    panel = ParameterPanel()
    assert not hasattr(panel, "live_preview_enabled")
    assert not hasattr(panel, "parallel_frames_changed")
    for name in ("_despeckle_check", "_despeckle_size", "_garbage_matte_check",
                 "_garbage_matte_px", "_refiner_slider", "_live_preview",
                 "_parallel_spin"):
        assert not hasattr(panel, name)


def test_new_controls_emit_params_changed():
    _app()
    panel = ParameterPanel()
    fired: list[int] = []
    panel.params_changed.connect(lambda: fired.append(1))
    panel._use_alpha_hint.setChecked(True)
    panel._hint_quality.setValue(0.5)
    panel._carry_hint.setChecked(False)
    panel._vram_profile.setCurrentIndex(1)
    assert len(fired) >= 4
    n = len(fired)
    panel.set_params(InferenceParams())
    assert len(fired) == n  # suppressed during restore


def test_chroma_params_and_bg_color_still_work():
    _app()
    panel = ParameterPanel()
    panel.set_chroma_params({"strength": 2.0, "clip_black": 0.1, "shrink_grow": 3, "edge_blur": 4})
    cp = panel.get_chroma_params()
    assert cp["strength"] == 2.0 and cp["shrink_grow"] == 3 and cp["edge_blur"] == 4
    panel._bg_color.setCurrentIndex(2)
    assert panel.get_chroma_params()["screen_type"] == "blue"


def test_reset_hint_button_signal_and_default_disabled():
    _app()
    panel = ParameterPanel()
    got = []
    panel.reset_hint_requested.connect(lambda: got.append(1))
    assert panel._reset_hint_btn.text() == "RESET HINT"
    assert not panel._reset_hint_btn.isEnabled()
    panel.set_reset_hint_enabled(True)
    panel._reset_hint_btn.click()
    assert got == [1]
    assert panel._reset_hint_btn.parent() is panel._gvm_btn.parent()


def test_busy_track_gate_survives_annotation_info():
    _app()
    panel = ParameterPanel()
    panel.set_track_enabled(False)
    panel.set_annotation_info(3, 10)
    assert not panel._track_masks_btn.isEnabled()
    panel.set_track_enabled(True)
    assert panel._track_masks_btn.isEnabled()
    panel.set_annotation_info(0, 10)
    assert not panel._track_masks_btn.isEnabled()
