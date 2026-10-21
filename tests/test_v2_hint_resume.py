"""Hint-mode service handoff, manifest provenance and resume compatibility (fake engine)."""
import json
import os
import types

import pytest

from backend.clip_state import ClipState
from backend.errors import CorridorKeyError, JobCancelledError
from backend.service import InferenceParams, OutputConfig
from tests.test_v2_service_inference import (  # noqa: F401  (fixtures)
    EXR_CFG, FakeEngine, _manifest, clip, svc,
)
from ui.workers.job_helpers import create_inference_job, create_job_snapshot

CFG = lambda: OutputConfig(**EXR_CFG)  # noqa: E731


def _run(svc, clip, params, **kw):
    return svc.run_inference(clip, params, output_config=CFG(), **kw)


def _call():
    return FakeEngine.instances[0].calls[-1]


def test_none_and_full_mapping(svc, clip):
    clip.hint_keyframes = (1,)  # unrelated mark must be ignored
    _run(svc, clip, InferenceParams(hint_mode="none"))
    c = _call()
    assert c["hint"] is None and c["initial_hint_path"] is None
    assert c["hint_frame_indices"] is None and c["params"].hint_mode == "none"
    assert "hint_keyframes" not in _manifest(clip)
    clip.state = ClipState.RAW
    _run(svc, clip, InferenceParams(hint_mode="full"))
    c = _call()
    assert c["hint"] and c["initial_hint_path"] is None and c["hint_frame_indices"] is None
    assert "hint_keyframes" not in _manifest(clip)


def test_single_mark_mapping_and_manifest(svc, clip):
    clip.hint_keyframes = (2,)
    _run(svc, clip, InferenceParams(hint_mode="keyframes", temporal_window="8/4"))
    c = _call()
    assert c["hint"] is None and c["hint_frame_indices"] is None
    assert c["initial_hint_index"] == 2 and c["initial_hint_path"].endswith("000002.png")
    m = _manifest(clip)
    assert m["hint_keyframes"] == [2]
    assert m["params"]["hint_mode"] == "keyframes" and m["params"]["temporal_window"] == "8/4"


def test_multiple_marks_mapping_and_manifest(svc, clip):
    clip.hint_keyframes = (0, 2)
    _run(svc, clip, InferenceParams(hint_mode="keyframes"))
    c = _call()
    assert c["hint_frame_indices"] == (0, 2) and c["initial_hint_path"] is None
    assert sorted(os.listdir(c["hint"])) if os.path.isdir(c["hint"]) else True
    assert _manifest(clip)["hint_keyframes"] == [0, 2]


def test_keyframes_errors_and_no_complete(svc, clip):
    with pytest.raises(CorridorKeyError, match="no keyframe marks"):
        _run(svc, clip, InferenceParams(hint_mode="keyframes"))
    clip.hint_keyframes = (1,)
    clip.hint_keyframes_error = "bad sidecar"
    with pytest.raises(CorridorKeyError, match="bad sidecar"):
        _run(svc, clip, InferenceParams(hint_mode="keyframes"))
    clip.hint_keyframes_error = None
    clip.hint_keyframes = (1, 7)  # 7 out of range / missing hint
    with pytest.raises(CorridorKeyError):
        _run(svc, clip, InferenceParams(hint_mode="keyframes"))
    assert clip.state != ClipState.COMPLETE
    assert FakeEngine.instances == [] or not FakeEngine.instances[0].calls


def test_cancel_not_complete(svc, clip):
    clip.hint_keyframes = (1,)
    job = types.SimpleNamespace(is_cancelled=True)
    with pytest.raises(JobCancelledError):
        _run(svc, clip, InferenceParams(hint_mode="keyframes"), job=job)
    assert clip.state != ClipState.COMPLETE


def test_warnings_forwarded(svc, clip, monkeypatch):
    clip.hint_keyframes = (1,)
    orig = FakeEngine.run_clip

    def run(self, *a, **k):
        r = orig(self, *a, **k)
        r.warnings = ["est. over budget"]
        return r

    monkeypatch.setattr(FakeEngine, "run_clip", run)
    got = []
    _run(svc, clip, InferenceParams(hint_mode="keyframes"), on_warning=got.append)
    assert got == ["est. over budget"]


