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
