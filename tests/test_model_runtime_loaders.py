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
    with pytest.raises(model_runtime.ModelAssetsUnavailableError, match="gvm-code.*Repair required"):
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


def test_runtime_distinguishes_verify_required_from_digest_mismatch(monkeypatch, tmp_path):
    artifact = _artifact("model", "models/model", ("weights.bin",), sha256="a" * 64)
    catalog = Catalog(1, "", {}, {artifact.id: artifact})
    monkeypatch.setattr(model_runtime, "load_catalog", lambda: catalog)
    data = tmp_path / "data"
    model = data / "models/model/weights.bin"
    model.parent.mkdir(parents=True)
    model.write_bytes(b"synthetic")
    roots = Roots(data, tmp_path / "models", tmp_path / "package", tmp_path / "hf", tmp_path / "cache")

    with pytest.raises(model_runtime.ModelAssetsUnavailableError, match="Verify required"):
        model_runtime.require_model_artifact("model", roots=roots)

    _seed_verified(catalog, roots, mismatch=("model", "weights.bin"))
    with pytest.raises(model_runtime.ModelAssetsUnavailableError, match="known trusted-digest mismatch.*Repair required"):
        model_runtime.require_model_artifact("model", roots=roots)


def test_runtime_rejects_unverified_dependency_before_loading_code(monkeypatch, tmp_path):
    model = _artifact("model", "models/model", ("weights.bin",), depends=("remote-code",), sha256="a" * 64)
    code = _artifact("remote-code", "models/code", ("modeling.py",), sha256="b" * 64)
    catalog = Catalog(1, "", {}, {model.id: model, code.id: code})
    monkeypatch.setattr(model_runtime, "load_catalog", lambda: catalog)
    data = tmp_path / "data"
    for artifact in catalog.artifacts.values():
        file_path = data / artifact.layout_rel / artifact.required_files()[0].layout_path
        file_path.parent.mkdir(parents=True, exist_ok=True)
        file_path.write_bytes(b"synthetic")
    roots = Roots(data, tmp_path / "models", tmp_path / "package", tmp_path / "hf", tmp_path / "cache")
    _seed_verified(catalog, roots, mismatch=("remote-code", "modeling.py"))

    with pytest.raises(
        model_runtime.ModelAssetsUnavailableError,
        match="remote-code.*known trusted-digest mismatch.*Repair required",
    ):
        model_runtime.require_model_artifact("model", roots=roots)


def test_sam2_tracker_uses_catalog_artifact_without_hub_download(monkeypatch, tmp_path):
    build_calls = []
    artifact = load_catalog().get("sam2-small")
    sam2 = types.ModuleType("sam2")
    sam2.__path__ = []
    build_sam = types.ModuleType("sam2.build_sam")
    build_sam.HF_MODEL_ID_TO_FILENAMES = {
        artifact.repo_id: ("configs/sam2.1_hiera_s.yaml", artifact.hf_filename)
    }
    build_sam.build_sam2_video_predictor = lambda **kwargs: build_calls.append(kwargs) or object()
    monkeypatch.setitem(sys.modules, "sam2", sam2)
    monkeypatch.setitem(sys.modules, "sam2.build_sam", build_sam)

    checkpoint = tmp_path / artifact.hf_filename
    with checkpoint.open("wb") as f:
        f.write(b"PK")
        f.truncate(11 * 1024 * 1024)
    selected = []
    leases = []
    monkeypatch.setattr(model_runtime, "acquire_model_artifact",
                        lambda aid: selected.append(aid) or leases.append(FakeLease(checkpoint)) or leases[-1])
    monkeypatch.setattr("sam2_tracker.wrapper._disable_external_progress_bars", lambda: None)

    tracker = __import__("sam2_tracker.wrapper", fromlist=["SAM2Tracker"]).SAM2Tracker(
        model_id=artifact.repo_id, device="cpu"
    )
    tracker._get_predictor()

    assert selected == ["sam2-small"]
    assert build_calls[0]["ckpt_path"] == str(checkpoint)
    tracker.unload()
    assert leases[-1].closed