def _resume(svc, clip, params, expect_skip):
    warns = []
    res = _run(svc, clip, params, skip_stems={"frame_00000", "frame_00001", "frame_00002"},
               on_warning=warns.append)
    skipped = [r for r in res if r.warning == "resumed (skipped)"]
    assert bool(skipped) is expect_skip
    clip.state = ClipState.RAW


def test_resume_invalidation(svc, clip):
    clip.hint_keyframes = (1, 2)
    kf = lambda **k: InferenceParams(hint_mode="keyframes", **k)  # noqa: E731
    _run(svc, clip, kf())
    clip.state = ClipState.RAW
    _resume(svc, clip, kf(), True)
    _resume(svc, clip, kf(temporal_window="24/12"), False)  # window changed (rewrites manifest)
    _resume(svc, clip, kf(temporal_window="24/12"), True)
    clip.hint_keyframes = (0, 2)
    _resume(svc, clip, kf(temporal_window="24/12"), False)
    _resume(svc, clip, kf(temporal_window="24/12"), True)
    _resume(svc, clip, InferenceParams(hint_mode="full"), False)  # mode changed
    _resume(svc, clip, InferenceParams(hint_mode="full"), True)


def test_none_full_ignore_marks_on_resume(svc, clip):
    for mode in ("none", "full"):
        clip.hint_keyframes = ()
        _run(svc, clip, InferenceParams(hint_mode=mode))
        clip.state = ClipState.RAW
        clip.hint_keyframes = (1, 2)
        _resume(svc, clip, InferenceParams(hint_mode=mode), True)


def test_old_manifest_defaults(svc, clip):
    _run(svc, clip, InferenceParams(hint_mode="full"))
    path = os.path.join(clip.output_dir, ".corridorkey_manifest.json")
    m = _manifest(clip)
    del m["params"]["hint_mode"], m["params"]["temporal_window"]
    json.dump(m, open(path, "w"))
    clip.state = ClipState.RAW
    _resume(svc, clip, InferenceParams(hint_mode="full"), True)  # provably equivalent
    m = _manifest(clip)
    del m["params"]["hint_mode"], m["params"]["temporal_window"]
    json.dump(m, open(path, "w"))
    _resume(svc, clip, InferenceParams(hint_mode="none"), False)
    m = _manifest(clip)
    del m["params"]["hint_mode"], m["params"]["temporal_window"]
    json.dump(m, open(path, "w"))
    clip.hint_keyframes = (1,)
    _resume(svc, clip, InferenceParams(hint_mode="keyframes"), False)
    clip.hint_keyframes = (1,)
    _resume(svc, clip, InferenceParams(hint_mode="full", temporal_window="8/4"), False)


def test_in_out_range_sends_absolute_k(svc, clip):
    clip.hint_keyframes = (2,)
    res = _run(svc, clip, InferenceParams(hint_mode="keyframes"), frame_range=(0, 0))
    assert [r.frame_index for r in res] == [0]
    c = _call()
    assert c["initial_hint_index"] == 2
    assert _manifest(clip)["hint_keyframes"] == [2] and _manifest(clip)["frame_range"] == [0, 0]


def test_job_snapshot_is_frozen(clip):
    clip.hint_keyframes = (1, 2)
    params, out = InferenceParams(hint_mode="keyframes"), CFG()
    job = create_inference_job(clip, params, out)
    clip.hint_keyframes = (0,)
    params.hint_mode, params.temporal_window = "none", "8/4"
    out.fg_format = "png"
    snap = job.params["_clip_snapshot"]
    assert snap.hint_keyframes == (1, 2)
    assert job.params["_inference_params"].hint_mode == "keyframes"
    assert job.params["_inference_params"].temporal_window == "auto"
    assert job.params["_output_config"].fg_format == "exr"
    assert create_job_snapshot(clip, params).params["_inference_params"] is not params
