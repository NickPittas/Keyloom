"""Offline model status engine: full-closure presence checks + stat-keyed verification.

Contract (standalone-release-plan.md §3):
  * **Startup never hashes and never touches the network.** ``status()`` uses
    only stat() plus a stat-keyed hash cache; rehashing happens exclusively
    in the explicit ``verify()`` operation.
  * **Closure, not sentinel:** an artifact is installed only when *every*
    required file in the catalog is present with the expected size.
  * **Verified never over-claims:** verified requires every required file to
    match a trusted upstream digest recorded in the local cache. Files whose
    upstream metadata supplies no digest (explicitly unresolved) are listed
    and keep the artifact unverified.

Digest sources, in precedence order per file (all upstream-supplied, never
self-authored):
  1. ``sha256`` — HF LFS oid / GitHub release digest (content sha256).
  2. ``git_blob_sha1`` — HF tree git blob oid; verified by computing the git
     blob hash (``sha1("blob <size>\\0" + bytes)``) of local bytes against it.
     It is stored separately from sha256 precisely because it is not sha256.
  3. artifact-level ``md5_b64`` (RVM) — upstream-published, base64 MD5.

stdlib only; importable at startup without torch/Qt/huggingface_hub.
"""
from __future__ import annotations

import base64
import hashlib
import importlib.util
from importlib import metadata as package_metadata
import json
import os
import stat
import sys
import time
from dataclasses import dataclass
from pathlib import Path

from backend.model_catalog import (
    Artifact,
    Catalog,
    FileSpec,
    hf_cache_snapshot_path,
    hf_hub_cache_dir,
    hf_hub_cache_dirs,
)

HASH_CACHE_NAME = "model_verify.json"

# Presence-only artifacts (no upstream bytes to hash by design).
PROBE_IMPORTS = {"sam2-package": "sam2.build_sam"}

_STATES = ("installed", "partial", "missing")


@dataclass(frozen=True)
class Roots:
    """Provisioning roots (contract: docs/design/rust-ui/install-descriptor.md).

    Resolution order per root mirrors the descriptor precedence table — no
    discovery is duplicated here: models comes from ``backend.v2_paths``
    (env → descriptor → platform), data/cache from ``backend.install_paths``
    (descriptor → platform), HF cache from the user's HF_HOME/HF_HUB_CACHE.
    Explicit constructor arguments always win (tests, relocation, Settings).
    """

    data_dir: Path
    v2_models_dir: Path
    v2_package_root: Path
    hf_cache: Path
    cache_dir: Path
    legacy_data_dirs: tuple[Path, ...] = ()
    legacy_models_dirs: tuple[Path, ...] = ()
    hf_caches: tuple[Path, ...] = ()

    @classmethod
    def defaults(
        cls,
        data_dir: Path | None = None,
        v2_models_dir: Path | None = None,
        hf_cache: Path | None = None,
        cache_dir: Path | None = None,
    ) -> "Roots":
        from backend import install_paths, v2_paths

        explicit_hf = hf_cache is not None
        if v2_models_dir is None:
            v2_models_dir = v2_paths.get_v2_models_dir()  # env → descriptor → platform
        install = install_paths.discover()
        if data_dir is None:
            data_dir = _default_data_dir(install)
        if cache_dir is None:
            cache_dir = _default_cache_dir(install, Path(data_dir))
        package_root = v2_paths.get_v2_package_root()
        from backend.install_paths import platform_user_data
        fallback_data = platform_user_data()
        legacy_data = tuple(p for p in (fallback_data, package_root, package_root.parent)
                            if p is not None and Path(p) != Path(data_dir))
        local = os.environ.get("LOCALAPPDATA")
        if local:
            fallback_models = Path(local) / "Keyloom" / "EngineV2"
        elif sys.platform == "win32":
            fallback_models = Path.home() / "AppData" / "Local" / "Keyloom" / "EngineV2"
        elif sys.platform == "darwin":
            fallback_models = Path.home() / "Library" / "Application Support" / "Keyloom" / "EngineV2"
        else:
            xdg = os.environ.get("XDG_DATA_HOME")
            fallback_models = (Path(xdg) if xdg else Path.home() / ".local" / "share") / "Keyloom" / "EngineV2"
        legacy_models = (fallback_models,) if fallback_models != Path(v2_models_dir) else ()
        caches = tuple(dict.fromkeys(
            ([Path(hf_cache)] if explicit_hf else []) + list(hf_hub_cache_dirs())))
        return cls(
            data_dir=Path(data_dir),
            v2_models_dir=Path(v2_models_dir),
            v2_package_root=package_root,
            hf_cache=Path(hf_cache) if hf_cache else hf_hub_cache_dir(),
            cache_dir=Path(cache_dir),
            legacy_data_dirs=legacy_data,
            legacy_models_dirs=legacy_models,
            hf_caches=caches,
        )


