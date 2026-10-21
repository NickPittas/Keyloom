"""Display transform module for preview rendering.

Handles conversion of VFX image data to display-ready 8-bit sRGB QImage.
Supports EXR (float16/32, 1ch/3ch/4ch), PNG, and video frames.

Codex critical finding: naive clip-to-8bit looks wrong for:
- Linear EXR (needs linear→sRGB gamma)
- Matte (1ch → grayscale visualization)
- Processed (straight RGBA → composite with alpha for display)
- Negative/HDR values (needs clamping before conversion)
"""
from __future__ import annotations

import os
import logging
import threading
from collections import OrderedDict
from functools import lru_cache

os.environ.setdefault("OPENCV_IO_ENABLE_OPENEXR", "1")

import cv2
import numpy as np
from PySide6.QtGui import QImage

from .frame_index import ViewMode
from backend.frame_io import imread_unicode, open_video

logger = logging.getLogger(__name__)

# LRU cache for decoded QImages (max 20 frames)
# Lock protects concurrent access from thread pool workers
_QIMAGE_CACHE: OrderedDict[str, QImage] = OrderedDict()
_CACHE_LOCK = threading.Lock()
_CACHE_MAX = 10


def _cache_key(
    path: str,
    mode: ViewMode,
    input_exr_is_linear: bool = False,
    target_size: tuple[int, int] | None = None,
) -> str:
    """Cache key combining path, mode and target size for display transforms."""
    if target_size is None:
        return f"{mode.value}:{int(input_exr_is_linear)}:{path}"
    return f"{mode.value}:{int(input_exr_is_linear)}:{target_size[0]}x{target_size[1]}:{path}"


def _fit_size(width: int, height: int, target_size) -> tuple[int, int] | None:
    """Aspect-fit (width, height) into target_size, never upscaling.

    Returns None when no reduction is needed.
    """
    if target_size is None:
        return None
    tw, th = int(target_size[0]), int(target_size[1])
    if tw <= 0 or th <= 0 or width <= 0 or height <= 0:
        return None
    scale = min(tw / width, th / height)
    if scale >= 1.0:
        return None
    return max(1, round(width * scale)), max(1, round(height * scale))


def _reduce(arr: np.ndarray, target_size) -> np.ndarray:
    """Area-downsample a numeric array to fit target_size (no upscaling)."""
    fit = _fit_size(arr.shape[1], arr.shape[0], target_size)
    if fit is None:
        return arr
    out = cv2.resize(arr, fit, interpolation=cv2.INTER_AREA)
    if arr.ndim == 3 and out.ndim == 2:
        out = out[:, :, None]
    return out


def image_source_size(image: QImage) -> tuple[int, int]:
    """Original (width, height) of the decoded source, else the image size."""
    try:
        w = int(image.text("source_width"))
        h = int(image.text("source_height"))
        if w > 0 and h > 0:
            return w, h
    except (TypeError, ValueError):
        pass
    return image.width(), image.height()


def clear_cache() -> None:
    """Clear the frame cache (call on clip switch)."""
    with _CACHE_LOCK:
        _QIMAGE_CACHE.clear()


def decode_frame(
    path: str,
    mode: ViewMode,
    *,
    input_exr_is_linear: bool = False,
    target_size: tuple[int, int] | None = None,
) -> QImage | None:
    """Decode a frame file to a display-ready QImage.

    Applies mode-specific display transforms:
    - COMP: Already 8-bit sRGB PNG, direct load
    - INPUT: Uses the current source interpretation for EXR, PNG, JPG, etc.
    - ALPHA: Matte visualization from AlphaHint image frames
    - FG: Linear float EXR → sRGB gamma
    - MATTE: 1-channel float → grayscale visualization
    - PROCESSED: Straight RGBA float → composite over black for display

    With target_size, the image is aspect-fit (never upscaled) before the
    expensive gamma/8-bit steps; original size is kept in QImage text
    ``source_width``/``source_height``.

    Returns None if the file can't be read.
    """
    key = _cache_key(path, mode, input_exr_is_linear, target_size)
    with _CACHE_LOCK:
        if key in _QIMAGE_CACHE:
            _QIMAGE_CACHE.move_to_end(key)
            return _QIMAGE_CACHE[key]

    qimg = _do_decode(
        path, mode, input_exr_is_linear=input_exr_is_linear, target_size=target_size,
    )
    if qimg is not None:
        with _CACHE_LOCK:
            _QIMAGE_CACHE[key] = qimg
            while len(_QIMAGE_CACHE) > _CACHE_MAX:
                _QIMAGE_CACHE.popitem(last=False)

    return qimg


def _do_decode(
    path: str,
    mode: ViewMode,
    *,
    input_exr_is_linear: bool = False,
    target_size: tuple[int, int] | None = None,
) -> QImage | None:
    """Actual decode + transform logic."""
    if not os.path.isfile(path):
        return None

    is_exr = path.lower().endswith('.exr')

    try:
        if is_exr:
            return _decode_exr(
                path, mode, input_exr_is_linear=input_exr_is_linear, target_size=target_size,
            )
        else:
            return _decode_ldr(
                path, mode, input_exr_is_linear=input_exr_is_linear, target_size=target_size,
            )
    except Exception as e:
        logger.warning(f"Failed to decode frame {path}: {e}")
        return None


