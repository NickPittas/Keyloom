"""Round 3: verification survives Verify -> reopen, and cache read/write failures are exposed, not silent."""

import os

from backend.model_status import HASH_CACHE_NAME, HashCache, StatusChecker
from tests.test_model_status import hf_snapshot_artifact, make_catalog, roots, write_files  # noqa: F401


def _checker(roots, tmp_path):  # noqa: F811
    specs = write_files(roots.data_dir / "model", {"w.bin": b"weights"}, lfs_like={"w.bin"})
    cat = make_catalog(hf_snapshot_artifact("m", specs))
    return cat, StatusChecker(cat, roots)


def test_verify_then_reopen_same_roots_is_ready(roots, tmp_path):  # noqa: F811
    cat, sc = _checker(roots, tmp_path)
    assert sc.verify(["m"])[0]["verified"] is True
    again = StatusChecker(cat, roots)
    assert again.status(["m"])["artifacts"][0]["ready"] is True


def test_unreadable_record_is_reported_not_silent(roots, tmp_path):  # noqa: F811
    cat, sc = _checker(roots, tmp_path)
    sc.verify(["m"])
    (roots.cache_dir / HASH_CACHE_NAME).write_text("{not json", encoding="utf-8")
    item = StatusChecker(cat, roots).status(["m"])["artifacts"][0]
    assert item["present"] and not item["verified"]
    assert "could not be read" in item["verify_note"]


def test_missing_record_is_normal(roots, tmp_path):  # noqa: F811
    cat, sc = _checker(roots, tmp_path)
    item = sc.status(["m"])["artifacts"][0]
    assert not item["verified"] and "verify_note" not in item
    assert sc.hash_cache.load_error is None


def test_failed_save_is_reported_by_verify(roots, tmp_path):  # noqa: F811
    cat, sc = _checker(roots, tmp_path)
    roots.cache_dir.mkdir(parents=True, exist_ok=True)
    (roots.cache_dir / HASH_CACHE_NAME).mkdir()  # a directory where the file belongs: save must refuse
    res = sc.verify(["m"])[0]
    assert "not saved" in res["verify_note"] and "Repair is not needed" in res["verify_note"]
    assert not any("not saved" in p for p in res["problems"])  # not a red file problem
    assert sc.hash_cache.save_error


def test_failed_save_never_calls_failed_files_fine(roots, tmp_path):  # noqa: F811
    cat, sc = _checker(roots, tmp_path)
    (next((roots.data_dir / "model").rglob("w.bin"))).write_bytes(b"WEIGHTS")  # same size, wrong digest
    roots.cache_dir.mkdir(parents=True, exist_ok=True)
    (roots.cache_dir / HASH_CACHE_NAME).mkdir()
    res = sc.verify(["m"])[0]
    assert res["verified"] is False and res["problems"]  # the mismatch stays visible
    assert "not saved" in res["verify_note"] and "Repair is not needed" not in res["verify_note"]


def test_hashcache_save_error_cleared_on_success(tmp_path):
    hc = HashCache(tmp_path / "c" / HASH_CACHE_NAME)
    assert hc.save() and hc.save_error is None
    assert os.path.isfile(hc.path)
