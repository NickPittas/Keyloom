"""Provision manager: mocked transports exercise staging/promotion/cancel/corruption/disk."""
import base64
import hashlib
import importlib.util
import json
import os
import subprocess
import sys
import threading
import time
import urllib.error
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

import backend.model_provision as mp  # noqa: E402
import backend.model_status as ms  # noqa: E402
import backend.model_runtime as runtime  # noqa: E402
from backend.model_catalog import (Artifact, Catalog, FileSpec, hf_cache_snapshot_path)  # noqa: E402
from scripts import setup_models  # noqa: E402


def make_filespec(rel: str, data: bytes, trusted: bool = True) -> FileSpec:
    return FileSpec(path=rel, layout_path=rel, size=len(data),
                    sha256=hashlib.sha256(data).hexdigest() if trusted else None,
                    sha256_source="hf-lfs-oid" if trusted else None,
                    lfs=trusted, required=True)


def snapshot_artifact(aid, layout_rel, files, remap=None, repo_files=None):
    return Artifact(id=aid, label=aid, capability="t", kind="hf_snapshot",
                    required_profile=False, layout_root="data_dir", layout_rel=layout_rel,
                    repo_id=f"t/{aid}", revision="b" * 40, revision_status="pinned",
                    files=tuple(files), path_remap=remap or {},
                    provenance={"source": "test"}, __post_init__=None) if False else Artifact(
        id=aid, label=aid, capability="t", kind="hf_snapshot", required_profile=False,
        layout_root="data_dir", layout_rel=layout_rel, repo_id=f"t/{aid}",
        revision="b" * 40, revision_status="pinned", files=tuple(files),
        path_remap=remap or {})


@pytest.fixture
def roots(tmp_path, monkeypatch):
    monkeypatch.delenv("HF_HUB_CACHE", raising=False)
    monkeypatch.delenv("HF_HOME", raising=False)
    monkeypatch.delenv("CK_INSTALL", raising=False)
    return ms.Roots(data_dir=tmp_path / "data", v2_models_dir=tmp_path / "v2",
                    v2_package_root=tmp_path / "pkg", hf_cache=tmp_path / "hf",
                    cache_dir=tmp_path / "cache")


class Collector:
    def __init__(self):
        self.events = []

    def __call__(self, ev):
        self.events.append(ev)

    def of(self, kind):
        return [e for e in self.events if e["event"] == kind]


def manager(catalog, roots, collect=None):
    return mp.ProvisionManager(catalog, roots, emit=collect) if collect else \
        mp.ProvisionManager(catalog, roots)


def fake_snapshot_repo(monkeypatch, repo_id_to_files: dict[str, dict[str, bytes]],
                       hook=None, cache_dir: Path | None = None):
    """Mock the supervised HF worker without crossing the network boundary."""
    def fake(self, request, artifact):
        repo_id = request["repo_id"]
        if cache_dir is not None:
            assert Path(request["cache_dir"]) == cache_dir
        local_dir = request["local_dir"]
        if hook:
            hook(repo_id, Path(local_dir))
        files = repo_id_to_files[repo_id]
        selected = [(rel, data) for rel, data in files.items()
                    if request.get("allow_patterns") is None or any(
                        rel == p or (p.endswith("*") and rel.startswith(p[:-1]))
                        for p in request.get("allow_patterns", []))]
        for i, (rel, data) in enumerate(selected, 1):
            p = Path(local_dir) / rel
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_bytes(data)
            self.emit({"event": "progress", "artifact": artifact,
                       "unit": "files", "files_done": i, "files_total": len(selected)})
    monkeypatch.setattr(mp.ProvisionManager, "_run_download_worker", fake)


def fake_hf_cache_file(monkeypatch, data: bytes):
    def fake(self, request, artifact):
        path = Path(request["local_dir"]) / request["filename"]
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
    monkeypatch.setattr(mp.ProvisionManager, "_run_download_worker", fake)


