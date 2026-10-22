"""Offline model paths shared by inference loaders."""
from __future__ import annotations

import os
from contextlib import ExitStack
from dataclasses import replace
from pathlib import Path

from backend.model_catalog import CatalogError, hf_cache_snapshot_path, load_catalog
from backend.model_lock import DestinationBusyError, DestinationLock
from backend.model_status import Roots, StatusChecker


class ModelAssetsUnavailableError(RuntimeError):
    """A catalog model or one of its required dependencies is incomplete."""


class ModelArtifactLease:
    """Validated catalog paths protected until ``close`` or object destruction."""

    def __init__(self, artifact_id: str, paths: dict[str, Path], locks: ExitStack):
        self.artifact_id = artifact_id
        self.paths = paths
        self.path = paths[artifact_id]
        self._locks = locks

    def close(self) -> None:
        if self._locks is not None:
            locks, self._locks = self._locks, None
            locks.close()

    def __enter__(self) -> "ModelArtifactLease":
        return self

    def __exit__(self, *_exc) -> None:
        self.close()

    def __fspath__(self) -> str:
        return os.fspath(self.path)

    def __del__(self):
        self.close()


def _catalog_artifact(artifact_id: str):
    catalog = load_catalog()
    try:
        artifact = catalog.get(artifact_id)
    except CatalogError as exc:
        raise ModelAssetsUnavailableError(str(exc)) from exc
    return catalog, artifact


def model_artifact_dir(artifact_id: str, *, roots: Roots | None = None) -> Path:
    """Resolve a catalog model directory without checking or loading its files."""
    catalog, artifact = _catalog_artifact(artifact_id)
    roots = roots or Roots.defaults()
    if artifact.kind == "hf_cache_file":
        path = hf_cache_snapshot_path(
            roots.hf_cache, artifact.repo_id, artifact.revision, artifact.hf_filename
        ).parent
    else:
        base = {
            "v2_models_dir": roots.v2_models_dir,
            "data_dir": roots.data_dir,
            "v2_package_root": roots.v2_package_root,
        }.get(artifact.layout_root)
        path = base / artifact.layout_rel if base is not None and artifact.layout_rel else base
    remap_roots = {destination.split("/", 1)[0] for destination in artifact.path_remap.values()}
    if path is not None and len(remap_roots) == 1:
        path /= remap_roots.pop()
    if path is None:
        raise ModelAssetsUnavailableError(
            f"Model catalog entry {artifact_id} has no local directory path."
        )
    return Path(path)


def validate_local_model_path(
    artifact_id: str,
    path: str | Path,
    *,
    components: tuple[str, ...] | None = None,
) -> Path:
    """Check required catalog files in an explicit local model root, without hashing."""
    _catalog, artifact = _catalog_artifact(artifact_id)
    root = Path(path).expanduser()
    if not root.is_dir():
        raise ModelAssetsUnavailableError(f"Explicit model directory is missing: {root}")

    remap_roots = {destination.split("/", 1)[0] for destination in artifact.path_remap.values()}
    remap_root = remap_roots.pop() if len(remap_roots) == 1 else None
    selected = []
    problems = []
    for file in artifact.required_files():
        parts = file.layout_path.split("/")
        if remap_root:
            if parts[0] != remap_root:
                continue
            parts = parts[1:]
        if components and (not parts or parts[0] not in components):
            continue
        selected.append(file)
        local_path = root.joinpath(*parts)
        if not local_path.is_file():
            problems.append(f"missing: {'/'.join(parts)}")
        elif local_path.stat().st_size == 0:
            problems.append(f"empty: {'/'.join(parts)}")
        elif file.size is not None and local_path.stat().st_size != file.size:
            problems.append(
                f"size mismatch: {'/'.join(parts)} expected {file.size} got {local_path.stat().st_size}"
            )
    if not selected:
        raise ModelAssetsUnavailableError(
            f"Catalog entry {artifact_id} has no required files for the selected local component."
        )
    if problems:
        raise ModelAssetsUnavailableError(
            f"Explicit model assets for {artifact_id} are incomplete at {root}: "
            f"{', '.join(problems)}. Repair the local model files and retry."
        )
    return root


