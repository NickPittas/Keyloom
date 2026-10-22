"""Shared model provision manager: catalog/check/verify/download/repair/cancel.

One supervised local helper for installers, startup and Settings, emitting
**structured JSON events on stdout** (one NDJSON object per line) with all
human logs going to stderr. Existing CLI/Qt/install callers of
``scripts/setup_models.py`` keep their human output; this manager is the
machine surface behind the new verbs.

Event stream (stdout, NDJSON):
  {"event":"op-start","op":"download","targets":[...]}
  {"event":"status", ...}            # check: one per artifact (offline)
  {"event":"verify", ...}            # verify: one per artifact (hashes)
  {"event":"artifact-start","artifact":"gvm","bytes_total":6500000000}
  {"event":"progress","artifact":"gvm","unit":"files","files_done":1,
       "files_total":4,"path":"unet/model.safetensors"}  # HF snapshots
  {"event":"progress","artifact":"matanyone2","unit":"bytes","bytes":123,
       "bytes_total":...}  # streaming files
  {"event":"progress","artifact":"matanyone2","bytes":123,"bytes_total":...}
  {"event":"file-done","artifact":"gvm","path":"unet/config.json"}
  {"event":"artifact-done","artifact":"gvm","verified":true,...}
  {"event":"artifact-error","artifact":"gvm","error":"disk-space|hash-mismatch|
           download-failed|gated|not-installable|unresolved-revision","message":"..."}
  {"event":"cancelled","op":"download","artifact":"gvm"}
  {"event":"op-done","op":"download","ok":false,"summary":{...}}

Download properties (standalone-release-plan.md §3):
  * pinned ids only — every HF call passes the catalog revision;
  * download to revision-keyed staging, validate the complete closure and
    trusted hashes, then promote; failure/cancel leaves the current install live;
  * staging retains resumable HF/HTTP partials (HTTP resume only when supported);
  * progress + cooperative cancellation (in-process event AND a marker file
    so a separate ``cancel`` CLI op can stop a running download);
  * disk preflight charges the full missing-byte closure + 10% headroom per
    destination volume;
  * no network in check/verify/catalog ops.

stdlib only at import; huggingface_hub/urllib are imported inside transports
so tests can monkeypatch :meth:`_transport_*` without network.
"""
from __future__ import annotations

import contextlib
import hashlib
import importlib.util
import json
import os
import queue
import re
import shutil
import stat
import subprocess
import sys
import threading
import time
import uuid
import urllib.request
from pathlib import Path

from backend.model_catalog import Artifact, Catalog, FLAG_TARGET_MAP
from backend.model_lock import DestinationBusyError, DestinationLock
from backend.model_status import Roots, StatusChecker, _stat_identity, digest_file, file_digest

CANCEL_MARKER = "install_cancel.flag"
HEADROOM = 1.1  # 10% headroom, matching scripts/setup_models.check_disk_space
_OPERATION_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$")
_PUBLISHED_IDENTITY_FIELDS = ("dev", "ino", "mode", "size", "mtime_ns")


def _same_published_file(before: dict | None, after: dict | None) -> bool:
    """Compare the same inode across rename; ctime may change on publication."""
    return bool(before and after and all(before.get(key) == after.get(key)
                                         for key in _PUBLISHED_IDENTITY_FIELDS))


class Cancelled(Exception):
    """Cooperative cancellation reached a terminal state; nothing was promoted."""


class ArtifactError(Exception):
    def __init__(self, code: str, message: str):
        super().__init__(message)
        self.code = code


def _log(msg: str) -> None:
    sys.stderr.write(f"[model-provision] {msg}\n")
    sys.stderr.flush()


