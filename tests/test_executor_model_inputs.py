"""A5: every model load takes the exact files the app supplied; a missing one fails with no fallback."""
from __future__ import annotations

import sys
import types
from pathlib import Path

import pytest

from backend import model_runtime
from backend.service import CorridorKeyService
from backend.service.key_job import KeyJobRejected, _check_model_files


@pytest.fixture(autouse=True)
def no_discovery(monkeypatch):
    """Any fallback to the catalog/lease path fails the test."""
    def boom(*a, **k):
        pytest.fail("fell back to discovering a model")
    monkeypatch.setattr(model_runtime, "acquire_model_artifact", boom)


@pytest.fixture
def custom(tmp_path):
    return tmp_path / "custom models"  # a folder no catalog root points at


def _dir(p: Path) -> Path:
    p.mkdir(parents=True, exist_ok=True)
    return p


def _file(p: Path, size: int = 8) -> Path:
    _dir(p.parent)
    p.write_bytes(b"PK" + b"x" * size)
    return p


def _service(files: dict) -> CorridorKeyService:
    svc = CorridorKeyService()
    svc._device = "cpu"
    svc._model_files = {k: str(v) for k, v in files.items()}
    return svc


# ---------------------------------------------------------------- Key and its dependencies

def _key_files(custom: Path) -> dict:
    weights = _dir(custom / "w")
    _file(weights / "corridorkey_v2.safetensors")
    _file(weights / "foundation_stats.safetensors")
    moge = _dir(custom / "m")
    _file(moge / "model.pt")
    cradio = _dir(custom / "c")
    _file(cradio / "config.json")
    _file(cradio / "hf_model.py")
    _file(cradio / "x.safetensors")
    rvm = _file(custom / "r" / "rvm.npz")
    return {"corridorkey-weights": weights, "moge": moge, "cradio": cradio, "rvm": rvm}


def test_key_engine_loads_every_supplied_file(monkeypatch, custom):
    import torch
    from CorridorKeyV2.corridorkey import runtime as rt
    from backend.corridorkey_v2 import CorridorKeyV2Engine
    from backend.v2_paths import v2_asset_paths_from_files

    files = _key_files(custom)
    seen = {}
    monkeypatch.setattr(torch.cuda, "is_available", lambda: True)
    monkeypatch.setattr(rt, "load_model", lambda paths, device: seen.update(paths=paths) or object())
    engine = CorridorKeyV2Engine(asset_paths=v2_asset_paths_from_files(files), device="cuda")
    engine.load()

    p = seen["paths"]
    assert p.weights == files["corridorkey-weights"] / "corridorkey_v2.safetensors"
    assert p.foundation_stats == files["corridorkey-weights"] / "foundation_stats.safetensors"
    assert p.moge_checkpoint == files["moge"] / "model.pt"
    assert p.cradio_dir == files["cradio"]
    assert p.rvm_ckpt == files["rvm"]
    engine.unload()
    assert not engine.is_loaded and engine._model_asset_lease is None


def test_service_builds_engine_from_job_files_and_reloads_on_change(monkeypatch, custom):
    import backend.corridorkey_v2 as ck

    built = []

    class Fake:
        def __init__(self, asset_paths=None, device="cuda"):
            built.append(asset_paths)
            self.unloaded = False

        def load(self, on_status=None): pass
        def unload(self): self.unloaded = True

    monkeypatch.setattr(ck, "CorridorKeyV2Engine", Fake)
    files = _key_files(custom)
    svc = _service(files)
    first = svc._get_v2_engine()
    assert built[0].weights == files["corridorkey-weights"] / "corridorkey_v2.safetensors"
    assert svc._get_v2_engine() is first and len(built) == 1
    other = _key_files(custom / "second")
    svc._model_files = {k: str(v) for k, v in other.items()}
    assert svc._get_v2_engine() is not first and first.unloaded
    assert built[1].cradio_dir == other["cradio"]


@pytest.mark.parametrize("missing", ["corridorkey-weights", "moge", "cradio", "rvm"])
def test_key_missing_supplied_file_fails_without_fallback(custom, missing):
    files = _key_files(custom)
    files[missing] = custom / "gone"
    with pytest.raises(FileNotFoundError, match="model file missing"):
        _service(files)._get_v2_engine()


def test_key_unlisted_dependency_fails_clearly(custom):
    files = _key_files(custom)
    del files["rvm"]
    with pytest.raises(FileNotFoundError, match="model file missing: no path supplied for rvm"):
        _service(files)._get_v2_engine()