def _default_data_dir(install) -> Path:
    """Descriptor user_data when installed; developer checkout otherwise.

    Mirrors the descriptor doc: null optional roots mean platform defaults,
    which preserves existing model locations — no moves, no deletes.
    """
    if install is not None:
        if install.user_data is not None:
            return install.user_data
        from backend.install_paths import platform_user_data

        ud = platform_user_data()
        if ud is not None:
            return ud
    if not getattr(sys, "frozen", False):
        return Path(__file__).resolve().parent.parent
    try:
        from backend.project import get_data_dir

        return Path(get_data_dir())
    except Exception:
        return Path.home() / ".local" / "share" / "EZ-CorridorKey2"


def _default_cache_dir(install, data_dir: Path) -> Path:
    """Descriptor cache root (or its platform default) when installed;
    the repo's logs/diagnostics convention in a developer checkout."""
    if install is not None:
        if install.cache is not None:
            base = install.cache
        else:
            from backend.install_paths import platform_cache

            base = platform_cache()
        if base is not None:
            return Path(base) / "model-provision"
    return data_dir / "logs" / "diagnostics"


@dataclass(frozen=True)
class FileDigest:
    algo: str  # "sha256" | "git-blob-sha1" | "md5"
    expected: str
    b64: bool = False  # md5 stored base64 (RVM convention)


def file_digest(art: Artifact, f: FileSpec) -> FileDigest | None:
    """Trusted upstream digest for a file, or None when unresolved."""
    if f.sha256:
        return FileDigest("sha256", f.sha256.lower())
    if f.git_blob_sha1:
        return FileDigest("git-blob-sha1", f.git_blob_sha1.lower())
    if art.md5_b64 and len(art.required_files()) == 1 and f in art.required_files():
        return FileDigest("md5", art.md5_b64, b64=True)
    return None


def _stat_identity(path: Path) -> dict | None:
    try:
        link = path.lstat()
        target = path.stat()
    except OSError:
        return None
    return {
        "link_dev": link.st_dev, "link_ino": link.st_ino,
        "link_mode": link.st_mode, "link_mtime_ns": link.st_mtime_ns,
        "link_ctime_ns": link.st_ctime_ns,
        "dev": target.st_dev, "ino": target.st_ino, "size": target.st_size,
        "mtime_ns": target.st_mtime_ns, "ctime_ns": target.st_ctime_ns,
    }


def _digest_file(path: Path, dg: FileDigest) -> str:
    if dg.algo == "sha256":
        h = hashlib.sha256()
    else:
        h = hashlib.md5()  # noqa: S324 - verification against upstream-published MD5
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(4 << 20), b""):
            h.update(chunk)
    return base64.b64encode(h.digest()).decode() if dg.b64 else h.hexdigest()


