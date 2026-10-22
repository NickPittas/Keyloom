"""Pinned model catalog for EZ-CorridorKey2 (stdlib only, offline).

Loads ``backend/model_catalog.json``: the single authoritative inventory of
every downloadable model artifact (CorridorKey 2.0 + foundations, SAM2.1
variants, GVM, VideoMaMa + SVD base, MatAnyone2, BiRefNet variants) with
immutable revisions, per-file layout maps, trusted hashes and provenance.

Hash policy (see catalog ``meta``):
  * ``sha256`` is present only when upstream metadata supplies it — HF LFS
    ``lfs.oid`` (a true content sha256) or a GitHub release asset ``digest``.
    Git blob SHA-1s are recorded separately (``git_blob_sha1``) and are NOT
    content hashes; they identify file content within git but verify nothing
    about downloaded bytes and must never be used as sha256.
  * Hashes are never invented and never computed from downloaded bytes.
  * An artifact whose files lack trusted hashes can be *installed* but never
    reports ``verified``.

This module performs no network access and no filesystem assumptions beyond
reading the JSON next to it.
"""
from __future__ import annotations

import json
import os
from dataclasses import dataclass, field
from pathlib import Path
from urllib.parse import urlparse

CATALOG_FILENAME = "model_catalog.json"

# Roots a layout can reference. ``v2_package_root`` is code (repo content),
# the others are the provisioning roots the runtime agent's descriptor owns.
LAYOUT_ROOTS = ("v2_models_dir", "data_dir", "v2_package_root")

PROFILE_REQUIRED = "required"
PROFILE_FULL = "full"
PROFILES = (PROFILE_REQUIRED, PROFILE_FULL)

BIREFNET_VARIANTS = (
    "BiRefNet", "BiRefNet-COD", "BiRefNet-DIS5K", "BiRefNet-DIS5K-TR_TEs",
    "BiRefNet-HRSOD", "BiRefNet-legacy", "BiRefNet-matting", "BiRefNet-portrait",
    "BiRefNet_512x512", "BiRefNet_HR", "BiRefNet_HR-Matting", "BiRefNet_dynamic",
    "BiRefNet_dynamic-matting", "BiRefNet_lite", "BiRefNet_lite-2K",
    "BiRefNet_lite-matting",
)

# Old setup_models.py flags / wizard keys -> catalog artifact ids (groups).
FLAG_TARGET_MAP = {
    "corridorkey": ["corridorkey-weights", "cradio", "moge", "rvm"],
    "gvm": ["gvm"],
    "videomama": ["videomama"],  # videomama depends on videomama-svd
    "matanyone2": ["matanyone2"],
    "birefnet": [f"birefnet-{name}" for name in BIREFNET_VARIANTS],
    "sam2": ["sam2-small", "sam2-base-plus", "sam2-large"],
    "sam2-small": ["sam2-small"],
    "sam2-base-plus": ["sam2-base-plus"],
    "sam2-large": ["sam2-large"],
}


class CatalogError(Exception):
    """Catalog file missing or schema-invalid."""


def _catalog_path(value: object, artifact_id: str, field_name: str) -> str:
    """Reject paths that could escape an artifact root on either OS."""
    if not isinstance(value, str) or not value or "\\" in value or ":" in value:
        raise CatalogError(f"artifact {artifact_id}: invalid {field_name} {value!r}")
    if value.startswith("/") or any(part in ("", ".", "..") for part in value.rstrip("/").split("/")):
        raise CatalogError(f"artifact {artifact_id}: unsafe {field_name} {value!r}")
    return value


@dataclass(frozen=True)
class FileSpec:
    path: str  # path inside the source repo / URL basename
    layout_path: str  # path relative to the artifact's layout root
    size: int | None
    sha256: str | None  # upstream-supplied content hash (HF LFS oid / GH digest) or None
    sha256_source: str | None  # e.g. "hf-lfs-oid", "github-release-digest"
    lfs: bool
    required: bool
    git_blob_sha1: str | None = None
    note: str | None = None

    def to_json(self) -> dict:
        return {
            "path": self.path,
            "layout_path": self.layout_path,
            "size": self.size,
            "sha256": self.sha256,
            "sha256_source": self.sha256_source,
            "lfs": self.lfs,
            "required": self.required,
            "git_blob_sha1": self.git_blob_sha1,
            "note": self.note,
        }


