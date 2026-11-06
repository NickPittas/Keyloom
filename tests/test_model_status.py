"""Offline status engine: full-closure presence, stat-keyed hash cache, explicit verify."""
import hashlib
import json
import sys
from dataclasses import replace
from pathlib import Path
from types import SimpleNamespace

import pytest

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

import backend.model_status as ms  # noqa: E402
from backend.model_catalog import Artifact, Catalog, FileSpec, hf_hub_cache_dir, hf_hub_cache_dirs  # noqa: E402


def git_blob_sha1(data: bytes) -> str:
    return hashlib.sha1(b"blob %d\x00" % len(data) + data).hexdigest()  # noqa: S324


def make_catalog(*arts: Artifact) -> Catalog:
    return Catalog(version=1, updated="test", meta={},
                   artifacts={a.id: a for a in arts})


def write_files(root: Path, paths: dict[str, bytes], lfs_like: set[str] | None = None) -> list[FileSpec]:
    """Write fixture files and return FileSpecs with true size + trusted digests."""
    specs = []
    for rel, data in paths.items():
        p = root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(data)
        is_lfs = rel in (lfs_like or set())
        specs.append(FileSpec(
            path=rel, layout_path=rel, size=len(data),
            sha256=hashlib.sha256(data).hexdigest() if is_lfs else None,
            sha256_source="hf-lfs-oid" if is_lfs else None,
            lfs=is_lfs, required=True,
            git_blob_sha1=None if is_lfs else git_blob_sha1(data),
        ))
    return specs


@pytest.fixture
def roots(tmp_path, monkeypatch):
    monkeypatch.delenv("HF_HUB_CACHE", raising=False)
    monkeypatch.delenv("HF_HOME", raising=False)
    from backend.model_status import Roots
    return Roots(
        data_dir=tmp_path / "data",
        v2_models_dir=tmp_path / "v2",
        v2_package_root=tmp_path / "pkg",
        hf_cache=tmp_path / "hf",
        cache_dir=tmp_path / "diag",
    )


def hf_snapshot_artifact(aid: str, files: list[FileSpec], layout_rel: str = "model") -> Artifact:
    return Artifact(id=aid, label=aid, capability="test", kind="hf_snapshot",
                    required_profile=False, layout_root="data_dir", layout_rel=layout_rel,
                    repo_id="t/{}".format(aid), revision="a" * 40, revision_status="pinned",
                    files=tuple(files))


class TestClosure:
    def test_missing_partial_installed(self, roots):
        art_dir = roots.data_dir / "model"
        files = write_files(art_dir, {"config.json": b"{}", "weights.bin": b"w" * 100,
                                      "code.py": b"print(1)"})
        art = hf_snapshot_artifact("m", files)
        cat = make_catalog(art)
        sc = ms.StatusChecker(cat, roots)

        assert sc.check_artifact(art)["state"] == "installed"

        (art_dir / "weights.bin").unlink()
        st = sc.check_artifact(art)
        assert st["state"] == "partial"
        assert any("missing" in p for p in st["problems"])

        for f in art_dir.iterdir():
            f.unlink()
        assert sc.check_artifact(art)["state"] == "missing"

    def test_not_sentinel_single_file(self, roots):
        """One sentinel present does not make a multi-file artifact installed."""
        art_dir = roots.data_dir / "model"
        files = write_files(art_dir, {"config.json": b"{}", "weights.bin": b"w" * 100})
        (art_dir / "weights.bin").unlink()  # only config.json (the old sentinel) remains
        art = hf_snapshot_artifact("m", files)
        sc = ms.StatusChecker(make_catalog(art), roots)
        assert sc.check_artifact(art)["state"] == "partial"

    def test_size_mismatch_flagged(self, roots):
        art_dir = roots.data_dir / "model"
        files = write_files(art_dir, {"weights.bin": b"w" * 100}, lfs_like={"weights.bin"})
        (art_dir / "weights.bin").write_bytes(b"short")
        art = hf_snapshot_artifact("m", files)
        st = ms.StatusChecker(make_catalog(art), roots).check_artifact(art)
        assert st["state"] == "partial"
        assert any("size mismatch" in p for p in st["problems"])

    def test_directory_entry_counts_as_present(self, roots):
        d = roots.v2_package_root / "third_party/MoGe"
        d.mkdir(parents=True)
        files = [FileSpec("third_party/MoGe", "third_party/MoGe", None, None, None, False, True)]
        art = Artifact(id="code", label="c", capability="t", kind="local_code",
                       required_profile=True, layout_root="v2_package_root", layout_rel="",
                       files=tuple(files))
        st = ms.StatusChecker(make_catalog(art), roots).check_artifact(art)
        assert st["state"] == "installed" and st["ready"]

    def test_directory_cannot_satisfy_a_required_file(self, roots):
        art_dir = roots.data_dir / "model"
        (art_dir / "weights.bin").mkdir(parents=True)
        spec = FileSpec("weights.bin", "weights.bin", 10, None, None, False, True)
        art = hf_snapshot_artifact("m", [spec])
        status = ms.StatusChecker(make_catalog(art), roots).check_artifact(art)
        assert status["state"] == "missing"
        assert status["problems"] == ["not a file: weights.bin"]


