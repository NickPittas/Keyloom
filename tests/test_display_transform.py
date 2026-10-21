"""Tests for display transform — EXR preview conversion for various channel layouts."""
import os
import tempfile
from unittest.mock import patch
import pytest
import numpy as np
import cv2

# Must set before importing display_transform
os.environ["OPENCV_IO_ENABLE_OPENEXR"] = "1"

pyside6 = pytest.importorskip("PySide6", reason="PySide6 not installed")
from PySide6.QtGui import QImage
from PySide6.QtWidgets import QApplication

from ui.preview.display_transform import (
    decode_frame, clear_cache, _cache_key, _transform_matte,
    _transform_linear_rgb, _transform_processed_rgba, _numpy_to_qimage,
    processed_rgba_to_qimage, decode_video_frame, image_source_size,
)
from ui.preview.frame_index import ViewMode

_APP = QApplication.instance() or QApplication([])


class TestNumpyToQImage:
    def test_basic_rgb(self):
        rgb = np.zeros((10, 20, 3), dtype=np.uint8)
        rgb[5, 10] = [255, 0, 0]
        qimg = _numpy_to_qimage(rgb)
        assert isinstance(qimg, QImage)
        assert qimg.width() == 20
        assert qimg.height() == 10
        assert not qimg.isNull()


class TestTransformMatte:
    def test_1ch_float(self):
        """Codex test: 1-channel float EXR matte visualization."""
        data = np.full((10, 10), 0.5, dtype=np.float32)
        qimg = _transform_matte(data)
        assert isinstance(qimg, QImage)
        assert qimg.width() == 10

    def test_negative_values(self):
        """Codex test: negative values should be clamped to 0."""
        data = np.full((10, 10), -0.5, dtype=np.float32)
        qimg = _transform_matte(data)
        assert isinstance(qimg, QImage)

    def test_values_above_one(self):
        """Codex test: values > 1 should be clamped to 1."""
        data = np.full((10, 10), 2.5, dtype=np.float32)
        qimg = _transform_matte(data)
        assert isinstance(qimg, QImage)


class TestTransformLinearRGB:
    def test_3ch_float(self):
        """Codex test: 3-channel float EXR linear RGB."""
        bgr = np.full((10, 10, 3), 0.5, dtype=np.float32)
        qimg = _transform_linear_rgb(bgr, ViewMode.FG)
        assert isinstance(qimg, QImage)

    def test_input_mode_preserves_display_encoded_exr_values(self):
        """Video-derived EXRs should not be gamma-lifted again in INPUT mode."""
        bgr = np.full((1, 1, 3), 0.18, dtype=np.float32)
        qimg = _transform_linear_rgb(bgr, ViewMode.INPUT, input_exr_is_linear=False)
        color = qimg.pixelColor(0, 0)
        assert 45 <= color.red() <= 46
        assert 45 <= color.green() <= 46
        assert 45 <= color.blue() <= 46

    def test_input_mode_gamma_corrects_true_linear_exr_values(self):
        """Standalone linear EXRs should be display-transformed in INPUT mode."""
        bgr = np.full((1, 1, 3), 0.18, dtype=np.float32)
        qimg = _transform_linear_rgb(bgr, ViewMode.INPUT, input_exr_is_linear=True)
        color = qimg.pixelColor(0, 0)
        assert 117 <= color.red() <= 118
        assert 117 <= color.green() <= 118
        assert 117 <= color.blue() <= 118

    def test_negative_values(self):
        """Codex test: negative values in linear EXR."""
        bgr = np.full((10, 10, 3), -0.1, dtype=np.float32)
        qimg = _transform_linear_rgb(bgr, ViewMode.FG)
        assert isinstance(qimg, QImage)

    def test_hdr_values(self):
        """Codex test: HDR values > 1 should be tone-mapped."""
        bgr = np.full((10, 10, 3), 5.0, dtype=np.float32)
        qimg = _transform_linear_rgb(bgr, ViewMode.FG)
        assert isinstance(qimg, QImage)


