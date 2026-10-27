"""Model loading, residency management, and VRAM lifecycle.

Implements the single-model-in-VRAM policy: only one heavy model
stays loaded at a time. Switching triggers offload + gc + cache clear.
"""
from __future__ import annotations

import logging
import os
import sys
import time
from enum import Enum
from typing import Callable

from .helpers import BASE_DIR, _import_matanyone2_processor_class

logger = logging.getLogger(__name__)


class _ActiveModel(Enum):
    """Tracks which heavy model is currently loaded in VRAM."""
    NONE = "none"
    INFERENCE = "inference"
    GVM = "gvm"
    SAM2 = "sam2"
    VIDEOMAMA = "videomama"
    MATANYONE2 = "matanyone2"
    BIREFNET = "birefnet"


class ModelManagerMixin:
    """Mixin providing model loading, residency, and VRAM management."""

    @staticmethod
    def _vram_allocated_mb() -> float:
        """Return current VRAM allocated in MB, or 0 if unavailable."""
        try:
            import torch
            if torch.cuda.is_available():
                return torch.cuda.memory_allocated(0) / (1024 ** 2)
        except Exception:
            pass
        return 0.0

    @staticmethod
    def _safe_offload(obj: object) -> None:
        """Move a model's GPU tensors to CPU before dropping the reference.

        Handles diffusers pipelines (.to('cpu')), plain nn.Modules (.cpu()),
        and objects with an explicit unload() method.
        """
        if obj is None:
            return
        name = type(obj).__name__
        logger.debug(f"Offloading model: {name}")
        try:
            if hasattr(obj, 'unload'):
                obj.unload()
            elif hasattr(obj, 'to'):
                obj.to('cpu')
            elif hasattr(obj, 'cpu'):
                obj.cpu()
            else:
                logger.warning(f"Model {name} has no .to()/.cpu()/.unload() — VRAM may leak")
        except Exception as e:
            logger.warning(f"Model offload failed for {name}: {e}")

    def _ensure_model(self, needed: _ActiveModel, on_status=None) -> None:
        """Model residency manager — unload current model if switching types.

        Only ONE heavy model stays in VRAM at a time. Before loading a
        different model, the previous is moved to CPU and dereferenced.

        Callers must hold ``_gpu_lock`` so a switch never unloads a model
        that a running job is using.
        """
        if self._active_model == needed:
            return

        # Unload whatever is currently loaded
        if self._active_model != _ActiveModel.NONE:
            # Snapshot VRAM before unload for leak diagnosis
            vram_before_mb = self._vram_allocated_mb()
            if on_status:
                on_status(f"Switching {self._active_model.value} -> {needed.value}...")
            logger.info(f"Unloading {self._active_model.value} model for {needed.value}"
                        f" (VRAM before: {vram_before_mb:.0f}MB)")

            t0 = time.monotonic()
            if on_status:
                on_status(f"Offloading {self._active_model.value}...")
            if self._active_model == _ActiveModel.INFERENCE:
                if self._v2_engine is not None:
                    self._v2_engine.unload()
                    self._v2_engine = None
            elif self._active_model == _ActiveModel.GVM:
                # GVM has circular refs (pipe <-> vae <-> unet) — break them
                # explicitly so gc can reclaim everything in one pass.
                gvm = self._gvm_processor
                self._gvm_processor = None
                if gvm is not None:
                    self._safe_offload(gvm)
                    # Break circular references inside the diffusion pipeline
                    for attr in ('pipe', 'vae', 'unet', 'scheduler'):
                        try:
                            setattr(gvm, attr, None)
                        except Exception:
                            pass
                    del gvm
            elif self._active_model == _ActiveModel.SAM2:
                self._safe_offload(self._sam2_tracker)
                self._sam2_tracker = None
            elif self._active_model == _ActiveModel.VIDEOMAMA:
                self._safe_offload(self._videomama_pipeline)
                self._videomama_pipeline = None
            elif self._active_model == _ActiveModel.MATANYONE2:
                self._safe_offload(self._matanyone2_processor)
                self._matanyone2_processor = None
            elif self._active_model == _ActiveModel.BIREFNET:
                self._safe_offload(self._birefnet_processor)
                self._birefnet_processor = None
            logger.info(f"_safe_offload took {time.monotonic() - t0:.1f}s")

            import gc as _gc
            t0 = time.monotonic()
            # Two GC passes: first breaks cycles, second reclaims freed refs
            if on_status:
                on_status("Releasing Python references...")
            _gc.collect()
            _gc.collect()
            logger.info(f"gc.collect took {time.monotonic() - t0:.1f}s")

            try:
                import torch
                if torch.cuda.is_available():
                    t0 = time.monotonic()
                    if on_status:
                        on_status("Waiting for CUDA to finish...")
                    torch.cuda.synchronize()
                    logger.info(f"cuda.synchronize took {time.monotonic() - t0:.1f}s")
                    t0 = time.monotonic()
                    if on_status:
                        on_status("Clearing CUDA cache...")
                    torch.cuda.empty_cache()
                    torch.cuda.reset_peak_memory_stats()
                    logger.info(f"cuda.empty_cache took {time.monotonic() - t0:.1f}s")
            except ImportError:
                logger.debug("torch not available for cache clear during model switch")

            vram_after_mb = self._vram_allocated_mb()
            freed = vram_before_mb - vram_after_mb
            logger.info(f"VRAM after unload: {vram_after_mb:.0f}MB (freed {freed:.0f}MB)")

        self._active_model = needed

    def _get_v2_engine(self, on_status=None):
        """Lazy-load the single CorridorKey 2.0 engine. Caller holds ``_gpu_lock``."""
        self._ensure_model(_ActiveModel.INFERENCE, on_status=on_status)
        if self._v2_engine is None:
            from backend.corridorkey_v2 import CorridorKeyV2Engine
            self._v2_engine = CorridorKeyV2Engine(device=self._device)
        try:
            self._v2_engine.load(on_status)
        except BaseException:
            self._v2_engine = None
            self._active_model = _ActiveModel.NONE
            raise
        return self._v2_engine

    def _get_gvm(self):
        """Lazy-load the GVM processor."""
        self._ensure_model(_ActiveModel.GVM)

        if self._gvm_processor is not None:
            return self._gvm_processor

        from gvm_core import GVMProcessor
        logger.info("Loading GVM processor...")
        t0 = time.monotonic()
        self._gvm_processor = GVMProcessor(device=self._device)
        logger.info(f"GVM loaded in {time.monotonic() - t0:.1f}s")
        return self._gvm_processor

    def _get_sam2_tracker(
        self,
        *,
        on_progress: Callable[[int, int], None] | None = None,
        on_status: Callable[[str], None] | None = None,
    ):
        """Lazy-load the optional SAM2 tracker."""
        self._drop_stale_sam2_tracker()
        self._ensure_model(_ActiveModel.SAM2)

        if self._sam2_tracker is not None:
            return self._sam2_tracker

        from sam2_tracker import SAM2Tracker

        logger.info("Loading SAM2 tracker...")
        t0 = time.monotonic()
        self._sam2_tracker = SAM2Tracker(
            model_id=self._sam2_model_id,
            device=self._device,
            vos_optimized=False,
            offload_video_to_cpu=self._device.startswith("cuda"),
            offload_state_to_cpu=False,
        )
        self._sam2_tracker_model_id = self._sam2_model_id
        if self._sam2_tracker is not None:
            self._sam2_tracker.prepare(on_progress=on_progress, on_status=on_status)
        logger.info(f"SAM2 tracker ready in {time.monotonic() - t0:.1f}s")
        return self._sam2_tracker

    def _get_videomama_pipeline(self):
        """Lazy-load the VideoMaMa inference pipeline."""
        self._ensure_model(_ActiveModel.VIDEOMAMA)

        if self._videomama_pipeline is not None:
            return self._videomama_pipeline

        sys.path.insert(0, os.path.join(BASE_DIR, "VideoMaMaInferenceModule"))
        from VideoMaMaInferenceModule.inference import load_videomama_model
        logger.info("Loading VideoMaMa pipeline...")
        t0 = time.monotonic()
        self._videomama_pipeline = load_videomama_model(device=self._device)
        logger.info(f"VideoMaMa loaded in {time.monotonic() - t0:.1f}s")
        return self._videomama_pipeline

    def _get_matanyone2(self):
        """Lazy-load the MatAnyone2 processor."""
        self._ensure_model(_ActiveModel.MATANYONE2)

        if self._matanyone2_processor is not None:
            return self._matanyone2_processor

        MatAnyone2Processor = _import_matanyone2_processor_class()
        logger.info("Loading MatAnyone2 processor...")
        t0 = time.monotonic()
        self._matanyone2_processor = MatAnyone2Processor(device=self._device)
        logger.info(f"MatAnyone2 loaded in {time.monotonic() - t0:.1f}s")
        return self._matanyone2_processor

    def _get_birefnet(self, usage: str = "Matting", on_status=None):
        """Lazy-load the BiRefNet processor for a given model variant."""
        self._ensure_model(_ActiveModel.BIREFNET, on_status=on_status)

        # If already loaded with the same usage, reuse
        if (self._birefnet_processor is not None
                and self._birefnet_processor._usage == usage):
            return self._birefnet_processor

        # Different variant requested — release the old one
        if self._birefnet_processor is not None:
            self._safe_offload(self._birefnet_processor)
            self._birefnet_processor = None

        from modules.BiRefNetModule.wrapper import BiRefNetProcessor
        logger.info(f"Loading BiRefNet ({usage})...")
        t0 = time.monotonic()
        self._birefnet_processor = BiRefNetProcessor(
            device=self._device, usage=usage,
        )
        # Trigger actual model download/load now so VRAM is claimed
        self._birefnet_processor._ensure_loaded(on_status=on_status)
        logger.info(f"BiRefNet ({usage}) loaded in {time.monotonic() - t0:.1f}s")
        return self._birefnet_processor

    def unload_engines(self) -> None:
        """Free GPU memory by unloading all engines."""
        if self._v2_engine is not None:
            self._v2_engine.unload()
            self._v2_engine = None
        self._safe_offload(self._gvm_processor)
        self._safe_offload(self._sam2_tracker)
        self._safe_offload(self._videomama_pipeline)
        self._safe_offload(self._matanyone2_processor)
        self._safe_offload(self._birefnet_processor)
        self._gvm_processor = None
        self._sam2_tracker = None
        self._videomama_pipeline = None
        self._matanyone2_processor = None
        self._birefnet_processor = None
        self._active_model = _ActiveModel.NONE
        import gc as _gc
        _gc.collect()
        try:
            import torch
            if torch.cuda.is_available():
                torch.cuda.synchronize()
                torch.cuda.empty_cache()
        except ImportError:
            logger.debug("torch not available for cache clear during unload")
        logger.info("All engines unloaded, VRAM freed")

    def is_engine_loaded(self) -> bool:
        """True if the inference engine is already loaded in VRAM."""
        return self._active_model == _ActiveModel.INFERENCE and self._v2_engine is not None and self._v2_engine.is_loaded
