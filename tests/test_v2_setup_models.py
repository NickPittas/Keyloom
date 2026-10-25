"""setup_models CorridorKey 2.0 flow (no network)."""
import importlib.util
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))
from backend import v2_paths  # noqa: E402

_spec = importlib.util.spec_from_file_location("setup_models_v2", ROOT / "scripts" / "setup_models.py")
sm = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(sm)


def _write_bundle(root):
    """Lay out the files a successful CK2 bundle download leaves behind."""
    for n in ("corridorkey_v2", "foundation_stats"):
        f = root / "weights" / f"{n}.safetensors"
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_bytes(b"x")
    d = root / "vfms/cradio/C-RADIOv4-SO400M"
    d.mkdir(parents=True, exist_ok=True)
    (d / "config.json").write_text("{}")
    (d / "hf_model.py").write_text("#")
    (d / "model.safetensors").write_bytes(b"x")
    f = root / "vfms/moge/model.pt"
    f.parent.mkdir(parents=True, exist_ok=True)
    f.write_bytes(b"x")
    f = root / "vfms/rvm/pretrain_rvm_small16_256_204031069.npz"
    f.parent.mkdir(parents=True, exist_ok=True)
    f.write_bytes(b"x")


class FakeManager:
    """Stands in for ProvisionManager.op_download: writes the bundle only on success."""
    ok = True

    def __init__(self, root):
        self.root = root

    def op_download(self, targets):
        assert targets == ["corridorkey"]
        if self.ok:
            _write_bundle(self.root)
        return {"ok": self.ok, "failed": [], "cancelled": False}


@pytest.fixture
def env(tmp_path, monkeypatch):
    monkeypatch.setenv("CORRIDORKEY_V2_MODELS_DIR", str(tmp_path))
    monkeypatch.setattr(sm, "create_provision_manager", lambda **kwargs: FakeManager(tmp_path))
    return tmp_path


def test_not_installed_initially(env):
    assert sm.is_installed("corridorkey") is False


def test_download_goes_through_manager(env):
    assert sm.download_corridorkey() is True
    assert sm.is_installed("corridorkey") is True


def test_failed_manager_result_returns_false(env, monkeypatch):
    monkeypatch.setattr(FakeManager, "ok", False)
    assert sm.download_corridorkey() is False
    assert not sm.is_installed("corridorkey")


def test_download_exception_returns_false(env, monkeypatch):
    def boom(**kwargs):
        raise RuntimeError("offline")
    monkeypatch.setattr(sm, "create_provision_manager", boom)
    assert sm.download_corridorkey() is False


def test_check_lists_problems(env, monkeypatch, capsys):
    tmp = env
    monkeypatch.setattr(sys, "argv", ["setup_models.py", "--check"])
    sm.main()
    out = capsys.readouterr().out
    assert "CorridorKey 2.0" in out and "NOT INSTALLED" in out
    assert str(tmp) in out and "missing" in out


def test_check_installed(env, monkeypatch, capsys):
    sm.download_corridorkey()
    monkeypatch.setattr(sys, "argv", ["setup_models.py", "--check"])
    sm.main()
    out = capsys.readouterr().out
    assert "[OK] CorridorKey 2.0" in out


def test_help(monkeypatch, capsys):
    monkeypatch.setattr(sys, "argv", ["setup_models.py", "--help"])
    with pytest.raises(SystemExit) as e:
        sm.main()
    assert e.value.code == 0
    assert "CorridorKey 2.0 weights + foundation models (~5GB, required)" in " ".join(capsys.readouterr().out.split())


@pytest.mark.parametrize("flag", ["--corridorkey-blue", "--corridorkey-mlx"])
def test_removed_flags(monkeypatch, flag):
    monkeypatch.setattr(sys, "argv", ["setup_models.py", flag])
    with pytest.raises(SystemExit) as e:
        sm.main()
    assert e.value.code == 2


def test_removed_symbols():
    for n in ("MLX_CHECKPOINTS", "is_mlx_installed", "download_corridorkey_mlx", "download_corridorkey_blue"):
        assert not hasattr(sm, n)
    assert "corridorkey-blue" not in sm.MODELS


def test_real_loader_exposes_functions():
    mod = sm._load_v2_downloader()
    assert mod.CORRIDORKEY_REPO == "nikopueringer/CorridorKey2.0"
    assert callable(mod.download_rvm)
