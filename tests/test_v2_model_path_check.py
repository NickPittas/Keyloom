import json
import os
import subprocess
import sys
import types
from pathlib import Path

import pytest

import main as app_main
from backend import model_runtime
from backend.model_catalog import Artifact, Catalog, FileSpec
from backend.model_status import Roots, StatusChecker, file_digest

ROOT = Path(__file__).resolve().parent.parent


def _make_bundle(root: Path):
    for rel in (
        "weights/corridorkey_v2.safetensors",
        "weights/foundation_stats.safetensors",
        "vfms/rvm/pretrain_rvm_small16_256_204031069.npz",
        "vfms/moge/model.pt",
    ):
        f = root / rel
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_bytes(b"x")
    c = root / "vfms/cradio/C-RADIOv4-SO400M"
    c.mkdir(parents=True, exist_ok=True)
    (c / "config.json").write_text("{}")
    (c / "hf_model.py").write_text("#")
    (c / "model.safetensors").write_bytes(b"x")


def _run(tmp_path, capsys):
    rc = app_main.run_model_path_check(str(tmp_path / "data"))
    out = capsys.readouterr().out
    return rc, json.loads(out[out.index("{"):])


@pytest.fixture
def models(tmp_path, monkeypatch):
    d = tmp_path / "models"
    monkeypatch.setenv("CORRIDORKEY_V2_MODELS_DIR", str(d))
    return d


def test_missing_assets_reported(models, tmp_path, capsys):
    rc, res = _run(tmp_path, capsys)
    assert rc == 1
    assert res["corridorkey_v2"] == "MISSING"
    assert any("setup_models.py --corridorkey" in e for e in res["errors"])


def test_full_bundle_ok_and_no_model_load(models, tmp_path, capsys, monkeypatch):
    _make_bundle(models)

    def boom(*a, **k):
        raise AssertionError("engine.load called")

    monkeypatch.setattr("backend.corridorkey_v2.CorridorKeyV2Engine.load", boom)
    rc, res = _run(tmp_path, capsys)
    assert res["corridorkey_v2"] == "OK"
    assert not [e for e in res.get("errors", []) if "CorridorKey 2.0" in e]
    assert res["corridorkey_v2_models_dir"] == str(models)
    assert res["corridorkey_v2_code_root"].endswith("CorridorKeyV2")


def test_opt_mode_rejected():
    r = subprocess.run(
        [sys.executable, "main.py", "--opt-mode", "speed", "--help"],
        cwd=ROOT, capture_output=True, text=True,
        env={**os.environ, "QT_QPA_PLATFORM": "offscreen"},
    )
    assert "--opt-mode" not in r.stdout


# ── BiRefNet diagnostic load guard (--verify-model-loads) ──────────────────

_BIREFNET_ARTIFACT_ID = "birefnet-BiRefNet-matting"
_BIREFNET_SNAPSHOT_REL = "modules/BiRefNetModule/checkpoints/BiRefNet-matting"
_BIREFNET_FILES = ("BiRefNet_config.py", "birefnet.py", "config.json", "model.safetensors")


def _birefnet_catalog() -> Catalog:
    artifact = Artifact(
        id=_BIREFNET_ARTIFACT_ID,
        label="BiRefNet Matting",
        capability="hint-auto",
        kind="hf_snapshot",
        required_profile=False,
        layout_root="data_dir",
        layout_rel=_BIREFNET_SNAPSHOT_REL,
        files=tuple(
            FileSpec(
                path=name,
                layout_path=name,
                size=None,
                sha256="a" * 64,
                sha256_source="test",
                lfs=False,
                required=True,
            )
            for name in _BIREFNET_FILES
        ),
    )
    return Catalog(1, "", {}, {artifact.id: artifact})


def _birefnet_roots(tmp_path: Path) -> Roots:
    return Roots(
        tmp_path / "data",
        tmp_path / "models",
        tmp_path / "package",
        tmp_path / "hf",
        tmp_path / "cache",
    )