def test_birefnet_loads_catalog_path_local_only(monkeypatch, tmp_path):
    from modules.BiRefNetModule import wrapper

    model_dir = tmp_path / "BiRefNet-matting"
    model_dir.mkdir()
    calls = []

    class Model:
        def to(self, *_): return self
        def eval(self): return self
        def half(self): return self

    class AutoModel:
        @staticmethod
        def from_pretrained(path, **kwargs):
            calls.append((path, kwargs))
            return Model()

    leases = []
    monkeypatch.setattr(model_runtime, "acquire_model_artifact",
                        lambda aid: calls.append(aid) or leases.append(FakeLease(model_dir)) or leases[-1])
    monkeypatch.setitem(sys.modules, "transformers", types.SimpleNamespace(AutoModelForImageSegmentation=AutoModel))
    processor = wrapper.BiRefNetProcessor(device="cpu")
    processor._ensure_loaded()

    assert calls[0] == "birefnet-BiRefNet-matting"
    assert calls[1][0] == str(model_dir)
    assert calls[1][1]["local_files_only"] is True
    assert calls[1][1]["trust_remote_code"] is True
    assert processor._model_asset_lease is leases[-1]

    explicit_dir = tmp_path / "explicit"
    explicit_dir.mkdir()
    monkeypatch.setattr(model_runtime, "acquire_model_artifact", lambda _aid: pytest.fail("explicit path ignored"))
    wrapper.BiRefNetProcessor(device="cpu", model_path=explicit_dir)._ensure_loaded()
    assert calls[-1][0] == str(explicit_dir)


def test_matanyone_explicit_path_and_config_avoid_lazy_download(monkeypatch, tmp_path):
    from modules.MatAnyone2Module import wrapper

    checkpoint = tmp_path / "matanyone2.pth"
    checkpoint.write_bytes(b"synthetic")
    assert wrapper._ensure_checkpoint(str(checkpoint)) == str(checkpoint)
    with pytest.raises(FileNotFoundError, match="Explicit MatAnyone2 checkpoint"):
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


def test_gvm_uses_catalog_path_and_diffusers_local_only(monkeypatch, tmp_path):
    from gvm_core import wrapper

    calls = []
    requested = []
    validated = []

    class Component:
        @classmethod
        def from_pretrained(cls, path, **kwargs):
            calls.append((path, kwargs))
            return cls()

    class Pipeline:
        def __init__(self, **_): pass
        def to(self, *_args, **_kwargs): return self

    leases = []
    monkeypatch.setattr(
        model_runtime, "acquire_model_artifact",
        lambda aid, **kwargs: requested.append((aid, kwargs)) or leases.append(FakeLease(tmp_path)) or leases[-1],
    )
    monkeypatch.setattr(
        model_runtime, "validate_local_model_path",
        lambda aid, path, components=None: validated.append((aid, path, components)) or Path(path),
    )
    monkeypatch.setattr(wrapper, "AutoencoderKLTemporalDecoder", Component)
    monkeypatch.setattr(wrapper, "FlowMatchEulerDiscreteScheduler", Component)
    monkeypatch.setattr(wrapper, "UNetSpatioTemporalConditionModel", Component)
    monkeypatch.setattr(wrapper, "GVMPipeline", Pipeline)

    processor = wrapper.GVMProcessor(device="cpu", seed=1)

    assert requested == [("gvm", {"exclude_components": ()})]
    assert len(calls) == 3
    assert all(call[1]["local_files_only"] is True for call in calls)
    assert processor._model_asset_lease is leases[-1]

    calls.clear()
    explicit_processor = wrapper.GVMProcessor(model_base=str(tmp_path), device="cpu", seed=1)
    assert validated == [("gvm", str(tmp_path), None)]
    assert all(call[1]["local_files_only"] is True for call in calls)
    assert explicit_processor._model_asset_lease is None

    calls.clear()
    requested.clear()
    validated.clear()
    explicit_unet = tmp_path / "explicit-unet"
    explicit_unet.mkdir()
    explicit_unet_processor = wrapper.GVMProcessor(unet_base=str(explicit_unet), device="cpu", seed=1)
    assert requested == [("gvm", {"exclude_components": ("unet",)})]
    assert validated == [("gvm", str(explicit_unet), ("unet",))]
    assert calls[-1][0] == str(explicit_unet)
    assert all(call[1]["local_files_only"] is True for call in calls)
    assert explicit_unet_processor._model_asset_lease is leases[-1]

    from gvm_core.gvm.pipelines import pipeline_gvm
    lora_calls = []

    class LoraConfig:
        @staticmethod
        def from_pretrained(path, **kwargs):
            lora_calls.append((path, kwargs))
            return object()

    monkeypatch.setattr(pipeline_gvm, "LoraConfig", LoraConfig)
    monkeypatch.setattr(pipeline_gvm.torch, "load", lambda *_args, **_kwargs: {})
    monkeypatch.setattr(pipeline_gvm, "LoraModel", lambda model, *_: model)
    monkeypatch.setattr(pipeline_gvm, "set_peft_model_state_dict", lambda *_: None)
    pipeline_gvm.GVMLoraLoader.load_lora_weights(types.SimpleNamespace(unet=object()), str(tmp_path))
    assert lora_calls == [(str(tmp_path), {"local_files_only": True})]


