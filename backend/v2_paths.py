"""Path contract for CorridorKey 2.0 code and model assets (stdlib only)."""
from __future__ import annotations

import os
import sys
from dataclasses import dataclass, fields
from pathlib import Path

V2_ASSET_SUBDIR = "CorridorKeyV2"
MODELS_DIR_ENV = "CORRIDORKEY_V2_MODELS_DIR"


@dataclass(frozen=True)
class V2AssetPaths:
    package_root: Path
    weights: Path
    foundation_stats: Path
    rvm_ckpt: Path
    moge_checkpoint: Path
    cradio_dir: Path

    def as_runtime_kwargs(self) -> dict[str, Path]:
        return {f.name: getattr(self, f.name) for f in fields(self)}


def get_v2_package_root() -> Path:
    if getattr(sys, "frozen", False):
        base = Path(getattr(sys, "_MEIPASS", Path(sys.executable).parent))
        return base / V2_ASSET_SUBDIR
    return Path(__file__).resolve().parent.parent / V2_ASSET_SUBDIR


def _app_data_base() -> Path:
    if sys.platform == "win32":
        local = os.environ.get("LOCALAPPDATA")
        return Path(local) if local else Path.home() / "AppData" / "Local"
    if sys.platform == "darwin":
        return Path.home() / "Library" / "Application Support"
    xdg = os.environ.get("XDG_DATA_HOME")
    return Path(xdg) if xdg else Path.home() / ".local" / "share"


def get_v2_models_dir() -> Path:
    override = os.environ.get(MODELS_DIR_ENV)
    if override:
        return Path(override).expanduser()
    d = _install_descriptor_models_dir()
    if d is not None:
        return d
    return _app_data_base() / "Keyloom" / "EngineV2"


def _install_descriptor_models_dir() -> Path | None:
    """Models root from the shared install descriptor (backend/install_paths.py).

    No descriptor (developer checkout) -> None (platform default). A descriptor that exists but
    is unusable RAISES InstallDescriptorError: never silently fall back to another location.
    """
    from backend import install_paths

    install = install_paths.discover()
    if install is None:
        return None
    return install.models


def _prefer_safetensors(root: Path, stem: str) -> Path:
    legacy = root / f"{stem}.pt"
    if not (root / f"{stem}.safetensors").exists() and legacy.exists():
        return legacy
    return root / f"{stem}.safetensors"


def get_v2_asset_paths() -> V2AssetPaths:
    root = get_v2_models_dir()
    return V2AssetPaths(
        package_root=get_v2_package_root(),
        weights=_prefer_safetensors(root, "weights/corridorkey_v2"),
        foundation_stats=_prefer_safetensors(root, "weights/foundation_stats"),
        rvm_ckpt=root / "vfms/rvm/pretrain_rvm_small16_256_204031069.npz",
        moge_checkpoint=root / "vfms/moge/model.pt",
        cradio_dir=root / "vfms/cradio/C-RADIOv4-SO400M",
    )


def _check_file(label: str, path: Path) -> list[str]:
    if not path.is_file():
        return [f"{label} missing: {path}"]
    if path.stat().st_size == 0:
        return [f"{label} is empty: {path}"]
    return []


def _check_cradio(d: Path) -> list[str]:
    if not d.is_dir():
        return [f"C-RADIO directory missing: {d}"]
    problems = _check_file("C-RADIO config.json", d / "config.json")
    problems += _check_file("C-RADIO hf_model.py", d / "hf_model.py")
    if not any(p.stat().st_size for p in d.glob("*.safetensors")):
        problems.append(f"C-RADIO *.safetensors weights missing: {d}")
    return problems


def validate_v2_assets(paths: V2AssetPaths | None = None) -> list[str]:
    paths = paths or get_v2_asset_paths()
    problems: list[str] = []
    root = paths.package_root
    if not (root / "corridorkey" / "runtime.py").is_file():
        problems.append(f"Key engine (CorridorKeyV2) package missing runtime.py: {root}")
    if not (root / "third_party" / "MoGe").is_dir():
        problems.append(f"Key engine (CorridorKeyV2) third_party/MoGe missing: {root}")
    for label in ("weights", "foundation_stats", "rvm_ckpt", "moge_checkpoint"):
        problems += _check_file(label, getattr(paths, label))
    return problems + _check_cradio(paths.cradio_dir)


def v2_assets_ready(paths: V2AssetPaths | None = None) -> bool:
    return not validate_v2_assets(paths)
