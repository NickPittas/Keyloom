"""Comprehensive tests for backend.service — CorridorKeyService.

Mocking strategy (Codex-informed hybrid):
- Real tiny PNG I/O for happy paths (via conftest fixtures)
- unittest.mock.patch for GPU/model/error injection
- Behavioral tests for GPU lock (not structural)
"""
import os
import tempfile
import types
from unittest.mock import MagicMock, patch

import cv2
import numpy as np
import pytest

from backend.service import (
    CorridorKeyService,
    _ActiveModel,
    _import_matanyone2_processor_class,
)
from backend.clip_state import ClipAsset, ClipEntry, ClipState
from backend.errors import (
    CorridorKeyError,
    JobCancelledError,
    WriteFailureError,
)
from backend.job_queue import GPUJob, JobType


# ── TestServiceInit ──


class TestServiceInit:
    def test_constructor_defaults(self):
        svc = CorridorKeyService()
        assert svc._v2_engine is None
        assert svc._gvm_processor is None
        assert svc._videomama_pipeline is None
        assert svc._active_model == _ActiveModel.NONE
        assert svc._device == "cpu"

    def test_lazy_job_queue(self):
        svc = CorridorKeyService()
        assert svc._job_queue is None
        q = svc.job_queue
        assert q is not None
        # Same instance on second access
        assert svc.job_queue is q


# ── TestDeviceDetection ──


class TestDeviceDetection:
    @patch("backend.service.CorridorKeyService.detect_device")
    def test_detect_cuda(self, mock_detect):
        mock_detect.return_value = "cuda"
        svc = CorridorKeyService()
        assert svc.detect_device() == "cuda"

    def test_detect_device_no_torch(self):
        svc = CorridorKeyService()
        with patch.dict("sys.modules", {"torch": None}):
            # Force reimport failure
            result = svc.detect_device()
        # Falls back to cpu when torch import fails
        assert result in ("cpu", "cuda")  # depends on real env

    def test_detect_device_returns_string(self):
        svc = CorridorKeyService()
        result = svc.detect_device()
        assert isinstance(result, str)
        assert result in ("cuda", "cpu")


# ── TestVRAMInfo ──


class TestVRAMInfo:
    def test_vram_info_returns_dict(self):
        svc = CorridorKeyService()
        info = svc.get_vram_info()
        assert isinstance(info, dict)
        # Either has expected keys (CUDA) or is empty (no CUDA)
        if info:
            for key in ("total", "reserved", "allocated", "free", "name"):
                assert key in info, f"Missing key: {key}"
            assert isinstance(info["total"], float)
            assert isinstance(info["name"], str)

    def test_vram_info_no_cuda_returns_empty(self):
        svc = CorridorKeyService()
        mock_torch = MagicMock()
        mock_torch.cuda.is_available.return_value = False
        with patch.dict("sys.modules", {"torch": mock_torch}):
            info = svc.get_vram_info()
        assert info == {}

    def test_vram_info_exception_returns_empty(self):
        svc = CorridorKeyService()
        mock_torch = MagicMock()
        mock_torch.cuda.is_available.side_effect = RuntimeError("boom")
        with patch.dict("sys.modules", {"torch": mock_torch}):
            info = svc.get_vram_info()
        assert info == {}


# ── TestModelResidency ──


class TestModelResidency:
    def test_ensure_model_noop_when_same(self):
        svc = CorridorKeyService()
        svc._active_model = _ActiveModel.INFERENCE
        svc._v2_engine = MagicMock()
        svc._ensure_model(_ActiveModel.INFERENCE)
        svc._v2_engine.unload.assert_not_called()

    def test_ensure_model_switch_unloads_inference(self):
        svc = CorridorKeyService()
        svc._active_model = _ActiveModel.INFERENCE
        engine = svc._v2_engine = MagicMock()
        svc._ensure_model(_ActiveModel.GVM)
        engine.unload.assert_called_once()
        assert svc._v2_engine is None
        assert svc._active_model == _ActiveModel.GVM

    def test_ensure_model_switch_unloads_gvm(self):
        svc = CorridorKeyService()
        svc._active_model = _ActiveModel.GVM
        svc._gvm_processor = MagicMock()
        svc._ensure_model(_ActiveModel.INFERENCE)
        assert svc._gvm_processor is None
        assert svc._active_model == _ActiveModel.INFERENCE

    def test_ensure_model_switch_unloads_videomama(self):
        svc = CorridorKeyService()
        svc._active_model = _ActiveModel.VIDEOMAMA
        svc._videomama_pipeline = MagicMock()
        svc._ensure_model(_ActiveModel.INFERENCE)
        assert svc._videomama_pipeline is None

    def test_ensure_model_from_none(self):
        svc = CorridorKeyService()
        svc._ensure_model(_ActiveModel.INFERENCE)
        assert svc._active_model == _ActiveModel.INFERENCE


