"""Remove obsolete copies of catalog models from legacy and HF cache roots."""
from __future__ import annotations

import os
import stat
from pathlib import Path

from backend.model_catalog import load_catalog
from backend.model_status import Roots, StatusChecker


def _is_link(p: Path) -> bool:
    try:
        info = p.lstat()
        if stat.S_ISLNK(info.st_mode):
            return True
        isjunction = getattr(os.path, "isjunction", None)
        if isjunction is not None:
            return isjunction(p)
        return os.name == "nt" and bool(
            info.st_file_attributes & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0)
        )
    except OSError:
        return True


def _parent_is_real(p: Path) -> bool:
    try:
        return os.path.normcase(str(p.parent.resolve(strict=False))) == os.path.normcase(
            str(p.parent.absolute())
        )
    except OSError:
        return False


def prune_unused_model_copies(dry_run: bool = False) -> list[dict]:
    catalog = load_catalog()
    roots = Roots.defaults()
    checker = StatusChecker(catalog, roots)
    result: list[dict] = []
    planned_removed: set[Path] = set()
    processed_cache_roots: set[Path] = set()
    def remove(path: Path, reason: str, *, directory: bool = False) -> None:
        if not _parent_is_real(path):
            if not any(item["path"] == str(path) for item in result):
                result.append({"path": str(path), "bytes": 0, "reason": reason, "error": "skipped: reached through a link"})
            return
        try:
            info = path.lstat()
            if directory:
                if not stat.S_ISDIR(info.st_mode):
                    return
                if any(child not in planned_removed for child in path.iterdir()):
                    return
                size = 0
                if not dry_run:
                    path.rmdir()
            else:
                if not stat.S_ISREG(info.st_mode) and not stat.S_ISLNK(info.st_mode):
                    return
                size = info.st_size if stat.S_ISREG(info.st_mode) else 0
                if not dry_run:
                    path.unlink()
            planned_removed.add(path)
            result.append({"path": str(path), "bytes": size, "reason": reason})
        except FileNotFoundError:
            pass
        except OSError as exc:
            result.append({"path": str(path), "bytes": 0, "reason": reason, "error": str(exc)})

    def erase_tree(path: Path, reason: str) -> None:
        if not _parent_is_real(path):
            if not any(item["path"] == str(path) for item in result):
                result.append({"path": str(path), "bytes": 0, "reason": reason, "error": "skipped: reached through a link"})
            return
        try:
            if _is_link(path):
                info = path.lstat()
                if not dry_run:
                    if os.name == "nt" and not stat.S_ISLNK(info.st_mode):
                        os.rmdir(path)
                    else:
                        path.unlink()
                planned_removed.add(path)
                result.append({"path": str(path), "bytes": 0, "reason": reason})
                return
            info = path.lstat()
            if stat.S_ISDIR(info.st_mode):
                for child in path.iterdir():
                    erase_tree(child, reason)
                remove(path, reason, directory=True)
            else:
                remove(path, reason)
        except FileNotFoundError:
            pass
        except OSError as exc:
            result.append({"path": str(path), "bytes": 0, "reason": reason, "error": str(exc)})

    for art in catalog.artifacts.values():
        if art.kind in ("local_package", "local_code"):
            continue
        in_use = checker.artifact_dir(art)
        destination = checker.destination_dir(art)
        for legacy in (*roots.legacy_data_dirs, *roots.legacy_models_dirs):
            legacy = Path(legacy)
            for candidate in checker._candidate_dirs(art):
                candidate = Path(candidate)
                try:
                    relative = candidate.relative_to(legacy)
                except ValueError:
                    continue
                if not relative.parts or (in_use is not None and candidate.resolve(strict=False) == in_use.resolve(strict=False)) or (destination is not None and candidate.resolve(strict=False) == destination.resolve(strict=False)):
                    continue
                # Reject symlinked path components so catalog paths cannot escape a legacy root.
                current = legacy
                safe = True
                for part in relative.parts:
                    current /= part
                    try:
                        if _is_link(current):
                            safe = False
                            break
                    except FileNotFoundError:
                        safe = False
                        break
                if not safe:
                    continue
                for file in art.required_files():
                    remove(checker._file_at(art, candidate, file), f"legacy copy of {art.id}")
                parent = candidate
                while parent != legacy:
                    remove(parent, f"empty legacy directory for {art.id}", directory=True)
                    parent = parent.parent

        if not art.repo_id:
            continue
        repo_name = f"models--{art.repo_id.replace('/', '--')}"
        processed_caches: set[Path] = set()
        for cache in roots.hf_caches:
            cache = Path(cache)
            try:
                resolved_cache = cache.resolve(strict=False)
            except OSError:
                continue
            if resolved_cache in processed_caches:
                continue
            processed_caches.add(resolved_cache)
            processed_cache_roots.add(resolved_cache)
            cache = resolved_cache
            repo = cache / repo_name
            try:
                repo.relative_to(cache)
                repo_info = repo.lstat()
            except FileNotFoundError:
                continue
            except (OSError, ValueError) as exc:
                result.append({"path": str(repo), "bytes": 0, "reason": f"HF cache for {art.id}", "error": str(exc)})
                continue
            if not stat.S_ISDIR(repo_info.st_mode):
                continue
            reason = f"unused HF cache copy of {art.repo_id}"
            try:
                in_repo = in_use is not None and in_use.resolve(strict=False).is_relative_to(
                    repo.resolve(strict=False)
                )
            except (OSError, ValueError):
                in_repo = True
            if not in_repo:
                erase_tree(repo, reason)
                continue

            snapshots = repo / "snapshots"
            blobs = repo / "blobs"
            if _is_link(blobs) or _is_link(snapshots):
                continue
            keep = checker.artifact_dir(art)
            kept_blob_targets: set[Path] = set()
            try:
                resolved_keep = keep.resolve(strict=False)
                for snap in snapshots.iterdir():
                    try:
                        is_keep = snap.resolve(strict=False) == resolved_keep
                    except OSError:
                        continue
                    if is_keep or not snap.is_dir():
                        continue
                    erase_tree(snap, f"unused HF revision of {art.repo_id}")
                for snap in snapshots.iterdir():
                    try:
                        is_keep = snap.resolve(strict=False) == resolved_keep
                    except OSError:
                        continue
                    if not is_keep:
                        continue
                    for base, dirs, files in os.walk(snap, followlinks=False):
                        for name in files:
                            item = Path(base) / name
                            if item.is_symlink():
                                try:
                                    kept_blob_targets.add(item.resolve(strict=True))
                                except OSError:
                                    pass
                if blobs.is_dir():
                    for blob in blobs.iterdir():
                        if blob.resolve(strict=False) not in kept_blob_targets:
                            erase_tree(blob, f"unreferenced HF blob for {art.repo_id}")
                erase_tree(repo / ".no_exist", f"HF .no_exist cache for {art.repo_id}")
            except OSError as exc:
                result.append({"path": str(repo), "bytes": 0, "reason": reason, "error": str(exc)})

    for cache in processed_cache_roots:
        shared = cache / "blobs"
        marker = shared / ".huggingface-shared-blobs"
        if _is_link(shared) or not shared.is_dir() or not marker.is_file():
            continue
        try:
            for shard in shared.iterdir():
                if _is_link(shard) or not shard.is_dir():
                    continue
                for refs in shard.glob("*.refs"):
                    if _is_link(refs) or not refs.is_file():
                        continue
                    sha = refs.name[:-5]
                    try:
                        lines = refs.read_text(encoding="utf-8").splitlines()
                    except OSError:
                        continue
                    if not lines or any(not line.strip().startswith("models--") for line in lines):
                        continue
                    if any(os.path.lexists(cache / line.strip()) and (cache / line.strip()) not in planned_removed for line in lines):
                        continue
                    blob = shard / sha
                    remove(blob, f"orphaned shared HF blob {sha[:12]}")
                    remove(refs, f"orphaned shared HF blob refs {sha[:12]}")
                    remove(shard / f"{sha}.lock", f"orphaned shared HF blob lock {sha[:12]}")
        except OSError:
            continue
    return result
