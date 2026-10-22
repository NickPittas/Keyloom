"""Launch validation run by the bundled interpreter (`python -I selfcheck.py <app>`).

Proves the isolated runtime imports the app, EVERY model engine and its dependencies, and that the
bundled FFmpeg is the one the app finds and accepts, from an unrelated CWD. Prints one JSON line.
A GPU is reported, not required or faked: CorridorKey 2.0 has no CPU path.
"""
import importlib
import json
import sys
from pathlib import Path

app = Path(sys.argv[1]).resolve()
sys.path.insert(0, str(app))
mods = [
    "backend.install_paths", "backend.model_catalog", "backend.model_provision", "backend.worker",
    "backend.service", "CorridorKeyV2.corridorkey", "gvm_core.wrapper", "gvm_core.gvm.pipelines.pipeline_gvm",
    "sam2_tracker.wrapper", "sam2", "VideoMaMaInferenceModule.pipeline", "VideoMaMaInferenceModule.inference",
    "modules.MatAnyone2Module.wrapper", "modules.BiRefNetModule.wrapper",
    "cv2", "OpenEXR", "av", "numpy", "scipy", "safetensors", "transformers", "diffusers", "timm", "kornia", "peft",
    "einops", "easydict", "hydra", "utils3d", "imageio", "pims", "accelerate", "huggingface_hub", "cryptography",
    "pynvml", "matplotlib", "torch", "torchvision", "PySide6.QtCore",
]
bad = {}
# sam2._C (connected-components CUDA extension): built and required on Linux. Windows builds do not produce it;
# upstream SAM2 then falls back to its Python post-processing, so there it is reported, never claimed.
sam2_c = None
try:
    importlib.import_module("torch")  # the extension links torch's libs: torch must load first
    importlib.import_module("sam2._C")
    sam2_c = True
except BaseException as e:
    sam2_c = False
    if sys.platform != "win32":
        bad["sam2._C"] = f"{type(e).__name__}: {e}"[:200]
for m in mods:
    try:
        importlib.import_module(m)
    except BaseException as e:  # report every failure at once
        bad[m] = f"{type(e).__name__}: {e}"[:200]
ff = None
try:
    from backend.ffmpeg_tools.discovery import validate_ffmpeg_install
    r = validate_ffmpeg_install()
    ff = {"ok": r.ok, "message": r.message, "ffmpeg": r.ffmpeg_path, "ffprobe": r.ffprobe_path}
    inside = r.ok and all(p and Path(p).resolve().is_relative_to(app) for p in (r.ffmpeg_path, r.ffprobe_path))
    if not inside:
        bad["ffmpeg"] = f"not the bundled, accepted build: {r.message} [{r.ffmpeg_path}]"
except BaseException as e:
    bad["ffmpeg"] = f"{type(e).__name__}: {e}"[:200]
if bad:
    print(json.dumps({"ok": False, "failed": bad}), file=sys.stderr)
    sys.exit(1)
import torch  # noqa: E402

probe = {}
if torch.cuda.is_available():  # GPU operator probes: kernels must run, not merely import
    try:
        x = torch.randn(64, 64, device="cuda")
        assert torch.isfinite(x @ x).all()
        probe["matmul"] = True
        if sam2_c:
            from sam2 import _C
            mask = torch.zeros(1, 1, 32, 32, dtype=torch.uint8, device="cuda")
            mask[..., 4:12, 4:12] = 1
            labels, counts = _C.get_connected_componnets(mask.contiguous())  # sic: upstream's spelling
            torch.cuda.synchronize()
            probe["sam2_cc"] = int(counts.max()) == 64
            if not probe["sam2_cc"]:
                bad["sam2_probe"] = f"unexpected component size {int(counts.max())}"
    except BaseException as e:
        bad["gpu_probe"] = f"{type(e).__name__}: {e}"[:200]
    if bad:
        print(json.dumps({"ok": False, "failed": bad}), file=sys.stderr)
        sys.exit(1)

print(json.dumps({"ok": True, "python": sys.version.split()[0], "torch": torch.__version__,
                  "cuda_runtime": torch.version.cuda, "cuda": bool(torch.cuda.is_available()), "gpu_probe": probe, "sam2_c": sam2_c, "ffmpeg": ff}))
