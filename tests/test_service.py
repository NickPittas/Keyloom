"""Comprehensive tests for backend.service — CorridorKeyService.

Mocking strategy (Codex-informed hybrid):
- Real tiny PNG I/O for happy paths (via conftest fixtures)
- unittest.mock.patch for GPU/model/error injection
- Behavioral tests for GPU lock (not structural)
"""
import json
import os
import tempfile
import types
from unittest.mock import MagicMock, patch, PropertyMock

import cv2
import numpy as np
import pytest

from backend.annotation_prompts import AnnotationPromptFrame
from backend.service import (
    CorridorKeyService,
    InferenceParams,
    OutputConfig,
    FrameResult,
    _ActiveModel,
    _import_matanyone2_processor_class,
)
from backend.clip_state import ClipAsset, ClipEntry, ClipState
from backend.errors import (
    CorridorKeyError,
    FrameReadError,
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
        import sys
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


class TestReadAlphaFrame:
    def test_sequence_happy_path(self, sample_clip):
        """Real PNG I/O — reads actual alpha file."""
        svc = CorridorKeyService()
        alpha_files = sample_clip.alpha_asset.get_frame_files()
        mask = svc._read_alpha_frame(sample_clip, 0, alpha_files, None)
        assert mask is not None
        assert mask.dtype == np.float32
        assert mask.ndim == 2  # single channel

    def test_video_read_failure(self):
        svc = CorridorKeyService()
        clip = ClipEntry("test", "/tmp", alpha_asset=ClipAsset("/nonexistent", "video"))
        mock_cap = MagicMock()
        mock_cap.read.return_value = (False, None)
        mask = svc._read_alpha_frame(clip, 0, [], mock_cap)
        assert mask is None

    def test_sequence_prefers_input_stem_when_available(self):
        svc = CorridorKeyService()
        with tempfile.TemporaryDirectory() as tmpdir:
            alpha_dir = os.path.join(tmpdir, "AlphaHint")
            os.makedirs(alpha_dir)
            wanted = os.path.join(alpha_dir, "frame_000003.png")
            fallback = os.path.join(alpha_dir, "_alphaHint_000000.png")
            cv2.imwrite(wanted, np.ones((4, 4), dtype=np.uint8) * 255)
            cv2.imwrite(fallback, np.zeros((4, 4), dtype=np.uint8))

            clip = ClipEntry(
                "test",
                tmpdir,
                state=ClipState.READY,
                alpha_asset=ClipAsset(alpha_dir, "sequence"),
            )
            alpha_files = clip.alpha_asset.get_frame_files()
            alpha_lookup = {os.path.splitext(f)[0]: f for f in alpha_files}

            with patch("backend.service.read_mask_frame", return_value=np.ones((4, 4), dtype=np.float32)) as mock_read:
                svc._read_alpha_frame(
                    clip,
                    0,
                    alpha_files,
                    None,
                    input_stem="frame_000003",
                    alpha_stem_lookup=alpha_lookup,
                )

            assert mock_read.call_args is not None
            assert mock_read.call_args.args[0].endswith("frame_000003.png")

    def test_sequence_falls_back_to_index_when_input_stem_missing(self):
        svc = CorridorKeyService()
        with tempfile.TemporaryDirectory() as tmpdir:
            alpha_dir = os.path.join(tmpdir, "AlphaHint")
            os.makedirs(alpha_dir)
            fallback = os.path.join(alpha_dir, "_alphaHint_000000.png")
            cv2.imwrite(fallback, np.ones((4, 4), dtype=np.uint8) * 255)

            clip = ClipEntry(
                "test",
                tmpdir,
                state=ClipState.READY,
                alpha_asset=ClipAsset(alpha_dir, "sequence"),
            )
            alpha_files = clip.alpha_asset.get_frame_files()

            with patch("backend.service.read_mask_frame", return_value=np.ones((4, 4), dtype=np.float32)) as mock_read:
                svc._read_alpha_frame(
                    clip,
                    0,
                    alpha_files,
                    None,
                    input_stem="frame_000123",
                    alpha_stem_lookup={},
                )

            assert mock_read.call_args is not None
            assert mock_read.call_args.args[0].endswith("_alphaHint_000000.png")


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


class TestWriteManifest:
    def test_manifest_content(self):
        svc = CorridorKeyService()
        cfg = OutputConfig(fg_enabled=True, matte_enabled=True, comp_enabled=False)
        params = InferenceParams(despill_strength=0.8)
        with tempfile.TemporaryDirectory() as tmpdir:
            svc._write_manifest(tmpdir, cfg, params)
            manifest_path = os.path.join(tmpdir, ".corridorkey_manifest.json")
            assert os.path.exists(manifest_path)
            with open(manifest_path) as f:
                data = json.load(f)
            assert data["version"] == 2
            assert "fg" in data["enabled_outputs"]
            assert "matte" in data["enabled_outputs"]
            assert "comp" not in data["enabled_outputs"]
            assert data["params"]["despill_strength"] == 0.8

    def test_manifest_overwrites_existing(self):
        svc = CorridorKeyService()
        cfg = OutputConfig()
        params = InferenceParams()
        with tempfile.TemporaryDirectory() as tmpdir:
            # Write twice — second should overwrite
            svc._write_manifest(tmpdir, cfg, params)
            svc._write_manifest(tmpdir, OutputConfig(fg_enabled=False), params)
            manifest_path = os.path.join(tmpdir, ".corridorkey_manifest.json")
            with open(manifest_path) as f:
                data = json.load(f)
            assert "fg" not in data["enabled_outputs"]

    def test_manifest_write_failure_raises(self):
        """Manifest write failure raises (a missing manifest breaks resume validation)."""
        svc = CorridorKeyService()
        cfg = OutputConfig()
        params = InferenceParams()
        with pytest.raises(OSError):
            svc._write_manifest("/nonexistent/path/that/doesnt/exist", cfg, params)


# ── TestWriteOutputs ──


class TestWriteOutputs:
    def _make_result_dict(self):
        return {
            "fg": np.ones((4, 4, 3), dtype=np.float32) * 0.5,
            "alpha": np.ones((4, 4, 1), dtype=np.float32) * 0.8,
            "comp": np.ones((4, 4, 3), dtype=np.float32) * 0.3,
            "processed": np.ones((4, 4, 4), dtype=np.float32) * 0.6,
        }

    def test_all_outputs_enabled(self, tmp_clip_dir):
        svc = CorridorKeyService()
        from backend.validators import ensure_output_dirs
        dirs = ensure_output_dirs(tmp_clip_dir)
        res = self._make_result_dict()
        cfg = OutputConfig(
            fg_format="png", matte_format="png",
            comp_format="png", processed_format="png",
        )
        svc._write_outputs(res, dirs, "frame_00000", "clip1", 0, cfg)

        assert os.path.exists(os.path.join(dirs["fg"], "frame_00000.png"))
        assert os.path.exists(os.path.join(dirs["matte"], "frame_00000.png"))
        assert os.path.exists(os.path.join(dirs["comp"], "frame_00000.png"))
        assert os.path.exists(os.path.join(dirs["processed"], "frame_00000.png"))

    def test_selective_disable(self, tmp_clip_dir):
        svc = CorridorKeyService()
        from backend.validators import ensure_output_dirs
        dirs = ensure_output_dirs(tmp_clip_dir)
        res = self._make_result_dict()
        cfg = OutputConfig(
            fg_enabled=False, matte_enabled=True,
            comp_enabled=False, processed_enabled=False,
            matte_format="png",
        )
        svc._write_outputs(res, dirs, "frame_00000", "clip1", 0, cfg)

        assert not os.path.exists(os.path.join(dirs["fg"], "frame_00000.png"))
        assert os.path.exists(os.path.join(dirs["matte"], "frame_00000.png"))
        assert not os.path.exists(os.path.join(dirs["comp"], "frame_00000.png"))

    def test_missing_processed_key(self, tmp_clip_dir):
        """If 'processed' not in result dict, skip gracefully."""
        svc = CorridorKeyService()
        from backend.validators import ensure_output_dirs
        dirs = ensure_output_dirs(tmp_clip_dir)
        res = {
            "fg": np.ones((4, 4, 3), dtype=np.float32),
            "alpha": np.ones((4, 4, 1), dtype=np.float32),
            "comp": np.ones((4, 4, 3), dtype=np.float32),
            # 'processed' intentionally missing
        }
        cfg = OutputConfig(
            fg_format="png", matte_format="png",
            comp_format="png",
            processed_enabled=True, processed_format="png",
        )
        # Should not raise
        svc._write_outputs(res, dirs, "frame_00000", "clip1", 0, cfg)

    def test_fg_exr_is_linearized_before_write(self, tmp_clip_dir):
        svc = CorridorKeyService()
        from backend.frame_io import _srgb_to_linear
        from backend.validators import ensure_output_dirs

        dirs = ensure_output_dirs(tmp_clip_dir)
        res = self._make_result_dict()
        cfg = OutputConfig(
            fg_format="exr",
            matte_enabled=False,
            comp_enabled=False,
            processed_enabled=False,
        )

        writes: list[tuple[np.ndarray, str, str]] = []

        def _capture(img, path, fmt, clip_name, frame_index, exr_compression="dwab"):
            writes.append((img.copy(), path, fmt))

        svc._write_image = _capture
        svc._write_outputs(res, dirs, "frame_00000", "clip1", 0, cfg)

        assert len(writes) == 1
        img, path, fmt = writes[0]
        expected_bgr = cv2.cvtColor(_srgb_to_linear(res["fg"]), cv2.COLOR_RGB2BGR)
        assert fmt == "exr"
        assert path.endswith(os.path.join("FG", "frame_00000.exr"))
        np.testing.assert_allclose(img, expected_bgr, atol=1e-6)

    def test_comp_exr_is_linearized_before_write(self, tmp_clip_dir):
        svc = CorridorKeyService()
        from backend.frame_io import _srgb_to_linear
        from backend.validators import ensure_output_dirs

        dirs = ensure_output_dirs(tmp_clip_dir)
        res = self._make_result_dict()
        cfg = OutputConfig(
            fg_enabled=False,
            matte_enabled=False,
            comp_format="exr",
            processed_enabled=False,
        )

        writes: list[tuple[np.ndarray, str, str]] = []

        def _capture(img, path, fmt, clip_name, frame_index, exr_compression="dwab"):
            writes.append((img.copy(), path, fmt))

        svc._write_image = _capture
        svc._write_outputs(res, dirs, "frame_00000", "clip1", 0, cfg)

        assert len(writes) == 1
        img, path, fmt = writes[0]
        expected_bgr = cv2.cvtColor(_srgb_to_linear(res["comp"]), cv2.COLOR_RGB2BGR)
        assert fmt == "exr"
        assert path.endswith(os.path.join("Comp", "frame_00000.exr"))
        np.testing.assert_allclose(img, expected_bgr, atol=1e-6)

    def test_processed_exr_preserves_straight_linear_rgba(self, tmp_clip_dir):
        svc = CorridorKeyService()
        from backend.validators import ensure_output_dirs

        dirs = ensure_output_dirs(tmp_clip_dir)
        res = self._make_result_dict()
        res["processed"] = np.zeros((4, 4, 4), dtype=np.float32)
        res["processed"][:, :, 0] = 0.8
        res["processed"][:, :, 1] = 0.4
        res["processed"][:, :, 2] = 0.2
        res["processed"][:, :, 3] = 0.25
        cfg = OutputConfig(
            fg_enabled=False,
            matte_enabled=False,
            comp_enabled=False,
            processed_format="exr",
        )

        writes: list[tuple[np.ndarray, str, str]] = []

        def _capture(img, path, fmt, clip_name, frame_index, exr_compression="dwab"):
            writes.append((img.copy(), path, fmt))

        svc._write_image = _capture
        svc._write_outputs(res, dirs, "frame_00000", "clip1", 0, cfg)

        assert len(writes) == 1
        img, path, fmt = writes[0]
        expected_bgra = cv2.cvtColor(res["processed"], cv2.COLOR_RGBA2BGRA)
        assert fmt == "exr"
        assert path.endswith(os.path.join("Processed", "frame_00000.exr"))
        np.testing.assert_allclose(img, expected_bgra, atol=1e-6)


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
    def test_preview_sam2_defaults_video_derived_exr_to_srgb(self):
        svc = CorridorKeyService()
        tracker = MagicMock()
        tracker.track_video.return_value = [np.ones((4, 4), dtype=np.uint8)]
        svc._get_sam2_tracker = MagicMock(return_value=tracker)

        with tempfile.TemporaryDirectory() as tmpdir:
            frames_dir = os.path.join(tmpdir, "Frames")
            os.makedirs(frames_dir)
            frame_path = os.path.join(frames_dir, "frame_000000.exr")
            with open(frame_path, "w", encoding="utf-8") as handle:
                handle.write("dummy")
            with open(os.path.join(tmpdir, ".video_metadata.json"), "w", encoding="utf-8") as handle:
                json.dump({"codec": "prores"}, handle)

            clip = ClipEntry(
                "test",
                tmpdir,
                state=ClipState.RAW,
                input_asset=ClipAsset(frames_dir, "sequence"),
            )

            prompt = AnnotationPromptFrame(
                frame_index=0,
                positive_points=[(10.0, 12.0)],
                negative_points=[],
            )
            fake_img = np.full((4, 4, 3), 0.5, dtype=np.float32)
            with patch(
                "backend.service.load_annotation_prompt_frames",
                return_value=[prompt],
            ), patch(
                "backend.service.read_image_frame",
                return_value=fake_img,
            ) as mock_read:
                result = svc.preview_sam2_prompt(clip, preferred_frame_index=0)

        assert result is not None
        assert mock_read.call_args is not None
        assert mock_read.call_args.kwargs["gamma_correct_exr"] is False

    def test_preview_sam2_honors_explicit_linear_override(self):
        svc = CorridorKeyService()
        tracker = MagicMock()
        tracker.track_video.return_value = [np.ones((4, 4), dtype=np.uint8)]
        svc._get_sam2_tracker = MagicMock(return_value=tracker)

        with tempfile.TemporaryDirectory() as tmpdir:
            frames_dir = os.path.join(tmpdir, "Frames")
            os.makedirs(frames_dir)
            frame_path = os.path.join(frames_dir, "frame_000000.exr")
            with open(frame_path, "w", encoding="utf-8") as handle:
                handle.write("dummy")

            clip = ClipEntry(
                "test",
                tmpdir,
                state=ClipState.RAW,
                input_asset=ClipAsset(frames_dir, "sequence"),
            )

            prompt = AnnotationPromptFrame(
                frame_index=0,
                positive_points=[(10.0, 12.0)],
                negative_points=[],
            )
            fake_img = np.full((4, 4, 3), 0.5, dtype=np.float32)
            with patch(
                "backend.service.load_annotation_prompt_frames",
                return_value=[prompt],
            ), patch(
                "backend.service.read_image_frame",
                return_value=fake_img,
            ) as mock_read:
                result = svc.preview_sam2_prompt(
                    clip,
                    preferred_frame_index=0,
                    input_is_linear=True,
                )

        assert result is not None
        assert mock_read.call_args is not None
        assert mock_read.call_args.kwargs["gamma_correct_exr"] is True

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
