"""Download model weights for EZ-CorridorKey.

Uses huggingface_hub for robust downloading with resume support,
progress bars, and idempotent behavior (skips existing files).

Usage:
    python scripts/setup_models.py --corridorkey       # Required: CorridorKey 2.0 + foundation models (~5GB)
    python scripts/setup_models.py --sam2             # SAM2 Base+ (324MB)
    python scripts/setup_models.py --sam2 large       # SAM2 Large (898MB)
    python scripts/setup_models.py --gvm               # Optional (~6GB)
    python scripts/setup_models.py --videomama          # Optional (~37GB)
    python scripts/setup_models.py --matanyone2         # Optional (~141MB)
    python scripts/setup_models.py --all                # Everything
    python scripts/setup_models.py --check              # Status report
"""

from __future__ import annotations

import argparse
import glob
import importlib.util
import json
import os
import re
import shutil
import sys
import urllib.request
from pathlib import Path

_SCRIPT_ROOT = Path(__file__).resolve().parent.parent
if str(_SCRIPT_ROOT) not in sys.path:
    sys.path.insert(0, str(_SCRIPT_ROOT))


def _proxy_guard():
    """Proxy sanitation for downloads; no-op when backend is unavailable.

    See backend/net_proxy.py: upgrades socks4 system proxies to socks5 for
    the duration of a download so httpx-based huggingface_hub can work.
    """
    try:
        if str(_SCRIPT_ROOT) not in sys.path:
            sys.path.insert(0, str(_SCRIPT_ROOT))
        from backend.net_proxy import sanitized_proxy_env
        return sanitized_proxy_env()
    except Exception:
        from contextlib import nullcontext
        return nullcontext()


def _data_root() -> Path:
    """Writable root for model downloads.

    Use the shared descriptor/platform resolution so Qt, the manager and the
    legacy setup entry points install to the same data root.
    """
    try:
        from backend.model_status import Roots
        return Roots.defaults().data_dir
    except ImportError:
        if not getattr(sys, "frozen", False):
            return _SCRIPT_ROOT
    # Standalone fallback (script run outside frozen app)
    try:
        from PySide6.QtCore import QSettings
        saved = QSettings().value("app/install_path", "", type=str)
        if saved and os.path.isdir(saved):
            return Path(saved)
    except Exception:
        pass
    if sys.platform == "darwin":
        return Path.home() / "Library" / "Application Support" / "EZ-CorridorKey"
    elif sys.platform == "win32":
        return Path(os.environ.get("APPDATA", Path.home())) / "EZ-CorridorKey"
    else:
        return Path.home() / ".local" / "share" / "EZ-CorridorKey"


# Importing this module is also how the setup wizard reuses its downloaders.
# Resolve install descriptors only when an operation runs so CLI errors can
# be returned through the structured terminal event instead of import failure.
PROJECT_ROOT = _SCRIPT_ROOT


def hf_cache_dir() -> Path:
    """HF hub cache honouring HF_HUB_CACHE / HF_HOME at call time.

    Single source of truth: backend.model_catalog.hf_hub_cache_dir, shared with
    the wizard, the status engine and the provision manager so detection and
    download can never disagree (audit 2026-10-22 mismatch fix).
    """
    from backend.model_catalog import hf_hub_cache_dir as _resolve
    return _resolve()


# Kept as a module-level name for backwards compatibility with existing
# callers; new code should call hf_cache_dir() so late env changes are seen.
HF_CACHE_DIR: Path | None = None


def _catalog():
    """Load the authoritative catalog; download paths must fail closed."""
    from backend.model_catalog import load_catalog
    return load_catalog()


def _pin(artifact_id: str) -> str | None:
    from backend.model_catalog import CatalogError
    art = _catalog().get(artifact_id)
    if art.revision_status != "pinned" or not art.revision:
        raise CatalogError(f"{artifact_id} has no immutable catalog revision; metadata refresh required")
    return art.revision