# ---------------------------------------------------------------- SAM checkpoint and config

def _fake_sam(monkeypatch, repo="facebook/sam2.1-hiera-small"):
    calls = []
    sam2 = types.ModuleType("sam2")
    sam2.__path__ = []
    build = types.ModuleType("sam2.build_sam")
    build.HF_MODEL_ID_TO_FILENAMES = {repo: ("configs/sam2.1_hiera_s.yaml", "sam2.1_hiera_small.pt")}
    build.build_sam2_video_predictor = lambda **kw: calls.append(kw) or object()
    monkeypatch.setitem(sys.modules, "sam2", sam2)
    monkeypatch.setitem(sys.modules, "sam2.build_sam", build)
    monkeypatch.setattr("sam2_tracker.wrapper._disable_external_progress_bars", lambda: None)
    return calls


def test_sam_opens_supplied_checkpoint_and_config(monkeypatch, custom):
    from sam2_tracker.wrapper import SAM2Tracker

    calls = _fake_sam(monkeypatch)
    ckpt = custom / "s" / "my.pt"
    _dir(ckpt.parent)
    with ckpt.open("wb") as f:
        f.write(b"PK")
        f.truncate(11 * 1024 * 1024)
    t = SAM2Tracker("facebook/sam2.1-hiera-small", device="cpu", checkpoint_path=str(ckpt), config_file="cfg.yaml")
    t.prepare()
    assert calls == [{"config_file": "cfg.yaml", "ckpt_path": str(ckpt), "device": "cpu", "vos_optimized": False}]
    t.unload()
    assert t._predictor is None and t._model_asset_lease is None


def test_sam_missing_supplied_checkpoint_fails_without_fallback(monkeypatch, custom):
    from sam2_tracker.wrapper import SAM2Tracker

    calls = _fake_sam(monkeypatch)
    with pytest.raises(FileNotFoundError, match="model file missing"):
        SAM2Tracker("facebook/sam2.1-hiera-small", device="cpu", checkpoint_path=str(custom / "no.pt")).prepare()
    assert calls == []


def test_service_sam_uses_the_variant_binding(monkeypatch, custom):
    calls = _fake_sam(monkeypatch)
    ckpt = custom / "sam" / "small.pt"
    _dir(ckpt.parent)
    with ckpt.open("wb") as f:
        f.write(b"PK")
        f.truncate(11 * 1024 * 1024)
    svc = _service({"sam2-small": ckpt, "sam2-base-plus": custom / "not-used.pt"})
    svc._sam2_model_id = "facebook/sam2.1-hiera-small"
    svc._get_sam2_tracker(artifact_id="sam2-small")
    assert calls[0]["ckpt_path"] == str(ckpt)
    with pytest.raises(FileNotFoundError, match="no path supplied for sam2-large"):
        svc._get_sam2_tracker(artifact_id="sam2-large")


# ---------------------------------------------------------------- GVM

def _fake_gvm(monkeypatch):
    from gvm_core import wrapper

    calls = []

    class Component:
        @classmethod
        def from_pretrained(cls, path, **kw):
            calls.append(path)
            return cls()

    class Pipeline:
        def __init__(self, **_): pass
        def to(self, *_a, **_k): return self

    monkeypatch.setattr(model_runtime, "validate_local_model_path", lambda aid, path, components=None: Path(path))
    for name in ("AutoencoderKLTemporalDecoder", "FlowMatchEulerDiscreteScheduler", "UNetSpatioTemporalConditionModel"):
        monkeypatch.setattr(wrapper, name, Component)
    monkeypatch.setattr(wrapper, "GVMPipeline", Pipeline)
    return calls


def test_gvm_loads_supplied_folder_and_missing_fails(monkeypatch, custom):
    calls = _fake_gvm(monkeypatch)
    gvm = _dir(custom / "gvm")
    svc = _service({"gvm": gvm})
    proc = svc._get_gvm()
    assert calls == [str(gvm)] * 3 and proc._model_asset_lease is None
    svc.unload_engines()
    assert svc._gvm_processor is None
    with pytest.raises(FileNotFoundError, match="model file missing"):
        _service({"gvm": custom / "gone"})._get_gvm()


# ---------------------------------------------------------------- BiRefNet, every variant