@dataclass(frozen=True)
class Artifact:
    id: str
    label: str
    capability: str  # e.g. "key", "hint-auto", "hint-track", "hint-guided"
    kind: str  # hf_files | hf_snapshot | hf_cache_file | http_file | local_package | local_code
    required_profile: bool
    layout_root: str | None  # one of LAYOUT_ROOTS; None for local_package/hf_cache layouts
    layout_rel: str | None  # path relative to layout_root ("" = root itself)
    repo_id: str | None = None
    revision: str | None = None
    revision_status: str = "unresolved"  # pinned | unresolved
    url: str | None = None
    files: tuple[FileSpec, ...] = ()
    depends: tuple[str, ...] = ()
    size_bytes_estimate: int | None = None
    size_human: str | None = None
    ignore_patterns: tuple[str, ...] = ()
    path_remap: dict[str, str] = field(default_factory=dict)
    hf_filename: str | None = None  # hf_cache_file kind
    md5_b64: str | None = None  # http_file kind, upstream-supplied
    pip_spec: str | None = None  # local_package kind
    install_hint: str | None = None
    default_variant: bool = False
    gated: str | None = None  # HF gating status from upstream metadata ("auto"/"manual"/"false")
    upstream_head: str | None = None  # recorded when upstream HEAD drifted from our pin
    provenance: dict = field(default_factory=dict)
    notes: tuple[str, ...] = ()
    probe_imports: tuple[str, ...] = ()

    def required_files(self) -> tuple[FileSpec, ...]:
        return tuple(f for f in self.files if f.required)

    def download_files(self) -> tuple[FileSpec, ...]:
        """Runtime closure plus upstream notices; exclude unrelated model variants/media."""
        return tuple(f for f in self.files
                     if f.required or f.note == "documentation/notice file")

    def has_trusted_hashes(self) -> bool:
        files = self.required_files()
        return bool(files) and all(f.sha256 or f.git_blob_sha1 or self.md5_b64 and len(files) == 1
                                   for f in files)

    def trusted_bytes(self) -> int | None:
        """Total known byte size of required files, or None if any is unknown."""
        total = 0
        for f in self.required_files():
            if f.size is None:
                return None
            total += f.size
        return total


def _parse_file(entry: dict, artifact_id: str) -> FileSpec:
    if not isinstance(entry, dict) or "path" not in entry:
        raise CatalogError(f"artifact {artifact_id}: file entry without path")
    path = _catalog_path(entry["path"], artifact_id, "file path")
    layout_path = _catalog_path(entry.get("layout_path") or path, artifact_id, "layout_path")
    sha = entry.get("sha256")
    if sha is not None and (not isinstance(sha, str) or len(sha) != 64
                            or any(c not in "0123456789abcdef" for c in sha)):
        raise CatalogError(f"artifact {artifact_id}: malformed sha256 for {path}")
    git_sha = entry.get("git_blob_sha1")
    if git_sha is not None and (not isinstance(git_sha, str) or len(git_sha) != 40
                                or any(c not in "0123456789abcdef" for c in git_sha)):
        raise CatalogError(f"artifact {artifact_id}: malformed git_blob_sha1 for {path}")
    size = entry.get("size")
    if size is not None and (not isinstance(size, int) or isinstance(size, bool) or size < 0):
        raise CatalogError(f"artifact {artifact_id}: invalid size for {path}")
    return FileSpec(
        path=path,
        layout_path=layout_path,
        size=size,
        sha256=sha,
        sha256_source=entry.get("sha256_source"),
        lfs=bool(entry.get("lfs")),
        required=bool(entry.get("required", True)),
        git_blob_sha1=git_sha,
        note=entry.get("note"),
    )


