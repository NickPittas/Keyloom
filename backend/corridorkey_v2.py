"""Whole-clip adapter around the vendored CorridorKey 2.0 runtime."""
from __future__ import annotations

import gc
import logging
import os
from collections.abc import Callable
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path

from .errors import CorridorKeyError
from .v2_paths import V2AssetPaths, get_v2_asset_paths, validate_v2_assets

logger = logging.getLogger(__name__)

_PRODUCTS = ("alpha", "fg", "checker", "cutout")
_ENV_KEYS = ("HF_HUB_OFFLINE", "TRANSFORMERS_OFFLINE")


class V2EngineError(CorridorKeyError):
    """CorridorKey 2.0 engine could not load or run."""


@dataclass(frozen=True)
class V2RunResult:
    clip_name: str
    output_dir: str
    product_dirs: dict[str, str]
    settings_path: str | None
    hann_chunk: int
    hann_stride: int
    low_vram: bool
    warnings: tuple[str, ...] = ()


@contextmanager
def _offline_env():
    saved = {k: os.environ.get(k) for k in _ENV_KEYS}
    try:
        yield
    finally:
        for key, value in saved.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value


def runtime_product_dir(output_dir, clip_name: str, product: str,
                        despill_strength: float) -> str:
    """Mirror of the suffix table in runtime.run_inference (EXR output)."""
    suffix = {
        "alpha": "alpha",
        "fg": "fg",
        "checker": f"checker_comp_despill{int(despill_strength * 100):03d}",
        "cutout": "cutout",
    }[product]
    return os.path.join(str(output_dir), f"{clip_name}_{suffix}_exr")


