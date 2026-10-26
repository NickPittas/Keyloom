"""CorridorKey 2.0 whole-clip inference: prepare inputs, run the engine, publish outputs."""
from __future__ import annotations

import os
import shutil

from ..clip_state import ClipEntry
from ..errors import CorridorKeyError



# float32 channels per runtime product (uncompressed EXR upper bound; do not assume compression)
_PRODUCT_CHANNELS = {"alpha": 1, "fg": 3, "checker": 3, "cutout": 4}


def _scratch_root() -> str:
    root = os.environ.get("CORRIDORKEY_V2_SCRATCH_DIR")
    if not root:
        from ..install_paths import discover, platform_scratch
        install = discover()
        if install is not None:
            root = install.scratch or platform_scratch()
            if root is None:
                raise RuntimeError("No writable scratch location; configure roots.scratch in the installation descriptor.")
    if not root:
        from ..v2_paths import get_v2_models_dir
        root = str(get_v2_models_dir().parent)
    os.makedirs(root, exist_ok=True)
    return str(root)


class InferenceMixin:
    """Mixin providing the CorridorKey 2.0 inference pipeline for CorridorKeyService."""

    # ------------------------------------------------------------------ helpers

    @staticmethod
    def _v2_ffmpeg_bin() -> str:
        from ..ffmpeg_tools.discovery import find_ffmpeg
        return find_ffmpeg() or "ffmpeg"

    def _v2_check_disk(self, clip: ClipEntry, cfg, root: str, count=None, dims=None) -> None:
        """Fail early if scratch EXRs (assumed uncompressed float32) cannot fit.

        ``count``/``dims`` override the whole-clip probe (range jobs must not read frames
        outside their range).
        """
        dims = dims or clip.input_asset.get_dimensions()
        count = count or clip.input_asset.frame_count
        if not dims or not count:
            return
        channels = sum(_PRODUCT_CHANNELS[p] for p in self._v2_required_products(cfg))
        need = dims[0] * dims[1] * count * channels * 4
        free = shutil.disk_usage(root).free
        if free < need:
            raise CorridorKeyError(
                f"Not enough free disk space for the Key model scratch files in {root}: "
                f"need ~{need / 2**30:.1f} GB, have {free / 2**30:.1f} GB. "
                "Free space or set CORRIDORKEY_V2_SCRATCH_DIR to a larger drive."
            )

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