class TestVerification:
    def test_verify_sha256_and_cache_hit(self, roots, monkeypatch):
        art_dir = roots.data_dir / "model"
        files = write_files(art_dir, {"weights.bin": b"w" * 1000}, lfs_like={"weights.bin"})
        art = hf_snapshot_artifact("m", files)
        sc = ms.StatusChecker(make_catalog(art), roots)

        assert sc.check_artifact(art)["ready"] is False
        calls = []
        real = ms.digest_file
        monkeypatch.setattr(ms, "digest_file",
                            lambda p, dg: (calls.append(str(p)), real(p, dg))[1])

        assert sc.verify(["m"])[0]["verified"] is True
        assert len(calls) == 1
        assert sc.check_artifact(art)["ready"] is True
        # second check answers from the stat-keyed cache: no rehash
        st = sc.check_artifact(art)
        assert st["verified"] is True and st["verify_state"] == "verified"
        assert len(calls) == 1
        # but a new checker (fresh process) with the persisted cache also hits
        sc2 = ms.StatusChecker(make_catalog(art), roots)
        assert sc2.check_artifact(art)["verified"] is True
        calls.clear()
        sc2.check_artifact(art)
        assert not calls

    def test_stat_change_invalidates_cache(self, roots):
        art_dir = roots.data_dir / "model"
        files = write_files(art_dir, {"weights.bin": b"w" * 1000}, lfs_like={"weights.bin"})
        art = hf_snapshot_artifact("m", files)
        sc = ms.StatusChecker(make_catalog(art), roots)
        sc.verify_artifact(art)
        # same size, new mtime -> cache must not claim a verdict
        p = art_dir / "weights.bin"
        p.write_bytes(b"x" * 1000)
        st = sc.check_artifact(art)
        assert st["verified"] is False
        assert st["verify_state"] == "unknown (not verified yet)"

    def test_hash_mismatch_detected(self, roots):
        art_dir = roots.data_dir / "model"
        files = write_files(art_dir, {"weights.bin": b"w" * 1000}, lfs_like={"weights.bin"})
        (art_dir / "weights.bin").write_bytes(b"corrupted!!" * 100)  # same length, wrong bytes
        art = hf_snapshot_artifact("m", files)
        sc = ms.StatusChecker(make_catalog(art), roots)
        verdict = sc.verify_artifact(art)
        assert verdict["verified"] is False
        assert any("mismatch" in p for p in verdict["problems"])

    def test_file_mutated_during_hash_is_not_cached_or_ready(self, roots, monkeypatch):
        data = b"w" * 1000
        art_dir = roots.data_dir / "model"
        files = write_files(art_dir, {"weights.bin": data}, lfs_like={"weights.bin"})
        art = hf_snapshot_artifact("m", files)
        sc = ms.StatusChecker(make_catalog(art), roots)
        path = art_dir / "weights.bin"
        real = ms.digest_file

        def mutate_after_hash(p, dg):
            digest = real(p, dg)
            p.write_bytes(b"x" * len(data))
            return digest

        monkeypatch.setattr(ms, "digest_file", mutate_after_hash)
        result = sc.verify_artifact(art)
        assert result["verified"] is False
        assert any("changed during verification" in p for p in result["problems"])
        assert sc.hash_cache.entry(path) is None
        assert sc.check_artifact(art)["ready"] is False

    def test_git_blob_sha1_verification(self, roots):
        """Code/config files without LFS sha256 still verify via git blob oid."""
        art_dir = roots.data_dir / "model"
        files = write_files(art_dir, {"nets.py": b"class Net: pass\n"})
        art = hf_snapshot_artifact("m", files)
        sc = ms.StatusChecker(make_catalog(art), roots)
        assert sc.verify_artifact(art)["verified"] is True

    def test_unresolved_metadata_never_verified(self, roots):
        art_dir = roots.data_dir / "model"
        specs = write_files(art_dir, {"weights.bin": b"w" * 10})
        files = [FileSpec(s.path, s.layout_path, s.size, None, None, False, True)  # no digests at all
                 for s in specs]
        art = hf_snapshot_artifact("m", files)
        sc = ms.StatusChecker(make_catalog(art), roots)
        st = sc.check_artifact(art)
        assert st["state"] == "installed" and st["verified"] is False
        assert st["present"] is True and st["ready"] is False
        assert st["unhashed_files"] == ["weights.bin"]
        assert "no trusted upstream digest" in st["verify_note"]

    def test_md5_b64_verification(self, roots):
        import base64
        data = b"rvm-bytes"
        art_dir = roots.data_dir / "model"
        art_dir.mkdir(parents=True)
        (art_dir / "rvm.npz").write_bytes(data)
        f = FileSpec("rvm.npz", "rvm.npz", len(data), None, None, False, True)
        art = Artifact(id="rvm", label="rvm", capability="t", kind="http_file",
                       required_profile=False, layout_root="data_dir", layout_rel="model",
                       url="https://x/rvm.npz", md5_b64=base64.b64encode(
                           hashlib.md5(data).digest()).decode(),  # noqa: S324
                       files=(f,))
        sc = ms.StatusChecker(make_catalog(art), roots)
        assert sc.verify_artifact(art)["verified"] is True