class TestSnapshotInstall:
    def test_fresh_install_stages_and_promotes_atomically(self, roots, monkeypatch):
        files = {"config.json": b"{}", "weights.bin": b"w" * 500}
        specs = [make_filespec("config.json", files["config.json"], trusted=False),
                 make_filespec("weights.bin", files["weights.bin"])]
        art = snapshot_artifact("gvm", "gvm/weights",
                                [specs[0], specs[1]])
        cat = Catalog(1, "t", {}, {"gvm": art})
        col = Collector()
        mgr = manager(cat, roots, col)
        isolated_cache = mgr._hf_staging_dir(art, mgr.checker.artifact_dir(art)) / "hub-cache"
        fake_snapshot_repo(monkeypatch, {"t/gvm": files}, cache_dir=isolated_cache)

        assert mgr.op_download(["gvm"])["ok"] is True
        final = roots.data_dir / "gvm" / "weights"
        assert (final / "weights.bin").read_bytes() == files["weights.bin"]
        assert not any(p.name.startswith(".weights.gvm.") for p in final.parent.iterdir())
        done = col.of("artifact-done")
        assert done and done[0]["verified"] is False
        assert "1 file(s) lack trusted digests" in done[0]["note"]
        assert all({"files_done", "files_total", "unit"} <= e.keys() for e in col.of("progress"))
        assert all(e["unit"] == "files" for e in col.of("progress"))
        assert all("bytes" not in e for e in col.of("progress"))

    def test_videomama_remap_and_shared_root(self, roots, monkeypatch):
        """VideoMaMa and SVD share checkpoints/: staging must only touch VideoMaMa/."""
        vm_files = {"unet/config.json": b"{}", "unet/diffusion_pytorch_model.safetensors": b"u" * 300,
                    "dino_projection_mlp.pth": b"d", "README.md": b"notice"}
        svd_files = {"model_index.json": b"{}", "image_encoder/model.fp16.safetensors": b"i" * 300}

        vm_specs = [FileSpec(p, "VideoMaMa/unet/" + p[len("unet/"):], len(d),
                             hashlib.sha256(d).hexdigest(), "hf-lfs-oid", True, True)
                    for p, d in vm_files.items() if p.startswith("unet/")]
        vm_specs.append(FileSpec("dino_projection_mlp.pth", "dino_projection_mlp.pth", 1,
                                 hashlib.sha256(b"d").hexdigest(), "hf-lfs-oid", True, False))
        vm_specs.append(FileSpec("README.md", "README.md", 6,
                                 hashlib.sha256(b"notice").hexdigest(), "hf-lfs-oid", False,
                                 False, note="documentation/notice file"))
        vm = Artifact(id="videomama", label="vm", capability="t", kind="hf_snapshot",
                      required_profile=False, layout_root="data_dir",
                      layout_rel="VideoMaMaInferenceModule/checkpoints", repo_id="t/vm",
                      revision="c" * 40, revision_status="pinned", files=tuple(vm_specs),
                      depends=("videomama-svd",),
                      path_remap={"unet/": "VideoMaMa/unet/"})
        svd_specs = [FileSpec("model_index.json", "model_index.json", 2, None, None, False, True),
                     FileSpec("image_encoder/model.fp16.safetensors",
                              "image_encoder/model.fp16.safetensors", 300,
                              hashlib.sha256(b"i" * 300).hexdigest(), "hf-lfs-oid", True, True)]
        svd = Artifact(id="videomama-svd", label="svd", capability="t", kind="hf_snapshot",
                       required_profile=False, layout_root="data_dir",
                       layout_rel="VideoMaMaInferenceModule/checkpoints/stable-video-diffusion-img2vid-xt",
                       repo_id="t/svd",
                       revision="d" * 40, revision_status="pinned", files=tuple(svd_specs))
        cat = Catalog(1, "t", {}, {"videomama": vm, "videomama-svd": svd})

        fake_snapshot_repo(monkeypatch, {"t/vm": vm_files, "t/svd": svd_files})
        col = Collector()
        mgr = manager(cat, roots, col)
        assert mgr.op_download(["videomama"])["ok"] is True, col.events  # pulls svd via depends

        ck = roots.data_dir / "VideoMaMaInferenceModule" / "checkpoints"
        assert (ck / "VideoMaMa/unet/diffusion_pytorch_model.safetensors").is_file()
        assert not (ck / "VideoMaMa/.upstream/dino_projection_mlp.pth").exists()
        assert (ck / "VideoMaMa/.upstream/README.md").read_bytes() == b"notice"
        assert (ck / "stable-video-diffusion-img2vid-xt/model_index.json").is_file()
        checked = mgr.op_check(["videomama"])
        assert {item["artifact"] for item in checked["artifacts"]} == {
            "videomama", "videomama-svd"}

    def test_missing_files_after_download_fails_without_promotion(self, roots, monkeypatch):
        specs = [make_filespec("a.bin", b"a" * 10), make_filespec("b.bin", b"b" * 10)]
        art = snapshot_artifact("gvm", "gvm/weights", specs)
        cat = Catalog(1, "t", {}, {"gvm": art})
        fake_snapshot_repo(monkeypatch, {"t/gvm": {"a.bin": b"a" * 10}})  # b.bin never arrives
        col = Collector()
        mgr = manager(cat, roots, col)
        result = mgr.op_download(["gvm"])
        assert result["ok"] is False and result["failed"] == ["gvm"]
        errs = col.of("artifact-error")
        assert errs and errs[0]["error"] == "missing-files"
        assert not (roots.data_dir / "gvm" / "weights" / "a.bin").exists()  # nothing promoted

    def test_corrupt_download_never_replaces_good_asset(self, roots, monkeypatch):
        good = {"weights.bin": b"good-bytes" * 10}
        specs = [make_filespec("weights.bin", good["weights.bin"])]
        art = snapshot_artifact("gvm", "gvm/weights", specs)
        cat = Catalog(1, "t", {}, {"gvm": art})
        final = roots.data_dir / "gvm" / "weights" / "weights.bin"
        final.parent.mkdir(parents=True)
        final.write_bytes(good["weights.bin"])
        # "repair" fetch returns corrupted bytes for the same size
        corrupt = bytes(len(good["weights.bin"]))
        fake_snapshot_repo(monkeypatch, {"t/gvm": {"weights.bin": corrupt}})
        col = Collector()
        mgr = manager(cat, roots, col)
        result = mgr.op_repair(["gvm"])
        assert result["ok"] is False
        errs = col.of("artifact-error")
        assert errs and errs[0]["error"] == "hash-mismatch"
        assert final.read_bytes() == good["weights.bin"], "failed repair must preserve the good final"
        staging = next(final.parent.parent.glob(".weights.gvm.*.staging"))
        assert not (staging / "payload" / "weights.bin").exists(), "discard a corrupt staged file"

    def test_staging_symlink_cannot_alias_and_destroy_installed_asset(self, roots, monkeypatch):
        data = b"known-good"
        art = snapshot_artifact("gvm", "gvm/weights", [make_filespec("weights.bin", data)])
        final_dir = roots.data_dir / "gvm" / "weights"
        final_dir.mkdir(parents=True)
        final = final_dir / "weights.bin"
        final.write_bytes(data)
        col = Collector()
        mgr = manager(Catalog(1, "t", {}, {"gvm": art}), roots, col)
        staging = mgr._hf_staging_dir(art, mgr.checker.artifact_dir(art))
        staging.symlink_to(final_dir, target_is_directory=True)

        result = mgr.op_repair(["gvm"])
        assert result["ok"] is False
        assert col.of("artifact-error")[0]["error"] == "unsafe-path"
        assert final.read_bytes() == data
        assert staging.is_symlink()

    def test_active_artifact_lock_is_never_stolen(self, roots, monkeypatch):
        data = b"weights"
        art = snapshot_artifact("gvm", "gvm/weights", [make_filespec("weights.bin", data)])
        mgr = manager(Catalog(1, "t", {}, {"gvm": art}), roots, Collector())
        dest = mgr.checker.artifact_dir(art)

        with mgr._artifact_lock(art, dest):
            result = mgr.op_download(["gvm"])
            assert result["ok"] is False
            assert result["failed"] == ["gvm"]
        assert not list((roots.cache_dir / "model-locks").glob("*.lock"))

    def test_loaded_runtime_lease_blocks_repair_across_cache_roots(self, roots, monkeypatch):
        data = b"known-good-weights"
        art = snapshot_artifact("gvm", "gvm/weights", [make_filespec("weights.bin", data)])
        cat = Catalog(1, "t", {}, {"gvm": art})
        final = roots.data_dir / "gvm" / "weights" / "weights.bin"
        final.parent.mkdir(parents=True)
        final.write_bytes(data)
        monkeypatch.setattr(runtime, "load_catalog", lambda: cat)
        checker = ms.StatusChecker(cat, roots)
        assert checker.verify_artifact(art)["verified"] is True
        assert checker.hash_cache.save() is True

        other_roots = ms.Roots(roots.data_dir, roots.v2_models_dir, roots.v2_package_root,
                               roots.hf_cache, roots.cache_dir.parent / "other-cache")
        fake_snapshot_repo(monkeypatch, {"t/gvm": {"weights.bin": data}})
        writer = manager(cat, other_roots, Collector())
        lease = runtime.acquire_model_artifact("gvm", roots=roots)
        try:
            blocked = writer.op_repair(["gvm"])
            assert blocked["ok"] is False
            assert writer._emit_fn.of("artifact-error")[-1]["error"] == "locked"
            assert final.read_bytes() == data
        finally:
            lease.close()

        assert writer.op_repair(["gvm"])["ok"] is True