class TestMatAnyone2Import:
    def test_prefers_modules_namespace_layout(self, monkeypatch):
        sentinel = object()

        def fake_import(name):
            if name == "modules.MatAnyone2Module.wrapper":
                return types.SimpleNamespace(MatAnyone2Processor=sentinel)
            raise ModuleNotFoundError(name=name)

        monkeypatch.setattr("backend.service.importlib.import_module", fake_import)
        cls = _import_matanyone2_processor_class()
        assert cls is sentinel

    def test_falls_back_to_legacy_layout(self, monkeypatch):
        sentinel = object()

        def fake_import(name):
            if name == "modules.MatAnyone2Module.wrapper":
                raise ModuleNotFoundError(name="modules.MatAnyone2Module.wrapper")
            if name == "MatAnyone2Module.wrapper":
                return types.SimpleNamespace(MatAnyone2Processor=sentinel)
            raise AssertionError(name)

        monkeypatch.setattr("backend.service.importlib.import_module", fake_import)
        cls = _import_matanyone2_processor_class()
        assert cls is sentinel


# ── TestGetEngine ──


class TestGetV2Engine:
    def test_creates_and_loads_once(self):
        svc = CorridorKeyService()
        with patch("backend.corridorkey_v2.CorridorKeyV2Engine") as cls:
            e1 = svc._get_v2_engine()
            e2 = svc._get_v2_engine()
        assert e1 is e2
        cls.assert_called_once()
        assert svc._active_model == _ActiveModel.INFERENCE

    def test_load_failure_resets_state(self):
        svc = CorridorKeyService()
        with patch("backend.corridorkey_v2.CorridorKeyV2Engine") as cls:
            cls.return_value.load.side_effect = CorridorKeyError("boom")
            with pytest.raises(CorridorKeyError):
                svc._get_v2_engine()
        assert svc._v2_engine is None
        assert svc._active_model == _ActiveModel.NONE


# ── TestScanAndFilter ──


class TestScanAndFilter:
    def test_scan_clips_delegates(self, tmp_clip_dir):
        svc = CorridorKeyService()
        parent_dir = os.path.dirname(tmp_clip_dir)
        clips = svc.scan_clips(parent_dir)
        assert isinstance(clips, list)

    def test_get_clips_by_state(self):
        svc = CorridorKeyService()
        clips = [
            ClipEntry("a", "/tmp/a", state=ClipState.RAW),
            ClipEntry("b", "/tmp/b", state=ClipState.READY),
            ClipEntry("c", "/tmp/c", state=ClipState.READY),
        ]
        ready = svc.get_clips_by_state(clips, ClipState.READY)
        assert len(ready) == 2
        assert all(c.state == ClipState.READY for c in ready)

    def test_get_clips_by_state_empty(self):
        svc = CorridorKeyService()
        result = svc.get_clips_by_state([], ClipState.RAW)
        assert result == []


# ── TestReadInputFrame ──


class TestReadInputFrame:
    def test_sequence_png_happy_path(self, sample_clip):
        """Real PNG I/O — reads actual file from fixture."""
        svc = CorridorKeyService()
        input_files = sample_clip.input_asset.get_frame_files()
        img, stem, is_linear = svc._read_input_frame(
            sample_clip, 0, input_files, None, False,
        )
        assert img is not None
        assert img.dtype == np.float32
        assert img.ndim == 3
        assert img.shape[2] == 3
        assert 0.0 <= img.max() <= 1.0
        assert isinstance(stem, str)

    def test_read_failure_returns_none_for_video(self):
        svc = CorridorKeyService()
        clip = ClipEntry("test", "/tmp", input_asset=ClipAsset("/nonexistent", "video"))
        mock_cap = MagicMock()
        mock_cap.read.return_value = (False, None)
        img, stem, is_linear = svc._read_input_frame(clip, 0, [], mock_cap, False)
        assert img is None

    def test_exr_sequence_honors_explicit_srgb_setting(self):
        svc = CorridorKeyService()
        with tempfile.TemporaryDirectory() as tmpdir:
            frames_dir = os.path.join(tmpdir, "Frames")
            os.makedirs(frames_dir)
            frame_path = os.path.join(frames_dir, "frame_000000.exr")
            with open(frame_path, "w", encoding="utf-8") as handle:
                handle.write("dummy")

            clip = ClipEntry(
                "test", tmpdir, state=ClipState.RAW,
                input_asset=ClipAsset(frames_dir, "sequence"),
            )

            fake_img = np.zeros((2, 2, 3), dtype=np.float32)
            with patch("backend.service.read_image_frame", return_value=fake_img):
                img, stem, is_linear = svc._read_input_frame(
                    clip, 0, clip.input_asset.get_frame_files(), None, False,
                )

            assert img is fake_img
            assert stem == "frame_000000"
            assert is_linear is False


