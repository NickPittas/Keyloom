"""Manual temporal windows and pre-run warnings (no CUDA, no weights)."""
import logging

import pytest

pytest.importorskip("torch")

from backend.service.core import InferenceParams
from tests.test_v2_adapter import GIB, env, run  # noqa: F401
from CorridorKeyV2.corridorkey import video_io
import backend.corridorkey_v2 as v2


@pytest.mark.parametrize("preset", ["80/40", "48/24", "24/12", "8/4", "1/1"])
@pytest.mark.parametrize("profile", ["auto", "low"])
def test_manual_exact_window(env, preset, profile):
    res = run(env, InferenceParams(temporal_window=preset, vram_profile=profile))
    c, s = (int(x) for x in preset.split("/"))
    cfg = env.runs[0].cfg
    assert (cfg.hann_chunk, cfg.hann_stride) == (c, s)
    assert cfg.auto_vram is False and cfg.low_vram == (profile == "low")
    assert (res.hann_chunk, res.hann_stride, res.low_vram) == (c, s, profile == "low")


def test_auto_unchanged(env):
    run(env)
    assert env.runs[0].cfg.auto_vram is True
    run(env, InferenceParams(vram_profile="low"))
    assert (env.runs[1].cfg.hann_chunk, env.runs[1].cfg.hann_stride) == \
        v2._select_low_vram_window((1088, 1920), 50, 20.0)


def test_over_budget_warns_logs_before_runtime_and_keeps_window(env, caplog):
    env.budget = int(1 * GIB)
    seen = []
    env.run_exc = RuntimeError("oom")
    caplog.set_level(logging.WARNING)
    orig = env.engine.build_inference_config
    with pytest.raises(RuntimeError):
        run(env, InferenceParams(temporal_window="80/40"))
    assert (env.runs[0].cfg.hann_chunk, env.runs[0].cfg.hann_stride) == (80, 40)
    msgs = [r.getMessage() for r in caplog.records]
    assert any("80/40" in m and "GiB" in m for m in msgs)
    assert env.engine.last_warnings and "80/40" in env.engine.last_warnings[0]
    assert seen == [] and orig


def test_over_budget_in_completed_result(env):
    env.budget = int(1 * GIB)
    res = run(env, InferenceParams(temporal_window="80/40"))
    assert len(res.warnings) == 1
    assert "80/40" in res.warnings[0] and "1.0" in res.warnings[0]


def test_fitting_manual_no_warning(env):
    assert run(env, InferenceParams(temporal_window="8/4")).warnings == ()


def test_normal_1_1_caveat(env):
    env.budget = int(0.05 * GIB)
    res = run(env, InferenceParams(temporal_window="1/1"))
    assert "conservative" in res.warnings[0]


def test_build_config_without_metadata_has_no_warning(env):
    cfg = env.engine.build_inference_config(
        InferenceParams(temporal_window="24/12"), outputs={"alpha"}, temp_dir=env.tmp)
    assert (cfg.hann_chunk, cfg.hann_stride) == (24, 12) and env.engine.last_warnings == []


def test_unknown_budget_graceful(env, monkeypatch):
    monkeypatch.setattr("backend.v2_temporal.query_vram_budget_gib", lambda: None)
    assert run(env, InferenceParams(temporal_window="80/40")).warnings == ()


def _frame_dir(env, monkeypatch, name):
    d = env.tmp / "seq"
    d.mkdir(exist_ok=True)
    meta = {"shape_hw": (1088, 1920), "num_frames": 50, "frame_paths": [str(d / name)]}
    monkeypatch.setattr(video_io, "probe_frame_dir_meta", lambda *a, **k: meta)
    return d


def _run_dir(env, d, params):
    return env.engine.run_clip(str(d), str(env.tmp / f"o{len(env.runs)}"), params, outputs=("alpha",),
                               temp_dir=env.tmp / "t")


def test_display_coded_exr_warns(env, monkeypatch, caplog):
    d = _frame_dir(env, monkeypatch, "a_000000.exr")
    caplog.set_level(logging.WARNING)
    res = _run_dir(env, d, InferenceParams())
    assert len(res.warnings) == 1 and "display-coded" in res.warnings[0]
    assert any("display-coded" in r.getMessage() for r in caplog.records)


def test_linear_exr_and_png_no_warning(env, monkeypatch):
    d = _frame_dir(env, monkeypatch, "a_000000.exr")
    assert _run_dir(env, d, InferenceParams(input_is_linear=True)).warnings == ()
    d = _frame_dir(env, monkeypatch, "a_000000.png")
    assert _run_dir(env, d, InferenceParams()).warnings == ()
    assert run(env).warnings == ()  # video


def test_warnings_reset_each_run(env, monkeypatch):
    env.budget = int(1 * GIB)
    assert run(env, InferenceParams(temporal_window="80/40")).warnings
    assert run(env).warnings == ()


def test_reuse_resets_tuning_manual_and_low_to_auto(env):
    run(env, InferenceParams(temporal_window="8/4", vram_profile="low"))
    run(env)
    assert env.loads == 1 and len(env.resets) == 2
    assert env.runs[1].cfg.auto_vram is True
