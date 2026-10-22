"""Shipped catalog integrity: pinned inventory, provenance, no invented hashes."""
import json
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from backend.model_catalog import (  # noqa: E402
    BIREFNET_VARIANTS,
    FLAG_TARGET_MAP,
    CatalogError,
    FileSpec,
    hf_cache_snapshot_path,
    hf_hub_cache_dir,
    load_catalog,
)


@pytest.fixture(scope="module")
def catalog():
    return load_catalog()


def test_shipped_catalog_loads_with_full_inventory(catalog):
    ids = set(catalog.artifacts)
    # CK2 bundle + foundations
    assert {"corridorkey-code", "corridorkey-weights", "cradio", "moge", "rvm"} <= ids
    # SAM2: package + all three exposed variants
    assert {"sam2-package", "sam2-small", "sam2-base-plus", "sam2-large"} <= ids
    assert {"gvm", "videomama", "videomama-svd", "matanyone2"} <= ids
    # all 16 exposed BiRefNet variants, no substitutions
    birefnet = sorted(i for i in ids if i.startswith("birefnet-")
                       and i != "birefnet-runtime")
    assert len(birefnet) == 16, birefnet
    assert catalog.get("birefnet-BiRefNet-matting").default_variant is True


def test_every_remote_artifact_pinned_or_explicitly_unresolved(catalog):
    for art in catalog.artifacts.values():
        if art.kind.startswith("hf") or art.kind == "http_file":
            assert art.revision_status in ("pinned", "unresolved")
            if art.revision_status == "unresolved":
                assert not art.revision  # explicit null, never a mutable "main"


def test_sha256_fields_never_invented(catalog):
    """Every recorded sha256 must carry an upstream source; git blobs are separate."""
    for art in catalog.artifacts.values():
        for f in art.files:
            if f.sha256:
                assert len(f.sha256) == 64
                assert f.sha256_source in ("hf-lfs-oid", "github-release-digest"), (art.id, f.path)
                # a git blob sha1 must never masquerade as a sha256
                assert f.git_blob_sha1 is None or f.git_blob_sha1 != f.sha256


def test_rvm_records_upstream_md5_not_fake_sha256(catalog):
    rvm = catalog.get("rvm")
    assert rvm.md5_b64 == "ZJb6kj8+tasiJbVeplXu7Q=="
    assert all(f.sha256 is None for f in rvm.files)
    assert all(f.sha256_source is None for f in rvm.files)


def test_provenance_recorded_not_adjudicated(catalog):
    for art in catalog.artifacts.values():
        prov = art.provenance
        assert prov.get("source"), art.id
        assert any("not a legal conclusion" in str(v).lower() or "recorded" in str(v).lower()
                   for v in list(prov.values()) + [catalog.meta.get("license_note", "")]), art.id
    # the two flagged license situations stay explicit
    assert "CONFLICT RECORDED" in catalog.get("matanyone2").provenance.get("weights_license", "")
    assert "verify exact terms" in catalog.get("videomama-svd").provenance.get("weights_license", "")


def test_videomama_layout_matches_loader_contract(catalog):
    vm = catalog.get("videomama")
    layouts = {f.layout_path for f in vm.required_files()}
    assert "VideoMaMa/unet/config.json" in layouts
    assert "VideoMaMa/unet/diffusion_pytorch_model.safetensors" in layouts
    assert vm.depends == ("videomama-svd",)
    assert "VideoMaMa/unet/" in " ".join(layouts)  # nested, not the legacy flat layout


def test_videomama_svd_required_closure_excludes_unloaded_files(catalog):
    svd = catalog.get("videomama-svd")
    paths = {f.path for f in svd.required_files()}
    assert "image_encoder/model.fp16.safetensors" in paths
    assert "vae/diffusion_pytorch_model.fp16.safetensors" in paths
    # not loaded by VideoInferencePipeline
    assert "unet/diffusion_pytorch_model.fp16.safetensors" not in paths
    assert "svd_xt.safetensors" not in paths
    assert "output_tile.gif" not in paths


def test_snapshot_download_selection_is_required_files_plus_notices(catalog):
    svd = catalog.get("videomama-svd")
    selected = {f.path for f in svd.download_files()}
    assert {"LICENSE.md", "README.md", ".gitattributes"} <= selected
    assert {f.path for f in svd.required_files()} <= selected
    assert "unet/diffusion_pytorch_model.fp16.safetensors" not in selected
    assert "image_encoder/model.safetensors" not in selected
    assert sum(f.size for f in svd.download_files()) == svd.size_bytes_estimate


