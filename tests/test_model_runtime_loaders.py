from __future__ import annotations

import json
import sys
import types
from dataclasses import replace
from pathlib import Path

import pytest
import torch
from omegaconf import OmegaConf

from backend.model_catalog import Artifact, Catalog, FileSpec, load_catalog
from backend.model_status import Roots, StatusChecker, file_digest
from backend import model_runtime


class FakeLease:
    def __init__(self, path, paths=None):
        self.path = Path(path)
        self.paths = paths or {"artifact": self.path}
        self.closed = False

    def close(self):
        self.closed = True


def test_runtime_checks_package_dependencies_without_inventing_model_paths(monkeypatch, tmp_path):
    from backend import model_status

    package = replace(_artifact("runtime-package", "", (), kind="local_package"),
                      layout_root=None, layout_rel=None)
    model = _artifact("model", "weights", ("model.bin",),
                      depends=(package.id,), sha256="a" * 64)
    catalog = Catalog(1, "", {}, {model.id: model, package.id: package})
    monkeypatch.setattr(model_runtime, "load_catalog", lambda: catalog)
    monkeypatch.setattr(model_status, "_check_local_package", lambda _: (True, "installed", []))
    roots = Roots(tmp_path, tmp_path / "v2", tmp_path / "code", tmp_path / "hf", tmp_path / "cache")
    (tmp_path / "weights").mkdir()
    (tmp_path / "weights/model.bin").write_bytes(b"synthetic")
    _seed_verified(catalog, roots)
    with model_runtime.acquire_model_artifact(model.id, roots=roots) as lease:
        assert lease.paths == {model.id: tmp_path / "weights"}
    monkeypatch.setattr(model_status, "_check_local_package",
                        lambda _: (False, "missing", ["runtime missing"]))
    with pytest.raises(model_runtime.ModelAssetsUnavailableError, match="runtime missing"):
        model_runtime.acquire_model_artifact(model.id, roots=roots)


def _artifact(
    artifact_id: str,
    layout_rel: str,
    files: tuple[str, ...],
    *,
    depends: tuple[str, ...] = (),
    kind: str = "hf_snapshot",
    repo_id: str | None = None,
    revision: str | None = None,
    hf_filename: str | None = None,
    path_remap: dict[str, str] | None = None,
    sha256: str | None = None,
) -> Artifact:
    return Artifact(
        id=artifact_id,
        label=artifact_id,
        capability="test",
        kind=kind,
        required_profile=False,
        layout_root="data_dir" if kind != "hf_cache_file" else None,
        layout_rel=layout_rel if kind != "hf_cache_file" else None,
        repo_id=repo_id,
        revision=revision,
        revision_status="pinned" if revision else "unresolved",
        files=tuple(
            FileSpec(
                path=name,
                layout_path=name,
                size=None,
                sha256=sha256,
                sha256_source="test" if sha256 else None,
                lfs=False,
                required=True,
            )
            for name in files
        ),
        depends=depends,
        hf_filename=hf_filename,
        path_remap=path_remap or {},
    )


def _seed_verified(catalog: Catalog, roots: Roots, *, mismatch: tuple[str, str] | None = None):
    checker = StatusChecker(catalog, roots)
    for artifact in catalog.artifacts.values():
        for file in artifact.required_files():
            digest = file_digest(artifact, file)
            path = checker.file_path(artifact, file)
            if digest is None or path is None:
                continue
            bad = mismatch == (artifact.id, file.layout_path)
            checker.hash_cache.record(
                path, digest, not bad, ("f" * 64) if bad else digest.expected, artifact.id
            )
    checker.hash_cache.save()


def test_runtime_uses_descriptor_user_data_and_checks_dependency_closure(monkeypatch, tmp_path):
    app = tmp_path / "app"
    app.mkdir()
    data = tmp_path / "chosen-user-data"
    descriptor = app / "keyloom-install.json"
    descriptor.write_text(json.dumps({
        "schema": 1,
        "app_name": "Keyloom",
        "app_version": "test",
        "roots": {
            "python": "runtime/python",
            "helper": "helper",
            "models": "models",
            "cache": "cache",
            "user_data": str(data),
        },
    }))
    monkeypatch.setenv("CK_INSTALL", str(descriptor))
    monkeypatch.delenv("CORRIDORKEY_V2_MODELS_DIR", raising=False)
    monkeypatch.delenv("HF_HUB_CACHE", raising=False)
    monkeypatch.delenv("HF_HOME", raising=False)

    artifacts = {
        "gvm": _artifact("gvm", "gvm_core/weights", ("unet/config.json",), depends=("gvm-code",), sha256="a" * 64),
        "gvm-code": _artifact("gvm-code", "gvm_core/code", ("loader.py",), sha256="b" * 64),
    }
    catalog = Catalog(1, "", {}, artifacts)
    monkeypatch.setattr(model_runtime, "load_catalog", lambda: catalog)

    for artifact in artifacts.values():
        path = data / artifact.layout_rel / artifact.required_files()[0].layout_path
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("synthetic")

    roots = Roots.defaults()
    _seed_verified(catalog, roots)
    lease = model_runtime.require_model_artifact("gvm", roots=roots)
    assert lease.path == data / "gvm_core/weights"
    lease.close()

    (data / "gvm_core/code/loader.py").unlink()
    with pytest.raises(model_runtime.ModelAssetsUnavailableError, match="gvm-code.*download it on the Models page"):
        model_runtime.require_model_artifact("gvm", roots=roots)


