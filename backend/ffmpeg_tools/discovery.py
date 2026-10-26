"""FFmpeg binary discovery, validation, and repair.

Locates ffmpeg/ffprobe binaries, validates version requirements,
and provides repair/install functionality per platform.
"""
from __future__ import annotations

import logging
import os
import re
import shutil
import subprocess
import sys
from dataclasses import dataclass

logger = logging.getLogger(__name__)

_METADATA_FILENAME = ".video_metadata.json"
_MIN_FFMPEG_MAJOR = 7
_REPO_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
_LOCAL_FFMPEG_BIN = os.path.join(_REPO_ROOT, "tools", "ffmpeg", "bin")

# Repair installs this exact immutable asset: gyan.dev's static full build
# of the tagged stable release 8.1.2, from the versioned GyanD GitHub mirror
# (never a rolling URL, since rolling assets changing under us caused issue
# #184; nightlies also remove deprecated options: -vsync broke imports in
# issue #175, -apply_trc broke exports in the #184 regression report).
#
# When bumping this pin: extract a known clip with -hwaccel cuda and verify
# every EXR frame reads back (see issue #184), run the full FFmpeg operation
# matrix (probe, EXR extract, all stitch codecs) against every build in
# _VERIFIED_BUILD_TOKEN_RES, then update the constants below together.
_PINNED_ASSET = "ffmpeg-8.1.2-full_build.zip"
_PINNED_URL = (
    "https://github.com/GyanD/codexffmpeg/releases/download/8.1.2/"
    + _PINNED_ASSET
)

# On Windows, ONLY tag-exact FFmpeg 8.1.2 full builds are accepted (issue
# #184 round two: a gyan.dev git nightly parsed as release major "2026",
# validation passed, Repair refused to run, and export died on the removed
# -apply_trc option). These are the same upstream source tag from the two
# packagers Windows users actually have, so a user holding either is not
# asked to switch. Everything else (older, newer, nightlies, essentials,
# shared builds, post-tag rolling builds) is rejected and Repair installs
# the pin above. Only builds actually run through the operation matrix
# belong on this list.
#   BtbN tag autobuild prints  "n8.1.2-20260627"  (tag plus 8-digit build
#     date; what the 2.1.4-era Repair installed). Post-tag rolling builds
#     print "n8.1.2-22-g94138f6973-20260710" (commit count plus hash) and
#     must NOT match.
#   gyan full builds print     "8.1.2-full_build-www.gyan.dev" (also what
#     winget installs when it serves a release, and the pin above).
_VERIFIED_BUILD_TOKEN_RES = (
    re.compile(r"^n8\.1\.2(?:-\d{8})?$", re.IGNORECASE),
    re.compile(r"^8\.1\.2-full_build-www\.gyan\.dev$", re.IGNORECASE),
)


# QSettings key for user-configured FFmpeg directory
_QSETTINGS_FFMPEG_DIR = "tools/ffmpeg_custom_dir"


def _get_custom_ffmpeg_dir() -> str | None:
    """Read the user-configured custom FFmpeg directory from QSettings."""
    try:
        from PySide6.QtCore import QSettings
        val = QSettings().value(_QSETTINGS_FFMPEG_DIR, "", type=str)
        return val if val and os.path.isdir(val) else None
    except Exception:
        return None


def _build_windows_search_paths() -> list[str]:
    """Build an extended list of Windows directories to search for FFmpeg."""
    paths = [_LOCAL_FFMPEG_BIN]

    # User-configured custom path (highest priority after bundled)
    custom = _get_custom_ffmpeg_dir()
    if custom:
        paths.append(custom)

    # Standard install locations
    paths.extend([
        r"C:\Program Files\ffmpeg\bin",
        r"C:\Program Files (x86)\ffmpeg\bin",
        r"C:\ffmpeg\bin",
    ])

    # Common user-level locations
    local_app = os.environ.get("LOCALAPPDATA", "")
    if local_app:
        paths.append(os.path.join(local_app, "ffmpeg", "bin"))
        paths.append(os.path.join(local_app, "Programs", "ffmpeg", "bin"))

    # Scoop / Chocolatey / winget
    userprofile = os.environ.get("USERPROFILE", "")
    if userprofile:
        paths.append(os.path.join(userprofile, "scoop", "shims"))
        paths.append(os.path.join(userprofile, "scoop", "apps", "ffmpeg", "current", "bin"))

    # Chocolatey
    choco = os.environ.get("ChocolateyInstall", r"C:\ProgramData\chocolatey")
    paths.append(os.path.join(choco, "bin"))

    # Glob for extracted ffmpeg-* folders in Downloads and Desktop
    if userprofile:
        for parent in ("Downloads", "Desktop"):
            parent_dir = os.path.join(userprofile, parent)
            if os.path.isdir(parent_dir):
                try:
                    for name in os.listdir(parent_dir):
                        if name.lower().startswith("ffmpeg") and os.path.isdir(
                            os.path.join(parent_dir, name)
                        ):
                            candidate = os.path.join(parent_dir, name, "bin")
                            if os.path.isdir(candidate):
                                paths.append(candidate)
                            # Some users extract without the inner folder
                            paths.append(os.path.join(parent_dir, name))
                except OSError:
                    pass

    return paths


