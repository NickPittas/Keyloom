"""Hint-mode contract tests for the CorridorKey 2.0 adapter."""
import pytest

import backend.corridorkey_v2 as v2
from backend.service.core import InferenceParams
from tests.test_v2_adapter import GIB, env, run  # noqa: F401


def P(mode):
    return InferenceParams(hint_mode=mode)


def kw(st):
    r = st.runs[-1]
    return r.kw["initial_hint_path"], r.kw["hint_video_path"], r.cfg.initial_hint_index, \
        r.cfg.hint_frame_indices


def test_none_ignores_everything(env):
    run(env, P("none"), hint_video_path="h.mp4", initial_hint_path="r.png",
        initial_hint_index=3, hint_frame_indices=(1, 2))
    assert kw(env) == (None, None, 0, None)


def test_full_ignores_irrelevant(env):
    run(env, P("full"), hint_video_path="h.mp4", initial_hint_path="r.png",
        initial_hint_index=3, hint_frame_indices=(1, 2))
    assert kw(env) == (None, "h.mp4", 0, None)


def test_legacy_bool_full(env):
    run(env, InferenceParams(use_alpha_hint=True), hint_video_path="h.mp4")
    assert kw(env) == (None, "h.mp4", 0, None)


def test_single_keyframe(env):
    run(env, P("keyframes"), initial_hint_path="r.png", initial_hint_index=7)
    assert kw(env) == ("r.png", None, 7, None)


def test_single_keyframe_default_index(env):
    run(env, P("keyframes"), initial_hint_path="r.png")
    assert kw(env) == ("r.png", None, 0, None)


def test_multiple_keyframes(env):
    run(env, P("keyframes"), hint_video_path="h.mp4", hint_frame_indices=(4, 9, 30))
    assert kw(env) == (None, "h.mp4", 0, (4, 9, 30))


def test_fresh_config_defaults(env):
    cfg = env.engine.build_inference_config(P("none"), outputs=("alpha",), temp_dir=env.tmp)
    assert cfg.initial_hint_index == 0 and cfg.hint_frame_indices is None


@pytest.mark.parametrize("mode,kwargs", [
    ("full", {}),
    ("keyframes", {}),
    ("keyframes", {"initial_hint_path": "r.png", "hint_video_path": "h.mp4"}),
    ("keyframes", {"initial_hint_path": "r.png", "initial_hint_index": -1}),
    ("keyframes", {"initial_hint_path": "r.png", "initial_hint_index": True}),
    ("keyframes", {"initial_hint_path": "r.png", "initial_hint_index": 1.0}),
    ("keyframes", {"hint_video_path": "h.mp4", "hint_frame_indices": (5,)}),
    ("keyframes", {"hint_video_path": "h.mp4", "hint_frame_indices": ()}),
    ("keyframes", {"hint_video_path": "h.mp4", "hint_frame_indices": (5, 5)}),
    ("keyframes", {"hint_video_path": "h.mp4", "hint_frame_indices": (9, 5)}),
    ("keyframes", {"hint_video_path": "h.mp4", "hint_frame_indices": (-1, 5)}),
    ("keyframes", {"hint_video_path": "h.mp4", "hint_frame_indices": (True, 5)}),
    ("keyframes", {"hint_video_path": "h.mp4", "hint_frame_indices": (1, 5),
                   "initial_hint_path": "r.png"}),
    ("keyframes", {"hint_frame_indices": (1, 5)}),
])
def test_rejections_before_load(env, mode, kwargs):
    with pytest.raises(v2.V2EngineError):
        run(env, P(mode), **kwargs)
    assert env.loads == 0 and not env.runs


def test_mode_switch_reuses_model(env):
    run(env, P("keyframes"), initial_hint_path="r.png", initial_hint_index=2)
    run(env, P("none"))
    run(env, P("full"), hint_video_path="h.mp4")
    run(env, P("keyframes"), hint_video_path="h.mp4", hint_frame_indices=(0, 3))
    assert env.loads == 1 and len({id(r.model) for r in env.runs}) == 1
    assert [kw_[3] for kw_ in map(lambda r: (0, 0, 0, r.cfg.hint_frame_indices), env.runs)] \
        == [None, None, None, (0, 3)]
    assert len(env.resets) == 4
