"""Import smoke test — verify every module in backend/ and ui/ can be loaded."""
import sys
import importlib
import os

os.chdir(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, ".")

SKIP_PREFIXES = (
    "scripts", "tests", "CorridorKeyModule.core", "gvm_core",
    "VideoMaMaInferenceModule", "modules", "docker", "clip_manager",
    "sam2_tracker", "main",
    # Vendored CorridorKey 2.0 engine + MoGe: imported explicitly below.
    "CorridorKeyV2",
    # macOS-only (raises ImportError elsewhere).
    "backend.service.apple_vision",
)

EXPLICIT = (
    "backend.corridorkey_v2",
    "CorridorKeyV2.corridorkey.runtime",
    "CorridorKeyModule.core.chroma_key",
    "CorridorKeyModule.core.color_utils",
)

errors = []
ok = 0
for root, dirs, files in os.walk("."):
    dirs[:] = [d for d in dirs if d not in ("__pycache__", ".venv", "venv", ".git", ".serena")]
    for f in files:
        if not f.endswith(".py") or f == "__init__.py":
            continue
        path = os.path.join(root, f)
        module = path.replace(os.sep, "/").lstrip("./").replace("/", ".").removesuffix(".py")
        if any(module.startswith(p) for p in SKIP_PREFIXES):
            continue
        try:
            importlib.import_module(module)
            ok += 1
        except Exception as e:
            errors.append((module, f"{type(e).__name__}: {str(e)[:200]}"))

for module in EXPLICIT:
    try:
        importlib.import_module(module)
        ok += 1
    except Exception as e:
        errors.append((module, f"{type(e).__name__}: {str(e)[:200]}"))

print(f"Imported {ok} modules successfully")
if errors:
    print(f"FAILURES ({len(errors)}):")
    for mod, err in errors:
        print(f"  {mod}: {err}")
    sys.exit(1)
else:
    print("ALL CLEAR - no import errors")
