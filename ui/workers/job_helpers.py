"""Helper functions for GPU job creation and snapshot management."""
from __future__ import annotations

import copy

from backend import (
    ClipEntry,
    ClipState,
    GPUJob,
    InferenceParams,
    JobType,
    OutputConfig,
)


def create_job_snapshot(
    clip: ClipEntry,
    params: InferenceParams | None = None,
    job_type: JobType = JobType.INFERENCE,
    resume: bool = False,
    chunk_size: int = 16,
    birefnet_usage: str = "Matting",
    chroma_params: dict | None = None,
) -> GPUJob:
    """Create a frozen job snapshot for the queue.

    The clip is deep-copied so watcher rescans or UI mutations
    cannot desync the running job (Codex critical finding).

    Args:
        clip: The clip to process (will be deep-copied).
        params: Inference parameters (for INFERENCE jobs).
        job_type: Type of GPU job.
        resume: If True, populate skip_stems from existing outputs.
        chunk_size: VideoMaMa chunk size.
        birefnet_usage: BiRefNet model variant name (for BIREFNET_ALPHA jobs).
    """
    # Deep copy clip so the job holds frozen state, not a live reference
    clip_snapshot = copy.deepcopy(clip)

    job_params: dict = {"_clip_snapshot": clip_snapshot}

    if job_type == JobType.INFERENCE:
        if params is None:
            params = InferenceParams()
        job_params["_inference_params"] = copy.deepcopy(params)
        if resume:
            job_params["_skip_stems"] = clip.completed_stems()
    elif job_type in (JobType.SAM2_PREVIEW, JobType.SAM2_TRACK):
        if params is not None:
            job_params["_inference_params"] = copy.deepcopy(params)
    elif job_type == JobType.VIDEOMAMA_ALPHA:
        job_params["_chunk_size"] = chunk_size
    elif job_type == JobType.BIREFNET_ALPHA:
        job_params["_birefnet_usage"] = birefnet_usage
    elif job_type == JobType.CHROMA_KEY_ALPHA:
        job_params["_chroma_params"] = chroma_params or {}

    return GPUJob(
        job_type=job_type,
        clip_name=clip.name,
        params=job_params,
    )


def clip_frame_range(clip: ClipEntry) -> tuple[int, int] | None:
    """In/out range to publish, or None for the whole clip."""
    if clip.in_out_range is None:
        return None
    return (clip.in_out_range.in_point, clip.in_out_range.out_point)


def reset_complete_state(clip: ClipEntry) -> None:
    """Move a COMPLETE clip back to the state its assets support, for a re-run."""
    if clip.state != ClipState.COMPLETE:
        return
    if clip.alpha_asset is not None:
        clip.transition_to(ClipState.READY)
    elif clip.mask_asset is not None:
        clip.transition_to(ClipState.MASKED)
    else:
        clip.transition_to(ClipState.RAW)


def create_inference_job(
    clip: ClipEntry,
    params: InferenceParams,
    output_config: OutputConfig,
    *,
    frame_range: tuple[int, int] | None = None,
    resume: bool = False,
) -> GPUJob:
    """Create a CorridorKey 2.0 INFERENCE job with frozen params/output/range."""
    job = create_job_snapshot(clip, params, resume=resume)
    job.params["_output_config"] = copy.deepcopy(output_config)
    if frame_range is not None:
        job.params["_frame_range"] = tuple(frame_range)
    return job
