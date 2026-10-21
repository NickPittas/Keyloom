import dataclasses
import importlib
import sys
from pathlib import Path

import pytest

from backend import v2_paths as vp


@pytest.fixture
def linux(monkeypatch):
    monkeypatch.setattr(sys, "platform", "linux")
    monkeypatch.delenv("CORRIDORKEY_V2_MODELS_DIR", raising=False)
    monkeypatch.delenv("XDG_DATA_HOME", raising=False)


def test_env_override_wins(linux, monkeypatch, tmp_path):
    monkeypatch.setenv("CORRIDORKEY_V2_MODELS_DIR", str(tmp_path / "m"))
    monkeypatch.setenv("XDG_DATA_HOME", str(tmp_path / "xdg"))
    assert vp.get_v2_models_dir() == tmp_path / "m"


def test_xdg_used(linux, monkeypatch, tmp_path):
    monkeypatch.setenv("XDG_DATA_HOME", str(tmp_path))
    assert vp.get_v2_models_dir() == tmp_path / "EZ-CorridorKey2" / "CorridorKeyV2"


def test_home_fallback_and_no_mkdir(linux, monkeypatch, tmp_path):
    monkeypatch.setenv("HOME", str(tmp_path))
    d = vp.get_v2_models_dir()
    assert d == tmp_path / ".local/share/EZ-CorridorKey2/CorridorKeyV2"
    assert not d.exists()


def test_frozen_root(monkeypatch, tmp_path):
    monkeypatch.setattr(sys, "frozen", True, raising=False)
    monkeypatch.setattr(sys, "_MEIPASS", str(tmp_path), raising=False)
    assert vp.get_v2_package_root() == tmp_path / "CorridorKeyV2"


def test_real_package_root():
    root = vp.get_v2_package_root()
    assert (root / "corridorkey" / "runtime.py").is_file()
    assert (root / "third_party" / "MoGe").is_dir()


def _bundle(tmp_path, monkeypatch):
    monkeypatch.setenv("CORRIDORKEY_V2_MODELS_DIR", str(tmp_path))
    paths = vp.get_v2_asset_paths()
    for p in (paths.weights, paths.foundation_stats, paths.rvm_ckpt, paths.moge_checkpoint):
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(b"x")
    paths.cradio_dir.mkdir(parents=True)
    for name in ("config.json", "hf_model.py", "model.safetensors"):
        (paths.cradio_dir / name).write_bytes(b"x")
    return paths


def test_full_bundle_valid(tmp_path, monkeypatch):
    paths = _bundle(tmp_path, monkeypatch)
    assert vp.validate_v2_assets(paths) == []
    assert vp.v2_assets_ready(paths)


def test_missing_and_empty_reported(tmp_path, monkeypatch):
    paths = _bundle(tmp_path, monkeypatch)
    paths.moge_checkpoint.unlink()
    paths.rvm_ckpt.write_bytes(b"")
    problems = vp.validate_v2_assets(paths)
    assert len(problems) == 2
    assert not vp.v2_assets_ready(paths)


@pytest.mark.parametrize("name", ["config.json", "hf_model.py", "model.safetensors"])
def test_cradio_incomplete(tmp_path, monkeypatch, name):
    paths = _bundle(tmp_path, monkeypatch)
    (paths.cradio_dir / name).unlink()
    assert len(vp.validate_v2_assets(paths)) == 1


def test_runtime_kwargs_match_runtimepaths():
    rt = importlib.import_module("CorridorKeyV2.corridorkey.runtime")
    names = {f.name for f in dataclasses.fields(rt.RuntimePaths)}
    assert set(vp.get_v2_asset_paths().as_runtime_kwargs()) == names
    assert all(isinstance(v, Path) for v in vp.get_v2_asset_paths().as_runtime_kwargs().values())