def _install_birefnet_snapshot(roots: Roots) -> Path:
    snapshot = roots.data_dir / _BIREFNET_SNAPSHOT_REL
    snapshot.mkdir(parents=True)
    for name in _BIREFNET_FILES:
        (snapshot / name).write_text("synthetic")
    return snapshot


def _seed_birefnet_verification(
    catalog: Catalog, roots: Roots, *, mismatch_file: str | None = None
) -> None:
    """Record cached digests so the checker sees verified/mismatched files."""
    checker = StatusChecker(catalog, roots)
    artifact = catalog.get(_BIREFNET_ARTIFACT_ID)
    for file in artifact.required_files():
        digest = file_digest(artifact, file)
        path = checker.file_path(artifact, file)
        bad = file.layout_path == mismatch_file
        checker.hash_cache.record(
            path, digest, not bad, ("f" * 64) if bad else digest.expected, artifact.id
        )
    checker.hash_cache.save()


class _FakeLease:
    """Stand-in for backend.model_runtime.ModelArtifactLease (fspath + close)."""

    def __init__(self, path: Path):
        self.path = path
        self.closed = 0

    def __fspath__(self):
        return os.fspath(self.path)

    def close(self):
        self.closed += 1


def _fake_transformers(monkeypatch, calls: list, lease: "_FakeLease | None" = None) -> None:
    class Model:
        def to(self, *_args): return self
        def eval(self): return self
        def parameters(self): return iter(())

    def record(kind: str, path, kwargs: dict) -> None:
        if lease is None:
            calls.append((kind, path, kwargs))
        else:
            # Lease must still be open while the loaders run.
            calls.append((kind, path, kwargs, lease.closed))

    class AutoConfig:
        @staticmethod
        def from_pretrained(path, **kwargs):
            record("config", path, kwargs)
            return types.SimpleNamespace(model_type="birefnet")

    class AutoModel:
        @staticmethod
        def from_pretrained(path, **kwargs):
            record("model", path, kwargs)
            return Model()

    monkeypatch.setitem(
        sys.modules,
        "transformers",
        types.SimpleNamespace(AutoConfig=AutoConfig, AutoModelForImageSegmentation=AutoModel),
    )


def test_clear_mps_cache_skipped_without_mps_backend(monkeypatch):
    # Real availability gate on this CPU-only machine; empty_cache is recorded
    # only to prove the gate never calls it, so a regression fails here.
    import torch

    if torch.backends.mps.is_available():
        pytest.skip("this machine has an MPS backend")
    calls: list = []
    monkeypatch.setattr(torch.mps, "empty_cache", lambda: calls.append(1))
    app_main._clear_mps_cache()
    assert calls == []


def test_clear_mps_cache_runs_when_backend_available(monkeypatch):
    import torch

    calls: list = []
    monkeypatch.setattr(torch.backends.mps, "is_available", lambda: True)
    monkeypatch.setattr(torch.mps, "empty_cache", lambda: calls.append(1))
    app_main._clear_mps_cache()
    assert calls == [1]


def test_matanyone2_diagnostic_leases_managed_checkpoint_until_cleanup(monkeypatch, tmp_path):
    ckpt = tmp_path / "matanyone2.pth"
    ckpt.write_bytes(b"synthetic")
    lease = _FakeLease(ckpt)
    acquired: list = []
    events: list = []
    roots = _birefnet_roots(tmp_path)
    monkeypatch.setattr(
        model_runtime,
        "acquire_model_artifact",
        lambda artifact_id, roots=None, **kw: acquired.append((artifact_id, roots)) or lease,
    )

    class FakeProcessor:
        def __init__(self, *, device, ckpt_path, n_warmup):
            events.append(("load", device, ckpt_path, n_warmup, lease.closed))
            self._model = types.SimpleNamespace(parameters=lambda: iter(()))

        def _ensure_loaded(self):
            events.append(("ensure", lease.closed))

        def clear(self):
            events.append(("clear", lease.closed))

    monkeypatch.setattr(
        "modules.MatAnyone2Module.wrapper.MatAnyone2Processor", FakeProcessor
    )

    # No MPS stub: the real cleanup path must not fail on this CPU-only box.
    info = app_main._verify_matanyone2_model_load(roots=roots)

    assert acquired == [("matanyone2", roots)]
    assert events == [("load", "cpu", str(ckpt), 0, 0), ("ensure", 0), ("clear", 0)]
    assert lease.closed == 1
    assert info["path"] == str(ckpt) and info["parameter_count"] == 0