def _parse_artifact(aid: str, raw: dict) -> Artifact:
    if not isinstance(raw, dict):
        raise CatalogError(f"artifact {aid}: expected an object")
    probe_imports = raw.get("probe_imports", [])
    if not isinstance(probe_imports, list) or any(
        not isinstance(name, str) or not name for name in probe_imports
    ):
        raise CatalogError(f"artifact {aid}: probe_imports must be a list of module names")
    try:
        kind = raw["kind"]
        art = Artifact(
            id=aid,
            label=raw.get("label", aid),
            capability=raw.get("capability", ""),
            kind=kind,
            required_profile=bool(raw.get("required_profile", False)),
            layout_root=raw.get("layout_root"),
            layout_rel=raw.get("layout_rel"),
            repo_id=raw.get("repo_id"),
            revision=raw.get("revision"),
            revision_status=raw.get("revision_status", "pinned" if raw.get("revision") else "unresolved"),
            url=raw.get("url"),
            files=tuple(_parse_file(f, aid) for f in raw.get("files", [])),
            depends=tuple(raw.get("depends", [])),
            size_bytes_estimate=raw.get("size_bytes_estimate"),
            size_human=raw.get("size_human"),
            ignore_patterns=tuple(raw.get("ignore_patterns", [])),
            path_remap=dict(raw.get("path_remap", {})),
            hf_filename=raw.get("hf_filename"),
            md5_b64=raw.get("md5_b64"),
            pip_spec=raw.get("pip_spec"),
            install_hint=raw.get("install_hint"),
            default_variant=bool(raw.get("default_variant", False)),
            gated=raw.get("gated"),
            upstream_head=raw.get("upstream_head"),
            provenance=dict(raw.get("provenance", {})),
            notes=tuple(raw.get("notes", [])),
            probe_imports=tuple(probe_imports),
        )
    except KeyError as e:
        raise CatalogError(f"artifact {aid}: missing field {e}") from e
    if art.layout_root is not None and art.layout_root not in LAYOUT_ROOTS:
        raise CatalogError(f"artifact {aid}: unknown layout_root {art.layout_root}")
    if art.layout_rel:
        _catalog_path(art.layout_rel, aid, "layout_rel")
    elif art.layout_rel not in (None, ""):
        raise CatalogError(f"artifact {aid}: invalid layout_rel {art.layout_rel!r}")
    if kind in ("hf_files", "hf_snapshot", "hf_cache_file") and not isinstance(art.repo_id, str):
        raise CatalogError(f"artifact {aid}: kind {kind} requires repo_id")
    if art.revision_status not in ("pinned", "unresolved"):
        raise CatalogError(f"artifact {aid}: invalid revision_status {art.revision_status!r}")
    if kind in ("hf_files", "hf_snapshot", "hf_cache_file"):
        if art.revision_status == "pinned" and (
            not isinstance(art.revision, str) or len(art.revision) != 40
            or any(c not in "0123456789abcdef" for c in art.revision)
        ):
            raise CatalogError(f"artifact {aid}: pinned HF revision must be a commit SHA")
        if art.revision_status == "unresolved" and art.revision is not None:
            raise CatalogError(f"artifact {aid}: unresolved HF revision must be null")
    if kind == "http_file":
        parsed = urlparse(art.url or "")
        if parsed.scheme != "https" or not parsed.netloc:
            raise CatalogError(f"artifact {aid}: http_file requires an HTTPS URL")
    if art.revision_status == "pinned" and not art.revision:
        raise CatalogError(f"artifact {aid}: pinned artifact requires a revision")
    if art.revision_status == "unresolved" and art.revision is not None:
        raise CatalogError(f"artifact {aid}: unresolved revision must be null")
    if art.hf_filename is not None:
        _catalog_path(art.hf_filename, aid, "hf_filename")
    if any(not isinstance(dep, str) for dep in art.depends):
        raise CatalogError(f"artifact {aid}: dependencies must be artifact ids")
    for src, dst in art.path_remap.items():
        _catalog_path(src, aid, "path_remap source")
        _catalog_path(dst, aid, "path_remap destination")
    return art