def test_dependency_closure_expansion(catalog):
    assert catalog.closure(["videomama"]) == ["videomama-svd", "videomama"]
    assert catalog.closure(["corridorkey"]) == ["corridorkey-code", "cradio", "moge", "rvm",
                                                "corridorkey-weights"]
    assert catalog.closure(["birefnet"]) == [
        "birefnet-runtime", *[f"birefnet-{n}" for n in BIREFNET_VARIANTS]]
    assert catalog.closure(["sam2"]) == ["sam2-package", "sam2-small", "sam2-base-plus",
                                          "sam2-large"]
    assert set(catalog.get("corridorkey-weights").depends) == {
        "corridorkey-code", "cradio", "moge", "rvm"}
    assert all(catalog.get(f"sam2-{v}").depends == ("sam2-package",)
               for v in ("small", "base-plus", "large"))
    assert all(catalog.get(f"birefnet-{v}").depends == ("birefnet-runtime",)
               for v in BIREFNET_VARIANTS)
    with pytest.raises(CatalogError):
        catalog.closure(["nope"])


def test_birefnet_catalog_matches_pinned_remote_code_closure(catalog):
    runtime = catalog.get("birefnet-runtime")
    assert runtime.pip_spec == "timm==1.0.24"
    assert set(runtime.probe_imports) == {
        "cv2", "PIL", "numpy", "torch", "torchvision",
        "transformers", "timm", "kornia", "einops",
    }
    for name in BIREFNET_VARIANTS:
        art = catalog.get(f"birefnet-{name}")
        required = {f.path for f in art.required_files()}
        assert {"config.json", "birefnet.py", "BiRefNet_config.py", "model.safetensors"} <= required
        assert "handler.py" not in required
        assert "requirements.txt" not in required
        assert not {"nets.py", "builds.py"} & required
        assert {"birefnet.py", "BiRefNet_config.py", "config.json"} <= {
            f.path for f in art.download_files()}


def test_flag_target_map_groups_resolve(catalog):
    for group, ids in FLAG_TARGET_MAP.items():
        for i in ids:
            catalog.get(i)  # all group members exist


def test_profiles(catalog):
    req = catalog.profile("required")
    assert "corridorkey-weights" in req and "gvm" not in req
    full = catalog.profile("full")
    assert len(full) == len(catalog.artifacts)


def test_hf_cache_env_resolution(monkeypatch, tmp_path):
    monkeypatch.delenv("HF_HUB_CACHE", raising=False)
    monkeypatch.setenv("HF_HOME", str(tmp_path / "hfhome"))
    assert hf_hub_cache_dir() == tmp_path / "hfhome" / "hub"
    monkeypatch.setenv("HF_HUB_CACHE", str(tmp_path / "custom"))
    assert hf_hub_cache_dir() == tmp_path / "custom"


def test_hf_cache_snapshot_path_layout():
    p = hf_cache_snapshot_path(Path("/cache"), "facebook/sam2.1-hiera-large",
                               "abc123", "sam2.1_hiera_large.pt")
    assert p == Path("/cache/models--facebook--sam2.1-hiera-large/snapshots/abc123/"
                     "sam2.1_hiera_large.pt")


def test_malformed_catalog_rejected(tmp_path):
    bad = tmp_path / "bad.json"
    bad.write_text('{"catalog_version": 1, "artifacts": {"x": {"kind": "hf_snapshot", '
                   '"repo_id": "a/b", "revision": "main", "files": []}}}')
    with pytest.raises(CatalogError):
        load_catalog(bad)


def test_unresolved_revision_is_loadable_but_never_floats(tmp_path):
    path = tmp_path / "unresolved.json"
    path.write_text('{"catalog_version":1,"artifacts":{"x":{"kind":"hf_snapshot",'
                    '"repo_id":"a/b","revision":null,"revision_status":"unresolved",'
                    '"files":[{"path":"weights.bin"}]}}}')
    assert load_catalog(path).get("x").revision_status == "unresolved"


@pytest.mark.parametrize("field,value", [
    ("path", "../escape.bin"), ("path", "/absolute.bin"),
    ("layout_path", "..\\escape.bin"), ("layout_path", "C:/escape.bin"),
])
def test_catalog_paths_cannot_escape_roots(tmp_path, field, value):
    path = tmp_path / "unsafe.json"
    file = {"path": "safe.bin", "size": 1}
    file[field] = value
    path.write_text(json.dumps({"catalog_version": 1, "artifacts": {
        "x": {"kind": "http_file", "url": "https://example.test/x",
              "revision": "v1", "revision_status": "pinned", "files": [file]}
    }}))
    with pytest.raises(CatalogError):
        load_catalog(path)


def test_filespec_sha256_validation(tmp_path):
    with_sha = tmp_path / "c.json"
    with_sha.write_text('{"catalog_version":1,"artifacts":{"a":{"kind":"http_file",'
                        '"url":"https://x/f","files":[{"path":"f","sha256":"deadbeef"}]}}}')
    with pytest.raises(CatalogError):
        load_catalog(with_sha)


def test_runtime_notes_supersede_openexr_buildtools_claim(catalog):
    note = catalog.meta.get("runtime_notes", {}).get("openexr_windows", "")
    assert "OpenEXR 3.5.2" in note and "wheel" in note and "Imath" in note
    assert "superseded" in note or "NOT require" in note
