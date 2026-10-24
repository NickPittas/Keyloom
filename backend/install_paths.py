"""Installed-runtime descriptor: the one JSON contract shared with ``rust-ui/src/runtime.rs``.

A packaged install is relocatable: every root in ``keyloom-install.json`` resolves against the
descriptor's own folder at run time (lexical normalization only, no ``~`` expansion, no symlink
resolution — the Rust side behaves identically). A developer checkout keeps working unchanged;
a descriptor, once present, wins over checkout discovery, and a BROKEN descriptor raises
:class:`InstallDescriptorError` instead of silently falling back to some other location.

Stdlib only. Contract: docs/design/rust-ui/install-descriptor.md.
"""
from __future__ import annotations

import os
from dataclasses import dataclass
from pathlib import Path

DESCRIPTOR_NAME = "keyloom-install.json"
LEGACY_DESCRIPTOR_NAME = "corridorkey-install.json"  # installs made before the Keyloom rename
CK_INSTALL_ENV = "CK_INSTALL"
MODELS_DIR_ENV = "CORRIDORKEY_V2_MODELS_DIR"
SCHEMA = 1

_ALLOWED_TOP = frozenset({"schema", "app_name", "app_version", "roots"})
_ALLOWED_ROOTS = frozenset({"python", "helper", "models", "cache", "scratch", "user_data"})


def descriptor_in(directory: Path) -> Path:
    """The descriptor in `directory`: the Keyloom name, else a pre-rename one, else the Keyloom path."""
    legacy = directory / LEGACY_DESCRIPTOR_NAME
    if not (directory / DESCRIPTOR_NAME).is_file() and legacy.is_file():
        return legacy
    return directory / DESCRIPTOR_NAME


def user_area(base: Path, sub: str = "") -> Path:
    """`base/Keyloom[/sub]`, unless only the pre-rename `base/CorridorKey` exists (models/data stay where they are)."""
    old = base / "CorridorKey"
    return (old if old.is_dir() and not (base / "Keyloom").exists() else base / "Keyloom") / sub


class InstallDescriptorError(RuntimeError):
    """A descriptor was found but is unusable. Never fall back silently; surface this."""


@dataclass(frozen=True)
class InstallPaths:
    descriptor: Path
    app_root: Path
    python: Path
    helper: Path
    models: Path | None
    cache: Path | None
    scratch: Path | None
    user_data: Path | None


def _normalize(p: Path) -> Path:
    return Path(os.path.normpath(str(p)))


def _resolve(app_root: Path, value: str) -> Path:
    p = Path(value)
    if not p.is_absolute():
        p = app_root / p
    return _normalize(p)


def load(path: str | Path) -> InstallPaths:
    """Parse and resolve a descriptor. Errors name the file and field, and say what to do."""
    path = _normalize(Path(path))
    try:
        raw_bytes = path.read_bytes()
    except OSError as e:
        raise InstallDescriptorError(f"cannot read {path}: {e}") from e
    import json

    try:
        raw = json.loads(raw_bytes)
    except ValueError as e:
        raise InstallDescriptorError(f"invalid JSON in {path}: {e}") from e
    if not isinstance(raw, dict):
        raise InstallDescriptorError(f"{path} must contain a JSON object")
    unknown_top = sorted(set(raw) - _ALLOWED_TOP)
    if unknown_top:
        raise InstallDescriptorError(
            f'unknown field "{", ".join(unknown_top)}" in {path}: this Keyloom is older '
            f"than the install; update the app (or point {CK_INSTALL_ENV} at a matching install)."
        )
    schema = raw.get("schema")
    if not isinstance(schema, int) or isinstance(schema, bool):
        raise InstallDescriptorError(
            f"missing or non-integer schema in {path} (expected {SCHEMA}); reinstall or repair "
            "the installation."
        )
    if schema != SCHEMA:
        raise InstallDescriptorError(
            f"schema version {schema} in {path}, this build reads {SCHEMA}: the install was made "
            f"by a newer Keyloom; update the app (or point {CK_INSTALL_ENV} at a matching install)."
        )
    for key in ("app_name", "app_version"):
        if key in raw and not isinstance(raw[key], str):
            raise InstallDescriptorError(f"{key} must be text in {path}")
    roots = raw.get("roots")
    if not isinstance(roots, dict):
        raise InstallDescriptorError(f"missing roots object in {path}")
    unknown_roots = sorted(set(roots) - _ALLOWED_ROOTS)
    if unknown_roots:
        raise InstallDescriptorError(
            f'unknown roots field "{", ".join(unknown_roots)}" in {path}: this Keyloom is '
            "older than the install; update the app."
        )

    def opt(key: str) -> Path | None:
        v = roots.get(key)
        if v is None:
            return None
        if not isinstance(v, str) or not v.strip():
            raise InstallDescriptorError(
                f"roots.{key} must be a non-empty path or null in {path}"
            )
        return _resolve(app_root, v)

    def req(key: str) -> Path:
        v = roots.get(key)
        if not isinstance(v, str) or not v.strip():
            raise InstallDescriptorError(
                f"roots.python and roots.helper are required in {path}; reinstall or repair "
                "the installation."
            )
        return _resolve(app_root, v)

    app_root = path.parent
    return InstallPaths(
        descriptor=path,
        app_root=app_root,
        python=req("python"),
        helper=req("helper"),
        models=opt("models"),
        cache=opt("cache"),
        scratch=opt("scratch"),
        user_data=opt("user_data"),
    )