MODELS = {
    "corridorkey": {
        "size_human": "~5 GB",
        "size_bytes": 5_500_000_000,
        "required": True,
    },
    "gvm": {
        "repo_id": "geyongtao/gvm",
        "local_dir": PROJECT_ROOT / "gvm_core" / "weights",
        "check_file": "unet/diffusion_pytorch_model.safetensors",
        "size_human": "~6 GB",
        "size_bytes": 6_500_000_000,
        "required": False,
    },
    "videomama": {
        "repo_id": "SammyLim/VideoMaMa",
        "local_dir": PROJECT_ROOT / "VideoMaMaInferenceModule" / "checkpoints",
        # Loader contract (VideoMaMaInferenceModule/pipeline.py) reads
        # <checkpoints>/VideoMaMa/unet/… ; the old flat sentinel matched a
        # layout the runtime cannot load.
        "check_file": "VideoMaMa/unet/diffusion_pytorch_model.safetensors",
        "size_human": "~6.1 GB + ~1.46 GB SVD base",
        "size_bytes": 7_558_504_449,
        "required": False,
        "base_model": {
            "repo_id": "stabilityai/stable-video-diffusion-img2vid-xt",
            "subfolder": "stable-video-diffusion-img2vid-xt",
            "check_file": "stable-video-diffusion-img2vid-xt/model_index.json",
        },
    },
}

MATANYONE2_CHECKPOINT = {
    "url": "https://github.com/pq-yang/MatAnyone2/releases/download/v1.0.0/matanyone2.pth",
    "filename": "matanyone2.pth",
    "local_dir": PROJECT_ROOT / "modules" / "MatAnyone2Module" / "checkpoints",
    "size_human": "141 MB",
    "size_bytes": 141_429_115,
}

# BiRefNet: 16 model variants all live under ZhengPeng7/ on HuggingFace.
# The setup wizard offers the default "Matting" variant as an optional
# pre-download (~940 MB) so installed macOS users don't hit a lazy
# write-to-.app-bundle path on first use. Users who skip this get the
# same model lazy-downloaded at first BiRefNet click — the wrapper's
# ``_resolve_model_dir`` routes that lazy download to the same writable
# location, so cold starts still work without pre-selection.
BIREFNET_DEFAULT_CHECKPOINT = {
    "repo_id": "ZhengPeng7/BiRefNet-matting",
    "repo_name": "BiRefNet-matting",
    "local_dir": PROJECT_ROOT / "modules" / "BiRefNetModule" / "checkpoints" / "BiRefNet-matting",
    "size_human": "~940 MB",
    "size_bytes": 940_000_000,
}


def _configure_legacy_roots(*, roots=None, data_dir: Path | None = None):
    """Refresh root-derived legacy paths from one shared root resolution."""
    global PROJECT_ROOT, HF_CACHE_DIR
    if roots is None:
        from backend.model_status import Roots
        roots = Roots.defaults(data_dir=data_dir)
    PROJECT_ROOT = roots.data_dir
    HF_CACHE_DIR = roots.hf_cache
    MODELS["gvm"]["local_dir"] = PROJECT_ROOT / "gvm_core" / "weights"
    MODELS["videomama"]["local_dir"] = PROJECT_ROOT / "VideoMaMaInferenceModule" / "checkpoints"
    MATANYONE2_CHECKPOINT["local_dir"] = PROJECT_ROOT / "modules" / "MatAnyone2Module" / "checkpoints"
    BIREFNET_DEFAULT_CHECKPOINT["local_dir"] = (
        PROJECT_ROOT / "modules" / "BiRefNetModule" / "checkpoints" / "BiRefNet-matting")
    return roots


def create_provision_manager(*, roots=None, data_dir: Path | None = None,
                             v2_models_dir: Path | None = None,
                             hf_cache: Path | None = None,
                             cache_dir: Path | None = None,
                             operation_id: str | None = None,
                             emit=None):
    """Build the shared manager and synchronize legacy paths from its Roots."""
    from backend.model_catalog import load_catalog
    from backend.model_provision import ProvisionManager
    from backend.model_status import Roots

    if roots is None:
        roots = Roots.defaults(data_dir=data_dir, v2_models_dir=v2_models_dir,
                               hf_cache=hf_cache, cache_dir=cache_dir)
    _configure_legacy_roots(roots=roots)
    return ProvisionManager(load_catalog(), roots, emit=emit, operation_id=operation_id)

