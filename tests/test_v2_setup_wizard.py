"""Setup wizard checks/downloads the CorridorKey 2.0 bundle (no v1 .pth)."""
import os
from pathlib import Path

import pytest

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PySide6.QtWidgets import QApplication

from backend.v2_paths import get_v2_asset_paths
from ui.widgets import setup_wizard as sw


@pytest.fixture(scope="module")
def qapp():
    return QApplication.instance() or QApplication([])


@pytest.fixture
def models_dir(tmp_path, monkeypatch):
    monkeypatch.setenv("CORRIDORKEY_V2_MODELS_DIR", str(tmp_path))
    return tmp_path


def _touch(p: Path, data=b"x"):
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_bytes(data)


def _fill_bundle():
    a = get_v2_asset_paths()
    for f in (a.weights, a.foundation_stats, a.rvm_ckpt, a.moge_checkpoint):
        _touch(f)
    _touch(a.cradio_dir / "config.json", b"{}")
    _touch(a.cradio_dir / "hf_model.py")
    _touch(a.cradio_dir / "model.safetensors")
    return a


def _ready(tmp_path):
    return sw.detect_installed_models(tmp_path)["corridorkey"]


def test_empty_not_ready(models_dir):
    assert _ready(models_dir) is False


def test_v1_pth_does_not_count(models_dir, tmp_path):
    _touch(tmp_path / "CorridorKeyModule" / "checkpoints" / "CorridorKey_v1.0.pth")
    assert _ready(tmp_path) is False


def test_incomplete_cradio_not_ready(models_dir):
    a = _fill_bundle()
    (a.cradio_dir / "model.safetensors").unlink()
    assert _ready(models_dir) is False


def test_full_bundle_ready(models_dir):
    _fill_bundle()
    assert _ready(models_dir) is True


def test_catalogue_has_no_v1_blue_mlx():
    keys = [m["key"] for m in sw.MODELS]
    assert "corridorkey-blue" not in keys and "corridorkey-mlx" not in keys
    ck = next(m for m in sw.MODELS if m["key"] == "corridorkey")
    assert ck["required"] and ck["size"] == "~5 GB" and "2.0" in ck["label"]


class _FakeSetup:
    def __init__(self, result):
        self.result = result
        self.calls = 0

    def download_corridorkey(self):
        self.calls += 1
        return self.result


@pytest.mark.parametrize("result", [True, False])
def test_download_worker_calls_v2(result):
    fake = _FakeSetup(result)
    worker = sw._DownloadWorker(["corridorkey"])
    assert worker._download_one(fake, "corridorkey") is result
    assert fake.calls == 1


def test_run_surfaces_false_as_error(monkeypatch):
    fake = _FakeSetup(False)
    monkeypatch.setattr(sw, "_load_setup_models", lambda: fake)
    worker = sw._DownloadWorker(["corridorkey"])
    finished, done = [], []
    worker.model_finished.connect(lambda k, ok, msg: finished.append((k, ok, msg)))
    worker.all_finished.connect(done.append)
    worker.run()
    assert finished == [("corridorkey", False, "Failed")]
    assert done == [False]


def test_wizard_constructs(qapp):
    w = sw.SetupWizard()
    keys = {r.key for r in w.findChildren(sw._ModelRow)}
    assert "corridorkey" in keys
    assert not keys & {"corridorkey-blue", "corridorkey-mlx"}
    w.close()
