"""Behavioral tests for GPU lock serialization of whole-clip CorridorKey 2.0 runs."""
import threading
import time
from unittest.mock import MagicMock

from backend.service import CorridorKeyService, InferenceParams, OutputConfig, _ActiveModel
from backend.clip_state import ClipAsset, ClipEntry, ClipState


class TestGPULockSerialization:
    """Two whole-clip runs share one engine and must not overlap on the GPU."""

    def test_concurrent_runs_serialized_single_engine(self, sample_clip, tmp_path, monkeypatch):
        from backend.service.v2_inputs import V2PreparedInput

        monkeypatch.setenv("CORRIDORKEY_V2_SCRATCH_DIR", str(tmp_path))
        svc = CorridorKeyService()
        log = []
        engine = MagicMock()

        def slow_run(*args, **kwargs):
            log.append(("start", time.monotonic()))
            time.sleep(0.05)
            log.append(("end", time.monotonic()))
            raise RuntimeError("stop after run_clip")

        engine.run_clip.side_effect = slow_run
        svc._get_v2_engine = MagicMock(return_value=engine)
        prepared = V2PreparedInput("x", ["a"], 1, 24.0, False, None)
        monkeypatch.setattr("backend.service.inference.prepare_v2_inputs",
                            lambda *a, **k: prepared)
        errors = []

        def worker():
            try:
                svc.run_inference(sample_clip, InferenceParams(), output_config=OutputConfig())
            except RuntimeError as e:
                errors.append(e)

        threads = [threading.Thread(target=worker) for _ in range(2)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=5)

        assert len(errors) == 2
        starts = sorted(t for k, t in log if k == "start")
        ends = sorted(t for k, t in log if k == "end")
        assert len(starts) == 2 and len(ends) == 2
        assert starts[1] >= ends[0], "run_clip calls overlapped"


class TestGPULockModelSwitch:
    """Concurrent inference + GVM must switch models without corruption."""

    def test_model_residency_consistent_after_concurrent_access(self):
        svc = CorridorKeyService()
        svc._v2_engine = MagicMock()
        svc._gvm_processor = MagicMock()
        svc._active_model = _ActiveModel.INFERENCE

        errors = []

        def switch_to_gvm():
            try:
                with svc._gpu_lock:
                    svc._ensure_model(_ActiveModel.GVM)
            except Exception as e:
                errors.append(e)

        def switch_to_inference():
            try:
                with svc._gpu_lock:
                    svc._ensure_model(_ActiveModel.INFERENCE)
            except Exception as e:
                errors.append(e)

        threads = []
        for _ in range(5):
            threads.append(threading.Thread(target=switch_to_gvm))
            threads.append(threading.Thread(target=switch_to_inference))

        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=5)

        assert not errors, f"Errors during concurrent model switch: {errors}"
        # Model must be in a valid state (one of the enum values)
        assert svc._active_model in _ActiveModel


# ---------------------------------------------------------------------------
# AlphaHint generators must hold _gpu_lock across GPU compute
# ---------------------------------------------------------------------------
import json
import os
from unittest.mock import patch

import cv2
import numpy as np
import pytest

from backend.errors import JobCancelledError
from backend.job_queue import GPUJob, JobType


class _BlockingCompute:
    """Fake model whose compute blocks on an Event and records lock state."""

    def __init__(self, svc, on_enter=None):
        self.svc = svc
        self.on_enter = on_enter
        self.started = threading.Event()
        self.release = threading.Event()
        self.locked_during_compute = None

    def compute(self, *args, **kwargs):
        self.locked_during_compute = self.svc._gpu_lock.locked()
        self.started.set()
        assert self.release.wait(5), "test never released compute"
        if self.on_enter:
            return self.on_enter(*args, **kwargs)
        return None


def _make_clip(root, with_mask=False, annotations=False):
    in_dir = os.path.join(root, "Input")
    os.makedirs(in_dir)
    cv2.imwrite(os.path.join(in_dir, "frame_00000.png"), np.full((4, 4, 3), 64, np.uint8))
    mask_asset = None
    if with_mask:
        m_dir = os.path.join(root, "VideoMamaMaskHint")
        os.makedirs(m_dir)
        cv2.imwrite(os.path.join(m_dir, "frame_00000.png"), np.full((4, 4), 255, np.uint8))
        with open(os.path.join(root, ".corridorkey_mask_manifest.json"), "w", encoding="utf-8") as h:
            json.dump({"source": "sam2", "frame_stems": ["frame_00000"]}, h)
        mask_asset = ClipAsset(m_dir, "sequence")
    if annotations:
        with open(os.path.join(root, "annotations.json"), "w", encoding="utf-8") as h:
            json.dump({"0": [{"points": [[1, 1], [2, 2], [3, 3]], "brush_type": "fg", "radius": 15.0}]}, h)
    return ClipEntry(
        "c", root, state=ClipState.MASKED if with_mask else ClipState.RAW,
        input_asset=ClipAsset(in_dir, "sequence"), mask_asset=mask_asset,
    )