def acquire_model_artifact(
    artifact_id: str,
    *,
    roots: Roots | None = None,
    exclude_dependencies: tuple[str, ...] = (),
    exclude_components: tuple[str, ...] = (),
) -> ModelArtifactLease:
    """Validate and lease an artifact and its dependency closure for its loaded lifetime."""
    catalog, artifact = _catalog_artifact(artifact_id)
    checker = StatusChecker(catalog, roots or Roots.defaults())
    problems: list[str] = []
    closure = catalog.closure([artifact_id])
    excluded = {
        dependency_id
        for skipped_id in exclude_dependencies
        for dependency_id in catalog.closure([skipped_id])
    }
    selected = []
    for dependency_id in closure:
        if dependency_id in excluded:
            continue
        dependency = catalog.get(dependency_id)
        if dependency_id == artifact_id and exclude_components:
            dependency = replace(
                dependency,
                files=tuple(
                    file for file in dependency.files
                    if file.layout_path.split("/", 1)[0] not in exclude_components
                ),
            )
        selected.append(dependency)

    def lock_destination(dependency):
        return checker.artifact_dir(dependency) or checker.roots.v2_package_root

    ordered = sorted(
        {os.path.normcase(str(lock_destination(dep).resolve(strict=False))): lock_destination(dep)
         for dep in selected}.items()
    )
    locks = ExitStack()
    try:
        for _identity, destination in ordered:
            locks.enter_context(DestinationLock(destination, shared=True))
        for dependency in selected:
            status = checker.check_artifact(dependency)
            messages = list(status["problems"])
            for file in dependency.required_files():
                path = checker.file_path(dependency, file)
                if path is not None and path.is_file() and path.stat().st_size == 0:
                    messages.append(f"empty: {file.layout_path}")
            if status["state"] != "installed" or messages:
                action = "Repair required"
                detail = ", ".join(messages) or "required files are missing"
            elif status["ready"]:
                continue
            elif status["verify_state"] == "unknown (not verified yet)":
                action = "Verify required"
                detail = "installed files have not been verified against trusted digests"
            elif status["unhashed_files"]:
                action = "Verification unavailable"
                detail = "catalog has no trusted digest for " + ", ".join(status["unhashed_files"])
            else:
                action = "Repair required"
                detail = "known trusted-digest mismatch"
            if not status["ready"] or messages:
                problems.append(
                    f"{dependency.id} at {status['path'] or 'package'}: {detail} ({action})"
                )
        if problems:
            raise ModelAssetsUnavailableError(
                f"Model assets for {artifact_id} are missing or incomplete: "
                f"{' ; '.join(problems)}. Verify or repair the selected model in the app's "
                "model installer, then retry."
            )

        paths = {}
        for dependency in selected:
            if dependency.kind in ("hf_cache_file", "http_file"):
                required = dependency.required_files()
                path = checker.file_path(dependency, required[0]) if required else None
            else:
                path = model_artifact_dir(dependency.id, roots=checker.roots)
            if path is not None:
                paths[dependency.id] = Path(path)
        if artifact_id not in paths:
            raise ModelAssetsUnavailableError(
                f"Model catalog entry {artifact_id} has no local runtime path; repair the install."
            )
        return ModelArtifactLease(artifact_id, paths, locks)
    except DestinationBusyError as exc:
        locks.close()
        raise ModelAssetsUnavailableError(
            f"Model assets for {artifact_id} are being repaired; retry after provisioning completes."
        ) from exc
    except Exception:
        locks.close()
        raise


def require_model_artifact(*args, **kwargs) -> ModelArtifactLease:
    """Compatibility name; keep the returned lease alive with the loaded model."""
    return acquire_model_artifact(*args, **kwargs)