SAM2_MODELS = {
    "small": {
        "repo_id": "facebook/sam2.1-hiera-small",
        "filename": "sam2.1_hiera_small.pt",
        "size_human": "184 MB",
        "size_bytes": 184_000_000,
        "default": False,
    },
    "base-plus": {
        "repo_id": "facebook/sam2.1-hiera-base-plus",
        "filename": "sam2.1_hiera_base_plus.pt",
        "size_human": "324 MB",
        "size_bytes": 324_000_000,
        "default": True,
    },
    "large": {
        "repo_id": "facebook/sam2.1-hiera-large",
        "filename": "sam2.1_hiera_large.pt",
        "size_human": "898 MB",
        "size_bytes": 898_000_000,
        "default": False,
    },
}


def tracker_dependency_installed() -> bool:
    """Check whether the optional SAM2 Python package is installed.

    Probes sam2.build_sam, the exact module the runtime imports, rather
    than the bare top-level package so a broken partial install does not
    report as ready.
    """
    try:
        return importlib.util.find_spec("sam2.build_sam") is not None
    except Exception:
        return False


def is_installed(name: str) -> bool:
    """Check if a model's weights are already downloaded."""
    if name == "corridorkey":
        return _v2_paths().v2_assets_ready()
    cfg = MODELS[name]
    local_dir = cfg["local_dir"]
    if "check_glob" in cfg:
        if len(glob.glob(str(local_dir / cfg["check_glob"]))) == 0:
            return False
    if "check_file" in cfg:
        if not (local_dir / cfg["check_file"]).is_file():
            return False
    # Also check base_model dependency if defined
    if "base_model" in cfg:
        base_check = cfg["base_model"].get("check_file")
        if base_check and not (local_dir / base_check).is_file():
            return False
    if "check_glob" not in cfg and "check_file" not in cfg:
        return False
    return True


def is_sam2_installed(name: str) -> bool:
    """Check the pinned SAM2 snapshot path without touching the network."""
    from backend.model_catalog import hf_cache_snapshot_path

    art = _catalog().get(f"sam2-{name}")
    f = art.required_files()[0]
    path = hf_cache_snapshot_path(hf_cache_dir(), art.repo_id, art.revision, art.hf_filename)
    return path.is_file() and (f.size is None or path.stat().st_size == f.size)


def check_disk_space(needed_bytes: int, path: Path) -> bool:
    """Check if there's enough disk space for a download."""
    path.mkdir(parents=True, exist_ok=True)
    usage = shutil.disk_usage(path)
    # Require 10% headroom beyond the download size
    return usage.free > needed_bytes * 1.1


def _v2_paths():
    """Import backend.v2_paths (stdlib-only) from the project root."""
    if str(_SCRIPT_ROOT) not in sys.path:
        sys.path.insert(0, str(_SCRIPT_ROOT))
    from backend import v2_paths
    return v2_paths


