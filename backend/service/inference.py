"""CorridorKey 2.0 whole-clip inference: prepare inputs, run the engine, publish outputs."""
from __future__ import annotations

import logging
import os
import shutil
import tempfile
import time
from typing import Callable, Optional

from ..clip_state import ClipEntry, ClipState
from ..errors import CorridorKeyError, InvalidStateTransitionError, JobCancelledError
from ..job_queue import GPUJob
from ..validators import ensure_output_dirs
from .v2_inputs import prepare_v2_inputs

logger = logging.getLogger(__name__)

_MANIFEST_NAME = ".corridorkey_manifest.json"
# float32 channels per runtime product (uncompressed EXR upper bound; do not assume compression)
_PRODUCT_CHANNELS = {"alpha": 1, "fg": 3, "checker": 3, "cutout": 4}


def _scratch_root() -> str:
    root = os.environ.get("CORRIDORKEY_V2_SCRATCH_DIR")
    if not root:
        from ..v2_paths import get_v2_models_dir
        root = str(get_v2_models_dir().parent)
    os.makedirs(root, exist_ok=True)
    return root


class InferenceMixin:
    """Mixin providing the CorridorKey 2.0 inference pipeline for CorridorKeyService."""

    def run_inference(
        self,
        clip: ClipEntry,
        params,
        job: Optional[GPUJob] = None,
        on_progress: Optional[Callable[..., None]] = None,
        on_warning: Optional[Callable[[str], None]] = None,
        on_status: Optional[Callable[[str], None]] = None,
        skip_stems: Optional[set[str]] = None,
        output_config=None,
        frame_range: Optional[tuple[int, int]] = None,
    ) -> list:
        """Key one clip with CorridorKey 2.0.

        The whole clip is always inferred (temporal context); ``frame_range``
        (inclusive) and ``skip_stems`` only limit which frames are published.
        """
        from .core import FrameResult, OutputConfig

        if clip.input_asset is None:
            raise CorridorKeyError(f"Clip '{clip.name}' has no input asset")
        mode = self._v2_hint_mode(params)
        if mode != "none" and clip.alpha_asset is None:
            raise CorridorKeyError(
                f"Clip '{clip.name}' has no AlphaHint; choose hint mode 'None' or generate/import an AlphaHint"
            )
        if mode == "keyframes":
            if clip.hint_keyframes_error:
                raise CorridorKeyError(
                    f"Clip '{clip.name}': keyframe marks unusable: {clip.hint_keyframes_error}")
            if not clip.hint_keyframes:
                raise CorridorKeyError(
                    f"Clip '{clip.name}' has no keyframe marks; mark at least one frame")
        cfg = output_config or OutputConfig()
        cfg.validate_selection()

        def check_cancel() -> None:
            if job is not None and job.is_cancelled:
                raise JobCancelledError(clip.name)

        t_start = time.monotonic()
        if on_status:
            on_status("Loading model...")
        root = _scratch_root()
        self._v2_check_disk(clip, cfg, root)
        with tempfile.TemporaryDirectory(prefix="run_", dir=root) as scratch:
            with self._gpu_lock:
                engine = self._get_v2_engine(on_status)
                prepared = prepare_v2_inputs(
                    clip, use_alpha_hint=mode != "none", hint_mode=mode,
                    staging_dir=os.path.join(scratch, "staging"), check_cancel=check_cancel,
                )
                check_cancel()
                os.makedirs(os.path.join(scratch, "tmp"), exist_ok=True)
                result = engine.run_clip(
                    prepared.input_path, os.path.join(scratch, "out"), params,
                    outputs=self._v2_required_products(cfg),
                    temp_dir=os.path.join(scratch, "tmp"),
                    frame_dir_fps=prepared.fps,
                    hint_video_path=prepared.hint_path,
                    initial_hint_path=prepared.initial_hint_path,
                    initial_hint_index=prepared.initial_hint_index,
                    hint_frame_indices=prepared.hint_frame_indices,
                    ffmpeg_bin=self._v2_ffmpeg_bin(),
                    on_progress=(lambda ph, cur, tot: on_progress(clip.name, cur, tot, phase=ph))
                    if on_progress else None,
                    check_cancel=check_cancel, on_status=on_status,
                )
                for w in getattr(result, "warnings", ()) or ():
                    logger.warning("Clip '%s': %s", clip.name, w)
                    if on_warning:
                        on_warning(w)
                logger.info(
                    "Clip '%s': v2 run done (hann_chunk=%s hann_stride=%s low_vram=%s)",
                    clip.name, result.hann_chunk, result.hann_stride, result.low_vram,
                )
                results, bounds = self._v2_publish(
                    clip, params, cfg, prepared, result, frame_range, skip_stems,
                    check_cancel, on_progress, on_warning, on_status, FrameResult,
                )
        return self._finalize_inference(clip, results, bounds, prepared.frame_count,
                                        t_start, on_warning=on_warning)

    # ------------------------------------------------------------------ helpers

    @staticmethod
    def _v2_ffmpeg_bin() -> str:
        from ..ffmpeg_tools.discovery import find_ffmpeg
        return find_ffmpeg() or "ffmpeg"

    def _v2_check_disk(self, clip: ClipEntry, cfg, root: str) -> None:
        """Fail early if scratch EXRs (assumed uncompressed float32) cannot fit."""
        dims = clip.input_asset.get_dimensions()
        count = clip.input_asset.frame_count
        if not dims or not count:
            return
        channels = sum(_PRODUCT_CHANNELS[p] for p in self._v2_required_products(cfg))
        need = dims[0] * dims[1] * count * channels * 4
        free = shutil.disk_usage(root).free
        if free < need:
            raise CorridorKeyError(
                f"Not enough free disk space for CorridorKey 2.0 scratch files in {root}: "
                f"need ~{need / 2**30:.1f} GB, have {free / 2**30:.1f} GB. "
                "Free space or set CORRIDORKEY_V2_SCRATCH_DIR to a larger drive."
            )

    @staticmethod
    def _v2_hint_mode(params) -> str:
        mode = getattr(params, "hint_mode", None)
        return mode or ("full" if params.use_alpha_hint else "none")

    @staticmethod
    def _v2_effective_keyframes(params, prepared) -> Optional[list]:
        """Absolute source indices actually sent to the engine (Keyframes mode only)."""
        if InferenceMixin._v2_hint_mode(params) != "keyframes":
            return None
        if prepared.hint_frame_indices is not None:
            return [int(i) for i in prepared.hint_frame_indices]
        if prepared.initial_hint_path:
            return [int(prepared.initial_hint_index)]
        return []

    @staticmethod
    def _v2_resume_compatible(manifest, cfg, params, source_stems, hint_keyframes=None) -> bool:
        if not manifest:
            return False
        recorded = manifest.get("params")
        if isinstance(recorded, dict):
            # Pre-hint-mode manifests: only the pre-feature meaning is provable
            # (legacy boolean -> none/full, automatic temporal window).
            recorded = dict(recorded)
            recorded.setdefault(
                "hint_mode", "full" if recorded.get("use_alpha_hint") else "none")
            recorded.setdefault("temporal_window", "auto")
        if hint_keyframes is not None:
            hint_keyframes = [int(i) for i in hint_keyframes]
        return bool(
            manifest.get("engine") == "corridorkey-v2"
            and recorded == params.to_dict()
            and manifest.get("hint_keyframes") == hint_keyframes
            and manifest.get("enabled_outputs") == cfg.enabled_outputs
            and manifest.get("formats") == {
                "fg": cfg.fg_format, "matte": cfg.matte_format,
                "comp": cfg.comp_format, "processed": cfg.processed_format,
            }
            and manifest.get("source_stems") == list(source_stems)
        )

    def _v2_publish(self, clip, params, cfg, prepared, result, frame_range, skip_stems,
                    check_cancel, on_progress, on_warning, on_status, FrameResult):
        """Publish runtime outputs to EZ folders. Returns (results, (start, end))."""
        dirs = ensure_output_dirs(clip.output_dir)
        stems = prepared.source_stems
        kf = self._v2_effective_keyframes(params, prepared)
        n = len(stems)
        start, end = (0, n - 1) if frame_range is None else (
            max(0, frame_range[0]), min(n - 1, frame_range[1]))
        indices = list(range(start, end + 1))
        skip = set(skip_stems or ())
        if skip:
            manifest = self._v2_read_manifest(dirs["root"])
            if not self._v2_resume_compatible(manifest, cfg, params, stems, kf):
                skip = set()
                if on_warning:
                    on_warning("Existing outputs came from a different run/settings; "
                               "re-publishing all selected frames.")
        # None = full clip; clip_state only treats full-range manifests as COMPLETE.
        full = start == 0 and end == n - 1
        manifest_kw = dict(source_stems=stems, frame_range=None if full else [start, end],
                           hint_keyframes=kf)
        self._write_manifest(dirs["root"], cfg, params, complete=False, **manifest_kw)
        if on_status:
            on_status("Writing outputs...")
        total = len(indices)
        done = [0]

        def on_frame(_i, _stem):
            done[0] += 1
            if on_progress:
                on_progress(clip.name, done[0], total, phase="publish")

        published = set(self._publish_v2_outputs(
            result.product_dirs, dirs, stems, cfg, clip.name,
            publish_indices=indices, skip_stems=skip,
            check_cancel=check_cancel, on_frame=on_frame, on_warning=on_warning,
        ))
        self._copy_v2_settings(result.settings_path, dirs["root"])
        self._write_manifest(dirs["root"], cfg, params, complete=True, **manifest_kw)
        results = [
            FrameResult(i, stems[i], True) if stems[i] in published
            else FrameResult(i, stems[i], True, "resumed (skipped)")
            for i in indices
        ]
        return results, (start, end)

    @staticmethod
    def _v2_read_manifest(output_root: str) -> Optional[dict]:
        import json
        try:
            with open(os.path.join(output_root, _MANIFEST_NAME)) as f:
                return json.load(f)
        except (OSError, ValueError):
            return None

    def _finalize_inference(
        self,
        clip: ClipEntry,
        results: list,
        bounds: tuple[int, int],
        num_frames: int,
        t_start: float,
        on_warning: Optional[Callable[[str], None]] = None,
    ) -> list:
        """Log timing and mark the clip COMPLETE only if the whole clip was published."""
        t_total = time.monotonic() - t_start
        published = sum(1 for r in results if r.success and not r.warning)
        logger.info(
            "Clip '%s': inference complete (range %d-%d). %d published, %d skipped in %.1fs",
            clip.name, bounds[0], bounds[1], published, len(results) - published, t_total,
        )
        full = bounds[0] == 0 and bounds[1] >= num_frames - 1
        if full and all(r.success for r in results) and clip.state != ClipState.COMPLETE:
            try:
                clip.transition_to(ClipState.COMPLETE)
            except InvalidStateTransitionError as e:
                logger.warning("Clip '%s': state transition to COMPLETE failed: %s", clip.name, e)
        return results