def digest_file(path: Path, dg: FileDigest) -> str:
    """Compute a file digest in the digest's own representation.

    git blob id = sha1("blob <size>\\0" + content), computed streaming via a
    second pass header; sha256/md5 are plain content digests.
    """
    if dg.algo != "git-blob-sha1":
        return _digest_file(path, dg)
    h = hashlib.sha1()  # noqa: S324
    size = path.stat().st_size
    h.update(f"blob {size}\0".encode())
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(4 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


class HashCache:
    """Stat-keyed verification cache (path -> identity + digest). A hit is
    trusted only when file and link identities match, so any change to the
    file invalidates the entry without rehashing gigabytes per launch."""

    def __init__(self, path: Path):
        self.path = path
        self.load_error: str | None = None  # why saved verdicts are missing (absent file is normal)
        self.save_error: str | None = None  # why the last save failed
        self._cache = self._load()

    def _load(self) -> dict:
        self.load_error = None
        try:
            with open(self.path, encoding="utf-8") as f:
                data = json.load(f)
            if not isinstance(data, dict):
                self.load_error = "the verification record has an unexpected format"
                return {}
            return data
        except FileNotFoundError:
            return {}
        except (OSError, ValueError) as e:
            self.load_error = f"the verification record could not be read ({e})"
            return {}

    def reload(self) -> None:
        """Discard unpersisted verdicts after a failed cache write."""
        self._cache = self._load()

    def entry(self, path: Path) -> dict | None:
        ent = self._cache.get(str(path.resolve()))
        return ent if isinstance(ent, dict) else None

    def match(self, path: Path, dg: FileDigest) -> bool | None:
        """True/False when the cache can answer, None when a rehash is needed."""
        ent = self.entry(path)
        ident = _stat_identity(path)
        if ent is None or ident is None:
            return None
        if any(ent.get(k) != v for k, v in ident.items()):
            return None
        if ent.get("algo") != dg.algo:
            return None
        return ent.get("digest") == dg.expected

    def record(self, path: Path, dg: FileDigest, ok: bool, digest: str, artifact: str,
               identity: dict | None = None) -> None:
        ident = identity if identity is not None else _stat_identity(path)
        if ident is None:
            return
        self._cache[str(path.resolve())] = {**ident, "algo": dg.algo, "digest": digest,
                                            "expected": dg.expected, "ok": ok, "artifact": artifact}

    def save(self) -> bool:
        tmp = None
        self.save_error = None
        try:
            for ancestor in reversed((self.path.parent, *self.path.parent.parents)):
                try:
                    st = ancestor.lstat()
                except FileNotFoundError:
                    continue
                attrs = getattr(st, "st_file_attributes", 0)
                if stat.S_ISLNK(st.st_mode) or attrs & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0):
                    self.save_error = f"{ancestor} is a link"
                    return False
            self.path.parent.mkdir(parents=True, exist_ok=True)
            if os.path.lexists(self.path) and (self.path.is_symlink() or not self.path.is_file()):
                self.save_error = f"{self.path} is not a regular file"
                return False
            tmp = self.path.with_name(f".{self.path.name}.{os.getpid()}.{time.time_ns()}.tmp")
            with open(tmp, "x", encoding="utf-8") as f:
                json.dump(self._cache, f, indent=1, sort_keys=True)
            os.replace(tmp, self.path)
            return True
        except OSError as e:
            self.save_error = str(e)
            return False  # cache write failure never blocks offline status, but verify() reports it
        finally:
            if tmp is not None:
                try:
                    tmp.unlink(missing_ok=True)
                except OSError:
                    pass