def test_runtime_resolves_hf_cache_file_at_catalog_revision(monkeypatch, tmp_path):
    revision = "a" * 40
    artifact = _artifact(
        "sam2-small", "", ("sam2.pt",), kind="hf_cache_file",
        repo_id="facebook/sam2.1-hiera-small", revision=revision,
        hf_filename="sam2.pt", sha256="a" * 64,
    )
    catalog = Catalog(1, "", {}, {artifact.id: artifact})
    monkeypatch.setattr(model_runtime, "load_catalog", lambda: catalog)
    cache = tmp_path / "hf-cache"
    expected = cache / "models--facebook--sam2.1-hiera-small" / "snapshots" / revision / "sam2.pt"
    expected.parent.mkdir(parents=True)
    expected.write_bytes(b"synthetic checkpoint")
    roots = Roots(tmp_path / "data", tmp_path / "models", tmp_path / "package", cache, tmp_path / "cache")
    _seed_verified(catalog, roots)

    lease = model_runtime.require_model_artifact("sam2-small", roots=roots)
    assert lease.path == expected
    lease.close()


def test_runtime_uses_catalog_remap_for_nested_model_root(monkeypatch, tmp_path):
    artifact = _artifact(
        "videomama", "VideoMaMaInferenceModule/checkpoints", ("VideoMaMa/unet/config.json",),
        path_remap={"unet/": "VideoMaMa/unet/"},
        sha256="a" * 64,
    )
    catalog = Catalog(1, "", {}, {artifact.id: artifact})
    monkeypatch.setattr(model_runtime, "load_catalog", lambda: catalog)
    data = tmp_path / "data"
    (data / artifact.layout_rel / "VideoMaMa/unet").mkdir(parents=True)
    (data / artifact.layout_rel / "VideoMaMa/unet/config.json").write_text("synthetic")
    roots = Roots(data, tmp_path / "models", tmp_path / "package", tmp_path / "hf", tmp_path / "cache")
    _seed_verified(catalog, roots)

    lease = model_runtime.require_model_artifact("videomama", roots=roots)
    assert lease.path == data / artifact.layout_rel / "VideoMaMa"
    lease.close()


def test_explicit_local_model_root_requires_selected_component_files(monkeypatch, tmp_path):
    artifact = _artifact(
        "videomama", "checkpoints", ("VideoMaMa/unet/config.json", "VideoMaMa/unet/weights.safetensors"),
        path_remap={"unet/": "VideoMaMa/unet/"},
    )
    monkeypatch.setattr(model_runtime, "load_catalog", lambda: Catalog(1, "", {}, {artifact.id: artifact}))
    local = tmp_path / "explicit-unet"
    (local / "unet").mkdir(parents=True)
    (local / "unet/config.json").write_text("synthetic")

    with pytest.raises(model_runtime.ModelAssetsUnavailableError, match="missing: unet/weights.safetensors"):
        model_runtime.validate_local_model_path("videomama", local, components=("unet",))

    (local / "unet/weights.safetensors").write_bytes(b"synthetic")
    assert model_runtime.validate_local_model_path(
        "videomama", local, components=("unet",)
    ) == local


def test_explicit_local_model_root_accepts_legacy_flat_videomama_unet(monkeypatch, tmp_path):
    config = b"legacy config"
    weights = b"legacy weights"
    artifact = _artifact(
        "videomama", "checkpoints",
        ("VideoMaMa/unet/config.json", "VideoMaMa/unet/weights.safetensors"),
        path_remap={"unet/": "VideoMaMa/unet/"},
    )
    artifact = replace(artifact, files=(
        replace(artifact.files[0], size=len(config)),
        replace(artifact.files[1], size=len(weights)),
    ))
    monkeypatch.setattr(model_runtime, "load_catalog", lambda: Catalog(1, "", {}, {artifact.id: artifact}))
    local = tmp_path / "flat-videomama"
    local.mkdir()
    (local / "config.json").write_bytes(config)
    (local / "weights.safetensors").write_bytes(weights)

    assert model_runtime.validate_local_model_path(
        "videomama", local, components=("unet",)
    ) == local