def _decode_ldr(
    path: str,
    mode: ViewMode,
    *,
    input_exr_is_linear: bool = False,
    target_size: tuple[int, int] | None = None,
) -> QImage | None:
    """Decode an 8-bit image (PNG/JPG/etc) to QImage."""
    img = imread_unicode(path, cv2.IMREAD_COLOR)
    if img is None:
        return None
    source_size = (img.shape[1], img.shape[0])
    img = _reduce(img, target_size)
    if mode == ViewMode.INPUT and input_exr_is_linear:
        return _transform_linear_rgb(
            img.astype(np.float32) / 255.0,
            ViewMode.INPUT,
            input_exr_is_linear=True,
            source_size=source_size,
        )
    # BGR → RGB
    rgb = cv2.cvtColor(img, cv2.COLOR_BGR2RGB)
    return _numpy_to_qimage(rgb, source_size=source_size)


def _decode_exr(
    path: str,
    mode: ViewMode,
    *,
    input_exr_is_linear: bool = False,
    target_size: tuple[int, int] | None = None,
) -> QImage | None:
    """Decode an EXR file with mode-specific display transform."""
    img = imread_unicode(path, cv2.IMREAD_UNCHANGED)
    if img is None:
        return None
    sz = {"target_size": target_size, "source_size": (img.shape[1], img.shape[0])}

    # Determine channel layout
    if img.ndim == 2:
        # Single channel (typical for Matte)
        return _transform_matte(img, **sz)
    elif img.ndim == 3:
        channels = img.shape[2]
        if channels == 1:
            return _transform_matte(img[:, :, 0], **sz)
        elif channels == 2:
            # Rare: treat first channel as matte
            return _transform_matte(img[:, :, 0], **sz)
        elif channels == 3:
            if mode in (ViewMode.ALPHA, ViewMode.MASK, ViewMode.MATTE):
                return _transform_matte(img[:, :, 0], **sz)
            # BGR float — FG or Input
            return _transform_linear_rgb(img, mode, input_exr_is_linear=input_exr_is_linear, **sz)
        elif channels == 4:
            if mode in (ViewMode.ALPHA, ViewMode.MASK, ViewMode.MATTE):
                return _transform_matte(img[:, :, 0], **sz)
            # BGRA float — Processed (straight RGBA)
            if mode == ViewMode.PROCESSED:
                return _transform_processed_rgba(img, **sz)
            else:
                # Strip alpha, treat as RGB
                return _transform_linear_rgb(
                    img[:, :, :3], mode, input_exr_is_linear=input_exr_is_linear, **sz,
                )
        else:
            # Unexpected channel count, take first 3
            return _transform_linear_rgb(
                img[:, :, :3], mode, input_exr_is_linear=input_exr_is_linear, **sz,
            )
    return None


def _transform_matte(
    data: np.ndarray,
    *,
    target_size: tuple[int, int] | None = None,
    source_size: tuple[int, int] | None = None,
) -> QImage:
    """Single-channel float → grayscale visualization.

    Clamp to [0,1], apply slight gamma for better visualization of
    subtle alpha edges, convert to 8-bit grayscale displayed as RGB.
    """
    if source_size is None:
        source_size = (data.shape[1], data.shape[0])
    clamped = np.clip(data.astype(np.float32), 0.0, 1.0)
    clamped = _reduce(clamped, target_size)
    # Light gamma lift for matte visualization (makes edges visible)
    display = np.power(clamped, 0.85)
    gray8 = (display * 255.0).astype(np.uint8)
    # Stack to 3-channel for consistent QImage format
    rgb = np.stack([gray8, gray8, gray8], axis=2)
    return _numpy_to_qimage(rgb, source_size=source_size)


def _transform_linear_rgb(
    bgr: np.ndarray,
    mode: ViewMode,
    *,
    input_exr_is_linear: bool = False,
    target_size: tuple[int, int] | None = None,
    source_size: tuple[int, int] | None = None,
) -> QImage:
    """Float BGR → sRGB 8-bit RGB.

    INPUT mode: extracted video EXRs are already display-encoded float, while
    standalone EXR plates are true linear-light and need gamma.
    FG mode: Inference output is linear float — apply linear→sRGB gamma.
    """
    if source_size is None:
        source_size = (bgr.shape[1], bgr.shape[0])
    # Clamp negatives (then reduce before the expensive gamma/8-bit steps)
    clamped = np.clip(bgr.astype(np.float32), 0.0, None)
    clamped = _reduce(clamped, target_size)

    if mode == ViewMode.INPUT and not input_exr_is_linear:
        # Extracted frames are display-encoded float (FFmpeg writes video
        # values as-is into EXR). Just clamp to [0,1] — no gamma.
        display = np.clip(clamped, 0.0, 1.0)
    else:
        # Linear EXR input and inference outputs both need gamma for display.
        # Clamp to [0,1] — stray super-whites from float rounding should not
        # trigger Reinhard tone mapping on the entire image.
        clamped = np.clip(clamped, 0.0, 1.0)
        display = _linear_to_srgb(clamped)

    # BGR → RGB
    rgb = cv2.cvtColor((display * 255.0).astype(np.uint8), cv2.COLOR_BGR2RGB)
    return _numpy_to_qimage(rgb, source_size=source_size)


