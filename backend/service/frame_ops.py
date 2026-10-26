"""Frame I/O operations for the service layer.

Read/write individual frames, masks, manifests, and output images.
"""
from __future__ import annotations

import json
import logging
import glob
import os
from typing import Any, Callable, Optional

import cv2
import numpy as np

from ..clip_state import ClipAsset, ClipEntry
from ..errors import CorridorKeyError, FrameReadError, JobCancelledError
from ..frame_io import (
    imread_unicode,
    read_video_frames,
)
from ..validators import validate_frame_read, validate_write

logger = logging.getLogger(__name__)



class FrameOpsMixin:
    """Mixin providing frame I/O operations for CorridorKeyService."""

    def _read_input_frame(
        self,
        clip: ClipEntry,
        frame_index: int,
        input_files: list[str],
        input_cap: Optional[Any],
        input_is_linear: bool,
    ) -> tuple[Optional[np.ndarray], str, bool]:
        """Read a single input frame.

        Returns:
            (image_float32, stem_name, is_linear)
        """
        from . import read_image_frame
        
        logger.debug(f"Reading input frame {frame_index} for '{clip.name}'")
        input_stem = f"{frame_index:05d}"

        if input_cap:
            ret, frame = input_cap.read()
            if not ret:
                return None, input_stem, False
            img_rgb = cv2.cvtColor(frame, cv2.COLOR_BGR2RGB)
            return img_rgb.astype(np.float32) / 255.0, input_stem, input_is_linear
        else:
            fpath = os.path.join(clip.input_asset.path, input_files[frame_index])
            input_stem = os.path.splitext(input_files[frame_index])[0]
            img = read_image_frame(fpath)
            validate_frame_read(img, clip.name, frame_index, fpath)
            return img, input_stem, input_is_linear


    def _write_image(
        self, img: np.ndarray, path: str, fmt: str, clip_name: str, frame_index: int,
        exr_compression: str = "dwab",
    ) -> None:
        """Write a single image in the requested format."""
        from . import write_exr
        from .. import frame_io

        if fmt == "exr":
            # EXR requires float32 — convert if uint8 (e.g. pre-converted comp)
            if img.dtype == np.uint8:
                img = img.astype(np.float32) / 255.0
            elif img.dtype != np.float32:
                img = img.astype(np.float32)
            validate_write(
                write_exr(path, img, compression=exr_compression),
                clip_name, frame_index, path,
            )
        else:
            # PNG 8-bit. Module-attribute call keeps the write unicode-safe
            # on Windows and patchable as backend.frame_io.imwrite_unicode.
            if img.dtype != np.uint8:
                img = (np.clip(img, 0.0, 1.0) * 255.0).astype(np.uint8)
            validate_write(
                frame_io.imwrite_unicode(path, img),
                clip_name, frame_index, path,
            )



    # ------------------------------------------------------------------
    # CorridorKey 2.0 output publication
    # ------------------------------------------------------------------

    V2_PRODUCT_TO_EZ = {"fg": "fg", "alpha": "matte", "checker": "comp", "cutout": "processed"}

    @staticmethod
    def _v2_required_products(output_config) -> list[str]:
        """Runtime products needed for the enabled EZ outputs."""
        enabled = set(output_config.enabled_outputs)
        products = [p for p, ez in FrameOpsMixin.V2_PRODUCT_TO_EZ.items() if ez in enabled]
        if not products:
            raise ValueError("No outputs selected; enable at least one output")
        return products

    @staticmethod
    def _v2_runtime_file(product: str, directory: str, index: int) -> str:
        if product == "checker":
            pattern = os.path.join(
                glob.escape(directory), f"checker_comp_despill*_{index:06d}.exr"
            )
            matches = sorted(glob.glob(pattern))
            if len(matches) != 1:
                raise CorridorKeyError(
                    f"Expected exactly one runtime checker file for index {index} in "
                    f"{directory}, found {len(matches)}"
                )
            return matches[0]
        return os.path.join(directory, f"{product}_{index:06d}.exr")

    @staticmethod
    def _v2_read(product: str, path: str) -> np.ndarray:
        if not os.path.isfile(path):
            raise CorridorKeyError(f"Missing CorridorKey 2.0 runtime output: {path}")
        img = imread_unicode(path, cv2.IMREAD_UNCHANGED)
        if img is None:
            raise CorridorKeyError(f"Unreadable CorridorKey 2.0 runtime output: {path}")
        if product == "alpha":
            if img.ndim == 3 and img.shape[2] == 1:
                img = img[:, :, 0]
            ok = img.ndim == 2
        elif product in ("fg", "checker"):
            ok = img.ndim == 3 and img.shape[2] == 3
        else:  # cutout
            ok = img.ndim == 3 and img.shape[2] == 4
        if not ok:
            raise CorridorKeyError(
                f"Unexpected shape {img.shape} for {product} runtime output: {path}"
            )
        img = img.astype(np.float32, copy=False)
        if not np.isfinite(img).all():
            raise CorridorKeyError(
                f"Non-finite values (NaN/inf) in runtime output: {path}"
            )
        return img





    def _load_first_frame_mask(
        self, clip: ClipEntry, frame_shape: tuple[int, int]
    ) -> Optional[np.ndarray]:
        """Load the first-frame mask for MatAnyone2.

        Tries mask_asset's first file. Returns grayscale uint8 (H,W) or None.
        """
        if clip.mask_asset is None:
            return None

        if clip.mask_asset.asset_type == 'sequence':
            mask_files = clip.mask_asset.get_frame_files()
            if not mask_files:
                return None
            first_mask_path = os.path.join(clip.mask_asset.path, mask_files[0])
            mask = imread_unicode(first_mask_path, cv2.IMREAD_GRAYSCALE)
            if mask is None:
                return None
            # Binarize
            _, mask = cv2.threshold(mask, 10, 255, cv2.THRESH_BINARY)
            # Resize to match input frame if needed
            target_h, target_w = frame_shape
            if mask.shape[:2] != (target_h, target_w):
                mask = cv2.resize(mask, (target_w, target_h), interpolation=cv2.INTER_NEAREST)
            return mask

        return None

    def _load_frames_for_videomama(
        self, asset: ClipAsset, clip_name: str,
        job=None,
        on_status: Optional[Callable[[str], None]] = None,
    ) -> list[np.ndarray]:
        """Load input frames for VideoMaMa as uint8 RGB [0, 255]."""
        from . import read_image_frame
        
        if asset.asset_type == 'video':
            raw = read_video_frames(asset.path)
            return [(np.clip(f, 0.0, 1.0) * 255.0).astype(np.uint8) for f in raw]
        frames = []
        files = asset.get_frame_files()
        total = len(files)
        for i, fname in enumerate(files):
            if job and job.is_cancelled:
                raise JobCancelledError(clip_name, i)
            fpath = os.path.join(asset.path, fname)
            img = read_image_frame(fpath, gamma_correct_exr=True)
            if img is not None:
                frames.append((np.clip(img, 0.0, 1.0) * 255.0).astype(np.uint8))
            if on_status and i % 20 == 0 and i > 0:
                on_status(f"Loading frames ({i}/{total})...")
        return frames

    def _load_mask_frames_for_videomama(
        self, asset: ClipAsset, clip_name: str
    ) -> list[np.ndarray]:
        """Load mask frames for VideoMaMa as uint8 grayscale [0, 255]."""
        def _threshold_mask(bgr_frame: np.ndarray) -> np.ndarray:
            gray = cv2.cvtColor(bgr_frame, cv2.COLOR_BGR2GRAY)
            _, binary = cv2.threshold(gray, 10, 255, cv2.THRESH_BINARY)
            return binary  # uint8

        if asset.asset_type == 'video':
            return read_video_frames(asset.path, processor=_threshold_mask)
        masks = []
        for fname in asset.get_frame_files():
            fpath = os.path.join(asset.path, fname)
            mask = imread_unicode(fpath, cv2.IMREAD_GRAYSCALE)
            if mask is None:
                continue
            _, binary = cv2.threshold(mask, 10, 255, cv2.THRESH_BINARY)
            masks.append(binary)  # uint8
        return masks

    def _selected_sequence_files(self, clip: ClipEntry) -> list[str]:
        """Return the ordered frame filenames for the clip's active in/out range."""
        if clip.input_asset is None or clip.input_asset.asset_type != "sequence":
            return []
        files = clip.input_asset.get_frame_files()
        if clip.in_out_range is not None:
            lo = clip.in_out_range.in_point
            hi = clip.in_out_range.out_point
            files = files[lo:hi + 1]
        return files

    def _load_named_sequence_frames(
        self,
        asset: ClipAsset,
        file_names: list[str],
        clip_name: str,
        *,
        gamma_correct_exr: bool = False,
        job=None,
        on_status: Optional[Callable[[str], None]] = None,
    ) -> list[tuple[str, np.ndarray]]:
        """Load named image-sequence frames as uint8 RGB into a list.

        Eager variant: materializes every frame in memory. Safe for
        short clips (a few hundred frames at HD), but OOMs at scale —
        a 109,192-frame 4K UHD clip would need ~2.6 TiB of RAM. Use
        :meth:`_iter_named_sequence_frames` for pipelines that can
        consume frames one at a time.
        """
        return list(
            self._iter_named_sequence_frames(
                asset,
                file_names,
                clip_name,
                gamma_correct_exr=gamma_correct_exr,
                job=job,
                on_status=on_status,
            )
        )

    def _iter_named_sequence_frames(
        self,
        asset: ClipAsset,
        file_names: list[str],
        clip_name: str,
        *,
        gamma_correct_exr: bool = False,
        job=None,
        on_status: Optional[Callable[[str], None]] = None,
    ):
        """Stream named image-sequence frames as uint8 RGB, one at a time.

        Yields ``(name, uint8_rgb_array)`` tuples lazily. Exactly one
        frame is ever held in memory across iterations, so peak RAM is
        O(1) frames regardless of clip length — a 100k+ frame clip
        streams at the same peak memory as a 10-frame clip.

        This is the fix for issue #95: BiRefNet on a 109,192-frame 4K
        UHD EXR sequence used to OOM at the list-materialization step
        in ``_load_named_sequence_frames`` because NumPy can't allocate
        ~2.6 TiB of uint8 pixel data in a single Python list. The
        per-frame wrapper loop is already one-at-a-time, so streaming
        the loader is a pure win: no speed impact (disk read was never
        the bottleneck), no quality impact, no behavior change for
        small clips.

        Consumers that need random access or length must either call
        :meth:`_load_named_sequence_frames` (eager list) or pass
        ``num_frames`` explicitly alongside the generator.
        """
        from . import read_image_frame

        total = len(file_names)
        for index, fname in enumerate(file_names):
            if job and job.is_cancelled:
                raise JobCancelledError(clip_name, index)
            fpath = os.path.join(asset.path, fname)
            img = read_image_frame(fpath, gamma_correct_exr=gamma_correct_exr)
            if img is None:
                raise FrameReadError(clip_name, index, fpath)
            frame = (np.clip(img, 0.0, 1.0) * 255.0).astype(np.uint8)
            if on_status and index % 20 == 0 and index > 0:
                on_status(f"Loading frames ({index}/{total})...")
            yield fname, frame

    @staticmethod
    def _resolve_sequence_input_is_linear(
        clip: ClipEntry,
        input_is_linear: bool | None,
    ) -> bool:
        """Honor explicit UI override, otherwise default from clip source type."""
        if input_is_linear is not None:
            return input_is_linear
        return clip.should_default_input_linear()

    @staticmethod
    def _write_mask_track_manifest(
        clip: ClipEntry,
        *,
        source: str,
        frame_stems: list[str],
        model_id: str | None = None,
    ) -> None:
        """Persist provenance for dense VideoMaMa-ready mask tracks."""
        from ..clip_state import MASK_TRACK_MANIFEST
        manifest_path = os.path.join(clip.root_path, MASK_TRACK_MANIFEST)
        payload = {
            "source": source,
            "frame_stems": frame_stems,
            "model_id": model_id,
        }
        with open(manifest_path, "w", encoding="utf-8") as handle:
            json.dump(payload, handle, indent=2)

    @staticmethod
    def _remove_alpha_hint_dir(clip: ClipEntry) -> None:
        """Remove AlphaHint so a new mask/alpha run is authoritative."""
        import shutil
        alpha_dir = os.path.join(clip.root_path, "AlphaHint")
        if os.path.isdir(alpha_dir):
            shutil.rmtree(alpha_dir, ignore_errors=True)