class TestStartupContract:
    def test_status_is_offline_and_cheap(self, roots, monkeypatch):
        """status() must not hash or touch the network; only verify() hashes."""
        art_dir = roots.data_dir / "model"
        files = write_files(art_dir, {"weights.bin": b"w" * 1000}, lfs_like={"weights.bin"})
        art = hf_snapshot_artifact("m", files)
        sc = ms.StatusChecker(make_catalog(art), roots)

        def no_network(*a, **k):
            raise AssertionError("status() attempted network access")
        monkeypatch.setattr("urllib.request.urlopen", no_network)

        hashed = []
        real = ms.digest_file
        monkeypatch.setattr(ms, "digest_file",
                            lambda p, dg: (hashed.append(1), real(p, dg))[1])

        sc.status()
        assert not hashed

    def test_local_package_probe(self, roots, monkeypatch):
        art = Artifact(id="sam2-package", label="s", capability="t", kind="local_package",
                       required_profile=False, layout_root=None, layout_rel=None,
                       pip_spec="sam-2")
        monkeypatch.setitem(ms.PROBE_IMPORTS, "sam2-package", "definitely_not_a_module_xyz")
        sc = ms.StatusChecker(make_catalog(art), roots)
        assert sc.check_artifact(art)["state"] == "missing"
        monkeypatch.setitem(ms.PROBE_IMPORTS, "sam2-package", "pathlib")
        assert sc.check_artifact(art)["state"] == "installed"

    def test_birefnet_runtime_probe_checks_import_closure_and_timm_pin(self, roots, monkeypatch):
        art = Artifact(id="birefnet-runtime", label="BiRefNet runtime", capability="hint-auto",
                       kind="local_package", required_profile=False, layout_root=None,
                       layout_rel=None, pip_spec="timm==1.0.24",
                       probe_imports=("timm", "torch", "kornia"))
        available = {"timm", "torch", "kornia"}
        monkeypatch.setattr(ms, "_probe_import", lambda name: name in available)
        monkeypatch.setattr(ms.package_metadata, "version", lambda name: "1.0.24")
        sc = ms.StatusChecker(make_catalog(art), roots)

        available.remove("kornia")
        absent = sc.check_artifact(art)
        assert absent["present"] is False and absent["ready"] is False
        assert any("kornia" in problem for problem in absent["problems"])

        available.add("kornia")
        monkeypatch.setattr(ms.package_metadata, "version", lambda name: "1.0.23")
        wrong_pin = sc.check_artifact(art)
        assert wrong_pin["present"] is False and wrong_pin["ready"] is False
        assert any("timm==1.0.24" in problem for problem in wrong_pin["problems"])

        monkeypatch.setattr(ms.package_metadata, "version", lambda name: "1.0.24")
        ready = sc.check_artifact(art)
        assert ready["present"] is True and ready["verified"] is True and ready["ready"] is True

    def test_roots_use_install_paths_module_defaults(self, monkeypatch, tmp_path):
        from backend import install_paths, v2_paths

        monkeypatch.delenv("LOCALAPPDATA", raising=False)
        monkeypatch.setenv("XDG_DATA_HOME", str(tmp_path / "xdg-data"))
        monkeypatch.setenv("HF_HUB_CACHE", str(tmp_path / "hf-explicit"))
        monkeypatch.setenv("HF_HOME", str(tmp_path / "hf-home"))
        monkeypatch.setattr(Path, "home", lambda: tmp_path / "home")
        fake = SimpleNamespace(user_data=None, cache=None)
        monkeypatch.setattr(install_paths, "discover", lambda: fake)
        monkeypatch.setattr(install_paths, "platform_user_data", lambda: tmp_path / "platform-data")
        monkeypatch.setattr(install_paths, "platform_cache", lambda: tmp_path / "platform-cache")
        monkeypatch.setattr(v2_paths, "get_v2_models_dir", lambda: tmp_path / "models")
        monkeypatch.setattr(v2_paths, "get_v2_package_root", lambda: tmp_path / "package")

        resolved = ms.Roots.defaults()
        assert resolved.data_dir == tmp_path / "platform-data"
        assert resolved.cache_dir == tmp_path / "platform-cache" / "model-provision"
        assert resolved.legacy_models_dirs == (tmp_path / "xdg-data" / "Keyloom" / "EngineV2",)
        assert resolved.hf_caches == (
            tmp_path / "hf-explicit", tmp_path / "hf-home" / "hub",
            tmp_path / "home" / ".cache" / "huggingface" / "hub")
        configured = ms.Roots.defaults(hf_cache=tmp_path / "explicit-override")
        assert configured.hf_caches == (tmp_path / "explicit-override", *resolved.hf_caches)

    def test_summary_ready_required(self, roots):
        art_dir = roots.data_dir / "model"
        files = write_files(art_dir, {"w.bin": b"w"})
        required = hf_snapshot_artifact("req", files)
        required = Artifact(**{**required.__dict__, "required_profile": True})
        optional = hf_snapshot_artifact("opt", write_files(roots.data_dir / "opt", {"w.bin": b"w"}))
        sc = ms.StatusChecker(make_catalog(required, optional), roots)
        s = sc.status()
        assert s["artifacts"][0]["present"] is True
        assert s["summary"]["ready_required"] is False
        sc.verify(["req"])
        s = sc.status()
        assert s["summary"]["ready_required"] is True
        assert sc.status(["opt"])["summary"]["ready_required"] is None
        (roots.data_dir / "model" / "w.bin").unlink()
        assert sc.status()["summary"]["ready_required"] is False


