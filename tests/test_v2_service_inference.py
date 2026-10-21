"""Service-level CorridorKey 2.0 run_inference tests (fake engine, no CUDA)."""
import json
import os
import threading
import types

import pytest

from backend.clip_state import ClipState
from backend.errors import CorridorKeyError, JobCancelledError
from backend.service import CorridorKeyService, InferenceParams, OutputConfig, _ActiveModel
from tests.test_v2_outputs import make_runtime

N = 3
EXR_CFG = dict(fg_format="exr", matte_format="exr", comp_format="exr", processed_format="exr")


class FakeEngine:
    instances = []

    def __init__(self, *a, **k):
        self.loaded = False
        self.load_count = 0
        self.calls = []
        self.on_run = None
        FakeEngine.instances.append(self)

    @property
    def is_loaded(self):
        return self.loaded

    def load(self, on_status=None):
        if not self.loaded:
            self.loaded = True
            self.load_count += 1

    def unload(self):
        self.loaded = False

    def run_clip(self, input_path, output_dir, params, *, outputs, temp_dir, frame_dir_fps,
                 hint_video_path, ffmpeg_bin, on_progress, check_cancel, on_status,
                 initial_hint_path=None, initial_hint_index=0, hint_frame_indices=None):
        self.calls.append(dict(input_path=input_path, hint=hint_video_path, outputs=outputs,
                               temp_dir=temp_dir, output_dir=output_dir, params=params,
                               initial_hint_path=initial_hint_path,
                               initial_hint_index=initial_hint_index,
                               hint_frame_indices=hint_frame_indices))
        os.makedirs(temp_dir, exist_ok=True)
        with open(os.path.join(temp_dir, "memmap.bin"), "wb") as f:
            f.write(b"x")
        if on_progress:
            on_progress("stage1", 1, 2)
        if self.on_run:
            self.on_run(check_cancel)
        if check_cancel:
            check_cancel()
        from pathlib import Path
        dirs = make_runtime(Path(output_dir), n=N)
        return types.SimpleNamespace(
            clip_name="c", output_dir=output_dir, product_dirs=dirs, settings_path=None,
            hann_chunk=4, hann_stride=2, low_vram=False)


@pytest.fixture
def svc(tmp_path, monkeypatch):
    FakeEngine.instances = []
    monkeypatch.setenv("CORRIDORKEY_V2_SCRATCH_DIR", str(tmp_path / "scratch"))
    monkeypatch.setattr("backend.service.model_manager.CorridorKeyV2Engine", FakeEngine,
                        raising=False)
    monkeypatch.setattr("backend.corridorkey_v2.CorridorKeyV2Engine", FakeEngine)
    return CorridorKeyService()


@pytest.fixture
def clip(sample_clip):
    sample_clip.state = ClipState.RAW
    return sample_clip


def _out(clip, sub, ext="exr"):
    d = os.path.join(clip.output_dir, sub)
    return sorted(os.listdir(d)) if os.path.isdir(d) else []


def _manifest(clip):
    with open(os.path.join(clip.output_dir, ".corridorkey_manifest.json")) as f:
        return json.load(f)


def _scratch_empty(tmp_path):
    root = tmp_path / "scratch"
    return not root.exists() or not list(root.iterdir())


def test_happy_path_with_hint(svc, clip, tmp_path):
    prog = []
    res = svc.run_inference(clip, InferenceParams(use_alpha_hint=True),
                            on_progress=lambda *a, **k: prog.append((a, k)),
                            output_config=OutputConfig(**EXR_CFG))
    assert [r.input_stem for r in res] == ["frame_00000", "frame_00001", "frame_00002"]
    assert all(r.success and r.warning is None for r in res)
    for sub in ("FG", "Matte", "Comp", "Processed"):
        assert _out(clip, sub) == [f"frame_{i:05d}.exr" for i in range(N)]
    assert clip.state == ClipState.COMPLETE
    m = _manifest(clip)
    assert m["complete"] is True and m["frame_range"] is None
    # A fresh scan from disk (app restart) must also resolve COMPLETE.
    clip.state = ClipState.RAW
    clip.find_assets()
    assert clip.state == ClipState.COMPLETE
    call = FakeEngine.instances[0].calls[0]
    assert call["hint"] is not None
    assert sorted(call["outputs"]) == ["alpha", "checker", "cutout", "fg"]
    assert any(k.get("phase") == "publish" for _, k in prog)
    assert _scratch_empty(tmp_path)


def test_no_hint_passes_none(svc, clip):
    svc.run_inference(clip, InferenceParams(use_alpha_hint=False),
                      output_config=OutputConfig(**EXR_CFG))
    assert FakeEngine.instances[0].calls[0]["hint"] is None


def test_preflight_errors(svc, clip):
    clip.alpha_asset = None
    with pytest.raises(CorridorKeyError, match="no AlphaHint"):
        svc.run_inference(clip, InferenceParams(use_alpha_hint=True))
    clip.input_asset = None
    with pytest.raises(CorridorKeyError, match="no input"):
        svc.run_inference(clip, InferenceParams())
    assert FakeEngine.instances == []


def test_two_clips_load_engine_once(svc, clip):
    cfg = OutputConfig(**EXR_CFG)
    svc.run_inference(clip, InferenceParams(), output_config=cfg)
    clip.state = ClipState.RAW
    svc.run_inference(clip, InferenceParams(), output_config=cfg)
    assert len(FakeEngine.instances) == 1
    assert FakeEngine.instances[0].load_count == 1
    assert svc.is_engine_loaded()


