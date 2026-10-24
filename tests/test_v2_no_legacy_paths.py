"""Guard: production code must not reference the removed v1 / MLX keyer."""
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCAN = ["backend", "pyproject.toml"]
SKIP_DIRS = {"__pycache__", ".venv", "tests"}

SYMBOLS = re.compile(
    r"CorridorKeyEngine|create_engine|resolve_backend|_discover_checkpoint|"
    r"has_blue_checkpoint|_MLXEngineAdapter|MLXCorridorKeyEngine|_get_engine_pool|"
    r"_engine_pool|set_pool_size|set_model_resolution|set_inference_backend|"
    r"KEY_MODEL_RESOLUTION|KEY_INFERENCE_BACKEND|KEY_PARALLEL_CLIPS|"
    r"CORRIDORKEY_OPT_MODE|CORRIDORKEY_BACKEND"
)
ARTIFACTS = re.compile(
    r"CorridorKey_v1\.0|CorridorKeyBlue|\.mlx\.safetensors|corridorkey-mlx|"
    r"--corridorkey-blue|--corridorkey-mlx"
)
LEGACY_IMPORT = re.compile(
    r"CorridorKeyModule\.(backend|inference_engine|mlx_inference_engine)|"
    r"core\.(model_transformer|mlx_model)|core import (model_transformer|mlx_model)"
)
# Files that legitimately mention v1 artifacts to refuse / detect / ignore them.
ARTIFACT_ALLOWED = {"scripts/setup_models.py"}


def _files():
    for entry in SCAN:
        p = ROOT / entry
        if p.is_file():
            yield p
        elif p.is_dir():
            for f in p.rglob("*"):
                if f.is_file() and not SKIP_DIRS & set(f.relative_to(ROOT).parts) \
                        and f.suffix in {".py", ".sh", ".bat", ".toml", ".txt", ".spec"}:
                    yield f


def _hits(pattern, allowed=()):
    out = []
    for f in _files():
        rel = f.relative_to(ROOT).as_posix()
        if rel in allowed:
            continue
        text = f.read_text(encoding="utf-8", errors="ignore")
        for n, line in enumerate(text.splitlines(), 1):
            if pattern.search(line):
                out.append(f"{rel}:{n}: {line.strip()[:100]}")
    return out


def test_no_v1_engine_symbols():
    assert not _hits(SYMBOLS)


def test_no_v1_artifact_names():
    assert not _hits(ARTIFACTS, ARTIFACT_ALLOWED)


def test_no_legacy_module_imports():
    assert not _hits(LEGACY_IMPORT)


def test_legacy_engine_modules_gone():
    for name in ("backend", "inference_engine", "mlx_inference_engine"):
        assert not (ROOT / "CorridorKeyModule" / f"{name}.py").exists()
    for name in ("model_transformer", "mlx_model"):
        assert not (ROOT / "CorridorKeyModule" / "core" / f"{name}.py").exists()


def test_corridorkey_module_has_no_engine_export():
    import CorridorKeyModule

    assert not hasattr(CorridorKeyModule, "CorridorKeyEngine")