def test_matanyone2_diagnostic_closes_lease_when_load_fails(monkeypatch, tmp_path):
    lease = _FakeLease(tmp_path / "matanyone2.pth")
    monkeypatch.setattr(model_runtime, "acquire_model_artifact", lambda *a, **k: lease)

    class FailingProcessor:
        def __init__(self, **_kw): pass
        def _ensure_loaded(self): raise RuntimeError("boom")
        def clear(self): pass

    monkeypatch.setattr(
        "modules.MatAnyone2Module.wrapper.MatAnyone2Processor", FailingProcessor
    )
    with pytest.raises(RuntimeError, match="boom"):
        app_main._verify_matanyone2_model_load(roots=_birefnet_roots(tmp_path))
    assert lease.closed == 1


def test_optional_downloads_use_explicit_data_dir_through_public_check(
    models, tmp_path, capsys, monkeypatch
):
    _make_bundle(models)
    _stub_v2_engine(monkeypatch)
    from backend import model_provision

    data = tmp_path / "chosen-data"
    seen: list = []

    def fake_op_download(self, targets):
        seen.append((targets, self.roots))
        return {"ok": True, "cancelled": False}

    monkeypatch.setattr(model_provision.ProvisionManager, "op_download", fake_op_download)
    app_main.run_model_path_check(str(data), verify_optional_model_downloads=True)
    out = capsys.readouterr().out
    res = json.loads(out[out.index("{"):])

    assert [t for t, _ in seen] == [["matanyone2"], ["birefnet-BiRefNet-matting"]]
    assert all(r.data_dir == Path(os.path.abspath(str(data))) for _, r in seen)
    assert not [e for e in res.get("errors", []) if "download failed" in e]


def test_birefnet_diagnostic_blocks_loaders_on_unverified_snapshot(monkeypatch, tmp_path):
    catalog = _birefnet_catalog()
    monkeypatch.setattr(model_runtime, "load_catalog", lambda: catalog)
    roots = _birefnet_roots(tmp_path)
    _install_birefnet_snapshot(roots)
    calls: list = []
    _fake_transformers(monkeypatch, calls)

    with pytest.raises(model_runtime.ModelAssetsUnavailableError, match="Verify required"):
        app_main._verify_birefnet_model_load(_BIREFNET_ARTIFACT_ID, roots=roots)
    assert calls == []


def test_birefnet_diagnostic_blocks_loaders_on_digest_mismatch(monkeypatch, tmp_path):
    catalog = _birefnet_catalog()
    monkeypatch.setattr(model_runtime, "load_catalog", lambda: catalog)
    roots = _birefnet_roots(tmp_path)
    _install_birefnet_snapshot(roots)
    _seed_birefnet_verification(catalog, roots, mismatch_file="model.safetensors")
    calls: list = []
    _fake_transformers(monkeypatch, calls)

    with pytest.raises(
        model_runtime.ModelAssetsUnavailableError, match="known trusted-digest mismatch"
    ):
        app_main._verify_birefnet_model_load(_BIREFNET_ARTIFACT_ID, roots=roots)
    assert calls == []