class TestTransformProcessedRGBA:
    def test_4ch_straight_rgba(self):
        """Codex test: 4-channel straight RGBA (Processed output)."""
        bgra = np.zeros((10, 10, 4), dtype=np.float32)
        bgra[:, :, :3] = 0.25
        bgra[:, :, 3] = 0.5   # alpha
        qimg = _transform_processed_rgba(bgra)
        assert isinstance(qimg, QImage)

    def test_zero_alpha(self):
        """Codex test: zero alpha should not cause divide-by-zero."""
        bgra = np.zeros((10, 10, 4), dtype=np.float32)
        qimg = _transform_processed_rgba(bgra)
        assert isinstance(qimg, QImage)

    def test_composites_straight_rgba_over_black_for_preview(self):
        """Processed preview should composite straight RGBA over black."""
        bgra = np.zeros((1, 130, 4), dtype=np.float32)
        bgra[:, :, :3] = 0.25
        bgra[:, :, 3] = 0.5
        qimg = _transform_processed_rgba(bgra)
        color = qimg.pixelColor(129, 0)
        assert 99 <= color.red() <= 100
        assert 99 <= color.green() <= 100
        assert 99 <= color.blue() <= 100

    def test_live_preview_rgba_matches_saved_bgra_display(self):
        """Live processed preview and saved EXR decode should use the same transform."""
        rgba = np.zeros((4, 4, 4), dtype=np.float32)
        rgba[:, :, :3] = 0.25
        rgba[:, :, 3] = 0.5
        bgra = rgba[:, :, [2, 1, 0, 3]]
        live_qimg = processed_rgba_to_qimage(rgba)
        saved_qimg = _transform_processed_rgba(bgra)
        live = live_qimg.pixelColor(0, 0)
        saved = saved_qimg.pixelColor(0, 0)
        assert live.red() == saved.red()
        assert live.green() == saved.green()
        assert live.blue() == saved.blue()


class TestDecodeFrame:
    def test_cache_key_distinguishes_input_exr_color_truth(self):
        assert _cache_key("/tmp/test.exr", ViewMode.INPUT, False) != _cache_key(
            "/tmp/test.exr", ViewMode.INPUT, True,
        )

    def test_decode_png(self, tmp_path):
        """Test decoding a PNG file (sRGB, 8-bit)."""
        clear_cache()
        img = np.zeros((20, 30, 3), dtype=np.uint8)
        img[10, 15] = [0, 255, 0]
        path = os.path.join(str(tmp_path), "test.png")
        cv2.imwrite(path, img)

        qimg = decode_frame(path, ViewMode.COMP)
        assert isinstance(qimg, QImage)
        assert qimg.width() == 30
        assert qimg.height() == 20

    def test_input_png_respects_linear_interpretation(self, tmp_path):
        """INPUT PNG preview should gamma-correct when the source is marked linear."""
        clear_cache()
        img = np.full((1, 1, 3), 46, dtype=np.uint8)
        path = os.path.join(str(tmp_path), "linear_input.png")
        cv2.imwrite(path, img)

        srgb_qimg = decode_frame(path, ViewMode.INPUT, input_exr_is_linear=False)
        linear_qimg = decode_frame(path, ViewMode.INPUT, input_exr_is_linear=True)

        srgb_color = srgb_qimg.pixelColor(0, 0)
        linear_color = linear_qimg.pixelColor(0, 0)
        assert 45 <= srgb_color.red() <= 46
        assert 117 <= linear_color.red() <= 118
        assert linear_color.red() > srgb_color.red()

    def test_cache_hit(self, tmp_path):
        """Test that second decode hits cache."""
        clear_cache()
        img = np.zeros((10, 10, 3), dtype=np.uint8)
        path = os.path.join(str(tmp_path), "cached.png")
        cv2.imwrite(path, img)

        qimg1 = decode_frame(path, ViewMode.COMP)
        qimg2 = decode_frame(path, ViewMode.COMP)
        # Both should succeed (cache hit on second)
        assert qimg1 is not None
        assert qimg2 is not None

    def test_nonexistent_file(self):
        clear_cache()
        result = decode_frame("/nonexistent/file.png", ViewMode.COMP)
        assert result is None


class TestDecodeVideoFrame:
    def test_input_video_respects_linear_interpretation(self):
        clear_cache()
        frame = np.full((1, 1, 3), 46, dtype=np.uint8)

        class _FakeCapture:
            def isOpened(self) -> bool:
                return True

            def set(self, *_args) -> bool:
                return True

            def read(self):
                return True, frame.copy()

            def release(self) -> None:
                return None

        with patch("ui.preview.display_transform.cv2.VideoCapture", return_value=_FakeCapture()):
            srgb_qimg = decode_video_frame("clip.mp4", 0, input_exr_is_linear=False)
        clear_cache()
        with patch("ui.preview.display_transform.cv2.VideoCapture", return_value=_FakeCapture()):
            linear_qimg = decode_video_frame("clip.mp4", 0, input_exr_is_linear=True)

        srgb_color = srgb_qimg.pixelColor(0, 0)
        linear_color = linear_qimg.pixelColor(0, 0)
        assert 45 <= srgb_color.red() <= 46
        assert 117 <= linear_color.red() <= 118
        assert linear_color.red() > srgb_color.red()