def test_every_birefnet_variant_loads_its_supplied_folder(monkeypatch, custom):
    from modules.BiRefNetModule.wrapper import BIREFNET_MODELS

    opened = []

    class Model:
        def to(self, *_): return self
        def eval(self): return self
        def half(self): return self

    class Auto:
        @staticmethod
        def from_pretrained(path, **kw):
            opened.append(path)
            return Model()

    monkeypatch.setitem(sys.modules, "transformers", types.SimpleNamespace(AutoModelForImageSegmentation=Auto))
    dirs = {f"birefnet-{repo}": _dir(custom / repo) for repo in set(BIREFNET_MODELS.values())}
    svc = _service(dirs)
    for usage, repo in BIREFNET_MODELS.items():
        proc = svc._get_birefnet(usage=usage)
        assert opened[-1] == str(dirs[f"birefnet-{repo}"]) and proc._model_asset_lease is None
    proc.unload()
    assert proc._model is None
    with pytest.raises(FileNotFoundError, match="model file missing"):
        _service({"birefnet-BiRefNet-matting": custom / "gone"})._get_birefnet(usage="Matting")


# ---------------------------------------------------------------- MatAnyone2

def test_matanyone2_loads_supplied_checkpoint(monkeypatch, custom):
    loaded = []
    for name in ("matanyone2", "matanyone2.utils", "matanyone2.inference"):
        m = types.ModuleType(name)
        m.__path__ = []
        monkeypatch.setitem(sys.modules, name, m)
    gm = types.ModuleType("matanyone2.utils.get_default_model")
    gm.get_matanyone2_model = lambda path, device: loaded.append(path) or types.SimpleNamespace(cfg=None)
    ic = types.ModuleType("matanyone2.inference.inference_core")
    ic.InferenceCore = lambda model, cfg: types.SimpleNamespace(clear_memory=lambda: None)
    monkeypatch.setitem(sys.modules, "matanyone2.utils.get_default_model", gm)
    monkeypatch.setitem(sys.modules, "matanyone2.inference.inference_core", ic)
    ckpt = _file(custom / "ma" / "mine.pth")
    svc = _service({"matanyone2": ckpt})
    proc = svc._get_matanyone2()
    proc._ensure_loaded()
    assert loaded == [str(ckpt)] and proc._model_asset_lease is None
    proc.unload()
    assert proc._processor is None
    with pytest.raises(FileNotFoundError, match="model file missing"):
        _service({"matanyone2": custom / "gone.pth"})._get_matanyone2()._ensure_loaded()
    assert loaded == [str(ckpt)]


# ---------------------------------------------------------------- VideoMaMa, both components

def test_videomama_loads_both_supplied_components(monkeypatch, custom):
    from VideoMaMaInferenceModule import inference

    made = []
    monkeypatch.setattr(model_runtime, "validate_local_model_path", lambda aid, path, components=None: Path(path))
    monkeypatch.setattr(inference, "VideoInferencePipeline", lambda **kw: made.append(kw) or types.SimpleNamespace())
    svd, unet = _dir(custom / "svd"), _dir(custom / "mama")
    svc = _service({"videomama-svd": svd, "videomama": unet})
    svc._get_videomama_pipeline()
    assert made[0]["base_model_path"] == str(svd) and made[0]["unet_checkpoint_path"] == str(unet)
    svc.unload_engines()
    assert svc._videomama_pipeline is None
    for bad in ({"videomama-svd": custom / "gone", "videomama": unet}, {"videomama-svd": svd, "videomama": custom / "gone"}):
        with pytest.raises(FileNotFoundError, match="model file missing"):
            _service(bad)._get_videomama_pipeline()
    with pytest.raises(FileNotFoundError, match="no path supplied for videomama-svd"):
        _service({"videomama": unet})._get_videomama_pipeline()
    assert len(made) == 1


# ---------------------------------------------------------------- job fields

def test_job_model_files_must_be_clean_absolute_paths(tmp_path):
    assert _check_model_files(None) is None
    ok = _check_model_files({"gvm": str(tmp_path)})
    assert dict(ok) == {"gvm": str(tmp_path)}
    for bad in ({}, {"gvm": "relative/x"}, {"": str(tmp_path)}, [str(tmp_path)], {"gvm": str(tmp_path) + "/../x"}):
        with pytest.raises(KeyJobRejected):
            _check_model_files(bad)


def test_job_without_bindings_fails_clearly_with_no_fallback():
    svc = CorridorKeyService()
    with pytest.raises(FileNotFoundError, match="supplied no model files"):
        svc._supplied("gvm")