class StatusChecker:
    def __init__(self, catalog: Catalog, roots: Roots):
        self.catalog = catalog
        self.roots = roots
        self.hash_cache = HashCache(roots.cache_dir / HASH_CACHE_NAME)
        self._selected_dirs: dict[str, Path] = {}

    # -- path resolution -------------------------------------------------
    def destination_dir(self, art: Artifact) -> Path | None:
        """Canonical writable destination; discovery never redirects downloads."""
        if art.kind == "local_package":
            return None
        if art.kind == "hf_cache_file":
            return hf_cache_snapshot_path(self.roots.hf_cache, art.repo_id, art.revision, art.hf_filename).parent
        base = {"v2_models_dir": self.roots.v2_models_dir,
                "data_dir": self.roots.data_dir,
                "v2_package_root": self.roots.v2_package_root}.get(art.layout_root)
        if base is None:
            return None
        return base / art.layout_rel if art.layout_rel else base

    def _candidate_dirs(self, art: Artifact) -> tuple[Path, ...]:
        if art.kind == "local_package":
            return ()
        if art.kind == "hf_cache_file" and art.repo_id:
            layout_candidates = [self.destination_dir(art)]
        else:
            root_map = {
                "v2_models_dir": (self.roots.v2_models_dir, *self.roots.legacy_models_dirs),
                "data_dir": (self.roots.data_dir, *self.roots.legacy_data_dirs),
                "v2_package_root": (self.roots.v2_package_root,),
            }
            bases = root_map.get(art.layout_root or "", ())
            suffix = Path(art.layout_rel) if art.layout_rel else Path()
            layout_candidates = [Path(base) / suffix for base in bases]
        if art.kind.startswith("hf_") and art.repo_id:
            roots = self.roots.hf_caches or (self.roots.hf_cache,)
            repo = f"models--{art.repo_id.replace('/', '--')}"
            for cache in roots:
                snapshots = Path(cache) / repo / "snapshots"
                pinned = snapshots / art.revision if art.revision else None
                if pinned is not None:
                    layout_candidates.append(pinned)
                if snapshots.is_dir():
                    layout_candidates.extend(sorted((p for p in snapshots.iterdir() if p.is_dir() and p != pinned),
                                                    key=lambda p: p.name))
        if art.id == "matanyone2":
            layout_candidates.append(self.roots.v2_package_root.parent / "pretrained_models")
        return tuple(dict.fromkeys(p for p in layout_candidates if p is not None))

    def is_hf_snapshot_dir(self, art: Artifact, root: Path) -> bool:
        if not art.kind.startswith("hf_") or not art.repo_id:
            return False
        repo = f"models--{art.repo_id.replace('/', '--')}"
        return any(Path(root).parent == Path(cache) / repo / "snapshots"
                   for cache in (self.roots.hf_caches or (self.roots.hf_cache,)))

    def _file_at(self, art: Artifact, root: Path, f: FileSpec) -> Path:
        if art.kind == "hf_cache_file":
            return root / str(art.hf_filename)
        if art.kind.startswith("hf_") and art.repo_id and self.is_hf_snapshot_dir(art, root):
            return root / f.path
        path = root / f.layout_path
        if art.id == "videomama":
            if f.layout_path.endswith("/unet/config.json"):
                alternatives = (root / "VideoMaMa" / "config.json",)
            elif f.layout_path.endswith("/unet/diffusion_pytorch_model.safetensors"):
                alternatives = (root / "VideoMaMa" / "diffusion_pytorch_model.safetensors",
                                root / "VideoMaMa" / "weights.safetensors")
            else:
                alternatives = ()
            if not self._is_size_match(path, f):
                path = next((candidate for candidate in alternatives if self._is_size_match(candidate, f)), path)
        return path

    def _is_size_match(self, path: Path, f: FileSpec) -> bool:
        try:
            return f.size is not None and path.is_file() and path.stat().st_size == f.size
        except OSError:
            return False

    def artifact_dir(self, art: Artifact) -> Path | None:
        if art.kind == "local_package":
            return None
        if art.id in self._selected_dirs:
            return self._selected_dirs[art.id]
        dest = self.destination_dir(art)
        candidates = self._candidate_dirs(art)
        if dest is None:
            return None
        if not candidates:
            self._selected_dirs[art.id] = dest
            return dest
        files = art.required_files()
        if art.kind == "hf_cache_file":
            files = art.required_files()[:1]
        best, best_score = dest, -1
        for root in candidates:
            score = sum(self._is_size_match(self._file_at(art, root, f), f) for f in files)
            if score > best_score:
                best, best_score = root, score
            if score == len(files) and files:
                best = root
                break
        self._selected_dirs[art.id] = best
        return best

    def refresh_paths(self, art: Artifact) -> None:
        self._selected_dirs.pop(art.id, None)

    def candidate_file_paths(self, art: Artifact, f: FileSpec) -> tuple[Path, ...]:
        return tuple(dict.fromkeys(self._file_at(art, root, f) for root in self._candidate_dirs(art)))

    def file_path(self, art: Artifact, f: FileSpec) -> Path | None:
        d = self.artifact_dir(art)
        return self._file_at(art, d, f) if d else None

    # -- status (offline, no hashing) ------------------------------------
    def check_artifact(self, art: Artifact) -> dict:
        problems: list[str] = []
        if art.kind in ("local_package", "local_code"):
            if art.kind == "local_package":
                installed, state, problems = _check_local_package(art)
                note = "Python import/version probe; no downloaded bytes to hash"
            else:
                installed = all(
                    (p := self.file_path(art, f)) is not None and (p.is_file() or p.is_dir())
                    for f in art.required_files()) if art.required_files() else False
                state = "installed" if installed else "missing"
                note = "presence-only artifact (vendored code; git is the integrity mechanism)"
            return self._result(art, state, problems,
                                files_ok=installed, unhashed=[], verify_note=note)

        required = art.required_files()
        present = 0
        unhashed: list[str] = []
        for f in required:
            path = self.file_path(art, f)
            if path is None or not path.is_file():
                if path is not None and path.is_dir():
                    problems.append(f"not a file: {f.layout_path}")
                    continue
                problems.append(f"missing: {f.layout_path}")
                continue
            present += 1
            if f.size is not None:
                actual = path.stat().st_size
                if actual != f.size:
                    problems.append(
                        f"size mismatch: {f.layout_path} expected {f.size} got {actual}")
            if file_digest(art, f) is None:
                unhashed.append(f.layout_path)

        state = "missing" if present == 0 else ("installed" if present == len(required) and not any(
            p.startswith("size mismatch") for p in problems) else "partial")
        return self._result(art, state, problems, files_ok=(present == len(required)), unhashed=unhashed)

    def _result(self, art: Artifact, state: str, problems: list[str], files_ok: bool,
                unhashed: list[str], verify_note: str | None = None) -> dict:
        if art.kind in ("local_package", "local_code"):
            cached_verdict = True if state == "installed" else False
        elif state == "installed" and files_ok:
            cached_verdict = self._cached_verify(art)
        else:
            cached_verdict = False
        present = state == "installed"
        verified = cached_verdict is True
        result = {
            "artifact": art.id,
            "label": art.label,
            "capability": art.capability,
            "kind": art.kind,
            "state": state,
            "present": present,
            "ready": present and verified,
            "required_profile": art.required_profile,
            "default_variant": art.default_variant,
            "revision": art.revision,
            "revision_status": art.revision_status,
            "gated": art.gated,
            "path": str(self.artifact_dir(art)) if self.artifact_dir(art) else None,
            "can_delete": self.artifact_dir(art) == self.destination_dir(art),
            "problems": problems,
            "verified": verified,
            "verify_state": ("verified" if verified else
                             "unverified" if cached_verdict is False else
                             "unknown (not verified yet)"),
            "unhashed_files": unhashed,
            "depends": list(art.depends),
            "bytes_required": art.trusted_bytes(),
        }
        if unhashed and state == "installed":
            result["verify_note"] = (f"{len(unhashed)} required file(s) have no trusted upstream "
                                     "digest; artifact stays unverified")
        elif verify_note:
            result["verify_note"] = verify_note
        elif state == "installed" and not verified and self.hash_cache.load_error:
            result["verify_note"] = f"Verify needed: {self.hash_cache.load_error}"
        return result

    def _cached_verify(self, art: Artifact) -> bool | None:
        """True only if every required file's cached digest matches its stat identity."""
        verdict: bool | None = True
        for f in art.required_files():
            dg = file_digest(art, f)
            path = self.file_path(art, f)
            if dg is None or path is None:
                verdict = False  # explicitly unresolved metadata: never verified
                continue
            m = self.hash_cache.match(path, dg)
            if m is None:
                return None  # needs explicit verify
            verdict = verdict and m
        return verdict

    # -- explicit verification (hashes) ----------------------------------
    def verify_artifact(self, art: Artifact) -> dict:
        """Rehash required files against trusted digests; updates the cache."""
        if art.kind in ("local_package", "local_code"):
            res = self.check_artifact(art)
            return {"artifact": art.id, "state": res["state"], "verified": res["verified"],
                    "files_verified": 0, "files_required": len(art.required_files()),
                    "files_unhashed": [], "problems": res["problems"]}
        problems: list[str] = list(self.check_artifact(art)["problems"])
        verified_files = 0
        unhashed: list[str] = []
        mismatches: list[str] = []
        for f in art.required_files():
            path = self.file_path(art, f)
            dg = file_digest(art, f)
            if path is None or not path.is_file():
                continue
            if dg is None:
                unhashed.append(f.layout_path)
                continue
            try:
                before = _stat_identity(path)
                digest = digest_file(path, dg)
            except OSError as e:
                problems.append(f"unreadable: {f.layout_path}: {e}")
                continue
            after = _stat_identity(path)
            if before is None or before != after:
                problems.append(f"changed during verification: {f.layout_path}")
                continue
            ok = digest == dg.expected
            self.hash_cache.record(path, dg, ok, digest, art.id, identity=after)
            if ok:
                verified_files += 1
            else:
                mismatches.append(f"{f.layout_path} {dg.algo} mismatch (computed {digest[:16]}…, "
                                  f"expected {dg.expected[:16]}…)")
        problems.extend(mismatches)
        required_n = len(art.required_files())
        verified = (required_n > 0 and verified_files + len(unhashed) == required_n
                    and not mismatches and not [p for p in problems if p.startswith(("missing", "size", "unreadable"))]
                    and not unhashed)
        # unhashed files keep the artifact unverified (explicitly unresolved metadata)
        return {
            "artifact": art.id,
            "state": self.check_artifact(art)["state"],
            "verified": verified,
            "files_verified": verified_files,
            "files_required": required_n,
            "files_unhashed": unhashed,
            "problems": problems,
        }

    def verify(self, artifact_ids: list[str] | None = None) -> list[dict]:
        ids = artifact_ids or sorted(self.catalog.artifacts)
        results = [self.verify_artifact(self.catalog.get(i)) for i in ids]
        if not self.hash_cache.save():
            # Say so: otherwise the next start shows "Verify needed" and Repair looks like the cure.
            err = self.hash_cache.save_error or "unknown error"
            for r in results:
                # Only files that passed may be called fine; a failed one keeps its problems and Repair stays the cure.
                fine = " Files are fine: Repair is not needed." if r["verified"] else ""
                r["verify_note"] = f"Verification result not saved: {err}; it is lost on restart.{fine}"
        return results

    # -- aggregate -------------------------------------------------------
    def status(self, artifact_ids: list[str] | None = None) -> dict:
        ids = artifact_ids or sorted(self.catalog.artifacts)
        items = [self.check_artifact(self.catalog.get(i)) for i in ids]
        req_ids = [a.id for a in self.catalog.artifacts.values() if a.required_profile]
        by_id = {it["artifact"]: it for it in items}
        reported_required = [i for i in req_ids if i in by_id]
        ready_required = (all(by_id[i]["ready"] for i in reported_required)
                          if len(reported_required) == len(req_ids) else None)
        return {
            "catalog_version": self.catalog.version,
            "catalog_updated": self.catalog.updated,
            "roots": {
                "data_dir": str(self.roots.data_dir),
                "v2_models_dir": str(self.roots.v2_models_dir),
                "hf_cache": str(self.roots.hf_cache),
            },
            "artifacts": items,
            "summary": {
                "installed": sum(1 for it in items if it["state"] == "installed"),
                "partial": sum(1 for it in items if it["state"] == "partial"),
                "missing": sum(1 for it in items if it["state"] == "missing"),
                "ready_required": ready_required,
                "required_profile": req_ids,
            },
        }