def _load_v2_downloader():
    """Load the vendored CorridorKeyV2/scripts/download_weights.py module."""
    path = _v2_paths().get_v2_package_root() / "scripts" / "download_weights.py"
    spec = importlib.util.spec_from_file_location("_ck_v2_download_weights", path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def _legacy_download(target: str, *, manager=None, roots=None) -> bool:
    """Route setup-wizard and legacy CLI calls through the verified manager."""
    def human_event(event):
        kind = event.get("event")
        if kind == "artifact-start":
            print(f"  Downloading {event['artifact']} ({event.get('bytes_total', 0):,} bytes)...")
            print("  This may take a while. Downloads resume when the transport supports it.")
        elif kind == "progress":
            if event.get("unit") == "bytes":
                print(f"\r  {event.get('bytes', 0):,}/{event.get('bytes_total', 0):,} bytes",
                      end="", flush=True)
            elif event.get("path"):
                print(f"  Downloaded {event['path']}")
        elif kind == "artifact-done":
            print(f"  [OK] {event['artifact']} ready at {event.get('path') or 'the package root'}")
        elif kind == "artifact-error":
            print(f"  [ERROR] {event['artifact']}: {event.get('message', event.get('error'))}")

    try:
        mgr = manager or create_provision_manager(roots=roots, emit=human_event)
        with _proxy_guard():
            result = mgr.op_download([target])
        if result.get("cancelled"):
            print("\n  [CANCELLED] Download stopped before publication")
        return bool(result.get("ok"))
    except Exception as exc:
        print(f"  [ERROR] Download failed: {exc}")
        return False


def download_corridorkey() -> bool:
    """Download CorridorKey 2.0 weights + C-RADIO, MoGe and RVM foundation models."""
    return _legacy_download("corridorkey")


def download_sam2(name: str) -> bool:
    """Download a SAM2 checkpoint into the shared Hugging Face cache."""
    if name not in SAM2_MODELS:
        raise KeyError(name)
    return _legacy_download(f"sam2-{name}")


def download_repo(name: str) -> bool:
    """Download a full HuggingFace repo (GVM or VideoMaMa)."""
    if name not in ("gvm", "videomama"):
        raise KeyError(name)
    return _legacy_download(name)


def download_model(name: str) -> bool:
    """Download a model's weights, skipping if already present."""
    if name not in MODELS:
        raise KeyError(name)
    return _legacy_download(name)


def download_sam2_model(name: str) -> bool:
    """Download one SAM2 checkpoint, skipping if already cached."""
    return download_sam2(name)


def is_matanyone2_installed() -> bool:
    """Check if the MatAnyone2 checkpoint is already present."""
    _configure_legacy_roots()
    return (MATANYONE2_CHECKPOINT["local_dir"] / MATANYONE2_CHECKPOINT["filename"]).is_file()


def download_matanyone2(*, manager=None, roots=None) -> bool:
    """Download the MatAnyone2 checkpoint from GitHub Releases."""
    return _legacy_download("matanyone2", manager=manager, roots=roots)


def is_birefnet_installed() -> bool:
    """Check if the default BiRefNet Matting variant is already downloaded.

    We only check for any .safetensors in the target directory because
    snapshot_download pulls a handful of files and any usable
    BiRefNet-matting snapshot will contain at least one .safetensors.
    """
    _configure_legacy_roots()
    local_dir = BIREFNET_DEFAULT_CHECKPOINT["local_dir"]
    try:
        if not local_dir.is_dir():
            return False
        return any(p.suffix == ".safetensors" for p in local_dir.iterdir() if p.is_file())
    except OSError:
        return False


def download_birefnet(*, manager=None, roots=None) -> bool:
    """Download the default BiRefNet Matting variant to the data-dir path.

    The wrapper's ``_resolve_model_dir`` will find this at first use and
    skip the lazy runtime download entirely. Users who skip this during
    setup get the same content lazy-downloaded on demand — this is
    strictly a UX improvement for pre-selecting the default variant.
    """
    return _legacy_download("birefnet-BiRefNet-matting", manager=manager, roots=roots)


def check_all():
    """Print status of all models."""
    print("\nModel Status:")
    print("-" * 50)
    for name, cfg in MODELS.items():
        if name == "corridorkey":
            v2 = _v2_paths()
            problems = v2.validate_v2_assets()
            mark = "[--]" if problems else "[OK]"
            status = "NOT INSTALLED" if problems else "INSTALLED"
            print(f"  {mark} CorridorKey 2.0 {cfg['size_human']:>8s}  {status} (required)")
            print(f"       -> target: {v2.get_v2_models_dir()}")
            for prob in problems:
                print(f"       - {prob}")
            continue
        installed = is_installed(name)
        status = "INSTALLED" if installed else "NOT INSTALLED"
        required = " (required)" if cfg["required"] else " (optional)"
        mark = "[OK]" if installed else "[--]"
        print(f"  {mark} {name:12s} {cfg['size_human']:>8s}  {status}{required}")

        if installed and "check_glob" in cfg:
            files = glob.glob(str(cfg["local_dir"] / cfg["check_glob"]))
            for f in files:
                size_mb = os.path.getsize(f) / (1024**2)
                print(f"       -> {os.path.basename(f)} ({size_mb:.0f} MB)")

    # MatAnyone2 checkpoint
    ma2_installed = is_matanyone2_installed()
    ma2_mark = "[OK]" if ma2_installed else "[--]"
    ma2_status = "INSTALLED" if ma2_installed else "NOT INSTALLED"
    print(f"  {ma2_mark} matanyone2   {MATANYONE2_CHECKPOINT['size_human']:>8s}  {ma2_status} (optional)")

    # BiRefNet default Matting variant
    brn_installed = is_birefnet_installed()
    brn_mark = "[OK]" if brn_installed else "[--]"
    brn_status = "INSTALLED" if brn_installed else "NOT INSTALLED"
    print(f"  {brn_mark} birefnet     {BIREFNET_DEFAULT_CHECKPOINT['size_human']:>8s}  {brn_status} (optional)")

    tracker_installed = tracker_dependency_installed()
    tracker_mark = "[OK]" if tracker_installed else "[--]"
    tracker_status = "INSTALLED" if tracker_installed else "NOT INSTALLED"
    print(f"  {tracker_mark} sam2-tracker  python pkg   {tracker_status} (optional)")
    print(f"       -> cache: {hf_cache_dir()}")
    for name, cfg in SAM2_MODELS.items():
        installed = is_sam2_installed(name)
        status = "CACHED" if installed else "NOT CACHED"
        mark = "[OK]" if installed else "[--]"
        label = f"sam2-{name}"
        if cfg.get("default"):
            label += " (default)"
        print(f"  {mark} {label:18s} {cfg['size_human']:>8s}  {status}")
    print()


def _run_manager_op(op: str, args) -> int:
    """New structured surface: NDJSON events on stdout, logs on stderr.

    catalog/check/verify are fully offline; download/repair/cancel supervise
    downloads with staging, pinned revisions and cooperative cancellation.
    """
    from backend.install_paths import InstallDescriptorError
    from backend.model_catalog import CatalogError
    if op in ("download", "repair", "cancel") and (
            not args.operation_id
            or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_-]{0,127}", args.operation_id)):
        event = {"event": "op-done", "op": op, "ok": False,
                 "operation_id": args.operation_id,
                 "error": "invalid-operation-id",
                 "message": "pass --operation-id matching [A-Za-z0-9][A-Za-z0-9_-]{0,127}"}
        if op == "cancel":
            event["request_id"] = __import__("uuid").uuid4().hex
        sys.stdout.write(json.dumps(event, sort_keys=True) + "\n")
        sys.stdout.flush()
        return 2
    try:
        mgr = create_provision_manager(
            data_dir=Path(args.data_dir).expanduser() if args.data_dir else None,
            v2_models_dir=Path(args.v2_models_dir).expanduser() if args.v2_models_dir else None,
            hf_cache=Path(args.hf_cache).expanduser() if args.hf_cache else None,
            cache_dir=Path(args.cache_dir).expanduser() if args.cache_dir else None,
            operation_id=args.operation_id,
        )
    except Exception as e:
        code = ("install-descriptor" if isinstance(e, InstallDescriptorError) else
                "catalog" if isinstance(e, CatalogError) else "operation-initialization")
        import json as _json
        sys.stderr.write(f"[model-provision] {e}\n")
        event = {"event": "op-done", "op": op, "ok": False,
                 "error": code, "message": str(e)}
        if args.operation_id:
            event["operation_id"] = args.operation_id
        if op == "cancel":
            event["request_id"] = __import__("uuid").uuid4().hex
        sys.stdout.write(_json.dumps(event) + "\n")
        return 2
    targets = list(args.target) if args.target else None
    def guarded(call):
        terminal_before = mgr._terminal_count
        try:
            return call()
        except Exception as e:
            sys.stderr.write(f"[model-provision] {op} failed: {e}\n")
            if mgr._terminal_count == terminal_before:
                mgr.emit({"event": "op-done", "op": op, "ok": False,
                          "error": "operation-failed", "message": str(e)})
            return None

    if op == "catalog":
        return 0 if guarded(mgr.op_catalog) is not None else 1
    if op == "check":
        return 0 if guarded(lambda: mgr.op_check(targets)) is not None else 2
    if op == "verify":
        result = guarded(lambda: mgr.op_verify(targets))
        if result is None:
            return 2
        return 0 if result["ok"] else 1
    if op == "download":
        if args.profile and targets:
            mgr.emit({"event": "op-done", "op": op, "ok": False,
                      "error": "ambiguous-target", "message": "use --target or --profile"})
            return 2
        if args.profile:
            targets = mgr.catalog.profile(args.profile)
        result = guarded(lambda: mgr.op_download(targets or []))
        if result is None:
            return 1
        return 0 if result["ok"] else 1
    if op == "repair":
        if args.profile:
            mgr.emit({"event": "op-done", "op": op, "ok": False,
                      "error": "profile-not-supported", "message": "use --target with --repair"})
            return 2
        result = guarded(lambda: mgr.op_repair(targets or []))
        if result is None:
            return 1
        return 0 if result["ok"] else 1
    if op == "delete":
        if args.profile or not targets:
            mgr.emit({"event": "op-done", "op": op, "ok": False,
                      "error": "target-required", "message": "select the managed model to delete with --target"})
            return 2
        result = guarded(lambda: mgr.op_delete(targets))
        return 0 if result is not None and result["ok"] else 1
    if op == "cancel":
        result = guarded(lambda: mgr.cancel_running_download(args.operation_id))
        if result is None:
            return 1
        return 0 if result["ok"] else (2 if result.get("error") == "invalid-operation-id" else 1)
    raise SystemExit(f"unknown manager op {op}")


def main():
    parser = argparse.ArgumentParser(description="Download model weights for EZ-CorridorKey")
    parser.add_argument("--corridorkey", action="store_true", help="Download CorridorKey 2.0 weights + foundation models (~5GB, required)")
    parser.add_argument(
        "--sam2",
        nargs="?",
        const="base-plus",
        choices=["small", "base-plus", "large", "all"],
        help="Download SAM2 checkpoint(s): small, base-plus, large, or all (default: base-plus)",
    )
    parser.add_argument("--gvm", action="store_true", help="Download GVM weights (~6GB, optional)")
    parser.add_argument("--videomama", action="store_true", help="Download VideoMaMa weights (~6.1GB + ~1.5GB SVD base, optional)")
    parser.add_argument("--matanyone2", action="store_true", help="Download MatAnyone2 weights (~141MB, optional)")
    parser.add_argument("--birefnet", action="store_true", help="Download default BiRefNet Matting variant (~940MB, optional)")
    parser.add_argument("--all", action="store_true", help="Download all models")
    parser.add_argument("--check", action="store_true", help="Check installation status")
    manager = parser.add_argument_group(
        "shared manager",
        "structured JSON-event operations shared by installer/startup/Settings "
        "(catalog/check/verify are offline; download/repair/cancel supervise downloads)")
    manager.add_argument("--catalog", action="store_true",
                         help="Dump the pinned model catalog as a JSON event (offline)")
    manager.add_argument("--verify", action="store_true",
                         help="Rehash artifacts against trusted digests (JSON events)")
    manager.add_argument("--download", action="store_true",
                         help="Download artifacts (JSON events; pinned, staged, cancellable)")
    manager.add_argument("--repair", action="store_true",
                         help="Repair invalid/unverified artifacts (JSON events)")
    manager.add_argument("--delete", action="store_true",
                         help="Delete only the selected catalog-managed model files")
    manager.add_argument("--cancel", action="store_true",
                         help="Signal cancellation to a running download")
    manager.add_argument("--operation-id", default=None, metavar="ID",
                         help="App-chosen ID shared by one download/repair and its --cancel request")
    manager.add_argument("--target", action="append", default=[], metavar="ID",
                         help="Artifact id or legacy group (corridorkey/gvm/videomama/...); repeatable")
    manager.add_argument("--profile", choices=["required", "full"], default=None,
                         help="Target set for --download: required (CK2 bundle) or full (every artifact)")
    manager.add_argument("--json", action="store_true",
                         help="With --check: emit structured JSON status events instead of human text")
    manager.add_argument("--data-dir", default=None, help="Override app data root")
    manager.add_argument("--v2-models-dir", default=None, help="Override CorridorKeyV2 models dir")
    manager.add_argument("--hf-cache", default=None, help="Override HF hub cache dir")
    manager.add_argument("--cache-dir", default=None, help="Override app cache/diagnostics root")
    args = parser.parse_args()

    # New structured verbs take precedence; they never print human text on stdout.
    if args.catalog:
        return _run_manager_op("catalog", args)
    if args.verify:
        return _run_manager_op("verify", args)
    if args.download:
        return _run_manager_op("download", args)
    if args.repair:
        return _run_manager_op("repair", args)
    if args.delete:
        return _run_manager_op("delete", args)
    if args.cancel:
        return _run_manager_op("cancel", args)
    if args.check and args.json:
        return _run_manager_op("check", args)

    try:
        _configure_legacy_roots()
    except Exception as e:
        sys.stderr.write(f"[model-provision] {e}\n")
        sys.stdout.write(json.dumps({"event": "op-done", "op": "setup", "ok": False,
                                    "error": "install-descriptor", "message": str(e)}) + "\n")
        return 2

    # Default to --check if no flags
    if not any([args.corridorkey, args.sam2, args.gvm, args.videomama, args.matanyone2, args.birefnet, args.all, args.check]):
        args.check = True

    if args.check:
        check_all()
        if not any([args.corridorkey, args.sam2, args.gvm, args.videomama, args.matanyone2, args.birefnet, args.all]):
            return

    targets = []
    sam2_targets: list[str] = []
    download_ma2 = False
    download_brn = False
    if args.all:
        targets = list(MODELS.keys())
        sam2_targets = list(SAM2_MODELS.keys())
        download_ma2 = True
        download_brn = True
    else:
        if args.corridorkey:
            targets.append("corridorkey")
        if args.matanyone2:
            download_ma2 = True
        if args.birefnet:
            download_brn = True
        if args.sam2:
            if args.sam2 == "all":
                sam2_targets = list(SAM2_MODELS.keys())
            else:
                sam2_targets = [args.sam2]
        if args.gvm:
            targets.append("gvm")
        if args.videomama:
            targets.append("videomama")

    if not targets and not sam2_targets and not download_ma2 and not download_brn:
        return

    total_targets = (
        len(targets)
        + len(sam2_targets)
        + (1 if download_ma2 else 0)
        + (1 if download_brn else 0)
    )
    print(f"\nDownloading {total_targets} model(s)...\n")
    results = {}
    for name in targets:
        print(f"[{name}]")
        results[name] = download_model(name)
        print()
    if download_ma2:
        print("[matanyone2]")
        results["matanyone2"] = download_matanyone2()
        print()
    if download_brn:
        print("[birefnet]")
        results["birefnet"] = download_birefnet()
        print()
    for name in sam2_targets:
        result_key = f"sam2-{name}"
        print(f"[{result_key}]")
        results[result_key] = download_sam2_model(name)
        print()

    # Summary
    print("Summary:")
    for name, ok in results.items():
        print(f"  {'[OK]' if ok else '[FAIL]'} {name}")

    if not all(results.values()):
        sys.exit(1)


if __name__ == "__main__":
    sys.exit(main())