def _transform_processed_rgba(
    bgra: np.ndarray,
    *,
    target_size: tuple[int, int] | None = None,
    source_size: tuple[int, int] | None = None,
) -> QImage:
    """Straight linear BGRA float → composite over black for display."""
    if source_size is None:
        source_size = (bgra.shape[1], bgra.shape[0])
    bgra_f = bgra.astype(np.float32)
    bgr = np.clip(bgra_f[:, :, :3], 0.0, None)
    alpha = np.clip(bgra_f[:, :, 3:4], 0.0, 1.0)
    # Premultiply in linear light BEFORE reducing so edges don't bleed.
    composite = np.clip(bgr * alpha, 0.0, 1.0)
    composite = _reduce(composite, target_size)
    srgb = _linear_to_srgb(composite)
    rgb = cv2.cvtColor((srgb * 255.0).astype(np.uint8), cv2.COLOR_BGR2RGB)
    return _numpy_to_qimage(rgb, source_size=source_size)


def processed_rgba_to_qimage(rgba: np.ndarray) -> QImage:
    """Render in-memory straight linear RGBA preview composited over black."""
    rgba_f = rgba.astype(np.float32)
    rgb = np.clip(rgba_f[:, :, :3], 0.0, None)
    alpha = np.clip(rgba_f[:, :, 3:4], 0.0, 1.0)
    composite = np.clip(rgb * alpha, 0.0, 1.0)
    srgb = _linear_to_srgb(composite)
    return _numpy_to_qimage((srgb * 255.0).astype(np.uint8))


def _linear_to_srgb(linear: np.ndarray) -> np.ndarray:
    """Apply sRGB gamma curve to linear float data."""
    linear = np.clip(linear, 0.0, 1.0)
    mask = linear <= 0.0031308
    return np.where(mask, linear * 12.92, 1.055 * np.power(linear, 1.0 / 2.4) - 0.055)


def _numpy_to_qimage(
    rgb: np.ndarray, *, source_size: tuple[int, int] | None = None,
) -> QImage:
    """Convert an RGB uint8 numpy array to QImage.

    Makes a copy of the data so the numpy array can be freed.
    """
    h, w, ch = rgb.shape
    assert ch == 3, f"Expected 3 channels, got {ch}"
    # Ensure contiguous
    rgb = np.ascontiguousarray(rgb)
    bytes_per_line = w * 3
    qimg = QImage(rgb.data, w, h, bytes_per_line, QImage.Format_RGB888)
    # QImage doesn't copy by default — force a deep copy
    qimg = qimg.copy()
    sw, sh = source_size if source_size is not None else (w, h)
    qimg.setText("source_width", str(int(sw)))
    qimg.setText("source_height", str(int(sh)))
    return qimg


def decode_video_frame(
    video_path: str,
    frame_index: int,
    *,
    input_exr_is_linear: bool = False,
    target_size: tuple[int, int] | None = None,
) -> QImage | None:
    """Decode a single frame from a video file.

    Uses cv2.VideoCapture with seek. Expensive for random access.
    Results are cached in the LRU cache.
    """
    key = _cache_key(
        f"video:{video_path}:{frame_index}",
        ViewMode.INPUT,
        input_exr_is_linear,
        target_size,
    )
    with _CACHE_LOCK:
        if key in _QIMAGE_CACHE:
            _QIMAGE_CACHE.move_to_end(key)
            return _QIMAGE_CACHE[key]

    cap = None
    try:
        cap = open_video(video_path)
        if not cap.isOpened():
            return None
        cap.set(cv2.CAP_PROP_POS_FRAMES, frame_index)
        ret, frame = cap.read()
        cap.release()
        cap = None
        if not ret or frame is None:
            return None
        source_size = (frame.shape[1], frame.shape[0])
        frame = _reduce(frame, target_size)
        if input_exr_is_linear:
            qimg = _transform_linear_rgb(
                frame.astype(np.float32) / 255.0,
                ViewMode.INPUT,
                input_exr_is_linear=True,
                source_size=source_size,
            )
        else:
            rgb = cv2.cvtColor(frame, cv2.COLOR_BGR2RGB)
            qimg = _numpy_to_qimage(rgb, source_size=source_size)

        with _CACHE_LOCK:
            _QIMAGE_CACHE[key] = qimg
            while len(_QIMAGE_CACHE) > _CACHE_MAX:
                _QIMAGE_CACHE.popitem(last=False)

        return qimg
    except Exception as e:
        logger.warning(f"Failed to decode video frame {frame_index} from {video_path}: {e}")
        return None
    finally:
        if cap is not None:
            try:
                cap.release()
            except Exception:
                pass