# Common install locations per platform
_FFMPEG_SEARCH_PATHS_WINDOWS = _build_windows_search_paths()

_FFMPEG_SEARCH_PATHS_UNIX = [
    _LOCAL_FFMPEG_BIN,
    "/opt/homebrew/bin",        # macOS Homebrew (Apple Silicon)
    "/usr/local/bin",           # macOS Homebrew (Intel) / Linux manual install
    "/usr/bin",                 # Linux system package
    "/snap/bin",                # Linux snap
    os.path.expanduser("~/bin"),
]

_FFMPEG_SEARCH_PATHS = (
    _FFMPEG_SEARCH_PATHS_WINDOWS if sys.platform == "win32"
    else _FFMPEG_SEARCH_PATHS_UNIX
)
# Version TOKEN is the first whitespace-delimited word after "version" in the
# -version banner, e.g. "n8.1.2-20260627", "8.0-full_build-www.gyan.dev",
# "2026-07-09-git-8de8405796-full_build-www.gyan.dev". Classification runs on
# the token only, never the whole line (the Copyright suffix contains years).
_FFMPEG_VERSION_TOKEN_RE = re.compile(
    r"\b(?:ffmpeg|ffprobe)(?:\.exe)?\s+version\s+(?P<token>\S+)",
    re.IGNORECASE,
)

# ALLOWLIST (macOS/Linux only; Windows uses the verified-builds check
# above):
# a token is a tagged stable release only if it starts with an optional "n"
# plus a 1-2 digit major and dotted minors, optionally followed by a -/_
# suffix (vendor tags: "-full_build-www.gyan.dev", "-1ubuntu1", "-latest").
# Everything else is unverifiable. The 1-2 digit major cap keeps date-style
# versions ("2026-07-09-git-...", gyan nightlies) from parsing as major
# 2026. That hole let issue #184 repeat on a gyan git build that sailed
# through validation.
_FFMPEG_RELEASE_TOKEN_RE = re.compile(
    r"^n?(?P<major>\d{1,2})(?:\.\d+){0,3}(?:[-_].*)?$"
)

# Markers of unverifiable snapshot builds: git/master/dev/nightly/rc tags,
# post-tag commit counts with a hash ("-22-g94138f6973"), dash-separated
# snapshot dates ("2026-07-09"), and FFmpeg's N-<rev> master scheme.
# NOTE: the pinned BtbN tag build prints "n8.1.2-20260627", an 8-digit
# date WITHOUT dashes, and must not match here. "dev" gets -/_ boundaries
# only: a dot boundary would match the ".dev" TLD in
# "8.1.2-full_build-www.gyan.dev" and misclassify gyan stable releases.
_FFMPEG_DEV_MARKER_RE = re.compile(
    r"(?:^|[-_.])(?:git|master|nightly|rc\d+)(?:$|[-_.])"
    r"|(?:^|[-_])dev(?:$|[-_])"
    r"|-g[0-9a-f]{6,}(?:$|[-_.])"
    r"|\d{4}-\d{2}-\d{2}"
    r"|^N-\d+",
    re.IGNORECASE,
)


@dataclass(frozen=True)
class FFmpegVersionInfo:
    """Parsed `ffmpeg -version` or `ffprobe -version` first-line summary."""

    first_line: str
    major: int | None
    is_dev_build: bool = False
    # True when the version token matches neither the stable-release shape
    # nor any known dev-build marker. Treated as unverifiable, like a dev
    # build: we cannot promise our command lines work against it.
    is_unknown: bool = False
    # Raw version token ("n8.1.2-20260627"), used for the Windows
    # verified-builds check.
    token: str = ""

    @property
    def is_verified_build(self) -> bool:
        """True when this is one of the verified tag-exact 8.1.2 builds."""
        return any(rx.match(self.token) for rx in _VERIFIED_BUILD_TOKEN_RES)


@dataclass(frozen=True)
class FFmpegValidationResult:
    """Validation result for the current FFmpeg installation."""

    ok: bool
    message: str
    ffmpeg_path: str | None = None
    ffprobe_path: str | None = None
    ffmpeg_version: FFmpegVersionInfo | None = None
    ffprobe_version: FFmpegVersionInfo | None = None
    # True when the installed build is on the known frame-corruption list
    # (issue #184). The UI shows a dedicated "run Repair FFmpeg" warning.
    known_bad: bool = False


