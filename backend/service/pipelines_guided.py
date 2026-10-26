"""Guided pipelines — SAM2, VideoMaMa, MatAnyone2 (require masks/annotations)."""
from __future__ import annotations

import logging
import os
import sys
import time
from typing import Callable, Optional

import cv2
import numpy as np

from ..clip_state import (
    ClipAsset,
    ClipEntry,
    ClipState,
    mask_sequence_is_videomama_ready,
)
from ..errors import (
    CorridorKeyError,
    GPURequiredError,
    JobCancelledError,
    WriteFailureError,
)
from ..frame_io import imread_unicode, imwrite_unicode
from .helpers import BASE_DIR
from .model_manager import _ActiveModel
from .pipelines_auto import _holds_gpu_lock

logger = logging.getLogger(__name__)


class GuidedPipelinesMixin:
    """Mixin providing guided pipelines (SAM2, VideoMaMa, MatAnyone2) for CorridorKeyService."""

    @_holds_gpu_lock
    def run_videomama(
        self,
        clip: ClipEntry,
        job=None,
        on_progress: Optional[Callable[[str, int, int], None]] = None,
        on_warning: Optional[Callable[[str], None]] = None,
        on_status: Optional[Callable[[str], None]] = None,
        chunk_size: int = 16,
    ) -> None:
        """Run VideoMaMa guided alpha generation for a clip.

        Transitions clip: MASKED -> READY (creates AlphaHint directory).
        """
        if clip.input_asset is None:
            raise CorridorKeyError(f"Clip '{clip.name}' missing input asset for VideoMaMa")
        if clip.mask_asset is None:
            raise CorridorKeyError(f"Clip '{clip.name}' missing mask asset for VideoMaMa")

        if self._device == 'cpu':
            raise GPURequiredError("VideoMaMa")
        if clip.input_asset.asset_type != "sequence":
            raise CorridorKeyError("VideoMaMa currently requires an extracted image sequence")
        ann_path = os.path.join(clip.root_path, "annotations.json")
        has_annotations = os.path.isfile(ann_path) and os.path.getsize(ann_path) > 2
        if has_annotations and not mask_sequence_is_videomama_ready(clip.root_path):
            raise CorridorKeyError(
                "VideoMaMa requires dense tracked masks. Run Track Mask first."
            )

        def _status(msg: str) -> None:
            logger.info(f"VideoMaMa [{clip.name}]: {msg}")
            if on_status:
                on_status(msg)

        def _check_cancel(phase: str = "") -> None:
            if job and job.is_cancelled:
                raise JobCancelledError(clip.name, 0)

        t_start = time.monotonic()

        # Phase 1: Load model
        _status("Loading model...")
        pipeline = self._get_videomama_pipeline()
        _check_cancel("model load")

        alpha_dir = os.path.join(clip.root_path, "AlphaHint")
        os.makedirs(alpha_dir, exist_ok=True)

        # Phase 2: Load input frames
        _status("Loading frames...")
        selected_input_names = self._selected_sequence_files(clip)
        if not selected_input_names:
            raise CorridorKeyError(f"Clip '{clip.name}' has no input frames for VideoMaMa")
        named_input_frames = self._load_named_sequence_frames(
            clip.input_asset,
            selected_input_names,
            clip.name,
            job=job,
            on_status=on_status,
        )
        input_frames = [frame for _, frame in named_input_frames]
        _check_cancel("frame load")

        # Phase 3: Load + stem-match masks
        _status("Loading masks...")
        mask_stems: dict[str, np.ndarray] = {}
        if clip.mask_asset.asset_type == 'sequence':
            mask_files = clip.mask_asset.get_frame_files()
            for i, fname in enumerate(mask_files):
                _check_cancel("mask load")
                fpath = os.path.join(clip.mask_asset.path, fname)
                m = imread_unicode(fpath, cv2.IMREAD_GRAYSCALE)
                if m is not None:
                    _, binary = cv2.threshold(m, 10, 255, cv2.THRESH_BINARY)
                    stem = os.path.splitext(fname)[0]
                    mask_stems[stem] = binary
        else:
            raw_masks = self._load_mask_frames_for_videomama(clip.mask_asset, clip.name)
            for i, m in enumerate(raw_masks):
                mask_stems[f"frame_{i:06d}"] = m

        # Build output filenames from the selected input stems
        input_names = [fname for fname, _ in named_input_frames]

        # Align masks to input frames by stem, defaulting to all-black
        num_frames = len(input_frames)
        mask_frames = []
        for fname in input_names:
            stem = os.path.splitext(fname)[0]
            if stem in mask_stems:
                mask_frames.append(mask_stems[stem])
            else:
                h_m, w_m = input_frames[0].shape[:2] if input_frames else (4, 4)
                mask_frames.append(np.zeros((h_m, w_m), dtype=np.uint8))

        # Resume logic
        existing_alpha = []
        if os.path.isdir(alpha_dir):
            existing_alpha = [f for f in os.listdir(alpha_dir)
                              if f.lower().endswith(('.png', '.jpg', '.jpeg'))]
        n_existing = len(existing_alpha)
        completed_chunks = n_existing // chunk_size
        start_chunk = max(0, completed_chunks - 1)
        start_frame = start_chunk * chunk_size
        if start_frame > 0:
            keep = set()
            for i in range(start_frame):
                if i < len(input_names):
                    stem = os.path.splitext(input_names[i])[0]
                    keep.add(f"{stem}.png")
            for fname in existing_alpha:
                if fname not in keep:
                    os.remove(os.path.join(alpha_dir, fname))
            logger.info(f"VideoMaMa resuming for '{clip.name}': {n_existing} alpha frames existed, "
                        f"rolling back to chunk {start_chunk} (frame {start_frame})")

        # Phase 4: Inference (per-chunk)
        sys.path.insert(0, os.path.join(BASE_DIR, "VideoMaMaInferenceModule"))
        from VideoMaMaInferenceModule.inference import run_inference

        total_chunks = (num_frames + chunk_size - 1) // chunk_size
        _status(f"Running inference (chunk 1/{total_chunks})...")
        frames_written = start_frame
        for chunk_idx, chunk_output in enumerate(
            run_inference(pipeline, input_frames, mask_frames,
                          chunk_size=chunk_size, on_status=on_status)
        ):
            _check_cancel("inference")

            # Skip already-completed chunks (resume)
            if chunk_idx < start_chunk:
                frames_written += len(chunk_output)
                if on_progress:
                    on_progress(clip.name, frames_written, num_frames)
                continue

            _status(f"Processing chunk {chunk_idx + 1}/{total_chunks}...")

            # Write chunk frames
            t_chunk = time.monotonic()
            for frame in chunk_output:
                if frame.dtype == np.uint8:
                    frame_u8 = frame
                else:
                    frame_u8 = (np.clip(frame, 0.0, 1.0) * 255.0).astype(np.uint8)
                out_bgr = cv2.cvtColor(frame_u8, cv2.COLOR_RGB2BGR)
                if frames_written < len(input_names):
                    stem = os.path.splitext(input_names[frames_written])[0]
                    out_name = f"{stem}.png"
                else:
                    out_name = f"frame_{frames_written:06d}.png"
                out_path = os.path.join(alpha_dir, out_name)
                if not imwrite_unicode(out_path, out_bgr):
                    raise WriteFailureError(clip.name, frames_written, out_path)
                frames_written += 1
            logger.debug(f"Clip '{clip.name}' chunk {chunk_idx}: {len(chunk_output)} frames in {time.monotonic() - t_chunk:.3f}s")

            if on_progress:
                on_progress(clip.name, frames_written, num_frames)

        # Refresh alpha asset
        clip.alpha_asset = ClipAsset(alpha_dir, 'sequence')

        # Transition MASKED -> READY
        try:
            clip.transition_to(ClipState.READY)
        except Exception as e:
            if on_warning:
                on_warning(f"State transition after VideoMaMa: {e}")

        logger.info(f"VideoMaMa complete for '{clip.name}': {frames_written} alpha frames in {time.monotonic() - t_start:.1f}s")

    @_holds_gpu_lock
    def run_matanyone2(
        self,
        clip: ClipEntry,
        job=None,
        on_progress: Optional[Callable[[str, int, int], None]] = None,
        on_warning: Optional[Callable[[str], None]] = None,
        on_status: Optional[Callable[[str], None]] = None,
    ) -> None:
        """Run MatAnyone2 video matting alpha generation for a clip.

        Transitions clip: MASKED -> READY (creates AlphaHint directory).
        """
        # Preflight validation
        if clip.input_asset is None:
            raise CorridorKeyError(f"Clip '{clip.name}' missing input asset for MatAnyone2")
        if clip.mask_asset is None:
            raise CorridorKeyError(f"Clip '{clip.name}' missing mask asset for MatAnyone2")
        if self._device == 'cpu':
            raise GPURequiredError("MatAnyone2")
        if clip.input_asset.asset_type != "sequence":
            raise CorridorKeyError("MatAnyone2 requires an extracted image sequence")

        def _status(msg: str) -> None:
            logger.info(f"MatAnyone2 [{clip.name}]: {msg}")
            if on_status:
                on_status(msg)

        def _check_cancel() -> bool:
            return bool(job and job.is_cancelled)

        t_start = time.monotonic()

        # Phase 1: Load model
        _status("Loading MatAnyone2 model...")
        processor = self._get_matanyone2()
        if _check_cancel():
            raise JobCancelledError(clip.name, 0)

        # Phase 2: Load input frames
        _status("Loading frames...")
        selected_input_names = self._selected_sequence_files(clip)
        if not selected_input_names:
            raise CorridorKeyError(f"Clip '{clip.name}' has no input frames for MatAnyone2")

        named_input_frames = self._load_named_sequence_frames(
            clip.input_asset,
            selected_input_names,
            clip.name,
            job=job,
            on_status=on_status,
        )
        input_frames = [frame for _, frame in named_input_frames]
        input_names = [fname for fname, _ in named_input_frames]
        frame_stems = [os.path.splitext(fname)[0] for fname in input_names]
        if _check_cancel():
            raise JobCancelledError(clip.name, 0)

        # Phase 3: Load first-frame mask
        _status("Loading first-frame mask...")
        mask_frame = self._load_first_frame_mask(clip, input_frames[0].shape[:2])
        if mask_frame is None:
            raise CorridorKeyError(
                f"Clip '{clip.name}': MatAnyone2 requires a mask for the first frame (frame 0). "
                f"Please ensure your annotation or mask covers the very first frame."
            )

        # Phase 4: Inference
        alpha_dir = os.path.join(clip.root_path, "AlphaHint")
        try:
            frames_written = processor.process_frames(
                input_frames=input_frames,
                mask_frame=mask_frame,
                output_dir=alpha_dir,
                frame_names=frame_stems,
                progress_callback=on_progress,
                on_status=on_status,
                cancel_check=_check_cancel,
                clip_name=clip.name,
            )
        except Exception as e:
            # On OOM, clean up and re-raise without poisoning service
            if "CUDA out of memory" in str(e) or "OutOfMemoryError" in type(e).__name__:
                logger.error(f"MatAnyone2 OOM for '{clip.name}': {e}")
                self._matanyone2_processor = None
                self._active_model = _ActiveModel.NONE
                try:
                    import torch as _torch
                    _torch.cuda.empty_cache()
                except Exception:
                    pass
                raise CorridorKeyError(
                    f"MatAnyone2 ran out of GPU memory processing '{clip.name}'. "
                    f"Try closing other GPU applications or using a smaller clip."
                ) from e
            raise

        # Phase 5: Finalize
        clip.alpha_asset = ClipAsset(alpha_dir, 'sequence')

        try:
            clip.transition_to(ClipState.READY)
        except Exception as e:
            if on_warning:
                on_warning(f"State transition after MatAnyone2: {e}")

        logger.info(
            f"MatAnyone2 complete for '{clip.name}': "
            f"{frames_written} alpha frames in {time.monotonic() - t_start:.1f}s"
        )
