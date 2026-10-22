"""
VideoMaMa Inference Module
Provides functions to load the model and run inference on video inputs.
"""

import logging
import os
import sys
import torch
import cv2
import numpy as np
from PIL import Image
from typing import Callable, Iterator, List, Optional

logger = logging.getLogger(__name__)

# Add current directory to path to ensure relative imports work if run directly
current_dir = os.path.dirname(os.path.abspath(__file__))
if current_dir not in sys.path:
    sys.path.append(current_dir)

from .pipeline import VideoInferencePipeline

def load_videomama_model(base_model_path: Optional[str] = None, unet_checkpoint_path: Optional[str] = None, device: str = "cuda") -> VideoInferencePipeline:
    """
    Load VideoMaMa pipeline with pretrained weights.

    Args:
        base_model_path (str, optional): Path to the base Stable Video Diffusion model.
                                         Defaults to ``<data_dir>/VideoMaMaInferenceModule/checkpoints/stable-video-diffusion-img2vid-xt``,
                                         falling back to the bundled module directory.
        unet_checkpoint_path (str, optional): Path to the fine-tuned UNet checkpoint.
                                              Defaults to ``<data_dir>/VideoMaMaInferenceModule/checkpoints/VideoMaMa``,
                                              falling back to the bundled module directory.
        device (str): Device to run on ("cuda" or "cpu").

    Returns:
        VideoInferencePipeline: Loaded pipeline instance.
    """
    from backend.model_runtime import acquire_model_artifact, validate_local_model_path

    lease = None
    if base_model_path is None and unet_checkpoint_path is None:
        lease = acquire_model_artifact("videomama")
        base_model_path = str(lease.paths["videomama-svd"])
        unet_checkpoint_path = str(lease.paths["videomama"])
    elif base_model_path is None:
        lease = acquire_model_artifact("videomama-svd")
        base_model_path = str(lease.path)
        unet_checkpoint_path = str(validate_local_model_path(
            "videomama", unet_checkpoint_path, components=("unet",)
        ))
    elif unet_checkpoint_path is None:
        lease = acquire_model_artifact(
            "videomama", exclude_dependencies=("videomama-svd",)
        )
        unet_checkpoint_path = str(lease.path)
        base_model_path = str(validate_local_model_path("videomama-svd", base_model_path))
    else:
        base_model_path = str(validate_local_model_path("videomama-svd", base_model_path))
        unet_checkpoint_path = str(validate_local_model_path(
            "videomama", unet_checkpoint_path, components=("unet",)
        ))

    logger.info(f"Loading Base model from {base_model_path}...")
    logger.info(f"Loading VideoMaMa UNet from {unet_checkpoint_path}...")

    try:
        if not os.path.isdir(base_model_path):
            raise FileNotFoundError(f"Local Stable Video Diffusion model directory is missing: {base_model_path}")
        if not os.path.isdir(unet_checkpoint_path):
            raise FileNotFoundError(f"Local VideoMaMa checkpoint directory is missing: {unet_checkpoint_path}")

        pipeline = VideoInferencePipeline(
            base_model_path=base_model_path,
            unet_checkpoint_path=unet_checkpoint_path,
            weight_dtype=torch.float16, # Use float16 for inference by default
            device=device
        )
    except Exception:
        if lease is not None:
            lease.close()
        raise
    if lease is not None:
        pipeline._model_asset_lease = lease
    
    logger.info("VideoMaMa pipeline loaded successfully")
    return pipeline

def extract_frames_from_video(video_path: str, max_frames: Optional[int] = None) -> tuple[List[np.ndarray], float]:
    """
    Extract frames from video file.

    Args:
        video_path (str): Path to video file.
        max_frames (int, optional): Maximum number of frames to extract.

    Returns:
        tuple: (List of numpy arrays (H,W,3) uint8 RGB, FPS)
    """
    if not os.path.exists(video_path):
        raise FileNotFoundError(f"Video file not found: {video_path}")

    # Unicode-safe open (issue #184): raw cv2.VideoCapture fails on
    # non-ASCII paths on Windows.
    from backend.frame_io import open_video  # Lazy import: avoid cycles

    cap = open_video(video_path)
    original_fps = cap.get(cv2.CAP_PROP_FPS)
    
    all_frames = []
    while cap.isOpened():
        ret, frame = cap.read()
        if not ret:
            break
        # Convert BGR to RGB
        frame_rgb = cv2.cvtColor(frame, cv2.COLOR_BGR2RGB)
        all_frames.append(frame_rgb)
    
    cap.release()
    
    if max_frames and len(all_frames) > max_frames:
        frames = all_frames[:max_frames]
    else:
        frames = all_frames
    
    return frames, original_fps