def test_birefnet_diagnostic_loads_verified_snapshot_local_only(monkeypatch, tmp_path):
    catalog = _birefnet_catalog()
    monkeypatch.setattr(model_runtime, "load_catalog", lambda: catalog)
    roots = _birefnet_roots(tmp_path)
    snapshot = _install_birefnet_snapshot(roots)
    _seed_birefnet_verification(catalog, roots)
    calls: list = []
    _fake_transformers(monkeypatch, calls)

    info = app_main._verify_birefnet_model_load(_BIREFNET_ARTIFACT_ID, roots=roots)

    assert [c[0] for c in calls] == ["config", "model"]
    assert all(c[1] == str(snapshot) for c in calls)
    assert all(c[2] == {"trust_remote_code": True, "local_files_only": True} for c in calls)
    assert info == {
        "path": str(snapshot),
        "model_type": "birefnet",
        "parameter_count": 0,
    }


def test_birefnet_diagnostic_keeps_lease_alive_until_model_cleanup(monkeypatch, tmp_path):
    snapshot = tmp_path / "BiRefNet-matting"
    snapshot.mkdir()
    lease = _FakeLease(snapshot)
    monkeypatch.setattr(model_runtime, "require_model_artifact", lambda *_a, **_k: lease)
    calls: list = []
    _fake_transformers(monkeypatch, calls, lease=lease)

    info = app_main._verify_birefnet_model_load(_BIREFNET_ARTIFACT_ID, roots=object())

    assert [c[0] for c in calls] == ["config", "model"]
    assert all(c[1] == str(snapshot) for c in calls)  # lease path, never an object repr
    assert all(c[3] == 0 for c in calls)              # lease still open for both loaders
    assert lease.closed == 1                           # closed with the model cleanup
    assert info["path"] == str(snapshot)


def test_birefnet_diagnostic_closes_lease_when_loader_fails(monkeypatch, tmp_path):
    snapshot = tmp_path / "BiRefNet-matting"
    snapshot.mkdir()
    lease = _FakeLease(snapshot)
    monkeypatch.setattr(model_runtime, "require_model_artifact", lambda *_a, **_k: lease)

    class AutoConfig:
        @staticmethod
        def from_pretrained(path, **kwargs):
            return types.SimpleNamespace(model_type="birefnet")

    class BoomModel:
        @staticmethod
        def from_pretrained(path, **kwargs):
            raise RuntimeError("snapshot load failed")

    monkeypatch.setitem(
        sys.modules,
        "transformers",
        types.SimpleNamespace(AutoConfig=AutoConfig, AutoModelForImageSegmentation=BoomModel),
    )

    with pytest.raises(RuntimeError, match="snapshot load failed"):
        app_main._verify_birefnet_model_load(_BIREFNET_ARTIFACT_ID, roots=object())
    assert lease.closed == 1


def _stub_v2_engine(monkeypatch):
    monkeypatch.setattr("backend.corridorkey_v2.CorridorKeyV2Engine.load", lambda self: None)
    monkeypatch.setattr("backend.corridorkey_v2.CorridorKeyV2Engine.unload", lambda self: None)


def _run_load_check(capsys, data: Path):
    app_main.run_model_path_check(str(data), verify_model_loads=True)
    out = capsys.readouterr().out
    return json.loads(out[out.index("{"):])