def test_runtime_needs_presence_only_never_a_hash_record(monkeypatch, tmp_path):
    artifact = _artifact("model", "models/model", ("weights.bin",), sha256="a" * 64)
    catalog = Catalog(1, "", {}, {artifact.id: artifact})
    monkeypatch.setattr(model_runtime, "load_catalog", lambda: catalog)
    data = tmp_path / "data"
    model = data / "models/model/weights.bin"
    model.parent.mkdir(parents=True)
    model.write_bytes(b"synthetic")
    roots = Roots(data, tmp_path / "models", tmp_path / "package", tmp_path / "hf", tmp_path / "cache")

    model_runtime.require_model_artifact("model", roots=roots).close()  # no verification record: still loads
    _seed_verified(catalog, roots, mismatch=("model", "weights.bin"))
    model_runtime.require_model_artifact("model", roots=roots).close()  # a stale hash record is never consulted
    model.unlink()
    with pytest.raises(model_runtime.ModelAssetsUnavailableError, match="missing: weights.bin.*download it"):
        model_runtime.require_model_artifact("model", roots=roots)


def test_runtime_rejects_missing_dependency_before_loading_code(monkeypatch, tmp_path):
    model = _artifact("model", "models/model", ("weights.bin",), depends=("remote-code",), sha256="a" * 64)
    code = _artifact("remote-code", "models/code", ("modeling.py",), sha256="b" * 64)
    catalog = Catalog(1, "", {}, {model.id: model, code.id: code})
    monkeypatch.setattr(model_runtime, "load_catalog", lambda: catalog)
    data = tmp_path / "data"
    file_path = data / model.layout_rel / "weights.bin"
    file_path.parent.mkdir(parents=True, exist_ok=True)
    file_path.write_bytes(b"synthetic")
    roots = Roots(data, tmp_path / "models", tmp_path / "package", tmp_path / "hf", tmp_path / "cache")

    with pytest.raises(model_runtime.ModelAssetsUnavailableError, match="remote-code.*download it"):
        model_runtime.require_model_artifact("model", roots=roots)


def test_matanyone_explicit_path_and_config_avoid_lazy_download(monkeypatch, tmp_path):
    from modules.MatAnyone2Module import wrapper

    checkpoint = tmp_path / "matanyone2.pth"
    checkpoint.write_bytes(b"synthetic")
    assert wrapper._ensure_checkpoint(str(checkpoint)) == str(checkpoint)
    with pytest.raises(FileNotFoundError, match="model file missing"):
        wrapper._ensure_checkpoint(str(tmp_path / "missing.pth"))

    module_path = Path(__file__).resolve().parents[1] / "modules/MatAnyone2Module"
    monkeypatch.syspath_prepend(str(module_path))
    from matanyone2.utils import get_default_model

    config = OmegaConf.create({"model": {"pretrained_resnet": True}, "weights": "old"})

    class HydraState:
        def clear(self): pass

    class GlobalHydra:
        @staticmethod
        def instance(): return HydraState()

    captured = {}

    class Model:
        def to(self, *_): return self
        def eval(self): return self
        def state_dict(self):
            return {
                "pixel_encoder.conv1.weight": None,
                "mask_encoder.conv1.weight": None,
            }
        def load_weights(self, weights): captured["weights"] = weights

    monkeypatch.setattr(get_default_model, "GlobalHydra", GlobalHydra)
    monkeypatch.setattr(get_default_model, "initialize", lambda **_: None)
    monkeypatch.setattr(get_default_model, "compose", lambda **_: config)
    def make_model(cfg, **_):
        captured["cfg"] = cfg
        return Model()

    monkeypatch.setattr(get_default_model, "MatAnyone2", make_model)
    monkeypatch.setattr(torch, "load", lambda *_args, **_kwargs: {
        "pixel_encoder.conv1.weight": None,
        "mask_encoder.conv1.weight": None,
    })
    # The local model factory receives a config that cannot trigger torchvision's URL loader.
    model = get_default_model.get_matanyone2_model(str(checkpoint), device="cpu")

    assert isinstance(model, Model)
    assert captured["cfg"].model.pretrained_resnet is False

    monkeypatch.setattr(torch, "load", lambda *_args, **_kwargs: {})
    with pytest.raises(RuntimeError, match="missing ResNet backbone weights.*Repair"):
        get_default_model.get_matanyone2_model(str(checkpoint), device="cpu")