def run_inference(
    pipeline: VideoInferencePipeline,
    input_frames: List[np.ndarray],
    mask_frames: List[np.ndarray],
    chunk_size: int = 16,
    on_status: Optional[Callable[[str], None]] = None,
) -> Iterator[list[np.ndarray]]:
    """
    Run VideoMaMa inference on video frames with mask conditioning.

    PIL conversion and resize are done per-chunk (lazy) to avoid a multi-minute
    upfront preprocessing stall on large sequences.

    Args:
        pipeline (VideoInferencePipeline): Loaded pipeline instance.
        input_frames (List[np.ndarray]): List of RGB frames (H,W,3) uint8.
        mask_frames (List[np.ndarray]): List of mask frames (H,W) uint8 (0-255) grayscale.
        chunk_size (int): Number of frames to process at once to avoid OOM.
        on_status: Optional callback for phase updates (e.g. "chunk 2/4 — VAE decode 3/7").

    Yields:
        List[np.ndarray]: Chunk of output RGB frames (H,W,3) uint8.
    """
    if len(input_frames) != len(mask_frames):
        raise ValueError(f"Input frames ({len(input_frames)}) and mask frames ({len(mask_frames)}) must have same length.")

    if not input_frames:
        return

    # Get original size from first frame (for resizing output back)
    original_size = (input_frames[0].shape[1], input_frames[0].shape[0])  # (W, H)
    target_width, target_height = 1024, 576
    total_chunks = (len(input_frames) + chunk_size - 1) // chunk_size

    logger.info(f"Processing {len(input_frames)} frames in chunks of {chunk_size} ({total_chunks} chunks)")

    for i in range(0, len(input_frames), chunk_size):
        chunk_idx = i // chunk_size + 1
        chunk_input = input_frames[i:i + chunk_size]
        chunk_masks = mask_frames[i:i + chunk_size]

        # Per-chunk PIL conversion + resize (2-3 sec instead of minutes upfront)
        chunk_frames_pil = [
            Image.fromarray(f).resize((target_width, target_height), Image.Resampling.BILINEAR)
            for f in chunk_input
        ]
        chunk_masks_pil = []
        for m in chunk_masks:
            if m.ndim == 3:
                m = cv2.cvtColor(m, cv2.COLOR_RGB2GRAY)
            chunk_masks_pil.append(
                Image.fromarray(m, mode='L').resize((target_width, target_height), Image.Resampling.BILINEAR)
            )

        logger.debug(f"Running inference on chunk {chunk_idx}/{total_chunks} ({len(chunk_frames_pil)} frames)")

        torch.cuda.empty_cache()

        # Build a sub-status callback that includes chunk context
        def _sub_status(msg: str, _ci=chunk_idx, _tc=total_chunks) -> None:
            full = f"Chunk {_ci}/{_tc} — {msg}"
            if on_status:
                on_status(full)

        chunk_output = pipeline.run(
            cond_frames=chunk_frames_pil,
            mask_frames=chunk_masks_pil,
            seed=42,
            mask_cond_mode="vae",
            on_status=_sub_status,
        )

        # Resize output back to original resolution
        chunk_output_resized = [f.resize(original_size, Image.Resampling.BILINEAR)
                                for f in chunk_output]

        yield [np.array(f) for f in chunk_output_resized]

def save_video(frames: List[np.ndarray], output_path: str, fps: float):
    """
    Save frames as a video file.

    Args:
        frames (List[np.ndarray]): List of frames (RGB).
        output_path (str): Output video path.
        fps (float): Frames per second.
    """
    if not frames:
        return

    # Unicode-safe write (issue #184): raw cv2.VideoWriter fails on
    # non-ASCII paths on Windows.
    from backend.frame_io import open_video_writer  # Lazy import: avoid cycles

    height, width = frames[0].shape[:2]
    fourcc = cv2.VideoWriter_fourcc(*'mp4v')
    out = open_video_writer(output_path, fourcc, fps, (width, height))
    
    for frame in frames:
        # Convert RGB to BGR for OpenCV
        frame_bgr = cv2.cvtColor(frame, cv2.COLOR_RGB2BGR)
        out.write(frame_bgr)
    
    out.release()
    logger.info(f"Saved video to {output_path}")