def test_legacy_data_and_compatible_hf_snapshot_bind_exact_read_only_paths(roots, tmp_path, monkeypatch):
    from backend.app_models import AppModels

    explicit = tmp_path / "hf-explicit"
    hf_home = tmp_path / "hf-home"
    fake_home = tmp_path / "user-home"
    monkeypatch.setenv("HF_HUB_CACHE", str(explicit))
    monkeypatch.setenv("HF_HOME", str(hf_home))
    monkeypatch.setattr(Path, "home", lambda: fake_home)
    assert hf_hub_cache_dirs() == (
        explicit, hf_home / "hub", fake_home / ".cache" / "huggingface" / "hub")

    legacy_files = write_files(tmp_path / "old-data" / "module" / "weights", {"w.bin": b"legacy"})
    snapshot_files = write_files(tmp_path / "placeholder", {"w.bin": b"snapshot"})
    legacy = hf_snapshot_artifact("birefnet-legacy", legacy_files, "module/weights")
    cached = hf_snapshot_artifact("birefnet-cached", snapshot_files, "module/weights")
    key = Artifact(id="corridorkey-weights", label="Key", capability="key", kind="hf_files",
                   required_profile=True, layout_root="v2_models_dir", layout_rel="weights",
                   repo_id="t/key", revision="c" * 40, revision_status="pinned",
                   files=tuple(write_files(tmp_path / "key-spec", {"w.bin": b"key"})))
    bundled = hf_snapshot_artifact("gvm", [FileSpec("w.bin", "w.bin", 7, None, None, False, True)],
                                   "gvm_core/weights")
    mat_file = FileSpec("matanyone2.pth", "matanyone2.pth", 9, None, None, False, True)
    matanyone = Artifact(id="matanyone2", label="MatAnyone2", capability="matanyone2",
                         kind="http_file", required_profile=False, layout_root="data_dir",
                         layout_rel="modules/MatAnyone2Module/checkpoints", url="https://example.test/model",
                         revision="v1.0.0", revision_status="pinned", files=(mat_file,))
    optional = hf_snapshot_artifact(
        "birefnet-missing", [FileSpec("w.bin", "w.bin", 8, None, None, False, True)], "optional")
    other_revision = "b" * 40
    cached_root = hf_home / "hub" / "models--t--birefnet-cached" / "snapshots" / other_revision
    (cached_root / "w.bin").parent.mkdir(parents=True, exist_ok=True)
    (cached_root / "w.bin").write_bytes(b"snapshot")
    old_key = tmp_path / "old-models" / "weights"
    old_key.mkdir(parents=True)
    (old_key / "w.bin").write_bytes(b"key")
    bundle_root = tmp_path / "bundle" / "CorridorKeyV2"
    bundled_model = bundle_root.parent / "gvm_core" / "weights"
    bundled_model.mkdir(parents=True)
    (bundled_model / "w.bin").write_bytes(b"bundled")
    matanyone_root = bundle_root.parent / "pretrained_models"
    matanyone_root.mkdir(parents=True)
    (matanyone_root / "matanyone2.pth").write_bytes(b"matanyone")
    roots = replace(roots, legacy_models_dirs=(tmp_path / "old-models",),
                    v2_package_root=bundle_root,
                    legacy_data_dirs=(tmp_path / "old-data", bundle_root, bundle_root.parent),
                    hf_caches=hf_hub_cache_dirs())
    catalog = make_catalog(legacy, cached, key, optional, bundled, matanyone)
    app_models = AppModels(roots, catalog)

    legacy_status = app_models.checker.check_artifact(legacy)
    legacy_binding = app_models.resolve("birefnet", legacy.id)
    assert legacy_status["present"] is True
    assert legacy_status["path"] == str(tmp_path / "old-data" / "module" / "weights")
    assert legacy_status["can_delete"] is False
    assert legacy_binding.paths[legacy.id] == Path(legacy_status["path"])
    assert legacy_binding.files == (Path(legacy_status["path"]) / "w.bin",)

    cached_status = app_models.checker.check_artifact(cached)
    cached_binding = app_models.resolve("birefnet", cached.id)
    assert cached_status["present"] is True
    assert cached_status["path"] == str(cached_root)
    assert cached_status["can_delete"] is False
    assert cached_binding.paths[cached.id] == cached_root
    assert cached_binding.files == (cached_root / "w.bin",)

    key_status = app_models.checker.check_artifact(key)
    key_binding = app_models.resolve("key")
    assert key_status["present"] is True
    assert key_status["path"] == str(old_key)
    assert key_status["can_delete"] is False
    assert key_binding.paths[key.id] == old_key
    assert key_binding.files == (old_key / "w.bin",)
    assert key_binding.ready is True
    assert app_models.resolve("birefnet", optional.id).ready is False

    bundled_status = app_models.checker.check_artifact(bundled)
    assert bundled_status["path"] == str(bundled_model)
    assert bundled_status["present"] is True
    mat_status = app_models.checker.check_artifact(matanyone)
    mat_binding = app_models.resolve("matanyone2")
    assert mat_status["path"] == str(matanyone_root)
    assert mat_status["present"] is True
    assert mat_binding.paths[matanyone.id] == matanyone_root / "matanyone2.pth"