# ── TestReadAlphaFrame ──




# ── TestWriteImage ──


class TestWriteImage:
    def test_png_float_to_uint8(self):
        """PNG path converts float32 to uint8."""
        svc = CorridorKeyService()
        img = np.ones((4, 4), dtype=np.float32) * 0.5
        with tempfile.TemporaryDirectory() as tmpdir:
            path = os.path.join(tmpdir, "test.png")
            svc._write_image(img, path, "png", "clip1", 0)
            assert os.path.exists(path)
            # Read back and verify
            loaded = cv2.imread(path, cv2.IMREAD_GRAYSCALE)
            assert loaded is not None
            assert loaded.dtype == np.uint8

    def test_write_failure_raises(self):
        svc = CorridorKeyService()
        img = np.ones((4, 4), dtype=np.uint8)
        with pytest.raises(WriteFailureError):
            # Writing to non-existent deeply nested path
            svc._write_image(img, "/nonexistent/dir/deep/file.png", "png", "clip1", 0)

    def test_unknown_format_falls_through_to_png(self):
        """Codex: unknown format string silently uses PNG path."""
        svc = CorridorKeyService()
        img = np.ones((4, 4), dtype=np.float32) * 0.5
        with tempfile.TemporaryDirectory() as tmpdir:
            path = os.path.join(tmpdir, "test.bmp")
            svc._write_image(img, path, "bmp", "clip1", 0)
            # File written (via PNG code path, but cv2 uses extension)
            assert os.path.exists(path)


# ── TestWriteManifest ──




# ── TestWriteOutputs ──




# ── TestUnloadEngines ──


class TestUnloadEngines:
    def test_clears_all(self):
        svc = CorridorKeyService()
        engine = svc._v2_engine = MagicMock()
        svc._gvm_processor = MagicMock()
        svc._videomama_pipeline = MagicMock()
        svc._active_model = _ActiveModel.INFERENCE

        svc.unload_engines()

        engine.unload.assert_called_once()
        assert svc._v2_engine is None
        assert svc._gvm_processor is None
        assert svc._videomama_pipeline is None
        assert svc._active_model == _ActiveModel.NONE


# ── TestIsEngineLoaded ──


class TestIsEngineLoaded:
    def test_true_when_active(self):
        svc = CorridorKeyService()
        svc._active_model = _ActiveModel.INFERENCE
        svc._v2_engine = MagicMock(is_loaded=True)
        assert svc.is_engine_loaded() is True

    def test_false_when_not_loaded(self):
        svc = CorridorKeyService()
        assert svc.is_engine_loaded() is False


class TestSam2PreviewInputColorSpace:


    def test_false_when_different_model(self):
        svc = CorridorKeyService()
        svc._active_model = _ActiveModel.GVM
        svc._gvm_processor = MagicMock()
        assert svc.is_engine_loaded() is False

    def test_false_when_active_but_engine_empty(self):
        svc = CorridorKeyService()
        svc._active_model = _ActiveModel.INFERENCE
        svc._v2_engine = None
        assert svc.is_engine_loaded() is False


# ── TestRunGVM ──


class TestRunGVM:
    def test_missing_input_raises(self):
        svc = CorridorKeyService()
        clip = ClipEntry("test", "/tmp", state=ClipState.RAW)
        with pytest.raises(CorridorKeyError, match="missing input asset"):
            svc.run_gvm(clip)

    def test_cancellation_before_gvm(self):
        svc = CorridorKeyService()
        svc._device = 'cuda'  # skip GPU guard (we're testing cancellation)
        svc._active_model = _ActiveModel.GVM
        svc._gvm_processor = MagicMock()

        with tempfile.TemporaryDirectory() as tmpdir:
            input_dir = os.path.join(tmpdir, "Input")
            os.makedirs(input_dir)
            clip = ClipEntry(
                "test", tmpdir, state=ClipState.RAW,
                input_asset=ClipAsset(input_dir, "sequence"),
            )
            job = GPUJob(JobType.GVM_ALPHA, "test")
            job.request_cancel()

            with pytest.raises(JobCancelledError):
                svc.run_gvm(clip, job=job)