class TestHFCacheFile:
    def make_artifact(self):
        data = b"good-checkpoint"
        spec = make_filespec("model.pt", data)
        return Artifact(id="sam2-small", label="SAM2", capability="track",
                        kind="hf_cache_file", required_profile=False, layout_root=None,
                        layout_rel=None, repo_id="facebook/sam2.1-hiera-small",
                        revision="a" * 40, revision_status="pinned",
                        hf_filename="model.pt", files=(spec,)), data

    def test_repair_downloads_to_isolated_cache_then_publishes(self, roots, monkeypatch):
        art, data = self.make_artifact()
        final = hf_cache_snapshot_path(roots.hf_cache, art.repo_id, art.revision, art.hf_filename)
        final.parent.mkdir(parents=True)
        final.write_bytes(b"bad--checkpoint")

        fake_hf_cache_file(monkeypatch, data)
        col = Collector()
        mgr = manager(Catalog(1, "t", {}, {art.id: art}), roots, col)

        assert mgr.op_repair([art.id])["ok"] is True, col.events
        assert final.read_bytes() == data
        assert col.of("artifact-done")[-1]["verified"] is True

    def test_bad_isolated_cache_file_never_replaces_existing_snapshot(self, roots, monkeypatch):
        art, data = self.make_artifact()
        final = hf_cache_snapshot_path(roots.hf_cache, art.repo_id, art.revision, art.hf_filename)
        final.parent.mkdir(parents=True)
        old = b"old--checkpoint"
        final.write_bytes(old)

        fake_hf_cache_file(monkeypatch, b"bad--checkpoint")
        col = Collector()
        mgr = manager(Catalog(1, "t", {}, {art.id: art}), roots, col)

        assert mgr.op_repair([art.id])["ok"] is False
        assert final.read_bytes() == old
        assert col.of("artifact-error")[-1]["error"] == "hash-mismatch"