def readiness_state(item: dict) -> str:
    """present | missing. Every file exists at its location with the catalog size; a size mismatch is missing.
    Hash verification is never part of this (it runs only inside a download or an explicit Verify)."""
    return "present" if item["state"] == "installed" and not item["problems"] else "missing"


def _probe_import(module_name: str | None) -> bool:
    if not module_name:
        return False
    try:
        return importlib.util.find_spec(module_name) is not None
    except Exception:
        return False


def _check_local_package(art: Artifact) -> tuple[bool, str, list[str]]:
    modules = art.probe_imports or ((PROBE_IMPORTS[art.id],) if art.id in PROBE_IMPORTS else ())
    available = [name for name in modules if _probe_import(name)]
    problems = [f"Python module unavailable: {name}" for name in modules if name not in available]
    partial = bool(available)
    if not modules:
        problems.append("no Python import probe is configured")

    if art.pip_spec and art.pip_spec.count("==") == 1 and ";" not in art.pip_spec:
        package, expected = (part.strip() for part in art.pip_spec.split("==", 1))
        if package and expected and not any(c in package for c in " <>!~[]"):
            try:
                found = package_metadata.version(package)
            except package_metadata.PackageNotFoundError:
                found = None
            if found != expected:
                problems.append(f"requires {package}=={expected}; found {found or 'not installed'}")
                partial = partial or bool(available)

    installed = bool(modules) and len(available) == len(modules) and not problems
    return installed, "installed" if installed else "partial" if partial else "missing", problems