def test_verify_loads_guards_selected_birefnet_with_diagnostic_roots(
    models, tmp_path, capsys, monkeypatch
):
    _make_bundle(models)
    _stub_v2_engine(monkeypatch)
    data = tmp_path / "data"
    snapshot = data / _BIREFNET_SNAPSHOT_REL
    snapshot.mkdir(parents=True)
    (snapshot / "model.safetensors").write_bytes(b"x")

    lease = _FakeLease(snapshot)
    guard_calls: list = []
    monkeypatch.setattr(
        model_runtime,
        "require_model_artifact",
        lambda artifact_id, roots=None, **kwargs: guard_calls.append((artifact_id, roots))
        or lease,
    )
    calls: list = []
    _fake_transformers(monkeypatch, calls, lease=lease)

    # Public argument path only: no backend.project._app_dir pinning.
    res = _run_load_check(capsys, data)

    assert guard_calls, res
    assert guard_calls[0][0] == _BIREFNET_ARTIFACT_ID
    assert guard_calls[0][1].data_dir == Path(os.path.abspath(str(data)))
    assert [c[0] for c in calls] == ["config", "model"]
    assert all(c[2] == {"trust_remote_code": True, "local_files_only": True} for c in calls)
    assert all(c[3] == 0 for c in calls)
    assert lease.closed == 1
    assert res["data_dir"] == os.path.abspath(str(data))
    assert res["setup_models_project_root"] == os.path.abspath(str(data))
    assert not [
        e for e in res.get("errors", [])
        if "mismatch" in e or "outside data dir" in e
    ]
    assert res["model_loads"]["birefnet"]["path"] == str(snapshot)
    assert not [e for e in res.get("errors", []) if "model load failed" in e.lower()]


def test_verify_loads_reports_unverified_birefnet_without_loading(
    models, tmp_path, capsys, monkeypatch
):
    _make_bundle(models)
    _stub_v2_engine(monkeypatch)
    data = tmp_path / "data"
    snapshot = data / _BIREFNET_SNAPSHOT_REL
    snapshot.mkdir(parents=True)
    (snapshot / "model.safetensors").write_bytes(b"x")

    def _guard_refuses(*_args, **_kwargs):
        raise model_runtime.ModelAssetsUnavailableError(
            "installed files have not been verified against trusted digests"
        )

    monkeypatch.setattr(model_runtime, "require_model_artifact", _guard_refuses)
    calls: list = []
    _fake_transformers(monkeypatch, calls)

    res = _run_load_check(capsys, data)

    assert calls == []
    assert "birefnet" not in res.get("model_loads", {})
    assert [
        e for e in res["errors"]
        if "BiRefNet model load failed" in e and "have not been verified" in e
    ]


def test_verify_loads_descriptor_data_root_distinct_from_code_root(
    models, tmp_path, capsys, monkeypatch
):
    # Installed-app simulation: descriptor user_data is the data root while
    # the code root stays the developer checkout; the setup helper's derived
    # targets must be re-rooted to the diagnostic data root before inspection.
    _make_bundle(models)
    _stub_v2_engine(monkeypatch)
    data = tmp_path / "chosen-user-data"
    descriptor = tmp_path / "app" / "corridorkey-install.json"
    descriptor.parent.mkdir()
    descriptor.write_text(json.dumps({
        "schema": 1,
        "app_name": "CorridorKey",
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
    snapshot = data / _BIREFNET_SNAPSHOT_REL
    snapshot.mkdir(parents=True)
    (snapshot / "model.safetensors").write_bytes(b"x")

    lease = _FakeLease(snapshot)
    guard_calls: list = []
    monkeypatch.setattr(
        model_runtime,
        "require_model_artifact",
        lambda artifact_id, roots=None, **kwargs: guard_calls.append((artifact_id, roots))
        or lease,
    )
    _fake_transformers(monkeypatch, [], lease=lease)

    res = _run_load_check(capsys, data)

    assert res["data_dir"] == os.path.abspath(str(data))
    assert res["data_dir"] != str(ROOT)  # distinct code and data roots
    assert guard_calls and guard_calls[0][0] == _BIREFNET_ARTIFACT_ID
    assert guard_calls[0][1].data_dir == Path(os.path.abspath(str(data)))
    assert res["setup_models_project_root"] == os.path.abspath(str(data))
    assert res["setup_model_dirs"]["birefnet"] == str(
        Path(data) / "modules" / "BiRefNetModule" / "checkpoints" / "BiRefNet-matting"
    )
    assert res["ok"] is True, res.get("errors")
    assert res["model_loads"]["birefnet"]["path"] == str(snapshot)
    assert lease.closed == 1