class TestCancellation:
    def test_cancel_marker_file_stops_before_artifact(self, roots, monkeypatch):
        specs = [make_filespec("a.bin", b"a" * 10)]
        art = snapshot_artifact("gvm", "gvm/weights", specs)
        cat = Catalog(1, "t", {}, {"gvm": art})
        col = Collector()
        mgr = mp.ProvisionManager(cat, roots, emit=col, operation_id="download-A")
        # A stale request must not poison a later operation with another ID.
        result = mgr.cancel_running_download("download-B")
        assert result["error"] == "not-active"
        fake_snapshot_repo(monkeypatch, {"t/gvm": {"a.bin": b"a" * 10}})
        result = mgr.op_download(["gvm"])
        assert result["ok"] is True and result["cancelled"] is False
        assert not col.of("cancelled")
        assert (roots.data_dir / "gvm" / "weights" / "a.bin").is_file()

    def test_starting_another_operation_does_not_consume_cancel_a(self, roots):
        catalog = Catalog(1, "t", {}, {})
        first = mp.ProvisionManager(catalog, roots, operation_id="download-A")
        first_owner = first._begin_operation()
        second = mp.ProvisionManager(catalog, roots, operation_id="download-B")
        second_owner = None
        try:
            assert mp.ProvisionManager(catalog, roots).cancel_running_download("download-A")["ok"]
            first_marker = first._cancel_marker_path("download-A")
            assert first_marker.is_file()
            second_owner = second._begin_operation()
            assert first_marker.is_file()
            assert first._cancelled() is True
        finally:
            first._clear_cancel_marker()
            if second_owner:
                second._end_operation(second_owner)
            first._end_operation(first_owner)

    def test_cancel_marker_is_published_complete_and_concurrent_cancels_agree(self, roots, monkeypatch):
        catalog = Catalog(1, "t", {}, {})
        owner_mgr = mp.ProvisionManager(catalog, roots, operation_id="download-A")
        owner = owner_mgr._begin_operation()
        marker = owner_mgr._cancel_marker_path("download-A")
        seen = {}
        real_dump = mp.json.dump
        racing = mp.ProvisionManager(catalog, roots)

        def paused_dump(obj, stream, *a, **k):
            if not seen:
                # Mid-serialization: readers must see no marker at all, and a
                # concurrent cancel may win the publish race.
                seen["marker_visible"] = marker.exists()
                seen["cancelled"] = owner_mgr._cancelled()
                seen["race"] = racing.cancel_running_download("download-A")
            return real_dump(obj, stream, *a, **k)

        monkeypatch.setattr(mp.json, "dump", paused_dump)
        try:
            result = mp.ProvisionManager(catalog, roots).cancel_running_download("download-A")
            assert seen["marker_visible"] is False and seen["cancelled"] is False
            assert seen["race"]["ok"] is True
            assert result["ok"] is True and result["requested"] is True
            assert json.loads(marker.read_text()) == {"operation_id": "download-A"}
            assert owner_mgr._cancelled() is True
            assert not list(marker.parent.glob("*.tmp"))
        finally:
            owner_mgr._clear_cancel_marker()
            owner_mgr._end_operation(owner)

    def test_real_cli_download_is_cancelled_by_matching_id(self, roots):
        operation_id = "app-selected_download-123"
        old_asset = roots.data_dir / "fixture" / "weights" / "weights.bin"
        old_asset.parent.mkdir(parents=True)
        old_asset.write_bytes(b"old-fixture-bytes")
        ready_file = roots.cache_dir / "model-provision-fixture.ready"
        fixture = str(ROOT / "tests" / "fixtures" / "model_provision_cli.py")
        shared_roots = ["--data-dir", str(roots.data_dir),
                        "--v2-models-dir", str(roots.v2_models_dir),
                        "--hf-cache", str(roots.hf_cache),
                        "--cache-dir", str(roots.cache_dir)]
        download = subprocess.Popen(
            [sys.executable, fixture, "--download", "--profile", "required",
             "--operation-id", operation_id,
             *shared_roots],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            deadline = time.monotonic() + 4
            while not ready_file.is_file() and time.monotonic() < deadline:
                if download.poll() is not None:
                    break
                time.sleep(0.01)
            assert ready_file.is_file(), "download helper never reached the local transport gate"

            cancelled = subprocess.run(
                [sys.executable, fixture, "--cancel", "--operation-id", operation_id,
                 *shared_roots], capture_output=True, text=True, timeout=5, check=False)
            cancel_events = [json.loads(line) for line in cancelled.stdout.splitlines()]
            assert cancelled.returncode == 0, cancelled.stderr + cancelled.stdout
            assert [event["event"] for event in cancel_events] == [
                "op-start", "cancel-requested", "op-done"]
            assert all(event["operation_id"] == operation_id for event in cancel_events)
            request_ids = {event["request_id"] for event in cancel_events}
            assert len(request_ids) == 1 and next(iter(request_ids))
            assert cancel_events[-1]["op"] == "cancel"
            assert cancel_events[-1]["ok"] is True and cancel_events[-1]["requested"] is True

            stdout, stderr = download.communicate(timeout=5)
            download_events = [json.loads(line) for line in stdout.splitlines()]
            assert download.returncode == 1, stderr + stdout
            assert download.poll() is not None, "download helper process was not reaped"
            assert all(event["operation_id"] == operation_id for event in download_events)
            assert any(event["event"] == "cancelled" for event in download_events)
            terminal = [event for event in download_events if event["event"] == "op-done"]
            assert len(terminal) == 1
            assert terminal[0]["op"] == "download"
            assert terminal[0]["operation_id"] == operation_id
            assert terminal[0]["ok"] is False and terminal[0]["cancelled"] is True
            assert old_asset.read_bytes() == b"old-fixture-bytes"
            assert not (roots.cache_dir / "model-operations" / f"{operation_id}.active").exists()
            assert not (roots.cache_dir / "model-cancel" / f"{operation_id}.cancel").exists()
        finally:
            if download.poll() is None:
                download.kill()
                download.communicate(timeout=2)

    def test_real_cli_helper_rejects_stale_id_with_failed_terminal(self, roots):
        command = [sys.executable, str(ROOT / "scripts" / "setup_models.py"),
                   "--cancel", "--operation-id", "stale-download-123",
                   "--data-dir", str(roots.data_dir),
                   "--v2-models-dir", str(roots.v2_models_dir),
                   "--hf-cache", str(roots.hf_cache),
                   "--cache-dir", str(roots.cache_dir)]
        completed = subprocess.run(command, capture_output=True, text=True,
                                   timeout=5, check=False)
        events = [json.loads(line) for line in completed.stdout.splitlines()]
        assert completed.returncode == 1
        assert [event["event"] for event in events] == ["op-start", "op-done"]
        assert events[-1]["operation_id"] == "stale-download-123"
        assert events[-1]["ok"] is False and events[-1]["error"] == "not-active"

    def test_real_cli_helper_rejects_missing_id(self, roots):
        fixture = str(ROOT / "tests" / "fixtures" / "model_provision_cli.py")
        command = [sys.executable, fixture, "--cancel",
                   "--data-dir", str(roots.data_dir),
                   "--v2-models-dir", str(roots.v2_models_dir),
                   "--hf-cache", str(roots.hf_cache),
                   "--cache-dir", str(roots.cache_dir)]
        completed = subprocess.run(command, capture_output=True, text=True,
                                   timeout=5, check=False)
        events = [json.loads(line) for line in completed.stdout.splitlines()]
        assert completed.returncode == 2
        assert len(events) == 1
        assert events[0]["event"] == "op-done"
        assert events[0]["error"] == "invalid-operation-id"
        assert events[0].get("operation_id") is None
        assert "request_id" in events[0]

    def test_cancel_mid_download_preserves_partial_and_no_promotion(self, roots, monkeypatch):
        specs = [make_filespec("a.bin", b"a" * 10), make_filespec("b.bin", b"b" * 10)]
        art = snapshot_artifact("gvm", "gvm/weights", specs)
        cat = Catalog(1, "t", {}, {"gvm": art})

        def hook(repo_id, local_dir):
            (local_dir / "a.bin").write_bytes(b"a" * 10)
            raise mp.Cancelled()
        fake_snapshot_repo(monkeypatch, {"t/gvm": {"a.bin": b"a" * 10, "b.bin": b"b" * 10}},
                           hook=hook)
        col = Collector()
        mgr = manager(cat, roots, col)
        result = mgr.op_download(["gvm"])
        assert result["cancelled"] is True
        assert not (roots.data_dir / "gvm" / "weights" / "a.bin").exists()
        staging = roots.data_dir / "gvm" / ".weights.gvm."  # partials remain in the isolated staging dir
        assert any(p.name.startswith(staging.name) for p in staging.parent.iterdir())

    def test_cancel_interrupts_a_supervised_transport(self, roots):
        mgr = manager(Catalog(1, "t", {}, {}), roots, Collector())
        threading.Timer(0.15, mgr.cancel_event.set).start()
        started = time.monotonic()
        with pytest.raises(mp.Cancelled):
            mgr._supervise([sys.executable, "-c", "import time; time.sleep(10)"], {}, "test")
        assert time.monotonic() - started < 2

    def test_cancel_marker_write_failure_is_a_failed_terminal(self, roots, monkeypatch):
        col = Collector()
        operation_id = "write-failure"
        owner = mp.ProvisionManager(Catalog(1, "t", {}, {}), roots,
                                    operation_id=operation_id)
        owner_lock = owner._begin_operation()
        mgr = mp.ProvisionManager(Catalog(1, "t", {}, {}), roots, emit=col,
                                  operation_id=operation_id)
        builtin_open = open

        def fail_marker(path, *args, **kwargs):
            if f"{operation_id}.cancel" in str(path):
                raise PermissionError("marker write denied")
            return builtin_open(path, *args, **kwargs)

        monkeypatch.setattr("builtins.open", fail_marker)
        try:
            result = mgr.cancel_running_download(operation_id)
            assert result["ok"] is False
            assert col.of("op-done")[-1]["ok"] is False
            assert col.of("op-done")[-1]["error"] == "cancel-marker"
            assert not col.of("cancel-requested")
        finally:
            owner._end_operation(owner_lock)


class TestDiskPreflight:
    def test_preflight_counts_only_verified_bytes_missing_from_pinned_staging(self, roots):
        specs = [make_filespec("a.bin", b"a" * 10), make_filespec("b.bin", b"b" * 10)]
        art = snapshot_artifact("gvm", "gvm/weights", specs)
        mgr = manager(Catalog(1, "t", {}, {"gvm": art}), roots)
        staging = mgr._hf_staging_dir(art, roots.data_dir / "gvm" / "weights")
        payload = staging / "payload"
        (payload / "a.bin").parent.mkdir(parents=True)
        (payload / "a.bin").write_bytes(b"a" * 10)
        assert mgr._missing_bytes(art) == 10

        (payload / "a.bin").write_bytes(b"x" * 10)
        assert mgr._missing_bytes(art) == 20

    def test_publish_identity_change_rolls_back_existing_directory(self, roots, monkeypatch):
        data = b"trusted-weights"
        art = snapshot_artifact("gvm", "gvm/weights", [make_filespec("weights.bin", data)])
        final_dir = roots.data_dir / "gvm" / "weights"
        final_dir.mkdir(parents=True)
        final = final_dir / "weights.bin"
        final.write_bytes(data)
        mgr = manager(Catalog(1, "t", {}, {"gvm": art}), roots, Collector())
        fake_snapshot_repo(monkeypatch, {"t/gvm": {"weights.bin": data}})
        publish = mgr._publish

        def mutate_after_publish(*args, **kwargs):
            backup = publish(*args, **kwargs)
            (args[1] / "weights.bin").write_bytes(b"mutated-after-check")
            return backup

        monkeypatch.setattr(mgr, "_publish", mutate_after_publish)
        result = mgr.op_repair(["gvm"])
        assert result["ok"] is False
        assert mgr._emit_fn.of("artifact-error")[-1]["error"] == "file-changed"
        assert final.read_bytes() == data

    def test_same_size_mtime_restored_mutation_fails_reverification(self, roots, monkeypatch):
        good = b"trusted-data"
        evil = b"evil-data---"
        assert len(good) == len(evil)
        art = snapshot_artifact("gvm", "gvm/weights", [make_filespec("weights.bin", good)])
        final = roots.data_dir / "gvm" / "weights" / "weights.bin"
        final.parent.mkdir(parents=True)
        final.write_bytes(good)
        fake_snapshot_repo(monkeypatch, {"t/gvm": {"weights.bin": good}})
        mgr = manager(Catalog(1, "t", {}, {"gvm": art}), roots, Collector())
        record = mgr._record_digests

        def mutate_before_record(artifact, hashes):
            path = mgr.checker.file_path(artifact, artifact.required_files()[0])
            info = path.stat()
            path.write_bytes(evil)
            os.utime(path, ns=(info.st_atime_ns, info.st_mtime_ns))
            return record(artifact, hashes)

        monkeypatch.setattr(mgr, "_record_digests", mutate_before_record)
        result = mgr.op_repair(["gvm"])
        assert result["ok"] is False
        assert mgr._emit_fn.of("artifact-error")[-1]["error"] == "verification-cache"
        assert final.read_bytes() == good

    def test_failed_verification_cache_write_never_reports_ready(self, roots, monkeypatch):
        data = b"known-good"
        art = snapshot_artifact("gvm", "gvm/weights", [make_filespec("weights.bin", data)])
        final = roots.data_dir / "gvm" / "weights" / "weights.bin"
        final.parent.mkdir(parents=True)
        final.write_bytes(data)
        col = Collector()
        mgr = manager(Catalog(1, "t", {}, {"gvm": art}), roots, col)
        monkeypatch.setattr(mgr.checker.hash_cache, "save", lambda: False)

        result = mgr.op_download(["gvm"])
        assert result["ok"] is False
        assert col.of("artifact-error")[-1]["error"] == "verification-cache"
        assert col.of("op-done")[-1]["ok"] is False
        assert mgr.checker.check_artifact(art)["ready"] is False
        assert final.read_bytes() == data

    def test_insufficient_disk_aborts_before_download(self, roots, monkeypatch):
        specs = [make_filespec("big.bin", b"x" * 10_000)]
        art = snapshot_artifact("gvm", "gvm/weights", specs)
        cat = Catalog(1, "t", {}, {"gvm": art})
        called = []
        fake_snapshot_repo(monkeypatch, {"t/gvm": {"big.bin": b"x" * 10_000}})
        monkeypatch.setattr(mp.shutil, "disk_usage",
                            lambda p: type("U", (), {"free": 1})())
        col = Collector()
        mgr = manager(cat, roots, col)
        result = mgr.op_download(["gvm"])
        assert result["ok"] is False
        errs = col.of("artifact-error")
        assert errs and errs[0]["error"] == "disk-space"
        assert "free" in errs[0]["message"] or "GB" in errs[0]["message"]


class TestHttpTransport:
    def make_http_artifact(self, roots, data: bytes, trusted=True):
        spec = FileSpec("f.bin", "f.bin", len(data),
                        hashlib.sha256(data).hexdigest() if trusted else None,
                        "github-release-digest" if trusted else None, False, True)
        src = roots.cache_dir / "src"
        src.mkdir(parents=True)
        (src / "f.bin").write_bytes(data)
        art = Artifact(id="ma2", label="m", capability="t", kind="http_file",
                       required_profile=False, layout_root="data_dir", layout_rel="dl",
                       url=(src / "f.bin").as_uri(), revision="v1.0.0",
                       revision_status="pinned", files=(spec,))
        return art, data

    def test_file_url_download_verifies_and_promotes(self, roots):
        art, data = self.make_http_artifact(roots, b"payload" * 20)
        col = Collector()
        mgr = manager(Catalog(1, "t", {}, {"ma2": art}), roots, col)
        assert mgr.op_download(["ma2"])["ok"] is True, col.events
        final = roots.data_dir / "dl" / "f.bin"
        assert final.read_bytes() == data
        assert not final.with_name("f.bin.part").exists()
        assert col.of("progress")

    def test_hash_mismatch_leaves_no_final(self, roots):
        art, data = self.make_http_artifact(roots, b"payload" * 20)
        src = Path(art.url.replace("file://", ""))
        # serve different bytes than the catalog digest expects
        col = Collector()
        mgr = manager(Catalog(1, "t", {}, {"ma2": art}), roots, col)
        # swap the source after constructing the trusted spec
        src.write_bytes(b"tampered!!" * 20)
        result = mgr.op_download(["ma2"])
        assert result["ok"] is False
        errs = col.of("artifact-error")
        assert errs and errs[0]["error"] == "hash-mismatch"
        assert not (roots.data_dir / "dl" / "f.bin").exists()

    def test_rvm_reuses_vendor_checksum_downloader_under_supervision(self, roots, monkeypatch):
        data = b"rvm-checkpoint"
        filename = "rvm.npz"
        md5_b64 = base64.b64encode(hashlib.md5(data).digest()).decode()  # noqa: S324
        art = Artifact(
            id="rvm", label="RVM", capability="key", kind="http_file",
            required_profile=True, layout_root="v2_models_dir", layout_rel="vfms/rvm",
            url="https://models.example/rvm.npz", revision="release",
            revision_status="pinned", md5_b64=md5_b64,
            files=(FileSpec(filename, filename, len(data), None, None, False, True),),
        )
        requests = []

        def fake_worker(self, request, artifact):
            requests.append(request)
            path = Path(request["staging_root"]) / "vfms" / "rvm" / filename
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)

        monkeypatch.setattr(mp.ProvisionManager, "_run_download_worker", fake_worker)
        mgr = manager(Catalog(1, "t", {}, {"rvm": art}), roots, Collector())
        assert mgr.op_download(["rvm"])["ok"] is True
        assert requests[0]["action"] == "rvm"
        assert (roots.v2_models_dir / "vfms" / "rvm" / filename).read_bytes() == data