def _local_ffmpeg_binary(name: str) -> str | None:
    """Return the bundled repo-local FFmpeg binary if present."""
    ext = ".exe" if sys.platform == "win32" else ""
    candidate = os.path.join(_LOCAL_FFMPEG_BIN, f"{name}{ext}")
    return candidate if os.path.isfile(candidate) else None


def _find_binary(name: str) -> str | None:
    """Locate an FFmpeg-family binary by name (e.g. 'ffmpeg', 'ffprobe')."""
    # 1. Bundled local build
    local = _local_ffmpeg_binary(name)
    if local:
        return local

    # 2. User-configured custom directory
    custom = _get_custom_ffmpeg_dir()
    if custom:
        ext = ".exe" if sys.platform == "win32" else ""
        candidate = os.path.join(custom, f"{name}{ext}")
        if os.path.isfile(candidate):
            return candidate

    # 3. System PATH
    found = shutil.which(name)
    if found:
        return found

    # 4. Platform-specific search paths
    ext = ".exe" if sys.platform == "win32" else ""
    # Rebuild Windows paths each call so a newly-set custom dir is picked up
    search_paths = (
        _build_windows_search_paths() if sys.platform == "win32"
        else _FFMPEG_SEARCH_PATHS_UNIX
    )
    for d in search_paths:
        candidate = os.path.join(d, f"{name}{ext}")
        if os.path.isfile(candidate):
            return candidate
    return None


def find_ffmpeg() -> str | None:
    """Locate ffmpeg binary. Prefer the bundled local build when present."""
    return _find_binary("ffmpeg")


def find_ffprobe() -> str | None:
    """Locate ffprobe binary. Prefer the bundled local build when present."""
    return _find_binary("ffprobe")


def _read_program_version(binary_path: str, program_name: str) -> FFmpegVersionInfo:
    """Run `<program> -version` and parse the first output line."""
    result = subprocess.run(
        [binary_path, "-version"],
        capture_output=True,
        text=True,
        timeout=10,
        creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0,
    )
    if result.returncode != 0:
        stderr = (result.stderr or "").strip()
        raise RuntimeError(
            f"{program_name} failed to report its version: {stderr[:300]}"
        )

    output = result.stdout or result.stderr or ""
    first_line = next((line.strip() for line in output.splitlines() if line.strip()), "")
    if not first_line:
        raise RuntimeError(f"{program_name} did not report a version string")

    token_match = _FFMPEG_VERSION_TOKEN_RE.search(first_line)
    if not token_match:
        raise RuntimeError(
            f"Could not determine {program_name} version from: {first_line}"
        )
    token = token_match.group("token")

    # Dev markers take precedence: a gyan git nightly like
    # "2026-07-09-git-8de8405796-full_build" must never be read as a release.
    if _FFMPEG_DEV_MARKER_RE.search(token):
        return FFmpegVersionInfo(
            first_line=first_line, major=None, is_dev_build=True, token=token,
        )
    release_match = _FFMPEG_RELEASE_TOKEN_RE.match(token)
    if release_match:
        return FFmpegVersionInfo(
            first_line=first_line,
            major=int(release_match.group("major")),
            token=token,
        )
    return FFmpegVersionInfo(
        first_line=first_line, major=None, is_unknown=True, token=token,
    )


