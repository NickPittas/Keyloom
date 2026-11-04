"""App model resolver: exact files per producer, Verify -> Check -> reopen, invalidation, in-use refusal."""
import pytest

from backend.app_models import FIXED, TASKS, AppModels, ModelInUseError
from backend.model_catalog import load_catalog
from backend.model_runtime import ModelAssetsUnavailableError
from backend.model_status import Roots
from tests.test_model_status import hf_snapshot_artifact, make_catalog, roots, write_files  # noqa: F401


def test_every_producer_closure_has_exact_files_under_temp_roots(roots):  # noqa: F811
    models, cat = AppModels(roots), load_catalog()
    seen = set()
    for task in TASKS:
        for variant in models.variants(task):
            seen.add(task)
            b = models.resolve(task, variant)
            closure = cat.closure([b.artifact_id])
            assert [d.id for d in b.dependencies] == closure
            assert len(b.files) == sum(len(cat.get(i).required_files()) for i in closure if cat.get(i).kind != "local_package")
            assert b.state == "missing" and not b.ready
            roots_in_use = {str(p) for p in (roots.data_dir, roots.v2_models_dir, roots.v2_package_root, roots.hf_cache)}
            assert all(any(str(f).startswith(r) for r in roots_in_use) for f in b.files)
    assert seen == set(TASKS)
    key = {d.id for d in models.resolve("key").dependencies}
    assert {"corridorkey-weights", "cradio", "moge", "rvm", "corridorkey-code"} <= key
    assert {"videomama", "videomama-svd"} == {d.id for d in models.resolve("videomama").dependencies}
    assert models.variants("sam2")[0] == "sam2-base-plus" and "birefnet-BiRefNet-matting" == models.variants("birefnet")[0]
    assert models.resolve("birefnet", "BiRefNet_lite").artifact_id == "birefnet-BiRefNet_lite"
    assert models.resolve("sam2", "small").artifact_id == "sam2-small"
    assert set(models.snapshot()) == set(TASKS)


def _models(roots):  # noqa: F811
    specs = write_files(roots.data_dir / "model", {"w.bin": b"weights", "c.json": b"{}"}, lfs_like={"w.bin"})
    return AppModels(roots, make_catalog(hf_snapshot_artifact("gvm", specs))), specs


def test_present_missing_and_wrong_size(roots):  # noqa: F811
    models, specs = _models(roots)
    b = models.resolve("gvm")  # never verified: present files are usable
    assert b.state == "present" and b.ready and not b.dependencies[0].detail
    assert {f.name for f in b.files} == {s.layout_path for s in specs}
    assert AppModels(roots, models.catalog).resolve("gvm").ready  # reopen
    (roots.data_dir / "model" / "w.bin").write_bytes(b"WEIGHTS")  # same size, new content: still present, no hashing
    assert models.resolve("gvm").state == "present"
    (roots.data_dir / "model" / "w.bin").write_bytes(b"short")  # wrong size
    b = models.resolve("gvm")
    assert b.state == "missing" and not b.ready and "size mismatch" in b.dependencies[0].detail
    (roots.data_dir / "model" / "w.bin").write_bytes(b"weights")
    (roots.data_dir / "model" / "c.json").unlink()
    b = models.resolve("gvm")
    assert b.state == "missing" and "missing: c.json" in b.dependencies[0].detail
    assert "download it on the Models page" in b.dependencies[0].detail


def test_idle_change_allowed_running_use_refused(roots):  # noqa: F811
    models, _ = _models(roots)
    with models.changing(["gvm"]):  # idle: allowed
        with pytest.raises(ModelInUseError):
            models.start_job("j1", "gvm")  # cannot start while being changed
    models.start_job("j1", "gvm")
    assert models.active_jobs(["gvm"]) == ["j1"]
    with pytest.raises(ModelInUseError, match="j1"):
        with models.changing(["gvm"]):
            pass
    assert models.verify("gvm").ready  # Verify never blocks
    models.finish_job("j1")
    with models.changing(["gvm"]):
        pass
    assert models.active_jobs(["gvm"]) == []


def test_missing_model_is_refused_with_download_advice(roots):  # noqa: F811
    models, _ = _models(roots)
    (roots.data_dir / "model" / "c.json").unlink()
    with pytest.raises(ModelAssetsUnavailableError, match="download it on the Models page"):
        models.start_job("j0", "gvm")
    assert models.active_jobs(["gvm"]) == []


def test_dependency_in_use_blocks_dependent_change(roots):  # noqa: F811
    dep = hf_snapshot_artifact("cradio", write_files(roots.data_dir / "d", {"a.bin": b"a"}, {"a.bin"}), "d")
    top = hf_snapshot_artifact("corridorkey-weights", write_files(roots.data_dir / "t", {"b.bin": b"b"}, {"b.bin"}), "t")
    from dataclasses import replace
    models = AppModels(roots, make_catalog(dep, replace(top, depends=("cradio",))))
    models.verify("key")
    models.start_job("k1", "key")
    with pytest.raises(ModelInUseError):
        with models.changing(["cradio"]):
            pass
    models.finish_job("k1")