def test_flat_videomama_candidate_binds_legacy_loader_layout(roots, tmp_path):
    from dataclasses import replace
    from backend.app_models import AppModels

    config = b"config"
    weights = b"weights"
    art = Artifact(
        id="videomama", label="VideoMaMa", capability="videomama", kind="hf_snapshot",
        required_profile=False, layout_root="data_dir",
        layout_rel="VideoMaMaInferenceModule/checkpoints", repo_id="t/videomama",
        revision="a" * 40, revision_status="pinned",
        files=(FileSpec("unet/config.json", "VideoMaMa/unet/config.json", len(config), None, None, False, True),
               FileSpec("unet/diffusion_pytorch_model.safetensors",
                        "VideoMaMa/unet/diffusion_pytorch_model.safetensors",
                        len(weights), None, None, False, True)),
        path_remap={"unet/": "VideoMaMa/unet/"})
    prior_data = tmp_path / "old-module-data"
    checkpoint_root = prior_data / art.layout_rel
    flat = checkpoint_root / "VideoMaMa"
    flat.mkdir(parents=True)
    (flat / "config.json").write_bytes(config)
    (flat / "diffusion_pytorch_model.safetensors").write_bytes(weights)
    roots = replace(roots, legacy_data_dirs=(prior_data,))
    catalog = make_catalog(art)
    app_models = AppModels(roots, catalog)

    status = app_models.checker.check_artifact(art)
    binding = app_models.resolve("videomama")
    assert status["present"] is True
    assert status["path"] == str(checkpoint_root)
    assert binding.paths[art.id] == flat
    assert binding.files == (flat / "config.json", flat / "diffusion_pytorch_model.safetensors")
