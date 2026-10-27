"""CorridorKey 2.0 adapter tests (no CUDA, no weights)."""
import os
from types import SimpleNamespace

import pytest

torch = pytest.importorskip("torch")

import backend.corridorkey_v2 as v2
from backend.service.core import InferenceParams
from CorridorKeyV2.corridorkey import runtime as rt
from CorridorKeyV2.corridorkey import video_io

GIB = 2**30


class Cancelled(Exception):
    pass


@pytest.fixture
def env(monkeypatch, tmp_path):
    st = SimpleNamespace(loads=0, runs=[], resets=[], budget=20 * GIB, problems=[],
                         load_env=None, run_exc=None)
    model = object()

    monkeypatch.setattr(v2, "validate_v2_assets", lambda p: list(st.problems))
    monkeypatch.setattr(torch.cuda, "is_available", lambda: True)
    monkeypatch.setattr(torch.cuda, "mem_get_info", lambda: (st.budget, 24 * GIB))
    monkeypatch.setattr(torch.cuda, "memory_reserved", lambda: 0)
    monkeypatch.setattr(torch.cuda, "empty_cache", lambda: None)

    def load_model(paths, device="cuda"):
        st.loads += 1
        if st.load_env:
            os.environ.update(st.load_env)
        if st.load_fail:
            raise RuntimeError("boom")
        return model

    def run_inference(m, inp, out, cfg, wcfg, **kw):
        st.runs.append(SimpleNamespace(model=m, inp=inp, out=out, cfg=cfg, kw=kw))
        if st.run_exc:
            raise st.run_exc
        clip = video_io.raw_clip_name(inp)
        for p in cfg.outputs:
            os.makedirs(v2.runtime_product_dir(out, clip, p, cfg.despill_strength))

    st.load_fail = False
    monkeypatch.setattr(rt, "load_model", load_model)
    monkeypatch.setattr(rt, "run_inference", run_inference)
    monkeypatch.setattr(rt, "configure_low_vram_inference",
                        lambda m, cfg: st.resets.append((m, cfg)))
    meta = {"shape_hw": (1088, 1920), "num_frames": 50}
    monkeypatch.setattr(video_io, "probe_video_meta", lambda *a, **k: meta)
    monkeypatch.setattr(video_io, "probe_frame_dir_meta", lambda *a, **k: meta)
    st.engine = v2.CorridorKeyV2Engine(asset_paths=object())
    # RuntimePaths(**kwargs) needs real kwargs; stub the paths object
    st.engine._paths = SimpleNamespace(as_runtime_kwargs=lambda: _paths_kwargs(tmp_path))
    st.tmp = tmp_path
    return st


def _paths_kwargs(tmp):
    keys = ("package_root", "weights", "foundation_stats", "rvm_ckpt",
            "moge_checkpoint", "cradio_dir")
    return dict.fromkeys(keys, tmp)


def run(st, params=None, outputs=("alpha", "fg"), **kw):
    params = params or InferenceParams()
    out = st.tmp / f"out{len(st.runs)}"
    return st.engine.run_clip(str(st.tmp / "shot.mp4"), str(out), params, outputs=outputs,
                              temp_dir=st.tmp / "t", **kw)


def test_load_once_and_reload_after_unload(env):
    run(env), run(env)
    assert env.loads == 1 and env.engine.load_count == 1
    env.engine.unload()
    assert not env.engine.is_loaded
    run(env)
    assert env.engine.load_count == 2


def test_missing_assets(env):
    env.problems = ["weights missing"]
    with pytest.raises(v2.V2EngineError, match="setup_models"):
        env.engine.load()


def test_no_cuda(env, monkeypatch):
    monkeypatch.setattr(torch.cuda, "is_available", lambda: False)
    with pytest.raises(v2.V2EngineError, match="CUDA"):
        env.engine.load()


def test_load_failure(env):
    env.load_fail = True
    with pytest.raises(v2.V2EngineError) as ei:
        env.engine.load()
    assert isinstance(ei.value.__cause__, RuntimeError)
    assert not env.engine.is_loaded and env.engine.load_count == 0


@pytest.mark.parametrize("initial", [None, "0"])
def test_env_restored(env, monkeypatch, initial):
    for k in v2._ENV_KEYS:
        if initial is None:
            monkeypatch.delenv(k, raising=False)
        else:
            monkeypatch.setenv(k, initial)
    env.load_env = {k: "1" for k in v2._ENV_KEYS}
    run(env)
    for k in v2._ENV_KEYS:
        assert os.environ.get(k) == initial
    env.load_fail = True
    env.engine.unload()
    with pytest.raises(v2.V2EngineError):
        env.engine.load()
    for k in v2._ENV_KEYS:
        assert os.environ.get(k) == initial


def test_hint_off(env):
    run(env, InferenceParams(hint_quality=0.7), hint_video_path="ignored.mp4")
    r = env.runs[0]
    assert r.kw["initial_hint_path"] is None and r.kw["hint_video_path"] is None
    assert r.cfg.hint_quality == 0.7


def test_hint_on(env):
    run(env, InferenceParams(use_alpha_hint=True), hint_video_path="h.mp4")
    assert env.runs[0].kw["hint_video_path"] == "h.mp4"
    assert env.runs[0].kw["initial_hint_path"] is None
    with pytest.raises(v2.V2EngineError, match="AlphaHint"):
        run(env, InferenceParams(use_alpha_hint=True))


def test_auto_profile(env):
    run(env)
    assert env.runs[0].cfg.auto_vram is True


