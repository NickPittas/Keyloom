"""Temporal-window (Hann chunk/stride) VRAM estimates for CorridorKey 2.0.

Thin wrapper over the vendored engine estimator: no VRAM numbers live here.
The engine is imported lazily, only inside the estimator.
"""
from __future__ import annotations

from dataclasses import dataclass

TEMPORAL_PRESETS = ("auto", "80/40", "48/24", "24/12", "8/4", "1/1")
VRAM_PROFILES = ("auto", "low")
SHAPE_QUANTUM = 128  # engine shape_quantum default

_FLOOR_CAVEAT = ("Normal-memory estimates are floored to the engine's smallest "
                 "calibrated window, so this figure is conservative.")


@dataclass(frozen=True)
class TemporalEstimate:
    chunk: int | None
    stride: int | None
    low_vram: bool | None
    peak_gib: float | None
    required_gib: float | None
    budget_gib: float | None
    over_budget: bool
    caveat: str | None = None


def _pad(value: int) -> int:
    return -(-value // SHAPE_QUANTUM) * SHAPE_QUANTUM


def query_vram_budget_gib() -> float | None:
    """Free GPU memory plus memory this process already reserved; None if unavailable."""
    try:
        import torch

        if not torch.cuda.is_available():
            return None
        free_bytes, _ = torch.cuda.mem_get_info()
        return (free_bytes + torch.cuda.memory_reserved()) / 2**30
    except (ImportError, RuntimeError, OSError):
        return None


def estimate_temporal_window(width: int | None, height: int | None, frame_count: int,
                             temporal_window: str = "auto", vram_profile: str = "auto",
                             budget_gib: float | None = None) -> TemporalEstimate:
    if temporal_window not in TEMPORAL_PRESETS:
        raise ValueError(f"unknown temporal window {temporal_window!r}")
    if vram_profile not in VRAM_PROFILES:
        raise ValueError(f"unknown vram profile {vram_profile!r}")
    low = vram_profile == "low"
    known_shape = bool(width and height and width > 0 and height > 0
                       and frame_count and frame_count > 0)
    shape_hw = (_pad(height), _pad(width)) if known_shape else None

    chunk = stride = None
    if temporal_window != "auto":
        chunk, stride = (int(p) for p in temporal_window.split("/"))
    elif known_shape and budget_gib is not None:
        if low:
            from .corridorkey_v2 import _select_low_vram_window

            chunk, stride = _select_low_vram_window(shape_hw, frame_count, budget_gib)
        else:
            from CorridorKeyV2.corridorkey import runtime as rt

            chunk, stride, low, _ = rt.select_vram_profile(shape_hw, frame_count, budget_gib)

    if chunk is None or not known_shape:
        return TemporalEstimate(chunk, stride, low if (chunk or low) else None,
                                None, None, budget_gib, False)

    from CorridorKeyV2.corridorkey import runtime as rt

    frames = min(chunk, frame_count)
    peak = rt.estimate_peak_vram_gb(shape_hw[0] * shape_hw[1] / 1e6, frames, low)
    required = peak * rt._VRAM_MARGIN_SCALE + rt._VRAM_MARGIN_GB
    caveat = (_FLOOR_CAVEAT if not low and frames < rt._VRAM_PEAK_GB[False][0][0]
              else None)
    return TemporalEstimate(chunk, stride, low, peak, required, budget_gib,
                            budget_gib is not None and required > budget_gib, caveat)
