"""Tests for the CorridorKey 2.0 InferenceParams / OutputConfig / service init."""
import pytest

from backend.service import CorridorKeyService, InferenceParams, OutputConfig


def test_defaults():
    p = InferenceParams()
    assert p.input_is_linear is False
    assert p.use_alpha_hint is False
    assert p.hint_quality == 0.95
    assert p.carry_hint is True
    assert p.carry_hint_quality == 0.95
    assert p.vram_profile == "auto"
    assert p.despill_strength == 0.5
    assert p.cutout_linear is False


def test_roundtrip():
    p = InferenceParams(use_alpha_hint=True, hint_quality=0.3, vram_profile="low", despill_strength=0.8)
    assert InferenceParams.from_dict(p.to_dict()) == p


def test_legacy_keys_ignored():
    legacy = {
        "despeckle_size": 400, "refiner_scale": 1.0, "screen_color": "green",
        "garbage_matte_px": 3, "auto_despeckle": True, "source_passthrough": True,
        "edge_erode_px": 1, "edge_blur_px": 2, "despill_strength": 0.7,
    }
    p = InferenceParams.from_dict(legacy)
    assert p.despill_strength == 0.7
    assert not hasattr(p, "despeckle_size")


@pytest.mark.parametrize("kw", [
    {"vram_profile": "high"},
    {"hint_quality": 1.5},
    {"hint_quality": -0.1},
    {"carry_hint_quality": 2},
    {"despill_strength": -1},
])
def test_invalid_params(kw):
    with pytest.raises(ValueError):
        InferenceParams(**kw)


@pytest.mark.parametrize("kw", [
    {"fg_format": "tiff"},
    {"processed_format": "jpg"},
    {"exr_compression": "lzw"},
])
def test_invalid_output_config(kw):
    with pytest.raises(ValueError):
        OutputConfig(**kw)


def test_validate_selection():
    OutputConfig().validate_selection()
    off = OutputConfig(fg_enabled=False, matte_enabled=False, comp_enabled=False, processed_enabled=False)
    with pytest.raises(ValueError):
        off.validate_selection()
    OutputConfig(fg_enabled=False, matte_enabled=False, comp_enabled=False).validate_selection()


def test_service_without_cuda(monkeypatch):
    torch = pytest.importorskip("torch")
    monkeypatch.setattr(torch.cuda, "is_available", lambda: False)
    svc = CorridorKeyService()
    assert svc._v2_engine is None
    assert svc.cuda_keying_available() is False  # lazy detect
    assert svc.detect_device() == "cpu"
    assert svc.detect_device() != "mps"


def test_removed_api():
    svc = CorridorKeyService()
    for name in ("set_model_resolution", "set_inference_backend", "model_resolution",
                 "_engine_screen_color", "_model_resolution", "_inference_backend"):
        assert not hasattr(svc, name), name
    assert not hasattr(CorridorKeyService, "VALID_MODEL_RESOLUTIONS")


# --- hint_mode / temporal_window ---

import dataclasses

from backend.service.core import VALID_HINT_MODES, VALID_TEMPORAL_WINDOWS


def test_hint_mode_defaults():
    p = InferenceParams()
    assert p.hint_mode == "none" and p.use_alpha_hint is False
    assert p.temporal_window == "auto"


def test_hint_mode_legacy_constructor_and_dicts():
    assert InferenceParams(False, True).hint_mode == "full"
    assert InferenceParams(use_alpha_hint=True).hint_mode == "full"
    p = InferenceParams.from_dict({"use_alpha_hint": True, "retired": 1})
    assert p.hint_mode == "full" and p.use_alpha_hint is True
    assert InferenceParams.from_dict({"use_alpha_hint": False}).hint_mode == "none"


def test_hint_mode_explicit_wins_over_stale_bool():
    p = InferenceParams(use_alpha_hint=True, hint_mode="none")
    assert p.hint_mode == "none" and p.use_alpha_hint is False
    p = InferenceParams(use_alpha_hint=False, hint_mode="keyframes")
    assert p.use_alpha_hint is True
    p = InferenceParams.from_dict({"use_alpha_hint": True, "hint_mode": "none"})
    assert p.hint_mode == "none"


@pytest.mark.parametrize("mode", VALID_HINT_MODES)
@pytest.mark.parametrize("win", VALID_TEMPORAL_WINDOWS)
def test_hint_mode_window_roundtrip(mode, win):
    p = InferenceParams(hint_mode=mode, temporal_window=win)
    d = p.to_dict()
    assert d["hint_mode"] == mode and d["temporal_window"] == win
    assert "keyframes" not in d
    assert InferenceParams.from_dict(d) == p


def test_hint_mode_window_invalid():
    with pytest.raises(ValueError):
        InferenceParams(hint_mode="bogus")
    with pytest.raises(ValueError):
        InferenceParams(temporal_window="7/3")
    with pytest.raises(ValueError):
        InferenceParams(vram_profile="huge")


def test_hint_mode_replace_explicit():
    p = InferenceParams(hint_mode="full")
    assert dataclasses.replace(p, hint_mode="none").use_alpha_hint is False
    assert dataclasses.replace(p, hint_mode="keyframes").hint_mode == "keyframes"
