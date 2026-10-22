"""Metadata refresh is metadata-only and every outbound request is capped."""
import io
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from backend import model_metadata as mm  # noqa: E402


def test_metadata_request_timeouts_are_capped_at_ten_seconds(monkeypatch):
    timeouts = []

    class Response(io.BytesIO):
        headers = {"Content-Length": "12"}

        def __enter__(self):
            return self

        def __exit__(self, *_):
            self.close()

    def fake_urlopen(request, timeout):
        timeouts.append(timeout)
        return Response(b'{"sha":"abc"}')

    monkeypatch.setattr(mm.urllib.request, "urlopen", fake_urlopen)
    mm.fetch_json("https://metadata.example/info", timeout=90)
    mm.http_head("https://metadata.example/file", timeout=90)
    assert timeouts == [mm.MAX_TIMEOUT, mm.MAX_TIMEOUT]


def test_refresh_does_not_replace_catalog_with_unsafe_upstream_paths(tmp_path, monkeypatch):
    from scripts import refresh_model_metadata as refresh

    path = tmp_path / "catalog.json"
    original = {"catalog_version": 1, "updated": "before", "artifacts": {
        "x": {"kind": "hf_snapshot", "repo_id": "a/b", "revision": "a" * 40,
              "revision_status": "pinned", "files": [{"path": "weights.bin"}]}
    }}
    path.write_text(json.dumps(original))
    monkeypatch.setattr(refresh, "catalog_path", lambda: path)
    monkeypatch.setattr(sys, "argv", ["refresh", "--write", "--artifact", "x"])
    monkeypatch.setattr(mm, "resolve_hf_artifact", lambda *args: mm.Resolution(
        artifact_id="x", ok=True, revision="a" * 40,
        files=[{"path": "../escape.bin", "layout_path": "../escape.bin", "size": 1,
                "sha256": None, "sha256_source": None, "git_blob_sha1": None,
                "lfs": False, "required": True}]))

    assert refresh.main() == 1
    assert json.loads(path.read_text()) == original
    assert not path.with_suffix(".json.tmp").exists()


def test_birefnet_refresh_preserves_the_imported_code_closure(monkeypatch):
    from backend.model_catalog import catalog_path

    catalog = json.loads(catalog_path().read_text())
    art = catalog["artifacts"]["birefnet-BiRefNet"]
    entries = [
        {"type": "file", "path": "config.json", "size": 405,
         "oid": "06d8fa9d7f2f4c6f1cf0dc6e7bfd194153176a42"},
        {"type": "file", "path": "BiRefNet_config.py", "size": 298,
         "oid": "37c8ac58bec2f52dac34204978a7b61b69e3da76"},
        {"type": "file", "path": "birefnet.py", "size": 91896,
         "oid": "8dea801e2b56facb8486ba351e6f91c3e664eb6d"},
        {"type": "file", "path": "handler.py", "size": 4762, "oid": "1" * 40},
        {"type": "file", "path": "requirements.txt", "size": 149, "oid": "2" * 40},
    ]
    monkeypatch.setattr(mm, "hf_model_info", lambda *_args, **_kwargs: {
        "sha": art["revision"], "gated": False,
    })
    monkeypatch.setattr(mm, "hf_tree", lambda *_args, **_kwargs: entries)

    resolved = mm.resolve_hf_artifact(art)
    by_path = {file["path"]: file for file in resolved.files}
    assert {"config.json", "birefnet.py", "BiRefNet_config.py"} <= by_path.keys()
    assert all(by_path[name]["required"] for name in
               ("config.json", "birefnet.py", "BiRefNet_config.py"))
    assert by_path["handler.py"]["required"] is False
    assert by_path["requirements.txt"]["required"] is False
    assert "nets.py" not in by_path and "builds.py" not in by_path
