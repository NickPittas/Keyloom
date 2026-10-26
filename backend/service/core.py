"""CorridorKeyService — clean backend API for the UI and CLI.

This module wraps all processing logic into a service layer. The UI never
calls inference engines directly — it calls methods here which handle
validation, state transitions, and error reporting.

Model Residency Policy:
    Only ONE heavy model is loaded at a time. Before loading a new
    model type, the previous is unloaded and VRAM freed via
    torch.cuda.empty_cache(). This prevents OOM on 24GB cards.
"""
from __future__ import annotations

import logging
import threading
from dataclasses import dataclass, asdict
from typing import Optional

from ..clip_state import ClipEntry, ClipState, scan_clips_dir
from ..job_queue import GPUJobQueue
from .model_manager import ModelManagerMixin, _ActiveModel
from .frame_ops import FrameOpsMixin
from .inference import InferenceMixin
from .pipelines import PipelinesMixin

logger = logging.getLogger(__name__)

VALID_VRAM_PROFILES = ("auto", "low")
VALID_EXR_COMPRESSIONS = ("dwab", "piz", "zip", "none")
VALID_OUTPUT_FORMATS = ("exr", "png")
VALID_HINT_MODES = ("none", "full", "keyframes")
VALID_TEMPORAL_WINDOWS = ("auto", "80/40", "48/24", "24/12", "8/4", "1/1")


@dataclass
class InferenceParams:
    """CorridorKey 2.0 inference parameters (mapped to the engine config)."""
    input_is_linear: bool = False  # frame-dir input is scene-linear (cfg.frame_dir_linear)
    use_alpha_hint: bool = False  # external AlphaHint off by default
    hint_quality: float = 0.95  # trust in external hint; only used if use_alpha_hint
    carry_hint: bool = True
    carry_hint_quality: float = 0.95
    vram_profile: str = "auto"  # "auto" | "low"
    despill_strength: float = 0.5
    cutout_linear: bool = False  # carried to cfg; no effect on EXR output
    hint_mode: str | None = None  # "none" | "full" | "keyframes"; None = from use_alpha_hint
    temporal_window: str = "auto"

    def __post_init__(self) -> None:
        if self.hint_mode is None:
            self.hint_mode = "full" if self.use_alpha_hint else "none"
        elif self.hint_mode not in VALID_HINT_MODES:
            raise ValueError(
                f"hint_mode must be one of {VALID_HINT_MODES}, got {self.hint_mode!r}"
            )
        self.use_alpha_hint = self.hint_mode != "none"
        if self.temporal_window not in VALID_TEMPORAL_WINDOWS:
            raise ValueError(
                f"temporal_window must be one of {VALID_TEMPORAL_WINDOWS}, "
                f"got {self.temporal_window!r}"
            )
        if self.vram_profile not in VALID_VRAM_PROFILES:
            raise ValueError(
                f"vram_profile must be one of {VALID_VRAM_PROFILES}, got {self.vram_profile!r}"
            )
        for name in ("hint_quality", "carry_hint_quality", "despill_strength"):
            v = getattr(self, name)
            if not 0.0 <= v <= 1.0:
                raise ValueError(f"{name} must be in [0, 1], got {v!r}")

    def to_dict(self) -> dict:
        return asdict(self)

    @classmethod
    def from_dict(cls, d: dict) -> 'InferenceParams':
        known = {f.name for f in cls.__dataclass_fields__.values()}
        return cls(**{k: v for k, v in d.items() if k in known})


@dataclass
class OutputConfig:
    """Which output types to produce and their format."""
    fg_enabled: bool = True
    fg_format: str = "exr"   # "exr" or "png"
    matte_enabled: bool = True
    matte_format: str = "exr"
    comp_enabled: bool = True
    comp_format: str = "png"
    processed_enabled: bool = True
    processed_format: str = "exr"
    exr_compression: str = "dwab"  # "dwab", "piz", "zip", or "none"

    def __post_init__(self) -> None:
        for name in ("fg_format", "matte_format", "comp_format", "processed_format"):
            v = getattr(self, name)
            if v not in VALID_OUTPUT_FORMATS:
                raise ValueError(f"{name} must be one of {VALID_OUTPUT_FORMATS}, got {v!r}")
        if self.exr_compression not in VALID_EXR_COMPRESSIONS:
            raise ValueError(
                f"exr_compression must be one of {VALID_EXR_COMPRESSIONS}, got {self.exr_compression!r}"
            )

    def validate_selection(self) -> None:
        """Raise ValueError if no output is enabled."""
        if not self.enabled_outputs:
            raise ValueError("No outputs selected; enable at least one output")

    def to_dict(self) -> dict:
        return asdict(self)

    @classmethod
    def from_dict(cls, d: dict) -> 'OutputConfig':
        known = {f.name for f in cls.__dataclass_fields__.values()}
        return cls(**{k: v for k, v in d.items() if k in known})

    @property
    def enabled_outputs(self) -> list[str]:
        """Return list of enabled output names for manifest."""
        out = []
        if self.fg_enabled:
            out.append("fg")
        if self.matte_enabled:
            out.append("matte")
        if self.comp_enabled:
            out.append("comp")
        if self.processed_enabled:
            out.append("processed")
        return out