class TestTargetSize:
    def _write(self, tmp_path, name, arr):
        path = os.path.join(str(tmp_path), name)
        assert cv2.imwrite(path, arr)
        return path

    def test_exr_fit_and_source_size(self, tmp_path):
        clear_cache()
        arr = np.full((180, 320, 3), 0.2, dtype=np.float32)
        path = self._write(tmp_path, "a.exr", arr)
        img = decode_frame(path, ViewMode.FG, target_size=(160, 90))
        assert (img.width(), img.height()) == (160, 90)
        assert img.text("source_width") == "320"
        assert img.text("source_height") == "180"
        assert image_source_size(img) == (320, 180)

    def test_large_source_text(self):
        from ui.preview import display_transform as d
        arr = np.full((1800, 3200, 3), 0.5, dtype=np.float32)
        img = d._transform_linear_rgb(arr, ViewMode.FG, target_size=(1280, 720))
        assert (img.width(), img.height()) == (1280, 720)
        assert image_source_size(img) == (3200, 1800)

    def test_linear_to_srgb_on_reduced_array(self, monkeypatch):
        from ui.preview import display_transform as d
        seen = []
        orig = d._linear_to_srgb
        monkeypatch.setattr(d, "_linear_to_srgb", lambda a: (seen.append(a.shape), orig(a))[1])
        d._transform_linear_rgb(
            np.full((400, 800, 3), 0.5, dtype=np.float32), ViewMode.FG, target_size=(200, 100)
        )
        assert seen == [(100, 200, 3)]

    def test_no_upscale(self, tmp_path):
        clear_cache()
        path = self._write(tmp_path, "s.png", np.zeros((10, 20, 3), np.uint8))
        img = decode_frame(path, ViewMode.COMP, target_size=(1280, 720))
        assert (img.width(), img.height()) == (20, 10)
        assert image_source_size(img) == (20, 10)

    def test_png_reduced(self, tmp_path):
        clear_cache()
        path = self._write(tmp_path, "p.png", np.zeros((90, 160, 3), np.uint8))
        img = decode_frame(path, ViewMode.COMP, target_size=(80, 80))
        assert (img.width(), img.height()) == (80, 45)
        assert image_source_size(img) == (160, 90)

    def test_none_identical_to_default(self, tmp_path):
        clear_cache()
        rng = np.random.default_rng(0)
        arr = rng.random((12, 16, 3), dtype=np.float32)
        path = self._write(tmp_path, "n.exr", arr)
        a = decode_frame(path, ViewMode.FG)
        clear_cache()
        b = decode_frame(path, ViewMode.FG, target_size=None)
        assert a == b
        bgr = arr.copy()
        expected = np.clip(bgr, 0, 1)
        lin = np.where(expected <= 0.0031308, expected * 12.92,
                       1.055 * np.power(expected, 1 / 2.4) - 0.055)
        rgb = cv2.cvtColor((lin * 255.0).astype(np.uint8), cv2.COLOR_BGR2RGB)
        ref = _numpy_to_qimage(rgb)
        assert a == ref

    def test_processed_alpha_edge_premultiplied(self):
        # left: transparent pixel with bright colour; right: opaque black
        bgra = np.zeros((2, 2, 4), dtype=np.float32)
        bgra[:, 0] = [1.0, 1.0, 1.0, 0.0]
        bgra[:, 1] = [0.0, 0.0, 0.0, 1.0]
        img = _transform_processed_rgba(bgra, target_size=(1, 1))
        assert img.width() == 1
        assert img.pixelColor(0, 0).red() == 0
        # half alpha white next to opaque black -> 0.25 linear average
        bgra[:, 0] = [1.0, 1.0, 1.0, 0.5]
        img = _transform_processed_rgba(bgra, target_size=(1, 1))
        assert 0.2 < img.pixelColor(0, 0).red() / 255.0 < 0.6

    def test_image_source_size_fallback(self):
        img = QImage(7, 5, QImage.Format_RGB888)
        assert image_source_size(img) == (7, 5)
        img.setText("source_width", "bad")
        img.setText("source_height", "3")
        assert image_source_size(img) == (7, 5)

    def test_video_release_on_failure(self):
        clear_cache()
        released = []

        class _Cap:
            def isOpened(self):
                return True

            def set(self, *_a):
                raise RuntimeError("boom")

            def read(self):
                return False, None

            def release(self):
                released.append(1)

        with patch("ui.preview.display_transform.cv2.VideoCapture", return_value=_Cap()):
            assert decode_video_frame("clip.mp4", 0, target_size=(10, 10)) is None
        assert released

    def test_video_target_size(self):
        clear_cache()
        frame = np.zeros((40, 80, 3), np.uint8)

        class _Cap:
            def isOpened(self):
                return True

            def set(self, *_a):
                return True

            def read(self):
                return True, frame.copy()

            def release(self):
                pass

        with patch("ui.preview.display_transform.cv2.VideoCapture", return_value=_Cap()):
            img = decode_video_frame("clip.mp4", 0, target_size=(40, 40))
        assert (img.width(), img.height()) == (40, 20)
        assert image_source_size(img) == (80, 40)