def _setup_case(kind, svc, root, fake):
    """Install the fake as the resident model; return (callable, ctx manager)."""
    import contextlib

    svc._device = "cuda"
    ctx = contextlib.nullcontext()
    if kind == "gvm":
        svc._active_model = _ActiveModel.GVM
        svc._gvm_processor = MagicMock(process_sequence=fake.compute)
        clip = _make_clip(root)
        return (lambda **kw: svc.run_gvm(clip, **kw)), ctx
    if kind == "birefnet":
        svc._active_model = _ActiveModel.BIREFNET
        svc._birefnet_processor = MagicMock(_usage="Matting", process_frames=lambda **kw: fake.compute() or 1)
        clip = _make_clip(root)
        return (lambda **kw: svc.run_birefnet(clip, **kw)), ctx
    if kind == "matanyone2":
        svc._active_model = _ActiveModel.MATANYONE2
        svc._matanyone2_processor = MagicMock(process_frames=lambda **kw: fake.compute() or 1)
        clip = _make_clip(root, with_mask=True)
        return (lambda **kw: svc.run_matanyone2(clip, **kw)), ctx
    if kind == "videomama":
        svc._active_model = _ActiveModel.VIDEOMAMA
        svc._videomama_pipeline = MagicMock()

        def fake_run_inference(pipeline, frames, masks, chunk_size=16, on_status=None):
            fake.compute()
            yield [np.full((4, 4, 3), 128, np.uint8)]

        mod = MagicMock(run_inference=fake_run_inference)
        ctx = patch.dict("sys.modules", {
            "VideoMaMaInferenceModule": MagicMock(),
            "VideoMaMaInferenceModule.inference": mod,
        })
        clip = _make_clip(root, with_mask=True)
        return (lambda **kw: svc.run_videomama(clip, **kw)), ctx
    if kind in ("sam2_track", "sam2_preview"):
        svc._active_model = _ActiveModel.SAM2

        def track_video(frames, prompts, **kw):
            fake.compute()
            return [np.full(f.shape[:2], 255, np.uint8) for f in frames]

        svc._sam2_tracker = MagicMock(track_video=track_video, model_id="m")
        clip = _make_clip(root, annotations=True)
        if kind == "sam2_track":
            return (lambda **kw: svc.run_sam2_track(clip, **kw)), ctx
        return (lambda **kw: svc.preview_sam2_prompt(clip, **kw)), ctx
    raise AssertionError(kind)


_GENERATORS = ["gvm", "birefnet", "matanyone2", "videomama", "sam2_track", "sam2_preview"]


class TestGeneratorsHoldGPULock:
    """Real run_* methods (only the model object is faked) keep _gpu_lock during compute."""

    @pytest.mark.parametrize("kind", _GENERATORS)
    def test_lock_held_during_compute_and_blocks_model_switch(self, kind, tmp_path):
        svc = CorridorKeyService()
        fake = _BlockingCompute(svc)
        call, ctx = _setup_case(kind, svc, str(tmp_path), fake)
        errors = []

        def thread_a():
            try:
                with ctx:
                    call()
            except Exception as e:  # pragma: no cover - surfaced below
                errors.append(e)

        a = threading.Thread(target=thread_a)
        a.start()
        try:
            assert fake.started.wait(5), "generator never reached compute"
            assert fake.locked_during_compute is True

            # Thread B: the inference-style switch must not get the lock.
            got = []

            def thread_b():
                if svc._gpu_lock.acquire(timeout=0.3):
                    try:
                        svc._ensure_model(_ActiveModel.INFERENCE)
                        got.append(True)
                    finally:
                        svc._gpu_lock.release()

            b = threading.Thread(target=thread_b)
            b.start()
            b.join(timeout=2)
            assert not got, "model switch ran while generator was computing"
            assert svc._active_model != _ActiveModel.INFERENCE
        finally:
            fake.release.set()
            a.join(timeout=5)
        assert not errors, errors
        assert not svc._gpu_lock.locked()
        assert svc._gpu_lock.acquire(timeout=1)
        svc._gpu_lock.release()

    def test_gvm_cancel_raises_and_releases_lock(self, tmp_path):
        svc = CorridorKeyService()
        job = GPUJob(JobType.GVM_ALPHA, "c")

        def process_sequence(progress_callback=None, **kw):
            job.request_cancel()
            progress_callback(0, 1)

        svc._device = "cuda"
        svc._active_model = _ActiveModel.GVM
        svc._gvm_processor = MagicMock(process_sequence=process_sequence)
        clip = _make_clip(str(tmp_path))
        with pytest.raises(JobCancelledError):
            svc.run_gvm(clip, job=job)
        assert not svc._gpu_lock.locked()

    def test_sam2_cancel_raises_and_releases_lock(self, tmp_path):
        svc = CorridorKeyService()
        job = GPUJob(JobType.SAM2_TRACK, "c") if hasattr(JobType, "SAM2_TRACK") else GPUJob(JobType.GVM_ALPHA, "c")

        def track_video(frames, prompts, check_cancel=None, **kw):
            job.request_cancel()
            check_cancel()

        svc._active_model = _ActiveModel.SAM2
        svc._sam2_tracker = MagicMock(track_video=track_video, model_id="m")
        clip = _make_clip(str(tmp_path), annotations=True)
        with pytest.raises(JobCancelledError):
            svc.run_sam2_track(clip, job=job)
        assert not svc._gpu_lock.locked()