def validate_ffmpeg_install(require_probe: bool = True) -> FFmpegValidationResult:
    """Validate FFmpeg/FFprobe availability, age, and Windows build type."""
    ffmpeg = find_ffmpeg()
    if not ffmpeg:
        return FFmpegValidationResult(
            ok=False,
            message=(
                "FFmpeg not found. Keyloom requires FFmpeg 7.0+ and FFprobe. "
                "Install a current FFmpeg build or re-run the installer."
            ),
        )

    ffprobe = find_ffprobe()
    if require_probe and not ffprobe:
        return FFmpegValidationResult(
            ok=False,
            message=(
                "FFprobe not found. Keyloom requires both FFmpeg and FFprobe. "
                "Install a full FFmpeg build or re-run the installer."
            ),
            ffmpeg_path=ffmpeg,
        )

    try:
        ffmpeg_version = _read_program_version(ffmpeg, "ffmpeg")
    except RuntimeError as exc:
        return FFmpegValidationResult(ok=False, message=str(exc), ffmpeg_path=ffmpeg)

    ffprobe_version: FFmpegVersionInfo | None = None
    if require_probe and ffprobe:
        try:
            ffprobe_version = _read_program_version(ffprobe, "ffprobe")
        except RuntimeError as exc:
            return FFmpegValidationResult(
                ok=False,
                message=str(exc),
                ffmpeg_path=ffmpeg,
                ffprobe_path=ffprobe,
                ffmpeg_version=ffmpeg_version,
            )

    programs: list[tuple[str, FFmpegVersionInfo]] = [("FFmpeg", ffmpeg_version)]
    if ffprobe_version is not None:
        programs.append(("FFprobe", ffprobe_version))

    def _fail(message: str, known_bad: bool = False) -> FFmpegValidationResult:
        return FFmpegValidationResult(
            ok=False,
            message=message,
            ffmpeg_path=ffmpeg,
            ffprobe_path=ffprobe,
            ffmpeg_version=ffmpeg_version,
            ffprobe_version=ffprobe_version,
            known_bad=known_bad,
        )

    if sys.platform == "win32":
        # Windows: verified-builds allowlist (_VERIFIED_BUILD_TOKEN_RES).
        # Only tag-exact 8.1.2 full builds pass. Each entry on the list has
        # been run through the full FFmpeg operation matrix. Every other
        # build (older, newer, nightly, essentials, unknown vendor) is
        # rejected and routed to Repair, which installs the pin. Issue #184
        # happened twice because validation tried to enumerate bad builds
        # instead of allowlisting known-good ones.
        for label, info in programs:
            if not info.is_verified_build:
                return _fail(
                    f"{label} is not a build verified for Keyloom. "
                    f"Detected {info.first_line}. Unverified builds have "
                    "caused corrupted frames and failed exports. "
                    "Run Repair FFmpeg to install the verified build.",
                    known_bad=True,
                )
    else:
        # macOS/Linux: package managers cannot pin one exact build, so accept
        # tagged stable releases meeting the version floor and reject
        # unverifiable dev/unknown builds.
        for label, info in programs:
            if info.is_dev_build or info.is_unknown:
                return _fail(
                    f"This {label} build is not a tagged stable release and "
                    "cannot be verified (development builds have caused "
                    "corrupted frames and failed exports). "
                    f"Detected {info.first_line}. "
                    "Run Repair FFmpeg to install a supported build.",
                    known_bad=True,
                )
            if info.major is not None and info.major < _MIN_FFMPEG_MAJOR:
                return _fail(
                    f"{label} {_MIN_FFMPEG_MAJOR}.0 or newer is required. "
                    f"Detected {info.first_line}."
                )

        if (
            ffprobe_version is not None
            and ffmpeg_version.major is not None
            and ffprobe_version.major is not None
            and ffmpeg_version.major != ffprobe_version.major
        ):
            return _fail(
                "FFmpeg and FFprobe major versions do not match. "
                f"Detected ffmpeg {ffmpeg_version.major} and "
                f"ffprobe {ffprobe_version.major}."
            )

    if ffprobe_version is not None:
        summary = (
            f"FFmpeg OK: {ffmpeg_version.first_line} | {ffprobe_version.first_line}"
        )
    else:
        summary = f"FFmpeg OK: {ffmpeg_version.first_line}"

    return FFmpegValidationResult(
        ok=True,
        message=summary,
        ffmpeg_path=ffmpeg,
        ffprobe_path=ffprobe,
        ffmpeg_version=ffmpeg_version,
        ffprobe_version=ffprobe_version,
    )


def require_ffmpeg_install(require_probe: bool = True) -> FFmpegValidationResult:
    """Return the validated FFmpeg install or raise RuntimeError with detail."""
    result = validate_ffmpeg_install(require_probe=require_probe)
    if not result.ok:
        raise RuntimeError(result.message)
    return result


def get_ffmpeg_install_help() -> str:
    """Return concise install guidance for the current platform."""
    if sys.platform == "win32":
        return (
            "Install a full FFmpeg build (ffmpeg.exe and ffprobe.exe)\n"
            "and place its bin folder in tools\\ffmpeg\\bin."
        )
    if sys.platform == "darwin":
        return (
            "Install a current FFmpeg build with Homebrew:\n"
            "    brew install ffmpeg\n\n"
            "Then verify:\n"
            "    ffmpeg -version\n"
            "    ffprobe -version"
        )
    if os.path.isfile("/etc/debian_version"):
        install_cmd = "sudo apt install ffmpeg"
    elif os.path.isfile("/etc/fedora-release"):
        install_cmd = "sudo dnf install ffmpeg"
    elif os.path.isfile("/etc/arch-release"):
        install_cmd = "sudo pacman -S ffmpeg"
    else:
        install_cmd = "Install ffmpeg with your package manager"
    return (
        f"{install_cmd}\n\n"
        "Then verify:\n"
        "    ffmpeg -version\n"
        "    ffprobe -version"
    )