def _load_v2_downloader(package_root: Path | None = None):
    """Reuse the vendored CorridorKeyV2/scripts/download_weights.py (RVM MD5 gate)."""
    if package_root is None:
        from backend import v2_paths
        package_root = v2_paths.get_v2_package_root()
    path = package_root / "scripts" / "download_weights.py"
    spec = importlib.util.spec_from_file_location("_ck_v2_download_weights_prov", path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


class ProvisionManager:
    def __init__(self, catalog: Catalog, roots: Roots, emit=None, operation_id: str | None = None):
        self.catalog = catalog
        self.roots = roots
        self.checker = StatusChecker(catalog, roots)
        self.cancel_event = threading.Event()
        self.operation_id = uuid.uuid4().hex if operation_id is None else operation_id
        if not _OPERATION_ID_RE.fullmatch(self.operation_id):
            raise ValueError("operation_id must match [A-Za-z0-9][A-Za-z0-9_-]{0,127}")
        self._terminal_count = 0
        # Capture stdout now: library redirects during downloads must never move
        # the JSON event stream to stderr.
        self._emit_stream = sys.stdout
        self._emit_fn = emit or self._default_emit

    def _default_emit(self, event: dict) -> None:
        self._emit_stream.write(json.dumps(event, sort_keys=True) + "\n")
        self._emit_stream.flush()

    @staticmethod
    def _revision_tag(art: Artifact) -> str:
        return hashlib.sha256((art.revision or "").encode()).hexdigest()[:12]

    def _hf_staging_dir(self, art: Artifact, dest: Path) -> Path:
        return dest.parent / f".{dest.name}.{art.id}.{self._revision_tag(art)}.staging"

    def _hf_cache_staging_dir(self, art: Artifact) -> Path:
        key = hashlib.sha256(f"{art.repo_id}@{art.revision}/{art.hf_filename}".encode()).hexdigest()[:16]
        return self.roots.hf_cache / "model-provision-staging" / f"{art.id}.{key}"

    def _rvm_staging_dir(self, art: Artifact) -> Path:
        return self.roots.v2_models_dir / f".{art.id}.{self._revision_tag(art)}.staging"

    def _http_staging_dir(self, art: Artifact, dest: Path) -> Path:
        return dest.parent / f".{dest.name}.{art.id}.{self._revision_tag(art)}.staging"

    @staticmethod
    def _is_link_or_reparse(path: Path) -> bool:
        try:
            st = path.lstat()
        except FileNotFoundError:
            return False
        attrs = getattr(st, "st_file_attributes", 0)
        reparse = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0)
        return stat.S_ISLNK(st.st_mode) or bool(attrs & reparse)

    def _assert_safe_path(self, path: Path, root: Path, *, allow_leaf_link: bool = False) -> None:
        """Keep writes below the chosen root and refuse links in managed paths."""
        base = Path(os.path.abspath(root))
        target = Path(os.path.abspath(path))
        if target != base and base not in target.parents:
            raise ArtifactError("unsafe-path", f"path escapes its root: {path}")
        # A symlink above the selected root redirects the whole managed tree too.
        for ancestor in reversed((base, *base.parents)):
            if self._is_link_or_reparse(ancestor):
                raise ArtifactError("unsafe-path", f"managed path ancestor is a link or reparse point: {ancestor}")
        if self._is_link_or_reparse(base):
            raise ArtifactError("unsafe-path", f"managed root is a link or reparse point: {base}")
        current = base
        parts = target.relative_to(base).parts if target != base else ()
        for index, part in enumerate(parts):
            current = current / part
            if self._is_link_or_reparse(current) and not (allow_leaf_link and index == len(parts) - 1):
                raise ArtifactError("unsafe-path", f"managed path contains a link or reparse point: {current}")
        try:
            real_base = base.resolve(strict=False)
            real_target = target.resolve(strict=False)
        except OSError as e:
            raise ArtifactError("unsafe-path", f"cannot resolve managed path {path}: {e}") from e
        if real_target != real_base and real_base not in real_target.parents:
            raise ArtifactError("unsafe-path", f"resolved path escapes its root: {path}")

    def _assert_safe_tree(self, path: Path, root: Path) -> None:
        self._assert_safe_path(path, root)
        if not path.exists():
            return
        if not path.is_dir():
            raise ArtifactError("unsafe-path", f"expected a directory: {path}")
        for entry in path.rglob("*"):
            if self._is_link_or_reparse(entry):
                raise ArtifactError("unsafe-path", f"managed tree contains a link or reparse point: {entry}")

    def _remove_path(self, path: Path, root: Path, *, allow_leaf_link: bool = False) -> None:
        self._assert_safe_path(path, root, allow_leaf_link=allow_leaf_link)
        if self._is_link_or_reparse(path):
            path.unlink(missing_ok=True)
        elif path.is_dir():
            self._assert_safe_tree(path, root)
            shutil.rmtree(path)
        else:
            path.unlink(missing_ok=True)

    @staticmethod
    def _fsync_dir(path: Path) -> None:
        if os.name == "nt":
            return
        fd = os.open(path, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
        try:
            os.fsync(fd)
        finally:
            os.close(fd)

    def _journal_path(self, target: Path) -> Path:
        return target.with_name(f".{target.name}.corridorkey-publish.json")

    def _write_journal(self, journal: dict, root: Path, *, allow_leaf_link: bool) -> None:
        target = Path(journal["target"])
        path = self._journal_path(target)
        self._assert_safe_path(target, root, allow_leaf_link=allow_leaf_link)
        self._assert_safe_path(path.parent, root)
        self._assert_safe_path(path, root)
        path.parent.mkdir(parents=True, exist_ok=True)
        temporary = path.with_name(f".{path.name}.{uuid.uuid4().hex}.tmp")
        self._assert_safe_path(temporary, root)
        try:
            with open(temporary, "x", encoding="utf-8") as stream:
                json.dump(journal, stream, sort_keys=True)
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(temporary, path)
            self._fsync_dir(path.parent)
        finally:
            temporary.unlink(missing_ok=True)

    def _recover_artifact_publication(self, art: Artifact, dest: Path, root: Path) -> None:
        if art.kind in ("local_package", "local_code"):
            return
        target, _directory = self._publication_target(art, dest)
        journal_path = self._journal_path(target)
        self._assert_safe_path(journal_path, root)
        if not journal_path.exists():
            return
        if not journal_path.is_file():
            raise ArtifactError("publication-recovery", f"invalid publication journal: {journal_path}")
        try:
            journal = json.loads(journal_path.read_text(encoding="utf-8"))
            phase = journal["phase"]
            recorded_target = Path(journal["target"])
            source = Path(journal["source"])
            backup = Path(journal["backup"]) if journal.get("backup") else None
            had_target = bool(journal["had_target"])
        except (OSError, ValueError, KeyError, TypeError) as exc:
            raise ArtifactError("publication-recovery", f"cannot read publication journal {journal_path}: {exc}") from exc
        if recorded_target != target or phase not in {"prepared", "old-moved", "new-moved", "committed"}:
            raise ArtifactError("publication-recovery", f"publication journal does not match {target}")
        allow_leaf_link = art.kind == "hf_cache_file"
        self._assert_safe_path(source, root)
        self._assert_safe_path(recorded_target, root, allow_leaf_link=allow_leaf_link)
        if backup is not None:
            self._assert_safe_path(backup, root, allow_leaf_link=allow_leaf_link)
        backup_exists = backup is not None and os.path.lexists(backup)
        target_exists = os.path.lexists(target)
        source_exists = os.path.lexists(source)

        if phase == "committed" and target_exists:
            if backup_exists:
                self._remove_path(backup, root, allow_leaf_link=allow_leaf_link)
        elif backup_exists:
            if target_exists:
                self._remove_path(target, root, allow_leaf_link=allow_leaf_link)
            backup.replace(target)
            self._fsync_dir(target.parent)
        elif phase in {"old-moved", "new-moved"} and not had_target and not source_exists and target_exists:
            self._remove_path(target, root, allow_leaf_link=allow_leaf_link)
        elif had_target and (
            (phase == "prepared" and not target_exists)
            or (phase != "prepared" and not backup_exists)
        ):
            raise ArtifactError("publication-recovery", f"old asset backup is missing for {target}")
        elif not had_target and phase in {"old-moved", "new-moved"} and not target_exists:
            # Publication never produced a usable destination; the staged source
            # is retained and the next attempt can safely fetch or resume it.
            pass
        self._remove_path(journal_path, root)
        self._fsync_dir(journal_path.parent)

    def _publication_target(self, art: Artifact, dest: Path) -> tuple[Path, bool]:
        directory = art.kind in ("hf_files", "hf_snapshot")
        if directory:
            top = self._artifact_top_dir(art)
            return (dest / top if top else dest), True
        files = art.download_files()
        if len(files) != 1:
            raise ArtifactError("download-failed", f"{art.id} transport must select one file")
        return dest / files[0].layout_path, False

    def _commit_publication(self, journal: dict, root: Path, *, allow_leaf_link: bool) -> None:
        journal["phase"] = "committed"
        self._write_journal(journal, root, allow_leaf_link=allow_leaf_link)
        backup = Path(journal["backup"]) if journal.get("backup") else None
        if backup is not None and os.path.lexists(backup):
            with contextlib.suppress(OSError, ArtifactError):
                self._remove_path(backup, root, allow_leaf_link=allow_leaf_link)
        with contextlib.suppress(OSError, ArtifactError):
            self._remove_path(self._journal_path(Path(journal["target"])), root)
            self._fsync_dir(Path(journal["target"]).parent)

    def _rollback_publication(self, journal: dict, root: Path, *, allow_leaf_link: bool) -> None:
        journal["phase"] = "new-moved" if journal.get("phase") in {"new-moved", "committed"} else journal.get("phase")
        self._write_journal(journal, root, allow_leaf_link=allow_leaf_link)
        self._recover_artifact_publication(
            self.catalog.get(journal["artifact"]), Path(journal["dest"]), root
        )

    @contextlib.contextmanager
    def _artifact_lock(self, art: Artifact, dest: Path):
        """Exclusive canonical-destination lock, shared with loaded-model leases."""
        root = self._asset_root(art) if art.kind not in ("local_package", "local_code") else dest
        self._assert_safe_path(dest, root)
        try:
            with DestinationLock(dest):
                self._recover_artifact_publication(art, dest, root)
                yield
        except DestinationBusyError as e:
            raise ArtifactError("locked", f"another process owns destination for {art.id}: {dest}") from e

    def _asset_root(self, art: Artifact) -> Path:
        if art.kind == "hf_cache_file":
            return self.roots.hf_cache
        return {
            "v2_models_dir": self.roots.v2_models_dir,
            "data_dir": self.roots.data_dir,
            "v2_package_root": self.roots.v2_package_root,
        }[art.layout_root]

    # -- events / cancellation -------------------------------------------
    def emit(self, event: dict) -> None:
        event.setdefault("operation_id", self.operation_id)
        if event.get("event") == "op-done":
            self._terminal_count += 1
        self._emit_fn(event)

    def _cancelled(self) -> bool:
        """In-process event or this operation's cross-process marker."""
        if self.cancel_event.is_set():
            return True
        marker = self._cancel_marker_path(self.operation_id)
        self._assert_safe_path(marker, self.roots.cache_dir)
        if not marker.is_file():
            return False
        try:
            return json.loads(marker.read_text(encoding="utf-8")).get("operation_id") == self.operation_id
        except (OSError, ValueError, AttributeError):
            raise ArtifactError("cancel-marker", f"invalid cancel marker for {self.operation_id}")

    def _cancel_marker_path(self, operation_id: str) -> Path:
        return self.roots.cache_dir / "model-cancel" / f"{operation_id}.cancel"

    def _operation_path(self, operation_id: str) -> Path:
        return self.roots.cache_dir / "model-operations" / f"{operation_id}.active"

    def _begin_operation(self) -> tuple[DestinationLock, Path]:
        path = self._operation_path(self.operation_id)
        self._assert_safe_path(path, self.roots.cache_dir)
        path.parent.mkdir(parents=True, exist_ok=True)
        lock = DestinationLock(path)
        try:
            lock.acquire()
            if os.path.lexists(path):
                if not path.is_file() or self._is_link_or_reparse(path):
                    raise ArtifactError("operation-owner", f"invalid operation owner record: {path}")
                self._remove_path(path, self.roots.cache_dir)
            self._clear_cancel_marker()
            with open(path, "x", encoding="utf-8") as stream:
                json.dump({"operation_id": self.operation_id, "pid": os.getpid()}, stream)
                stream.flush()
                os.fsync(stream.fileno())
            self._fsync_dir(path.parent)
            return lock, path
        except DestinationBusyError as exc:
            lock.close()
            raise ArtifactError("operation-active", f"operation ID is already active: {self.operation_id}") from exc
        except Exception:
            lock.close()
            raise

    def _end_operation(self, owner: tuple[DestinationLock, Path]) -> None:
        lock, path = owner
        with contextlib.suppress(OSError, ArtifactError):
            self._remove_path(path, self.roots.cache_dir)
            self._fsync_dir(path.parent)
        lock.close()

    def _check_cancel(self, artifact: str | None = None) -> None:
        if self._cancelled():
            raise Cancelled()

    def cancel_running_download(self, operation_id: str | None = None) -> dict:
        """Cancel only an active download holding the requested operation lease."""
        request_id = uuid.uuid4().hex
        if not isinstance(operation_id, str) or not _OPERATION_ID_RE.fullmatch(operation_id):
            result = {"event": "op-done", "op": "cancel", "operation_id": operation_id,
                      "request_id": request_id, "ok": False,
                      "error": "invalid-operation-id",
                      "message": "pass --operation-id matching [A-Za-z0-9][A-Za-z0-9_-]{0,127}"}
            self.emit(result)
            return result
        self.emit({"event": "op-start", "op": "cancel", "operation_id": operation_id,
                   "request_id": request_id})
        try:
            owner = self._operation_path(operation_id)
            self._assert_safe_path(owner, self.roots.cache_dir)
            if not owner.is_file():
                return self._cancel_not_active(operation_id, request_id)
            if json.loads(owner.read_text(encoding="utf-8")).get("operation_id") != operation_id:
                return self._cancel_not_active(operation_id, request_id)
        except (OSError, ValueError, AttributeError):
            return self._cancel_not_active(operation_id, request_id)
        except ArtifactError as e:
            result = {"event": "op-done", "op": "cancel", "operation_id": operation_id,
                      "request_id": request_id, "ok": False,
                      "error": e.code, "message": str(e)}
            self.emit(result)
            return result
        probe = DestinationLock(owner, shared=True)
        try:
            probe.acquire()
        except DestinationBusyError:
            pass  # The exact operation ID is held by a live kernel lock owner.
        except OSError as e:
            result = {"event": "op-done", "op": "cancel", "operation_id": operation_id,
                      "request_id": request_id, "ok": False,
                      "error": "operation-owner", "message": str(e)}
            self.emit(result)
            return result
        else:
            probe.close()
            return self._cancel_not_active(operation_id, request_id)

        marker = self._cancel_marker_path(operation_id)
        try:
            self._assert_safe_path(marker, self.roots.cache_dir)
            marker.parent.mkdir(parents=True, exist_ok=True)
            if os.path.lexists(marker):
                if self._is_link_or_reparse(marker):
                    raise ArtifactError("unsafe-path", f"cancel marker is a link: {marker}")
                try:
                    if json.loads(marker.read_text(encoding="utf-8")).get("operation_id") != operation_id:
                        raise ArtifactError("cancel-marker", f"cancel marker ID mismatch: {marker}")
                except (OSError, ValueError, AttributeError) as exc:
                    raise ArtifactError("cancel-marker", f"invalid cancel marker: {marker}") from exc
            else:
                # Publish complete-or-absent: readers never see a half-written marker.
                temporary = marker.with_name(f".{marker.name}.{uuid.uuid4().hex}.tmp")
                try:
                    with open(temporary, "x", encoding="utf-8") as stream:
                        json.dump({"operation_id": operation_id}, stream)
                        stream.flush()
                        os.fsync(stream.fileno())
                    try:
                        os.link(temporary, marker)  # fails if a concurrent cancel won
                    except FileExistsError:
                        if json.loads(marker.read_text(encoding="utf-8")).get("operation_id") != operation_id:
                            raise ArtifactError("cancel-marker", f"cancel marker ID mismatch: {marker}")
                    self._fsync_dir(marker.parent)
                except ValueError as exc:
                    raise ArtifactError("cancel-marker", f"invalid cancel marker: {marker}") from exc
                finally:
                    temporary.unlink(missing_ok=True)
            if operation_id == self.operation_id:
                self.cancel_event.set()
        except (OSError, ArtifactError) as e:
            _log(f"cannot write cancel marker: {e}")
            result = {"event": "op-done", "op": "cancel", "ok": False,
                      "operation_id": operation_id, "request_id": request_id,
                      "error": getattr(e, "code", "cancel-marker"), "message": str(e)}
            self.emit(result)
            return result
        self.emit({"event": "cancel-requested", "op": "cancel", "operation_id": operation_id,
                   "request_id": request_id})
        result = {"event": "op-done", "op": "cancel", "operation_id": operation_id,
                  "request_id": request_id, "ok": True, "requested": True}
        self.emit(result)
        return result

    def _cancel_not_active(self, operation_id: str, request_id: str) -> dict:
        result = {"event": "op-done", "op": "cancel", "operation_id": operation_id,
                  "request_id": request_id, "ok": False, "error": "not-active",
                  "message": f"no active download owns operation ID {operation_id}"}
        self.emit(result)
        return result

    def _clear_cancel_marker(self) -> None:
        marker = self._cancel_marker_path(self.operation_id)
        with contextlib.suppress(OSError, ArtifactError):
            self._remove_path(marker, self.roots.cache_dir)

    # -- ops --------------------------------------------------------------
    def op_catalog(self) -> dict:
        art_json = []
        for art in self.catalog.artifacts.values():
            art_json.append({
                "artifact": art.id, "label": art.label, "kind": art.kind,
                "capability": art.capability, "repo_id": art.repo_id,
                "revision": art.revision, "revision_status": art.revision_status,
                "gated": art.gated, "required_profile": art.required_profile,
                "default_variant": art.default_variant, "depends": list(art.depends),
                "layout_root": art.layout_root, "layout_rel": art.layout_rel,
                "url": art.url, "hf_filename": art.hf_filename,
                "files_required": len(art.required_files()),
                "files_download": len(art.download_files()),
                "bytes_required": art.trusted_bytes(),
                "bytes_download": sum(f.size for f in art.download_files()
                                      if f.size is not None),
                "size_bytes_estimate": art.size_bytes_estimate,
                "size_human": art.size_human,
                "files": [f.to_json() for f in art.files],
                "path_remap": art.path_remap,
                "install_hint": art.install_hint,
                "pip_spec": art.pip_spec,
                "probe_imports": list(art.probe_imports),
                "provenance": art.provenance, "notes": list(art.notes),
            })
        self.emit({"event": "catalog", "catalog_version": self.catalog.version,
                   "updated": self.catalog.updated, "meta": self.catalog.meta,
                   "artifacts": art_json})
        result = {"event": "op-done", "op": "catalog", "ok": True,
                  "artifacts": len(art_json)}
        self.emit(result)
        return result

    def op_check(self, targets: list[str] | None = None) -> dict:
        ids = self._resolve(targets)
        status = self.checker.status(ids)
        for item in status["artifacts"]:
            self.emit({"event": "status", **item})
        ok = (all(item["ready"] for item in status["artifacts"]) if targets
              else status["summary"]["ready_required"])
        self.emit({"event": "op-done", "op": "check", "ok": ok,
                   "summary": status["summary"]})
        return status

    def op_verify(self, targets: list[str] | None = None) -> dict:
        results = []
        for aid in self._resolve(targets):
            art = self.catalog.get(aid)
            dest = self.checker.artifact_dir(art) or self.roots.v2_package_root
            try:
                with self._artifact_lock(art, dest):
                    result = self.checker.verify_artifact(art)
                    if not self.checker.hash_cache.save():
                        self.checker.hash_cache.reload()
                        result["verified"] = False
                        result["problems"].append("could not persist verification cache")
            except ArtifactError as e:
                result = {"artifact": aid, "state": "locked", "verified": False,
                          "files_verified": 0, "files_required": len(art.required_files()),
                          "files_unhashed": [], "problems": [str(e)]}
            results.append(result)
        for r in results:
            self.emit({"event": "verify", **r})
        ok = all(r["verified"] for r in results)
        self.emit({"event": "op-done", "op": "verify", "ok": ok,
                   "summary": {"verified": sum(1 for r in results if r["verified"]),
                               "total": len(results)}})
        return {"ok": ok, "results": results}

    def op_download(self, targets: list[str], repair: bool = False) -> dict:
        op = "repair" if repair else "download"
        if not targets:
            result = {"ok": False, "failed": [], "cancelled": False,
                      "error": "target-required"}
            self.emit({"event": "op-done", "op": op, "ok": False,
                       "error": "target-required",
                       "message": "pass --target or --profile to select model assets"})
            return result
        try:
            ids = self._resolve(targets)
        except Exception as e:
            self.emit({"event": "op-done", "op": op, "ok": False,
                       "error": "invalid-target", "message": str(e)})
            return {"ok": False, "failed": [], "cancelled": False,
                    "error": "invalid-target"}
        try:
            owner = self._begin_operation()
        except (ArtifactError, OSError) as exc:
            result = {"ok": False, "failed": [], "cancelled": False,
                      "error": getattr(exc, "code", "operation-owner")}
            self.emit({"event": "op-done", "op": op, "ok": False,
                       "error": result["error"], "message": str(exc)})
            return result
        failed: list[str] = []
        cancelled = False
        self.emit({"event": "op-start", "op": op, "targets": ids})
        try:
            for aid in ids:
                try:
                    self._check_cancel(aid)
                    ok = self._install_artifact(self.catalog.get(aid), repair=repair)
                    if not ok:
                        failed.append(aid)
                except Cancelled:
                    cancelled = True
                    self.emit({"event": "cancelled", "op": op, "artifact": aid})
                    break
                except ArtifactError as e:
                    failed.append(aid)
                    self.emit({"event": "artifact-error", "artifact": aid,
                               "error": e.code, "message": str(e)})
                except Exception as e:
                    failed.append(aid)
                    _log(f"{op} {aid} failed: {e}")
                    self.emit({"event": "artifact-error", "artifact": aid,
                               "error": "download-failed", "message": str(e)})
            status = self.checker.status(ids)
        finally:
            self._clear_cancel_marker()
            self.cancel_event.clear()
            self._end_operation(owner)
        self.emit({"event": "op-done", "op": op,
                   "ok": not failed and not cancelled, "cancelled": cancelled,
                   "summary": {"installed": status["summary"]["installed"],
                               "failed": failed,
                               "verified": sum(1 for it in status["artifacts"] if it["verified"])}})
        return {"ok": not failed and not cancelled, "failed": failed, "cancelled": cancelled}

    def op_repair(self, targets: list[str]) -> dict:
        return self.op_download(targets, repair=True)

    def _resolve(self, targets: list[str] | None, expand: bool = True) -> list[str]:
        if not targets:
            return sorted(self.catalog.artifacts)
        ids: list[str] = []
        for t in targets:
            ids.extend(FLAG_TARGET_MAP.get(t, [t]))
        return self.catalog.closure(ids) if expand else sorted(set(ids))

    # -- download internals ------------------------------------------------
    def _install_artifact(self, art: Artifact, repair: bool) -> bool:
        if art.kind in ("local_package", "local_code"):
            with self._artifact_lock(art, self.roots.v2_package_root):
                return self._install_artifact_locked(art, repair)
        dest = self.checker.artifact_dir(art)
        if dest is None:
            raise ArtifactError("not-installable", f"{art.id} has no writable asset root")
        root = self._asset_root(art)
        self._assert_safe_path(dest, root, allow_leaf_link=(art.kind == "hf_cache_file"))
        if art.kind in ("hf_files", "hf_snapshot"):
            self._assert_safe_path(self._hf_staging_dir(art, dest), root)
        elif art.kind == "http_file":
            self._assert_safe_path(self._http_staging_dir(art, dest), root)
        with self._artifact_lock(art, dest):
            return self._install_artifact_locked(art, repair)

    def _install_artifact_locked(self, art: Artifact, repair: bool) -> bool:
        if art.kind in ("local_package", "local_code"):
            st = self.checker.check_artifact(art)
            if st["ready"]:
                self.emit({"event": "artifact-done", "artifact": art.id, "verified": st["verified"],
                           "path": st["path"], "note": st.get("verify_note", "")})
                return True
            raise ArtifactError("not-installable",
                                f"{art.id} is a {art.kind}; {art.install_hint or 'see documentation'}")

        if art.kind.startswith("hf_") and (
            art.revision_status != "pinned" or not isinstance(art.revision, str)
            or len(art.revision) != 40
            or any(c not in "0123456789abcdef" for c in art.revision)
        ):
            raise ArtifactError("unresolved-revision",
                                f"{art.id} has no immutable Hugging Face revision; refresh metadata first")
        if art.kind == "http_file" and (art.revision_status != "pinned" or not art.revision):
            raise ArtifactError("unresolved-revision",
                                f"{art.id} has no pinned source identity; refresh metadata first")

        if art.gated in ("auto", "manual") and not (os.environ.get("HF_TOKEN")
                                                    or os.environ.get("HUGGING_FACE_HUB_TOKEN")):
            raise ArtifactError("gated", f"{art.repo_id} requires license acceptance/token "
                                         "(gated={art.gated}); credentials stay out of logs")

        dest = self.checker.artifact_dir(art)
        if dest is None:
            raise ArtifactError("not-installable", f"{art.id} has no writable asset root")
        root = self._asset_root(art)
        allow_leaf_link = art.kind == "hf_cache_file"
        self._assert_safe_path(dest, root, allow_leaf_link=allow_leaf_link)
        for f in art.required_files():
            path = self.checker.file_path(art, f)
            if path is not None:
                self._assert_safe_path(path, root, allow_leaf_link=allow_leaf_link)
        if art.kind in ("hf_files", "hf_snapshot"):
            self._assert_safe_tree(self._hf_staging_dir(art, dest), root)
        elif art.kind == "hf_cache_file":
            self._assert_safe_tree(self._hf_cache_staging_dir(art), root)
        elif art.md5_b64:
            self._assert_safe_tree(self._rvm_staging_dir(art), root)
        else:
            self._assert_safe_tree(self._http_staging_dir(art, dest), root)

        before = self.checker.check_artifact(art)
        if before["present"] and not repair:
            self.checker.verify_artifact(art)
            if not self.checker.hash_cache.save():
                self.checker.hash_cache.reload()
                raise ArtifactError("verification-cache", "could not persist verification cache")
            before = self.checker.check_artifact(art)
            if before["ready"]:
                self.emit({"event": "artifact-done", "artifact": art.id,
                           "verified": True, "ready": True, "path": before["path"],
                           "note": "already installed and verified"})
                return True
        needed_bytes = self._missing_bytes(art)
        self.emit({"event": "artifact-start", "artifact": art.id,
                   "bytes_total": needed_bytes,
                   "files_total": len(art.download_files())})
        dest.parent.mkdir(parents=True, exist_ok=True)
        self._preflight_disk(art, dest, needed_bytes)
        stage_root: Path | None = None
        cleanup_root: Path | None = None
        publication = None
        try:
            with contextlib.redirect_stdout(sys.stderr):
                if art.kind == "hf_cache_file":
                    files_root, cleanup_root = self._transport_hf_cache_file(art)
                    stage_root = files_root
                elif art.kind in ("hf_files", "hf_snapshot"):
                    files_root, cleanup_root = self._transport_hf_snapshot(art, dest)
                    stage_root = files_root
                    self._apply_remap_dirs(art, files_root)
                elif art.kind == "http_file":
                    files_root, cleanup_root = self._transport_http(art, dest)
                    stage_root = files_root
                else:
                    raise ArtifactError("download-failed", f"unhandled kind {art.kind}")

            self._assert_safe_tree(cleanup_root, root)
            identities: dict[str, dict] = {}
            hashes = self._validate_files(art, stage_root, identities)
            published_paths = self._published_paths(art, dest, stage_root)
            files_done = 0
            for f in art.download_files():
                if (stage_root / f.layout_path).is_file():
                    files_done += 1
                    self.emit({"event": "progress", "artifact": art.id,
                               "unit": "files",
                               "files_done": files_done,
                               "files_total": len(art.download_files()),
                               "path": f.path})
                    self.emit({"event": "file-done", "artifact": art.id, "path": f.path})
            source = stage_root
            target = dest
            directory = art.kind in ("hf_files", "hf_snapshot")
            if directory:
                top = self._artifact_top_dir(art)
                if top:
                    self._move_upstream_notices(art, stage_root, top)
                    source, target = stage_root / top, dest / top
                else:
                    source, target = stage_root, dest
            else:
                if len(art.download_files()) != 1:
                    raise ArtifactError("download-failed", f"{art.id} transport must select one file")
                f = art.download_files()[0]
                source, target = stage_root / f.layout_path, dest / f.layout_path
            self._check_cancel(art.id)  # final cancellation point before publication
            publication = self._publish(source, target, root,
                                        allow_leaf_link=allow_leaf_link,
                                        directory=directory, art=art, dest=dest)
            self._check_cancel(art.id)

            for f in art.download_files():
                published = published_paths.get(f.layout_path)
                if published is None or not published.is_file():
                    if f.required:
                        raise ArtifactError("missing-files", f"published file missing: {f.layout_path}")
                    continue
                if not _same_published_file(identities.get(f.layout_path),
                                            _stat_identity(published)):
                    raise ArtifactError("file-changed", f"{f.layout_path} changed during publication")

            if not self._record_digests(art, hashes):
                self.checker.hash_cache.reload()
                raise ArtifactError("verification-cache", "could not persist verification cache")
            after = self.checker.check_artifact(art)
            if not after["present"]:
                raise ArtifactError("missing-files",
                                    f"closure incomplete after publication: {after['problems'][:5]}")
            if art.has_trusted_hashes() and not after["ready"]:
                raise ArtifactError("verification-failed",
                                    "downloaded files did not reach verified Ready state")
            self._check_cancel(art.id)
            self._commit_publication(publication, root, allow_leaf_link=allow_leaf_link)
            publication = None
            with contextlib.suppress(OSError, ArtifactError):
                self._remove_path(cleanup_root, root)
        except Exception:
            if publication is not None:
                self.checker.hash_cache.reload()
                self._rollback_publication(publication, root,
                                           allow_leaf_link=allow_leaf_link)
            raise

        after = self.checker.check_artifact(art)
        verified = after["verified"]
        files_verified = sum(1 for f in art.required_files() if file_digest(art, f) is not None)
        files_unhashed = [f.layout_path for f in art.required_files() if file_digest(art, f) is None]
        self.emit({"event": "artifact-done", "artifact": art.id,
                   "verified": verified,
                   "ready": after["ready"],
                   "files_verified": files_verified if verified else 0,
                   "files_required": len(art.required_files()),
                   "path": after["path"],
                   "note": "" if verified else
                   f"installed; {len(files_unhashed)} file(s) lack trusted digests"})
        return True

    def _missing_bytes(self, art: Artifact) -> int:
        """Preflight only the bytes absent from this artifact's pinned staging area."""
        if art.kind in ("hf_files", "hf_snapshot"):
            dest = self.checker.artifact_dir(art)
            root = self._hf_staging_dir(art, dest) / "payload"
            return self._remaining_bytes(art, root)
        if art.kind == "hf_cache_file":
            return self._remaining_bytes(art, self._hf_cache_staging_dir(art) / "payload")
        if art.kind == "http_file":
            if art.md5_b64:
                root = self._rvm_staging_dir(art) / (art.layout_rel or "")
                f = art.required_files()[0]
                staged = root / f.layout_path
                dg = file_digest(art, f)
                if staged.is_file() and dg is not None:
                    before = _stat_identity(staged)
                    valid = digest_file(staged, dg) == dg.expected
                    if before is not None and before == _stat_identity(staged) and valid:
                        return 0
                return art.trusted_bytes() or art.size_bytes_estimate or 0
            else:
                dest = self.checker.artifact_dir(art)
                root = self._http_staging_dir(art, dest)
                return self._remaining_bytes(art, root, parts=True)
        size = art.trusted_bytes()
        if size is not None:
            return size
        if art.size_bytes_estimate is not None:
            return art.size_bytes_estimate
        raise ArtifactError("size-unknown",
                            f"{art.id} has no complete upstream size metadata; refresh the catalog")

    def _remaining_bytes(self, art: Artifact, root: Path, parts: bool = False) -> int:
        selected = art.download_files()
        if not selected:
            return art.size_bytes_estimate or 0
        estimate = art.size_bytes_estimate
        total = 0
        for f in selected:
            if f.size is None:
                if estimate is None:
                    raise ArtifactError("size-unknown",
                                        f"{art.id} has incomplete upstream size metadata; refresh the catalog")
                return estimate
            candidates = [root / f.layout_path, root / f.path]
            if parts:
                candidates.extend((p.with_name(p.name + ".part") for p in tuple(candidates)))
            current = 0
            for path in dict.fromkeys(candidates):
                if not path.is_file():
                    continue
                size = path.stat().st_size
                if parts and path.name.endswith(".part") and 0 <= size < f.size:
                    current = size
                    break
                if size != f.size:
                    continue
                before = _stat_identity(path)
                dg = file_digest(art, f)
                valid = dg is None or digest_file(path, dg) == dg.expected
                if before is not None and before == _stat_identity(path) and valid:
                    current = size
                    break
            total += f.size - current if 0 <= current <= f.size else f.size
        return total

    def _record_digests(self, art: Artifact, staged_digests: dict[str, str]) -> bool:
        for f in art.required_files():
            path = self.checker.file_path(art, f)
            dg = file_digest(art, f)
            if path is not None and dg is not None:
                before = _stat_identity(path)
                if before is None:
                    return False
                value = digest_file(path, dg)
                after = _stat_identity(path)
                if (before != after or value != dg.expected
                        or value != staged_digests.get(f.layout_path)):
                    return False
                self.checker.hash_cache.record(path, dg, True, value, art.id, identity=after)
        return self.checker.hash_cache.save()

    def _validate_files(self, art: Artifact, root: Path,
                        identities: dict[str, dict] | None = None) -> dict[str, str]:
        """Validate a staged closure before it can replace any published asset."""
        self._assert_safe_tree(root, self._asset_root(art))
        digests: dict[str, str] = {}
        for f in art.download_files():
            path = root / f.layout_path
            if not path.is_file():
                if f.required:
                    if path.is_dir():
                        self._remove_path(path, self._asset_root(art))
                    raise ArtifactError("missing-files", f"required file not downloaded: {f.path}")
                continue
            before = _stat_identity(path)
            if f.size is not None and path.stat().st_size != f.size:
                actual_size = path.stat().st_size
                self._remove_path(path, self._asset_root(art))
                raise ArtifactError("size-mismatch",
                                    f"{f.layout_path}: expected {f.size}, got {actual_size}")
            dg = file_digest(art, f)
            actual = None
            if dg is not None:
                actual = digest_file(path, dg)
            after = _stat_identity(path)
            if before is None or before != after:
                raise ArtifactError("file-changed", f"{f.layout_path} changed during validation")
            if dg is not None:
                if actual != dg.expected:
                    self._remove_path(path, self._asset_root(art))
                    raise ArtifactError("hash-mismatch",
                                        f"{f.layout_path} failed trusted {dg.algo} check")
                digests[f.layout_path] = actual
            if identities is not None:
                identities[f.layout_path] = after
        return digests

    def _published_paths(self, art: Artifact, dest: Path, stage_root: Path) -> dict[str, Path]:
        top = self._artifact_top_dir(art) if art.kind in ("hf_files", "hf_snapshot") else ""
        published = {}
        for f in art.download_files():
            rel = Path(f.layout_path)
            if top and rel.parts[0] != top:
                published[f.layout_path] = dest / top / ".upstream" / rel
            else:
                published[f.layout_path] = dest / rel
        return published

    def _move_upstream_notices(self, art: Artifact, stage_root: Path, top: str) -> None:
        own = stage_root / top
        for f in art.download_files():
            rel = Path(f.layout_path)
            if f.required or rel.parts[0] == top:
                continue
            src = stage_root / rel
            if not src.is_file():
                continue
            target = own / ".upstream" / rel
            self._assert_safe_path(src, self._asset_root(art))
            self._assert_safe_path(target, self._asset_root(art))
            target.parent.mkdir(parents=True, exist_ok=True)
            src.replace(target)

    def _publish(self, source: Path, target: Path, root: Path, *,
                 allow_leaf_link: bool, directory: bool, art: Artifact,
                 dest: Path) -> dict:
        """Journal and replace one owned output while retaining rollback bytes."""
        self._assert_safe_path(source, root)
        if directory:
            self._assert_safe_tree(source, root)
        self._assert_safe_path(target.parent, root)
        target.parent.mkdir(parents=True, exist_ok=True)
        self._assert_safe_path(target, root, allow_leaf_link=allow_leaf_link)
        had_target = os.path.lexists(target)
        backup = None
        if had_target:
            if target.is_dir() and not self._is_link_or_reparse(target):
                self._assert_safe_tree(target, root)
            else:
                self._assert_safe_path(target, root, allow_leaf_link=allow_leaf_link)
            backup = target.with_name(f".{target.name}.{uuid.uuid4().hex}.previous")
            self._assert_safe_path(backup, root)
        journal = {
            "artifact": art.id,
            "dest": str(dest),
            "target": str(target),
            "source": str(source),
            "backup": str(backup) if backup is not None else None,
            "had_target": had_target,
            "directory": directory,
            "phase": "prepared",
        }
        self._write_journal(journal, root, allow_leaf_link=allow_leaf_link)
        try:
            if had_target:
                target.replace(backup)
                self._fsync_dir(target.parent)
            journal["phase"] = "old-moved"
            self._write_journal(journal, root, allow_leaf_link=allow_leaf_link)
            source.replace(target)
            self._fsync_dir(target.parent)
            journal["phase"] = "new-moved"
            self._write_journal(journal, root, allow_leaf_link=allow_leaf_link)
            if directory:
                self._assert_safe_tree(target, root)
            else:
                self._assert_safe_path(target, root)
                if not target.is_file():
                    raise ArtifactError("missing-files", f"published file is not regular: {target}")
        except Exception:
            self._recover_artifact_publication(art, dest, root)
            raise
        return journal

    def _preflight_disk(self, art: Artifact, dest: Path, needed: int) -> None:
        if needed <= 0:
            return
        try:
            dest.mkdir(parents=True, exist_ok=True)
            free = shutil.disk_usage(dest).free
        except OSError as e:
            raise ArtifactError("disk-space", f"cannot stat volume for {dest}: {e}") from e
        if free < needed * HEADROOM:
            raise ArtifactError(
                "disk-space",
                f"need ~{int(needed * HEADROOM / 1e9 * 10) / 10} GB (with headroom), "
                f"only {free / 1e9:.1f} GB free on volume of {dest}")

    # -- transports (override point for tests; HF is isolated in a supervised worker) --
    def _run_download_worker(self, request: dict, artifact: str) -> None:
        command = [sys.executable, "-c",
                   "from backend.model_provision import _download_worker_main; _download_worker_main()"]
        self._supervise(command, request, artifact)

    def _supervise(self, command: list[str], request: dict, artifact: str) -> None:
        process = subprocess.Popen(
            command, cwd=Path(__file__).resolve().parent.parent,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=sys.stderr,
            text=True, bufsize=1)
        lines: queue.Queue[str | None] = queue.Queue()

        def read_output() -> None:
            try:
                for line in process.stdout:
                    lines.put(line)
            finally:
                lines.put(None)

        reader = threading.Thread(target=read_output, daemon=True)
        reader.start()
        try:
            process.stdin.write(json.dumps(request) + "\n")
            process.stdin.close()
            failure = None
            stdout_closed = False
            while True:
                if self._cancelled():
                    process.terminate()
                    try:
                        process.wait(timeout=1)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()
                    raise Cancelled()
                try:
                    line = lines.get(timeout=0.1)
                except queue.Empty:
                    line = ""
                if line is None:
                    stdout_closed = True
                if line:
                    try:
                        event = json.loads(line)
                    except ValueError:
                        _log("download worker emitted a non-JSON line")
                        continue
                    if event.get("type") == "progress":
                        self.emit({"event": "progress", "artifact": artifact,
                                   "unit": "files",
                                   "files_done": int(event.get("files_done", 0)),
                                   "files_total": int(event.get("files_total", 0)),
                                   **({"path": event["path"]} if event.get("path") else {})})
                    elif event.get("type") == "error":
                        failure = str(event.get("message", "Hugging Face transport failed"))
                if process.poll() is not None and stdout_closed:
                    break
            code = process.wait()
            if code:
                message = failure or f"Hugging Face worker exited with status {code}"
                lowered = message.lower()
                err = "gated" if any(s in lowered for s in ("401", "403", "gated", "unauthorized")) else "download-failed"
                raise ArtifactError(err, message)
        finally:
            if process.poll() is None:
                process.terminate()
                with contextlib.suppress(subprocess.TimeoutExpired):
                    process.wait(timeout=1)
                if process.poll() is None:
                    process.kill()
                    process.wait()

    def _transport_hf_cache_file(self, art: Artifact) -> tuple[Path, Path]:
        staging = self._hf_cache_staging_dir(art)
        payload = staging / "payload"
        root = self._asset_root(art)
        self._assert_safe_tree(staging, root)
        payload.mkdir(parents=True, exist_ok=True)
        self._assert_safe_tree(payload, root)
        staged = payload / art.hf_filename
        if staged.is_file():
            try:
                self._validate_files(art, payload)
                return payload, staging
            except ArtifactError:
                self._remove_path(staged, root)
        isolated_cache = staging / "hub-cache"
        self._assert_safe_tree(isolated_cache, root)
        isolated_cache.mkdir(parents=True, exist_ok=True)
        self._run_download_worker({"action": "cache_file", "repo_id": art.repo_id,
                             "filename": art.hf_filename, "revision": art.revision,
                                  "cache_dir": str(isolated_cache),
                             "local_dir": str(payload), "force_download": True,
                             "files_total": len(art.download_files())}, art.id)
        return payload, staging

    def _transport_hf_snapshot(self, art: Artifact, dest: Path) -> tuple[Path, Path]:
        """Download selected files into resumable staging beside the destination."""
        staging = self._hf_staging_dir(art, dest)
        root = self._asset_root(art)
        self._assert_safe_tree(staging, root)
        payload = staging / "payload"
        payload.mkdir(parents=True, exist_ok=True)
        self._assert_safe_tree(payload, root)
        # A cancelled attempt may already have remapped weights and notices.
        top = self._artifact_top_dir(art)
        for f in art.download_files():
            path = payload / f.layout_path
            notice = payload / top / ".upstream" / f.layout_path
            if top and not f.required and not path.exists() and notice.is_file():
                self._assert_safe_path(path, root)
                path.parent.mkdir(parents=True, exist_ok=True)
                notice.replace(path)
        valid = self._discard_invalid_stage_files(art, payload)
        pending = [f.path for f in art.download_files() if f.path not in valid]
        if pending:
            self._run_download_worker(
                {"action": "snapshot", "repo_id": art.repo_id,
                 "revision": art.revision, "local_dir": str(payload),
                 "cache_dir": str(staging / "hub-cache"),
                 "allow_patterns": pending,
                 "ignore_patterns": list(art.ignore_patterns),
                 "files_total": len(art.download_files())}, art.id)
        return payload, staging

    def _discard_invalid_stage_files(self, art: Artifact, root: Path) -> set[str]:
        verified = set()
        for f in art.download_files():
            path = root / f.layout_path
            if not os.path.lexists(path):
                continue
            self._assert_safe_path(path, self._asset_root(art))
            before = _stat_identity(path)
            valid = path.is_file() and (f.size is None or path.stat().st_size == f.size)
            dg = file_digest(art, f)
            if valid and dg is not None:
                valid = digest_file(path, dg) == dg.expected
            if before is None or before != _stat_identity(path):
                raise ArtifactError("file-changed", f"{f.layout_path} changed while staging")
            if not valid:
                self._remove_path(path, self._asset_root(art))
            elif dg is not None:
                verified.add(f.path)
        return verified

    def _artifact_top_dir(self, art: Artifact) -> str:
        """Single top-level directory all required layout paths share, else "".

        VideoMaMa -> "VideoMaMa" (layout VideoMaMa/unet/...); SVD base -> its own
        folder; GVM/BiRefNet/C-RADIO files sit at the artifact root ("").
        """
        tops = set()
        for f in art.required_files():
            parts = f.layout_path.split("/", 1)
            tops.add(parts[0] if len(parts) == 2 else "")
        if len(tops) == 1:
            return tops.pop()
        return ""

    def _apply_remap_dirs(self, art: Artifact, root: Path) -> None:
        """Materialize path_remap in the isolated staging tree."""
        self._assert_safe_tree(root, self._asset_root(art))
        for src, dst in art.path_remap.items():
            src_dir = root / src
            if not src_dir.is_dir():
                continue
            dst_dir = root / dst
            self._assert_safe_path(src_dir, self._asset_root(art))
            self._assert_safe_path(dst_dir, self._asset_root(art))
            if not dst_dir.exists():
                self._assert_safe_path(dst_dir.parent, self._asset_root(art))
                dst_dir.parent.mkdir(parents=True, exist_ok=True)
                src_dir.rename(dst_dir)
                continue
            self._assert_safe_tree(dst_dir, self._asset_root(art))
            for p in sorted(src_dir.rglob("*")):
                if p.is_file():
                    target = dst_dir / p.relative_to(src_dir)
                    self._assert_safe_path(p, self._asset_root(art))
                    self._assert_safe_path(target, self._asset_root(art))
                    target.parent.mkdir(parents=True, exist_ok=True)
                    p.replace(target)  # same volume, atomic per file
            self._remove_path(src_dir, self._asset_root(art))

    def _transport_http(self, art: Artifact, dest: Path) -> tuple[Path, Path]:
        f = art.required_files()[0]
        root = self._asset_root(art)
        if art.md5_b64:  # RVM: reuse the vendored downloader's MD5-gated atomic path
            stage_root = self._rvm_staging_dir(art)
            self._assert_safe_tree(stage_root, root)
            stage_root.mkdir(parents=True, exist_ok=True)
            self._assert_safe_path(stage_root, root)
            self._run_download_worker({
                "action": "rvm", "package_root": str(self.roots.v2_package_root),
                "staging_root": str(stage_root), "url": art.url,
                "filename": f.path, "md5_b64": art.md5_b64,
            }, art.id)
            staged_dir = stage_root / (art.layout_rel or "")
            return staged_dir, stage_root

        stage_root = self._http_staging_dir(art, dest)
        self._assert_safe_tree(stage_root, root)
        stage_root.mkdir(parents=True, exist_ok=True)
        self._assert_safe_path(stage_root, root)
        staged = stage_root / f.layout_path
        self._assert_safe_path(staged.parent, root)
        staged.parent.mkdir(parents=True, exist_ok=True)
        if staged.is_file():
            try:
                self._validate_files(art, stage_root)
            except ArtifactError:
                self._remove_path(staged, root)
            else:
                return stage_root, stage_root

        dg = file_digest(art, f)
        part = staged.with_name(staged.name + ".part")
        downloaded = part.stat().st_size if part.is_file() else 0
        expected = f.size or 0
        if not expected and downloaded:
            self._remove_path(part, root)
            downloaded = 0  # resume is unsafe when upstream publishes no total size
        if expected and downloaded > expected:
            self._remove_path(part, root)
            downloaded = 0
        if expected and downloaded == expected:
            if dg is not None and digest_file(part, dg) != dg.expected:
                self._remove_path(part, root)
                downloaded = 0
            else:
                part.replace(staged)
                downloaded = 0
        req = urllib.request.Request(art.url, headers={"User-Agent": "EZ-CorridorKey2/1.0"})
        if staged.is_file():
            pass  # complete resumable part promoted to the staging filename
        elif downloaded and expected and downloaded < expected:
            req.add_header("Range", f"bytes={downloaded}-")
        if not staged.is_file():
            with urllib.request.urlopen(req, timeout=10) as resp:
                if downloaded and getattr(resp, "status", None) != 206:
                    downloaded = 0  # server ignored Range; restart instead of appending a duplicate
                    self._remove_path(part, root)
                total = int(resp.headers.get("Content-Length", 0)) + downloaded
                self._assert_safe_path(part, root)
                with open(part, "ab" if downloaded else "wb") as out:
                    while True:
                        self._check_cancel(art.id)
                        chunk = resp.read(1 << 20)
                        if not chunk:
                            break
                        out.write(chunk)
                        downloaded += len(chunk)
                        self.emit({"event": "progress", "artifact": art.id,
                                   "unit": "bytes",
                                   "bytes": downloaded, "bytes_total": total or expected})
            if dg and digest_file(part, dg) != dg.expected:
                self._remove_path(part, root)
                raise ArtifactError("hash-mismatch", f"{art.url} failed trusted-digest check")
            if expected and downloaded != expected:
                self._remove_path(part, root)
                raise ArtifactError("size-mismatch",
                                    f"download size mismatch: {downloaded} != {expected}")
            part.replace(staged)
        self._validate_files(art, stage_root)
        return stage_root, stage_root


def _download_worker_main() -> None:
    """One supervised model transfer process; progress reports completed file counts."""
    try:
        request = json.loads(sys.stdin.readline())
        if request["action"] == "rvm":
            from backend.model_provision import _load_v2_downloader

            dl = _load_v2_downloader(Path(request["package_root"]))
            dl.RVM_URL = request["url"]
            dl.RVM_FILE = request["filename"]
            dl.RVM_MD5_B64 = request["md5_b64"]
            urlopen = dl.urllib.request.urlopen

            def bounded_urlopen(url, *args, **kwargs):
                kwargs.setdefault("timeout", 10)
                return urlopen(url, *args, **kwargs)

            dl.urllib.request.urlopen = bounded_urlopen
            dl.download_rvm(Path(request["staging_root"]))
            return
        if request["action"] == "cache_file":
            from huggingface_hub import hf_hub_download

            hf_hub_download(
                repo_id=request["repo_id"], filename=request["filename"],
                revision=request["revision"], local_dir=request["local_dir"],
                cache_dir=request["cache_dir"],
                force_download=bool(request.get("force_download")),
            )
        elif request["action"] == "snapshot":
            from tqdm import tqdm
            from huggingface_hub import snapshot_download

            completed = [0]
            completed_lock = threading.Lock()
            total = int(request.get("files_total", 0))

            def write_progress(count: int) -> None:
                sys.stdout.write(json.dumps({"type": "progress", "files_done": count,
                                             "files_total": total}) + "\n")
                sys.stdout.flush()

            class FileProgress(tqdm):
                def __init__(self, *args, **kwargs):
                    kwargs["file"] = sys.stderr
                    super().__init__(*args, **kwargs)
                    self._reported = False

                def close(self):
                    done = self.total is not None and self.n >= self.total
                    super().close()
                    if done and not self._reported:
                        self._reported = True
                        with completed_lock:
                            completed[0] = min(total, completed[0] + 1)
                            write_progress(completed[0])

            snapshot_download(
                repo_id=request["repo_id"], revision=request["revision"],
                local_dir=request["local_dir"],
                cache_dir=request["cache_dir"],
                allow_patterns=request["allow_patterns"],
                ignore_patterns=request.get("ignore_patterns") or None,
                max_workers=4,
                tqdm_class=FileProgress,
            )
        else:
            raise ValueError(f"unknown download action: {request.get('action')}")
    except Exception as e:
        sys.stdout.write(json.dumps({"type": "error", "message": str(e)}) + "\n")
        sys.stdout.flush()
        raise SystemExit(1)