def test_videomama_uses_artifact_paths_and_component_loaders_are_local_only(monkeypatch, tmp_path):
    from VideoMaMaInferenceModule import inference, pipeline

    base = tmp_path / "svd"
    unet = tmp_path / "videomama"
    base.mkdir()
    unet.mkdir()
    resolved = []
    validated = []
    leases = []
    monkeypatch.setattr(
        model_runtime, "acquire_model_artifact",
        lambda aid, **kwargs: resolved.append((aid, kwargs)) or leases.append(FakeLease(
            {"videomama-svd": base, "videomama": unet}[aid],
            {"videomama-svd": base, "videomama": unet},
        )) or leases[-1],
    )
    monkeypatch.setattr(
        model_runtime, "model_artifact_dir",
        lambda aid: {"videomama-svd": base, "videomama": unet}[aid],
    )
    monkeypatch.setattr(
        model_runtime, "validate_local_model_path",
        lambda aid, path, components=None: validated.append((aid, path, components)) or Path(path),
    )

    class Loaded:
        def to(self, *_args, **_kwargs): return self
        def eval(self): return self

    class Loader:
        @staticmethod
        def from_pretrained(path, **kwargs):
            calls.append((path, kwargs))
            return Loaded()

    loaded_pipelines = []
    pipeline_calls = []
    def make_pipeline(**kwargs):
        pipeline_calls.append(kwargs)
        pipeline_instance = types.SimpleNamespace()
        loaded_pipelines.append(pipeline_instance)
        return pipeline_instance

    monkeypatch.setattr(inference, "VideoInferencePipeline", make_pipeline)
    assert inference.load_videomama_model(device="cpu") is loaded_pipelines[-1]
    assert resolved == [("videomama", {})]
    assert loaded_pipelines[-1]._model_asset_lease is leases[-1]
    assert pipeline_calls[-1]["base_model_path"] == str(base)
    assert pipeline_calls[-1]["unet_checkpoint_path"] == str(unet)

    resolved.clear()
    assert inference.load_videomama_model(str(base), device="cpu") is loaded_pipelines[-1]
    assert resolved == [("videomama", {"exclude_dependencies": ("videomama-svd",)})]
    assert loaded_pipelines[-1]._model_asset_lease is leases[-1]
    assert validated == [("videomama-svd", str(base), None)]

    resolved.clear()
    validated.clear()
    assert inference.load_videomama_model(unet_checkpoint_path=str(unet), device="cpu") is loaded_pipelines[-1]
    assert resolved == [("videomama-svd", {})]
    assert loaded_pipelines[-1]._model_asset_lease is leases[-1]
    assert validated == [("videomama", str(unet), ("unet",))]

    resolved.clear()
    validated.clear()
    assert inference.load_videomama_model(str(base), str(unet), device="cpu") is loaded_pipelines[-1]
    assert not hasattr(loaded_pipelines[-1], "_model_asset_lease")
    assert resolved == []
    assert validated == [
        ("videomama-svd", str(base), None), ("videomama", str(unet), ("unet",))
    ]
    assert pipeline_calls[-1]["base_model_path"] == str(base)
    assert pipeline_calls[-1]["unet_checkpoint_path"] == str(unet)

    calls = []
    for name in (
        "CLIPImageProcessor", "CLIPVisionModelWithProjection",
        "AutoencoderKLTemporalDecoder", "UNetSpatioTemporalConditionModel",
    ):
        monkeypatch.setattr(pipeline, name, Loader)
    pipeline.VideoInferencePipeline(str(base), str(unet), device="cpu")

    assert len(calls) == 4
    assert all(call[1]["local_files_only"] is True for call in calls)