def _default_search_start() -> Path:
    """Helper root (repo in dev, ``<install>/app`` packaged): the descriptor lives at or above it."""
    return Path(__file__).resolve().parent.parent


def _ancestors(start: Path, levels: int):
    p = start
    for _ in range(levels):
        yield p
        if p.parent == p:
            return
        p = p.parent


def discover(search_from: Path | None = None) -> InstallPaths | None:
    """Find the install descriptor. ``None`` = not an installed app (developer checkout).

    Raises :class:`InstallDescriptorError` when a descriptor is present but unusable, or when
    ``CK_INSTALL`` points somewhere without one: an install must never silently fall back.
    """
    env = os.environ.get(CK_INSTALL_ENV)
    if env:
        p = Path(env)
        cand = p if p.is_file() else descriptor_in(p)
        if not cand.is_file():
            raise InstallDescriptorError(
                f"{CK_INSTALL_ENV} points at {p}, but there is no {DESCRIPTOR_NAME} there; fix "
                f"{CK_INSTALL_ENV} or unset it."
            )
        return load(cand)
    start = _default_search_start() if search_from is None else Path(search_from)
    for directory in _ancestors(start, 4):
        cand = descriptor_in(directory)
        if cand.is_file():
            return load(cand)
    return None


def setup_helper_env(base: dict[str, str] | None = None) -> dict[str, str]:
    """Environment for setup/model helper processes: they see the same roots as the app.

    Passes ``CK_INSTALL`` through and points ``CORRIDORKEY_V2_MODELS_DIR`` at the descriptor's
    models root — but never overrides values the caller (or user) already set explicitly.
    Raises :class:`InstallDescriptorError` on a broken descriptor: no silent fallback.
    """
    env = dict(os.environ if base is None else base)
    install = discover()
    if install is None:
        return env
    env.setdefault(CK_INSTALL_ENV, str(install.descriptor))
    if install.models is not None:
        env.setdefault(MODELS_DIR_ENV, str(install.models))
    return env


def windows_user_data(appdata: str | None, local: str | None) -> Path | None:
    """Windows user area, no HOME involved: roaming APPDATA, then LOCALAPPDATA, then `Keyloom`."""
    base = appdata or local
    return user_area(Path(base)) if base else None


def unix_user_data(xdg: str | None, home: str | None) -> Path | None:
    if xdg:
        return user_area(Path(xdg))
    return user_area(Path(home) / ".local/share") if home else None


def platform_user_data() -> Path | None:
    """User area for app data (not settings). Windows needs no HOME: APPDATA, then LOCALAPPDATA."""
    if os.name == "nt":
        return windows_user_data(
            os.environ.get("APPDATA"), os.environ.get("LOCALAPPDATA")
        )
    return unix_user_data(os.environ.get("XDG_DATA_HOME"), os.environ.get("HOME"))


def windows_cache(local: str | None, appdata: str | None) -> Path | None:
    base = local or appdata
    return user_area(Path(base), "cache") if base else None


def unix_cache(xdg: str | None, home: str | None) -> Path | None:
    if xdg:
        return user_area(Path(xdg))
    return user_area(Path(home) / ".cache") if home else None


def platform_cache() -> Path | None:
    if os.name == "nt":
        return windows_cache(
            os.environ.get("LOCALAPPDATA"), os.environ.get("APPDATA")
        )
    return unix_cache(os.environ.get("XDG_CACHE_HOME"), os.environ.get("HOME"))


def platform_scratch() -> Path | None:
    ud = platform_user_data()
    return ud / "scratch" if ud else None