def test_low_profile_largest_fitting(env):
    res = run(env, InferenceParams(vram_profile="low"))
    cfg = env.runs[0].cfg
    exp = v2._select_low_vram_window((1088, 1920), 50, 20.0)
    assert (cfg.auto_vram, cfg.low_vram) == (False, True)
    assert (cfg.hann_chunk, cfg.hann_stride) == exp
    assert (res.hann_chunk, res.hann_stride, res.low_vram) == (*exp, True)
    chunks = rt._VRAM_PEAK_GB[True][0]
    mp = 1088 * 1920 / 1e6

    def fits(c):
        return (rt.estimate_peak_vram_gb(mp, min(c, 50), True)
                * rt._VRAM_MARGIN_SCALE + rt._VRAM_MARGIN_GB) <= 20.0
    assert fits(exp[0])
    assert not any(fits(c) for c in chunks if c > exp[0])
    assert exp[1] == max(1, exp[0] // 2)


def test_tiny_budget(env):
    assert v2._select_low_vram_window((1088, 1920), 50, 0.1) == (1, 1)
    env.budget = int(0.1 * GIB)
    run(env, InferenceParams(vram_profile="low"))
    assert (env.runs[0].cfg.hann_chunk, env.runs[0].cfg.hann_stride) == (1, 1)


def test_tiny_budget_warns(env):
    env.budget = int(0.1 * GIB)
    res = run(env, InferenceParams(vram_profile="low"))
    assert len(res.warnings) == 1 and "1-frame" in res.warnings[0]


def test_fitting_budget_no_warning(env):
    assert run(env, InferenceParams(vram_profile="low")).warnings == ()


def test_low_requires_probe_info(env):
    with pytest.raises(ValueError):
        env.engine.build_inference_config(InferenceParams(vram_profile="low"),
                                          outputs={"alpha"}, temp_dir=env.tmp)


def test_config_mapping(env):
    p = InferenceParams(despill_strength=0.3, carry_hint=False, carry_hint_quality=0.4,
                        input_is_linear=True, cutout_linear=True)
    run(env, p, outputs=["alpha", "cutout"], frame_dir_fps=30)
    c = env.runs[0].cfg
    assert (c.despill_strength, c.carry_hint, c.carry_hint_quality) == (0.3, False, 0.4)
    assert c.frame_dir_linear and c.cutout_linear and c.frame_dir_fps == 30.0
    assert (c.output_format, c.exr_dtype) == ("exr", "float")
    assert c.outputs == {"alpha", "cutout"} and c.temp_dir == env.tmp / "t"
    assert (c.model_scale, c.num_frames, c.start_mode, c.stage2_batch) == (1.0, -1, "begin", 1)


def test_invalid_outputs(env):
    for bad in ([], ["bogus"], ["alpha", "x"]):
        with pytest.raises(ValueError):
            env.engine.build_inference_config(InferenceParams(), outputs=bad, temp_dir=env.tmp)


def test_fresh_cfg_each_call(env):
    run(env), run(env)
    assert env.runs[0].cfg is not env.runs[1].cfg


def test_reset_before_each_run(env):
    run(env), run(env)
    assert len(env.resets) == 2
    for m, cfg in env.resets:
        assert cfg.low_vram_mlp_chunk_tokens == 0 and cfg.low_vram_enc_frame_chunk == 0
        assert cfg.low_vram_swin_window_batch == 0
        assert cfg.low_vram_full_attn_query_chunk_tokens == 0


def test_product_dirs(env):
    res = run(env, InferenceParams(despill_strength=0.5),
              outputs=["alpha", "fg", "checker", "cutout"])
    out = res.output_dir
    assert res.clip_name == "shot"
    assert res.product_dirs == {
        "alpha": f"{out}/shot_alpha_exr",
        "fg": f"{out}/shot_fg_exr",
        "checker": f"{out}/shot_checker_comp_despill050_exr",
        "cutout": f"{out}/shot_cutout_exr",
    }
    assert all(os.path.isdir(d) for d in res.product_dirs.values())
    assert res.settings_path is None
    assert set(run(env, outputs=["fg"]).product_dirs) == {"fg"}


def test_progress_cancel_forwarded_and_propagates(env):
    def prog(*a):
        pass

    def cancel():
        pass
    run(env, on_progress=prog, check_cancel=cancel)
    assert env.runs[0].kw["on_progress"] is prog and env.runs[0].kw["check_cancel"] is cancel
    env.run_exc = Cancelled("stop")
    with pytest.raises(Cancelled):
        run(env)


def test_auto_retries_with_the_next_smaller_window_on_out_of_memory(env, monkeypatch):
    seen = []
    real = rt.run_inference

    def oom_once(m, inp, out, cfg, wcfg, **kw):
        if cfg.auto_vram:  # what apply_auto_vram picks inside the real run, then the GPU runs out
            cfg.hann_chunk, cfg.hann_stride, cfg.low_vram = 24, 12, False
            seen.append((cfg.hann_chunk, cfg.low_vram, True))
            raise torch.OutOfMemoryError("CUDA out of memory. Tried to allocate 1.65 GiB")
        seen.append((cfg.hann_chunk, cfg.low_vram, False))
        return real(m, inp, out, cfg, wcfg, **kw)

    monkeypatch.setattr(rt, "run_inference", oom_once)
    status = []
    res = run(env, on_status=status.append)
    assert seen == [(24, False, True), (24, True, False)], seen
    assert (res.hann_chunk, res.low_vram) == (24, True)
    assert any("retrying with 24 frames (low VRAM)" in w for w in res.warnings), res.warnings
    assert status and "Out of GPU memory" in status[-1]


def test_a_fixed_window_is_never_changed_on_out_of_memory(env, monkeypatch):
    env.run_exc = torch.OutOfMemoryError("CUDA out of memory")
    with pytest.raises(torch.OutOfMemoryError):
        run(env, InferenceParams(temporal_window="8/4"))
    assert len(env.runs) == 1, "no retry with another window"
