"""App-side model resolver: task -> exact model files, readiness and in-use protection.

The app owns this mapping; executors receive the paths and decide nothing. Reuses the catalog,
StatusChecker (stat-only status, explicit hashing) and ModelArtifactLease (shared destination
locks) so roots, hash trust and visible cache failures stay exactly as in model_status.
"""
from __future__ import annotations

import threading
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path

from backend.model_catalog import Artifact, CatalogError, load_catalog
from backend.model_runtime import (
    ModelArtifactLease,
    ModelAssetsUnavailableError,
    acquire_model_artifact,
    dependency_path,
    dependency_problem,
)
from backend.model_status import Roots, StatusChecker, readiness_state

# Producers with one catalog entry; sam2 and birefnet have variants (see AppModels.variants).
FIXED = {"key": "corridorkey-weights", "gvm": "gvm", "matanyone2": "matanyone2", "videomama": "videomama"}
TASKS = (*FIXED, "sam2", "birefnet")


class ModelInUseError(RuntimeError):
    """A running job uses these model files, or they are being changed."""


@dataclass(frozen=True)
class Dependency:
    id: str
    state: str  # missing | verify_needed | verified | error
    detail: str | None  # dependency_problem text: what to Verify/Repair
    note: str | None  # visible cache failure (unreadable record), if any
    path: Path | None  # what the loader receives (file or directory)
    files: tuple[Path, ...]  # every required file, exact


@dataclass(frozen=True)
class ModelBinding:
    task: str
    artifact_id: str
    dependencies: tuple[Dependency, ...]  # dependency closure, loader order

    @property
    def ready(self) -> bool:
        return all(d.state == "verified" for d in self.dependencies)

    @property
    def state(self) -> str:
        """Worst dependency state: error > missing > verify_needed > verified."""
        states = {d.state for d in self.dependencies}
        return next(s for s in ("error", "missing", "verify_needed", "verified") if s in states)

    @property
    def files(self) -> tuple[Path, ...]:
        return tuple(f for d in self.dependencies for f in d.files)

    @property
    def paths(self) -> dict[str, Path]:
        return {d.id: d.path for d in self.dependencies if d.path is not None}

    def to_json(self) -> dict:
        return {"task": self.task, "artifact": self.artifact_id, "ready": self.ready, "state": self.state,
                "paths": {k: str(v) for k, v in self.paths.items()},
                "files": [str(f) for f in self.files],
                "dependencies": [{"id": d.id, "state": d.state, "detail": d.detail, "note": d.note}
                                 for d in self.dependencies]}


class AppModels:
    def __init__(self, roots: Roots | None = None, catalog=None):
        self.catalog = catalog or load_catalog()
        self.roots = roots or Roots.defaults()
        self.checker = StatusChecker(self.catalog, self.roots)
        self._lock = threading.Lock()
        self._jobs: dict[str, tuple[frozenset[str], ModelArtifactLease]] = {}
        self._changing: set[str] = set()

    # -- mapping ----------------------------------------------------------
    def variants(self, task: str) -> list[str]:
        """Catalog entry ids selectable for a task, default first."""
        if task in FIXED:
            return [FIXED[task]]
        if task == "sam2":
            found = [a for a in self.catalog.artifacts.values() if a.kind == "hf_cache_file" and a.capability == "hint-track"]
        elif task == "birefnet":
            found = [a for a in self.catalog.artifacts.values() if a.id.startswith("birefnet-") and a.kind == "hf_snapshot"]
        else:
            raise CatalogError(f"unknown model task: {task}")
        return [a.id for a in sorted(found, key=lambda a: (not a.default_variant, a.id))]

    def _artifact(self, task: str, variant: str | None) -> Artifact:
        ids = self.variants(task)
        if variant is None:
            return self.catalog.get(ids[0])
        for aid in ids:
            art = self.catalog.get(aid)
            if variant in (aid, art.repo_id) or f"{task}-{variant}" == aid or f"{task}-{variant}".lower() == aid.lower():
                return art
        raise CatalogError(f"no {task} model variant {variant!r}")

    # -- readiness (offline, never hashes) -------------------------------
    def resolve(self, task: str, variant: str | None = None) -> ModelBinding:
        art = self._artifact(task, variant)
        deps = []
        for dep_id in self.catalog.closure([art.id]):
            dep = self.catalog.get(dep_id)
            item = self.checker.check_artifact(dep)
            deps.append(Dependency(
                id=dep_id, state=readiness_state(item), detail=dependency_problem(self.checker, dep),
                note=item.get("verify_note"), path=dependency_path(self.checker, dep),
                files=tuple(p for f in dep.required_files() if (p := self.checker.file_path(dep, f)) is not None),
            ))
        return ModelBinding(task, art.id, tuple(deps))

    def snapshot(self) -> dict:
        """One readiness snapshot for Models, buttons, scheduler and API: {task: {variant id: binding json}}."""
        return {t: {v: self.resolve(t, v).to_json() for v in self.variants(t)} for t in TASKS}

    def refresh(self) -> None:
        """Re-read the saved verification record after another process changed it (Install/Repair)."""
        self.checker.hash_cache.reload()

    def verify(self, task: str, variant: str | None = None) -> ModelBinding:
        """Explicit Verify: hash the whole closure, save the record, return fresh readiness."""
        self.checker.verify(self.catalog.closure([self._artifact(task, variant).id]))
        return self.resolve(task, variant)

    # -- active-job protection -------------------------------------------
    def start_job(self, job_id: str, task: str, variant: str | None = None) -> ModelBinding:
        """Protect the files before dispatch; raises if not Verified or being changed. Release with finish_job."""
        binding = self.resolve(task, variant)
        closure = frozenset(self.catalog.closure([binding.artifact_id]))
        with self._lock:
            if job_id in self._jobs:
                raise ModelInUseError(f"job {job_id} already holds model files")
            if closure & self._changing:
                raise ModelInUseError(f"{binding.artifact_id} is being changed; retry when it finishes")
            lease = acquire_model_artifact(binding.artifact_id, roots=self.roots, catalog=self.catalog)
            self._jobs[job_id] = (closure, lease)
        return binding

    def finish_job(self, job_id: str) -> None:
        """Call only after the executor confirmed unloading or the worker exited."""
        with self._lock:
            held = self._jobs.pop(job_id, None)
        if held:
            held[1].close()

    def active_jobs(self, artifact_ids: list[str]) -> list[str]:
        want = set(artifact_ids)
        with self._lock:
            return sorted(j for j, (closure, _l) in self._jobs.items() if closure & want)

    @contextmanager
    def changing(self, artifact_ids: list[str]):
        """Install/Repair/Delete/root change: refused while a running job uses any of these files."""
        ids = set(artifact_ids)
        with self._lock:
            busy = sorted(j for j, (closure, _l) in self._jobs.items() if closure & ids)
            if busy:
                raise ModelInUseError(f"in use by running job(s): {', '.join(busy)}")
            if ids & self._changing:
                raise ModelInUseError("another change to these models is in progress")
            self._changing |= ids
        try:
            yield
        finally:
            with self._lock:
                self._changing -= ids
            self.refresh()