@dataclass(frozen=True)
class Catalog:
    version: int
    updated: str
    meta: dict
    artifacts: dict[str, Artifact]

    def get(self, artifact_id: str) -> Artifact:
        try:
            return self.artifacts[artifact_id]
        except KeyError:
            raise CatalogError(f"unknown artifact {artifact_id}") from None

    def profile(self, name: str) -> list[str]:
        if name == PROFILE_REQUIRED:
            return [a.id for a in self.artifacts.values() if a.required_profile]
        if name == PROFILE_FULL:
            return sorted(self.artifacts.keys())
        raise CatalogError(f"unknown profile {name}")

    def closure(self, artifact_ids: list[str]) -> list[str]:
        """Expand artifact ids/groups into a dependency-closed ordered list."""
        out: list[str] = []
        seen: set[str] = set()
        active: set[str] = set()

        def add(aid: str) -> None:
            ids = FLAG_TARGET_MAP.get(aid, [aid])
            for one in ids:
                art = self.get(one)  # raises on unknown
                if one in active:
                    raise CatalogError(f"dependency cycle at {one}")
                if one in seen:
                    continue
                active.add(one)
                for dep in art.depends:
                    add(dep)
                active.remove(one)
                seen.add(one)
                out.append(one)

        for aid in artifact_ids:
            add(aid)
        return out


def catalog_path() -> Path:
    return Path(__file__).resolve().parent / CATALOG_FILENAME


def load_catalog(path: Path | None = None) -> Catalog:
    path = path or catalog_path()
    try:
        with open(path, encoding="utf-8") as f:
            raw = json.load(f)
    except (OSError, ValueError) as e:
        raise CatalogError(f"cannot read catalog {path}: {e}") from e
    try:
        artifacts_raw = raw["artifacts"]
        if not isinstance(artifacts_raw, dict) or not artifacts_raw:
            raise CatalogError("catalog has no artifacts")
        artifacts = {aid: _parse_artifact(aid, a) for aid, a in artifacts_raw.items()}
        catalog = Catalog(
            version=int(raw["catalog_version"]),
            updated=str(raw.get("updated", "")),
            meta=dict(raw.get("meta", {})),
            artifacts=artifacts,
        )
        for aid, art in artifacts.items():
            for dep in art.depends:
                if dep not in artifacts:
                    raise CatalogError(f"artifact {aid}: unknown dependency {dep}")
        for aid in artifacts:
            catalog.closure([aid])
        return catalog
    except KeyError as e:
        raise CatalogError(f"catalog missing {e}") from e
    except (TypeError, ValueError) as e:
        raise CatalogError(f"catalog malformed: {e}") from e


def hf_hub_cache_dir() -> Path:
    """Canonical HF hub cache location, honouring HF_HUB_CACHE / HF_HOME.

    Single source of truth shared by setup_models.py, the status engine and
    the provision manager so detection and download can never disagree
    (the exact mismatch the 2026-10-22 audit flagged).
    """
    override = os.environ.get("HF_HUB_CACHE")
    if override:
        return Path(override).expanduser()
    hf_home = os.environ.get("HF_HOME")
    if hf_home:
        return Path(hf_home).expanduser() / "hub"
    return Path.home() / ".cache" / "huggingface" / "hub"


def hf_cache_snapshot_path(cache_dir: Path, repo_id: str, revision: str, filename: str) -> Path:
    """Expected blob-backed snapshot path for a pinned revision (stdlib only).

    Mirrors huggingface_hub layout ``models--<org>--<name>/snapshots/<sha>/<file>``
    without importing it; the snapshot entry is a symlink to blobs/.
    """
    return cache_dir / f"models--{repo_id.replace('/', '--')}" / "snapshots" / revision / filename