@dataclass
class FrameResult:
    """Result summary for a single processed frame (no numpy in this struct)."""
    frame_index: int
    input_stem: str
    success: bool
    warning: Optional[str] = None


class CorridorKeyService(
    ModelManagerMixin,
    FrameOpsMixin,
    InferenceMixin,
    PipelinesMixin,
):
    """Main backend service — scan, validate, process, write.

    Usage:
        service = CorridorKeyService()
        clips = service.scan_clips("/path/to/ClipsForInference")
        ready = service.get_clips_by_state(clips, ClipState.READY)

        for clip in ready:
            params = InferenceParams(despill_strength=0.8)
            service.run_inference(clip, params, on_progress=my_callback)
    """

    def __init__(self):
        self._v2_engine = None
        self._gvm_processor = None
        self._sam2_tracker = None
        self._videomama_pipeline = None
        self._matanyone2_processor = None
        self._birefnet_processor = None
        self._active_model = _ActiveModel.NONE
        self._device: str = 'cpu'
        self._device_detected: bool = False
        self._job_queue: Optional[GPUJobQueue] = None
        self._sam2_model_id: str = "facebook/sam2.1-hiera-base-plus"
        self._sam2_tracker_model_id: Optional[str] = None  # id the live tracker was built with
        # GPU mutex — serializes ALL model operations
        self._gpu_lock = threading.Lock()

    @property
    def job_queue(self) -> GPUJobQueue:
        """Lazy-init GPU job queue (only needed when UI is running)."""
        if self._job_queue is None:
            self._job_queue = GPUJobQueue()
        return self._job_queue

    @property
    def sam2_model_id(self) -> str:
        """Current SAM2 checkpoint preference."""
        return self._sam2_model_id

    def set_sam2_model(self, model_id: str) -> None:
        """Update the SAM2 checkpoint used for future tracking jobs."""
        if not model_id or model_id == self._sam2_model_id:
            return
        logger.info("SAM2 model preference changed: %s -> %s", self._sam2_model_id, model_id)
        if self._sam2_tracker is not None and self._sam2_tracker_model_id is None:
            self._sam2_tracker_model_id = self._sam2_model_id  # externally injected tracker
        self._sam2_model_id = model_id
        # Never block the GUI thread on a running GPU job: if the lock is busy the
        # stale tracker is replaced by the next _get_sam2_tracker() call (under lock).
        if not self._gpu_lock.acquire(blocking=False):
            return
        try:
            self._drop_stale_sam2_tracker()
        finally:
            self._gpu_lock.release()

    def _drop_stale_sam2_tracker(self) -> None:
        """Offload the SAM2 tracker if it was loaded for another model id. Caller holds the lock."""
        if self._sam2_tracker is None or self._sam2_tracker_model_id in (None, self._sam2_model_id):
            return
        self._safe_offload(self._sam2_tracker)
        self._sam2_tracker = None
        if self._active_model == _ActiveModel.SAM2:
            self._active_model = _ActiveModel.NONE
        try:
            import torch
            if torch.cuda.is_available():
                torch.cuda.empty_cache()
        except Exception:
            logger.debug("CUDA cache clear skipped after SAM2 model switch", exc_info=True)

    # --- Device Utilities ---

    def detect_device(self) -> str:
        """Detect compute device. CorridorKey 2.0 keying requires CUDA."""
        self._device_detected = True
        try:
            import torch
            cuda = torch.cuda.is_available()
        except ImportError:
            cuda = False
        if cuda:
            self._device = 'cuda'
        else:
            self._device = 'cpu'
            logger.error("Keyloom Key engine requires an NVIDIA CUDA GPU; keying unavailable")
        logger.info(f"Compute device: {self._device}")
        return self._device

    def cuda_keying_available(self) -> bool:
        """True if a CUDA GPU is available for CorridorKey 2.0 keying."""
        if not self._device_detected:
            self.detect_device()
        return self._device == 'cuda'

    def get_vram_info(self) -> dict[str, float]:
        """Get GPU VRAM info in GB. Returns empty dict if not CUDA."""
        try:
            import torch
            if not torch.cuda.is_available():
                return {}
            props = torch.cuda.get_device_properties(0)
            total_bytes = props.total_mem
            reserved = torch.cuda.memory_reserved(0)
            return {
                'total': total_bytes / (1024**3),
                'reserved': reserved / (1024**3),
                'allocated': torch.cuda.memory_allocated(0) / (1024**3),
                'free': (total_bytes - reserved) / (1024**3),
                'name': torch.cuda.get_device_name(0),
            }
        except Exception as e:
            logger.debug(f"VRAM query failed: {e}")
            return {}

    # --- Clip Scanning ---

    def scan_clips(
        self, clips_dir: str, allow_standalone_videos: bool = True,
    ) -> list[ClipEntry]:
        """Scan a directory for clip folders."""
        return scan_clips_dir(clips_dir, allow_standalone_videos=allow_standalone_videos)

    def get_clips_by_state(
        self,
        clips: list[ClipEntry],
        state: ClipState,
    ) -> list[ClipEntry]:
        """Filter clips by state."""
        return [c for c in clips if c.state == state]