def test_model_switch_unloads_and_reloads(svc, clip):
    cfg = OutputConfig(**EXR_CFG)
    svc.run_inference(clip, InferenceParams(), output_config=cfg)
    first = FakeEngine.instances[0]
    svc._ensure_model(_ActiveModel.GVM)
    assert not first.loaded and svc._v2_engine is None
    svc.run_inference(clip, InferenceParams(), output_config=cfg)
    assert len(FakeEngine.instances) == 2 and FakeEngine.instances[1].loaded


def test_cancel_during_run(svc, clip, tmp_path):
    job = types.SimpleNamespace(is_cancelled=False)

    def trigger(check_cancel):
        job.is_cancelled = True
        check_cancel()

    original = FakeEngine.run_clip

    def run(self, *a, **k):
        self.on_run = trigger
        return original(self, *a, **k)

    FakeEngine.run_clip = run
    try:
        with pytest.raises(JobCancelledError):
            svc.run_inference(clip, InferenceParams(), job=job,
                              output_config=OutputConfig(**EXR_CFG))
    finally:
        FakeEngine.run_clip = original
    assert clip.state != ClipState.COMPLETE
    assert _scratch_empty(tmp_path)
    assert not os.path.exists(os.path.join(clip.output_dir, ".corridorkey_manifest.json")) \
        or _manifest(clip)["complete"] is False


def test_cancel_during_publish_leaves_incomplete_manifest(svc, clip):
    job = types.SimpleNamespace(is_cancelled=False)

    def on_progress(name, cur, tot, **kw):
        if kw.get("phase") == "publish" and cur == 1:
            job.is_cancelled = True

    with pytest.raises(JobCancelledError):
        svc.run_inference(clip, InferenceParams(), job=job, on_progress=on_progress,
                          output_config=OutputConfig(**EXR_CFG))
    assert _manifest(clip)["complete"] is False
    assert clip.state != ClipState.COMPLETE


def test_frame_range_publishes_subset(svc, clip):
    res = svc.run_inference(clip, InferenceParams(), frame_range=(1, 1),
                            output_config=OutputConfig(**EXR_CFG))
    assert [r.frame_index for r in res] == [1]
    assert _out(clip, "FG") == ["frame_00001.exr"]
    m = _manifest(clip)
    assert m["frame_range"] == [1, 1] and m["complete"] is True
    assert clip.state != ClipState.COMPLETE


def test_resume_compatible_skips(svc, clip):
    cfg = OutputConfig(**EXR_CFG)
    svc.run_inference(clip, InferenceParams(), output_config=cfg)
    clip.state = ClipState.RAW
    warns = []
    res = svc.run_inference(clip, InferenceParams(), on_warning=warns.append,
                            skip_stems={"frame_00000", "frame_00001"}, output_config=cfg)
    assert [r.warning for r in res] == ["resumed (skipped)", "resumed (skipped)", None]
    assert not warns


def test_resume_incompatible_republishes(svc, clip):
    cfg = OutputConfig(**EXR_CFG)
    svc.run_inference(clip, InferenceParams(despill_strength=0.2), output_config=cfg)
    clip.state = ClipState.RAW
    warns = []
    res = svc.run_inference(clip, InferenceParams(despill_strength=0.9), on_warning=warns.append,
                            skip_stems={"frame_00000"}, output_config=cfg)
    assert all(r.warning is None for r in res)
    assert any("different run/settings" in w for w in warns)


def test_scratch_removed_after_error(svc, clip, tmp_path):
    def boom(check_cancel):
        raise RuntimeError("gpu exploded")

    original = FakeEngine.run_clip

    def run(self, *a, **k):
        self.on_run = boom
        return original(self, *a, **k)

    FakeEngine.run_clip = run
    try:
        with pytest.raises(RuntimeError):
            svc.run_inference(clip, InferenceParams(), output_config=OutputConfig(**EXR_CFG))
    finally:
        FakeEngine.run_clip = original
    assert _scratch_empty(tmp_path)
    assert clip.state != ClipState.COMPLETE


def test_gpu_lock_held_during_run_clip(svc, clip):
    entered, release = threading.Event(), threading.Event()
    seen = {}

    def block(check_cancel):
        seen["locked"] = svc._gpu_lock.locked()
        entered.set()
        assert release.wait(5)

    original = FakeEngine.run_clip

    def run(self, *a, **k):
        self.on_run = block
        return original(self, *a, **k)

    FakeEngine.run_clip = run
    try:
        t = threading.Thread(target=lambda: svc.run_inference(
            clip, InferenceParams(), output_config=OutputConfig(**EXR_CFG)))
        t.start()
        assert entered.wait(5)
        assert svc._gpu_lock.locked() and seen["locked"]
        assert not svc._gpu_lock.acquire(blocking=False)
        release.set()
        t.join(5)
    finally:
        release.set()
        FakeEngine.run_clip = original
    assert not svc._gpu_lock.locked()


def test_rerun_on_complete_clip_does_not_warn(svc, clip, caplog):
    cfgo = OutputConfig(**EXR_CFG)
    svc.run_inference(clip, InferenceParams(use_alpha_hint=True), output_config=cfgo)
    assert clip.state == ClipState.COMPLETE
    with caplog.at_level("WARNING"):
        svc.run_inference(clip, InferenceParams(use_alpha_hint=True), output_config=cfgo)
    assert clip.state == ClipState.COMPLETE
    assert "state transition to COMPLETE failed" not in caplog.text