def _select_low_vram_window(shape_hw, num_frames, budget_gb) -> tuple[int, int]:
    """Pick (chunk, stride); falls back to (1, 1) when nothing fits (see _low_vram_fits)."""
    from CorridorKeyV2.corridorkey import runtime as rt

    mp = shape_hw[0] * shape_hw[1] / 1e6
    for chunk in reversed(rt._VRAM_PEAK_GB[True][0]):
        peak = rt.estimate_peak_vram_gb(mp, min(chunk, num_frames), True)
        if peak * rt._VRAM_MARGIN_SCALE + rt._VRAM_MARGIN_GB <= budget_gb:
            return chunk, max(1, chunk // 2)
    return 1, 1


def _low_vram_window_fits(shape_hw, num_frames, budget_gb) -> bool:
    from CorridorKeyV2.corridorkey import runtime as rt

    mp = shape_hw[0] * shape_hw[1] / 1e6
    return any(
        rt.estimate_peak_vram_gb(mp, min(chunk, num_frames), True) * rt._VRAM_MARGIN_SCALE
        + rt._VRAM_MARGIN_GB <= budget_gb
        for chunk in rt._VRAM_PEAK_GB[True][0])


def _free_gpu_memory() -> None:
    gc.collect()
    try:
        import torch

        if torch.cuda.is_available():
            torch.cuda.empty_cache()
    except (ImportError, RuntimeError):  # best-effort cleanup
        return


def _is_index(v) -> bool:
    return isinstance(v, int) and not isinstance(v, bool) and v >= 0


def _resolve_hint_args(params, hint_video_path, initial_hint_path, initial_hint_index,
                       hint_frame_indices):
    """Return engine (initial_path, initial_index, video_path, sparse_indices)."""
    mode = getattr(params, "hint_mode", None)
    if mode is None:
        mode = "full" if params.use_alpha_hint else "none"
    if mode == "none":
        return None, 0, None, None
    if mode == "full":
        if not hint_video_path:
            raise V2EngineError("AlphaHint enabled but no hint provided")
        return None, 0, hint_video_path, None
    if mode != "keyframes":
        raise V2EngineError(f"Unknown hint mode: {mode!r}")
    if hint_frame_indices is None:
        if not initial_hint_path:
            raise V2EngineError("Keyframes hint mode needs a reference hint path")
        if hint_video_path:
            raise V2EngineError("Single keyframe cannot be combined with a hint video")
        if not _is_index(initial_hint_index):
            raise V2EngineError(f"Invalid keyframe index: {initial_hint_index!r}")
        return str(initial_hint_path), initial_hint_index, None, None
    try:
        idx = tuple(hint_frame_indices)
    except TypeError:
        raise V2EngineError("hint_frame_indices must be a sequence of frame indices")
    if len(idx) < 2:
        raise V2EngineError(
            "Sparse keyframes need at least 2 indices; use the single-keyframe path for one")
    if not all(_is_index(i) for i in idx):
        raise V2EngineError(f"Invalid keyframe indices: {idx!r}")
    if list(idx) != sorted(set(idx)):
        raise V2EngineError(f"Keyframe indices must be sorted and unique: {idx!r}")
    if initial_hint_path:
        raise V2EngineError("Sparse keyframes cannot be combined with an initial hint path")
    if not hint_video_path:
        raise V2EngineError("Sparse keyframes need a hint video path")
    return None, 0, hint_video_path, idx


class CorridorKeyV2Engine:
    def __init__(self, asset_paths: V2AssetPaths | None = None, device: str = "cuda"):
        self._paths = asset_paths or get_v2_asset_paths()
        self._device = device
        self._model = None
        self.load_count = 0
        self.last_warnings: list[str] = []

    @property
    def is_loaded(self) -> bool:
        return self._model is not None

    def load(self, on_status: Callable[[str], None] | None = None) -> None:
        if self._model is not None:
            return
        problems = validate_v2_assets(self._paths)
        if problems:
            raise V2EngineError(
                "CorridorKey 2.0 assets are missing or invalid:\n  "
                + "\n  ".join(problems)
                + "\nRun: python scripts/setup_models.py --corridorkey"
            )
        import torch

        if not torch.cuda.is_available():
            raise V2EngineError("CorridorKey 2.0 requires an NVIDIA CUDA GPU")
        from CorridorKeyV2.corridorkey import runtime as rt

        if on_status:
            on_status("Loading CorridorKey 2.0 model...")
        paths = rt.RuntimePaths(**self._paths.as_runtime_kwargs())
        try:
            with _offline_env():
                self._model = rt.load_model(paths, self._device)
        except Exception as exc:
            self._model = None
            _free_gpu_memory()
            raise V2EngineError(f"Failed to load CorridorKey 2.0 model: {exc}") from exc
        self.load_count += 1

    def unload(self) -> None:
        self._model = None
        _free_gpu_memory()

    def build_inference_config(self, params, *, outputs, temp_dir, frame_dir_fps=24.0,
                               shape_hw=None, num_frames=None, budget_gb=None,
                               first_frame_path=None):
        from CorridorKeyV2.corridorkey.runtime import InferenceConfig

        outs = set(outputs)
        if not outs or not outs <= set(_PRODUCTS):
            raise ValueError(f"outputs must be a non-empty subset of {_PRODUCTS}: {outs}")
        self.last_warnings = []
        cfg = InferenceConfig()
        cfg.initial_hint_index = 0
        cfg.hint_frame_indices = None
        cfg.hint_quality = params.hint_quality
        cfg.carry_hint = params.carry_hint
        cfg.carry_hint_quality = params.carry_hint_quality
        cfg.despill_strength = params.despill_strength
        cfg.frame_dir_linear = params.input_is_linear
        cfg.cutout_linear = params.cutout_linear
        cfg.frame_dir_fps = float(frame_dir_fps)
        cfg.outputs = outs
        cfg.temp_dir = Path(temp_dir)
        cfg.output_format = "exr"
        cfg.exr_dtype = "float"
        cfg.model_scale = 1.0
        cfg.num_frames = -1
        cfg.start_mode = "begin"
        cfg.stage2_batch = 1
        preset = getattr(params, "temporal_window", "auto")
        if preset != "auto":
            from .v2_temporal import estimate_temporal_window

            chunk, stride = (int(x) for x in preset.split("/"))
            cfg.auto_vram = False
            cfg.low_vram = params.vram_profile == "low"
            cfg.hann_chunk, cfg.hann_stride = chunk, stride
            if shape_hw is not None and num_frames and budget_gb is not None:
                est = estimate_temporal_window(
                    shape_hw[1], shape_hw[0], num_frames, preset,
                    params.vram_profile, budget_gb)
                if est.over_budget:
                    msg = (f"Temporal window {preset} ({'low' if cfg.low_vram else 'normal'} "
                           f"VRAM) is estimated to need {est.required_gib:.1f} GiB "
                           f"(peak {est.peak_gib:.1f} GiB) but only ~{est.budget_gib:.1f} "
                           "GiB of GPU memory is available; inference may run out of memory.")
                    if est.caveat:
                        msg += " " + est.caveat
                    self.last_warnings.append(msg)
        elif params.vram_profile == "auto":
            cfg.auto_vram = True
        else:
            if shape_hw is None or num_frames is None or budget_gb is None:
                raise ValueError("low VRAM profile needs shape_hw, num_frames and budget_gb")
            cfg.auto_vram = False
            cfg.low_vram = True
            cfg.hann_chunk, cfg.hann_stride = _select_low_vram_window(
                shape_hw, num_frames, budget_gb)
            if not _low_vram_window_fits(shape_hw, num_frames, budget_gb):
                self.last_warnings.append(
                    f"Low VRAM profile: even a 1-frame window may not fit in the "
                    f"~{budget_gb:.1f} GB of free GPU memory; using the smallest window "
                    "(1 frame). Inference may run out of memory.")
        if (first_frame_path and not params.input_is_linear
                and str(first_frame_path).lower().endswith(".exr")):
            self.last_warnings.append(
                "EXR input frames are being treated as display-coded and clipped to "
                "[0, 1]. For scene-linear HDR EXR plates, enable Input is Linear.")
        return cfg

    def _probe_clip(self, input_path, params, frame_dir_fps):
        from CorridorKeyV2.corridorkey import video_io
        from CorridorKeyV2.corridorkey.runtime import InferenceConfig

        d = InferenceConfig()
        if os.path.isdir(str(input_path)):
            meta = video_io.probe_frame_dir_meta(
                str(input_path), num_frames=-1, shape_quantum=d.shape_quantum,
                fps=float(frame_dir_fps), exr_linear=params.input_is_linear,
                model_scale=1.0)
        else:
            meta = video_io.probe_video_meta(
                str(input_path), num_frames=-1, shape_quantum=d.shape_quantum,
                native_resolution=True, start_mode="begin", seed=d.seed,
                model_scale=1.0, linear_brightness=d.linear_brightness,
                linear_contrast=d.linear_contrast,
                linear_contrast_pivot=d.linear_contrast_pivot)
        from .v2_temporal import query_vram_budget_gib

        paths = meta.get("frame_paths") or ()
        return (tuple(meta["shape_hw"]), int(meta["num_frames"]),
                query_vram_budget_gib(), str(paths[0]) if paths else None)

    def run_clip(self, input_path, output_dir, params, *, outputs, temp_dir,
                 frame_dir_fps=24.0, hint_video_path=None, ffmpeg_bin="ffmpeg",
                 on_progress=None, check_cancel=None, on_status=None,
                 initial_hint_path=None, initial_hint_index=0,
                 hint_frame_indices=None) -> V2RunResult:
        eng_initial, eng_index, eng_video, eng_sparse = _resolve_hint_args(
            params, hint_video_path, initial_hint_path, initial_hint_index,
            hint_frame_indices)
        self.load(on_status)
        from CorridorKeyV2.corridorkey import runtime as rt
        from CorridorKeyV2.corridorkey.video_io import WriterConfig, raw_clip_name

        extra = {}
        manual = getattr(params, "temporal_window", "auto") != "auto"
        if params.vram_profile != "auto" or manual or os.path.isdir(str(input_path)):
            shape_hw, n, budget, first = self._probe_clip(input_path, params, frame_dir_fps)
            extra = {"shape_hw": shape_hw, "num_frames": n, "budget_gb": budget,
                     "first_frame_path": first}
        cfg = self.build_inference_config(
            params, outputs=outputs, temp_dir=temp_dir,
            frame_dir_fps=frame_dir_fps, **extra)
        for w in self.last_warnings:
            logger.warning(w)
        cfg.initial_hint_index = eng_index
        cfg.hint_frame_indices = eng_sparse
        rt.configure_low_vram_inference(self._model, rt.InferenceConfig(
            low_vram_mlp_chunk_tokens=0, low_vram_enc_frame_chunk=0,
            low_vram_swin_window_batch=0, low_vram_full_attn_query_chunk_tokens=0))
        with _offline_env():
            rt.run_inference(
                self._model, str(input_path), str(output_dir), cfg,
                WriterConfig(ffmpeg_bin=ffmpeg_bin),
                initial_hint_path=eng_initial, hint_video_path=eng_video,
                on_progress=on_progress, check_cancel=check_cancel)
        clip = raw_clip_name(str(input_path))
        settings = os.path.join(str(output_dir), f"{clip}_settings.txt")
        return V2RunResult(
            clip_name=clip,
            output_dir=str(output_dir),
            product_dirs={p: runtime_product_dir(output_dir, clip, p, params.despill_strength)
                          for p in _PRODUCTS if p in cfg.outputs},
            settings_path=settings if os.path.exists(settings) else None,
            hann_chunk=cfg.hann_chunk,
            hann_stride=cfg.hann_stride,
            low_vram=cfg.low_vram,
            warnings=tuple(self.last_warnings),
        )