class TestEventStream:
    def test_events_are_json_serializable_ndjson(self, roots, monkeypatch, capsys):
        specs = [make_filespec("a.bin", b"a" * 10)]
        art = snapshot_artifact("gvm", "gvm/weights", specs)
        cat = Catalog(1, "t", {}, {"gvm": art})
        fake_snapshot_repo(monkeypatch, {"t/gvm": {"a.bin": b"a" * 10}})
        mgr = mp.ProvisionManager(cat, roots)  # default emitter -> captured stdout
        mgr.op_check(["gvm"])
        out = capsys.readouterr().out
        lines = [json.loads(l) for l in out.strip().splitlines()]
        assert any(l["event"] == "status" for l in lines)
        assert any(l["event"] == "op-done" for l in lines)
        assert capsys.readouterr().err == ""  # events never on stderr

    def test_cli_catalog_is_offline_and_download_requires_a_target(self, roots, monkeypatch, capsys):
        args = ["--data-dir", str(roots.data_dir), "--v2-models-dir", str(roots.v2_models_dir),
                "--hf-cache", str(roots.hf_cache), "--cache-dir", str(roots.cache_dir)]
        monkeypatch.setattr(sys, "argv", ["setup_models.py", "--catalog", *args])
        assert setup_models.main() == 0
        events = [json.loads(line) for line in capsys.readouterr().out.splitlines()]
        assert [event["event"] for event in events] == ["catalog", "op-done"]

        monkeypatch.setattr(sys, "argv", ["setup_models.py", "--download",
                                            "--operation-id", "download-test", *args])
        assert setup_models.main() == 1
        result = json.loads(capsys.readouterr().out.splitlines()[-1])
        assert result["error"] == "target-required"

    def test_verify_cli_emits_failed_terminal_for_regular_file_cache_root(self, roots,
                                                                           monkeypatch, capsys):
        blocked_cache = roots.cache_dir.parent / "not-a-directory"
        blocked_cache.write_text("file occupies cache root")
        monkeypatch.setattr(sys, "argv", [
            "setup_models.py", "--verify", "--target", "matanyone2",
            "--data-dir", str(roots.data_dir),
            "--v2-models-dir", str(roots.v2_models_dir),
            "--hf-cache", str(roots.hf_cache),
            "--cache-dir", str(blocked_cache),
        ])

        assert setup_models.main() == 1
        events = [json.loads(line) for line in capsys.readouterr().out.splitlines()]
        assert events[-1]["event"] == "op-done"
        assert events[-1]["op"] == "verify" and events[-1]["ok"] is False

    def test_process_crash_after_old_rename_recovers_before_retry(self, roots, tmp_path):
        good = b"fixture-weights"
        final = roots.data_dir / "fixture" / "weights" / "weights.bin"
        final.parent.mkdir(parents=True)
        final.write_bytes(good)
        art = snapshot_artifact("fixture-weights", "fixture/weights",
                                [make_filespec("weights.bin", good)])
        catalog = Catalog(1, "test", {}, {"fixture-weights": art})
        checker = ms.StatusChecker(catalog, roots)
        assert checker.verify_artifact(art)["verified"] is True
        assert checker.hash_cache.save() is True

        fixture = str(ROOT / "tests" / "fixtures" / "model_provision_cli.py")
        shared_roots = ["--data-dir", str(roots.data_dir),
                        "--v2-models-dir", str(roots.v2_models_dir),
                        "--hf-cache", str(roots.hf_cache),
                        "--cache-dir", str(roots.cache_dir)]
        crashed = subprocess.run(
            [sys.executable, fixture, "--fixture-crash-after-old-move", "--repair",
             "--target", "fixture-weights", "--operation-id", "crash-operation",
             *shared_roots], capture_output=True, text=True, timeout=5, check=False)
        assert crashed.returncode == 77
        assert not final.exists()
        assert list(final.parent.parent.glob(".weights.*.previous"))
        assert (final.parent.parent / ".weights.corridorkey-publish.json").exists()

        recovered = subprocess.run(
            [sys.executable, fixture, "--download", "--profile", "required",
             "--operation-id", "crash-operation", *shared_roots],
            capture_output=True, text=True, timeout=5, check=False)
        events = [json.loads(line) for line in recovered.stdout.splitlines()]
        assert recovered.returncode == 0, recovered.stderr + recovered.stdout
        assert final.read_bytes() == good
        assert not list(final.parent.parent.glob(".weights.*.previous"))
        assert not (final.parent.parent / ".weights.corridorkey-publish.json").exists()
        assert not (roots.cache_dir / "model-operations" / "crash-operation.active").exists()
        assert [event["event"] for event in events][-1] == "op-done"
        assert events[-1]["ok"] is True

    def test_cancel_cli_returns_failure_when_marker_cannot_be_written(
            self, roots, monkeypatch, capsys):
        args = ["--cache-dir", str(roots.cache_dir), "--data-dir", str(roots.data_dir),
                "--v2-models-dir", str(roots.v2_models_dir), "--hf-cache", str(roots.hf_cache)]
        operation_id = "write-failure-cli"
        owner = mp.ProvisionManager(Catalog(1, "t", {}, {}), roots,
                                    operation_id=operation_id)
        owner_lock = owner._begin_operation()
        monkeypatch.setattr(sys, "argv", ["setup_models.py", "--cancel",
                                            "--operation-id", operation_id, *args])
        builtin_open = open

        def fail_marker(path, *args, **kwargs):
            if f"{operation_id}.cancel" in str(path):
                raise PermissionError("marker write denied")
            return builtin_open(path, *args, **kwargs)

        monkeypatch.setattr("builtins.open", fail_marker)
        try:
            assert setup_models.main() == 1
            events = [json.loads(line) for line in capsys.readouterr().out.splitlines()]
            assert [event["event"] for event in events] == ["op-start", "op-done"]
            terminal = events[-1]
            assert terminal["event"] == "op-done" and terminal["ok"] is False
            assert terminal["error"] == "cancel-marker"
            assert terminal["operation_id"] == operation_id
        finally:
            owner._end_operation(owner_lock)

    def test_setup_import_defers_descriptor_failure_to_json_terminal(self, monkeypatch, capsys):
        from backend.install_paths import InstallDescriptorError
        from backend.model_status import Roots

        def broken_roots(*args, **kwargs):
            raise InstallDescriptorError("broken test descriptor")

        monkeypatch.setattr(Roots, "defaults", broken_roots)
        spec = importlib.util.spec_from_file_location(
            "setup_models_deferred", ROOT / "scripts" / "setup_models.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        monkeypatch.setattr(sys, "argv", ["setup_models.py", "--catalog"])
        assert module.main() == 2
        terminal = json.loads(capsys.readouterr().out.splitlines()[-1])
        assert terminal["event"] == "op-done"
        assert terminal["error"] == "install-descriptor"

    def test_same_size_corruption_is_verified_then_repaired_before_skip(self, roots, monkeypatch):
        correct = b"right-bytes"
        art = snapshot_artifact("gvm", "gvm/weights", [make_filespec("weights.bin", correct)])
        cat = Catalog(1, "t", {}, {"gvm": art})
        final = roots.data_dir / "gvm" / "weights" / "weights.bin"
        final.parent.mkdir(parents=True)
        final.write_bytes(b"wrong-bytes")
        calls = []
        fake_snapshot_repo(monkeypatch, {"t/gvm": {"weights.bin": correct}},
                           hook=lambda repo, stage: calls.append(repo))
        mgr = manager(cat, roots)

        assert mgr.op_download(["gvm"])["ok"] is True
        assert calls == ["t/gvm"]
        assert final.read_bytes() == correct
        assert mgr.checker.check_artifact(art)["ready"] is True

    def test_installed_artifact_skips_network(self, roots, monkeypatch):
        specs = [make_filespec("a.bin", b"a" * 10)]
        art = snapshot_artifact("gvm", "gvm/weights", specs)
        final = roots.data_dir / "gvm" / "weights" / "a.bin"
        final.parent.mkdir(parents=True)
        final.write_bytes(b"a" * 10)
        cat = Catalog(1, "t", {}, {"gvm": art})
        col = Collector()
        mgr = manager(cat, roots, col)

        def no_net(*a, **k):
            raise AssertionError("installed artifact must not touch the network")
        monkeypatch.setattr("huggingface_hub.snapshot_download", no_net)
        assert mgr.op_download(["gvm"])["ok"] is True
        done = col.of("artifact-done")
        assert done and "already installed" in done[0].get("note", "")

    def test_local_package_not_installable_via_manager(self, roots, monkeypatch):
        art = Artifact(id="sam2-package", label="s", capability="t", kind="local_package",
                       required_profile=False, layout_root=None, layout_rel=None,
                       pip_spec="sam-2", install_hint="pip install -e .[tracker]")
        col = Collector()
        monkeypatch.setattr(ms, "_probe_import", lambda _: False)
        mgr = manager(Catalog(1, "t", {}, {"sam2-package": art}), roots, col)
        assert mgr.op_download(["sam2-package"])["ok"] is False
        errs = col.of("artifact-error")
        assert errs and errs[0]["error"] == "not-installable"

    def test_download_requires_an_explicit_target(self, roots):
        col = Collector()
        mgr = manager(Catalog(1, "t", {}, {}), roots, col)
        assert mgr.op_download([])["error"] == "target-required"
        assert col.of("op-done")[-1]["error"] == "target-required"

    def test_unresolved_hf_revision_is_never_downloaded(self, roots):
        art = Artifact(id="gvm", label="gvm", capability="t", kind="hf_snapshot",
                      required_profile=False, layout_root="data_dir", layout_rel="gvm",
                      repo_id="t/gvm", revision=None, revision_status="unresolved",
                      files=(FileSpec("weights.bin", "weights.bin", 4, None, None, False, True),))
        col = Collector()
        mgr = manager(Catalog(1, "t", {}, {"gvm": art}), roots, col)
        assert mgr.op_download(["gvm"])["ok"] is False
        assert col.of("artifact-error")[0]["error"] == "unresolved-revision"

    @pytest.mark.parametrize(("helper", "args", "target"), [
        ("download_corridorkey", (), "corridorkey"),
        ("download_sam2", ("small",), "sam2-small"),
        ("download_repo", ("gvm",), "gvm"),
        ("download_model", ("videomama",), "videomama"),
        ("download_sam2_model", ("large",), "sam2-large"),
        ("download_matanyone2", (), "matanyone2"),
        ("download_birefnet", (), "birefnet-BiRefNet-matting"),
    ])
    def test_legacy_download_helpers_dispatch_through_manager(
            self, roots, monkeypatch, helper, args, target):
        requested = []

        class RecordingManager:
            def op_download(self, targets):
                requested.extend(targets)
                return {"ok": True, "failed": [], "cancelled": False}

        monkeypatch.setattr(setup_models, "create_provision_manager",
                            lambda **kwargs: RecordingManager())
        assert getattr(setup_models, helper)(*args) is True
        assert requested == [target]
