"""Cross-process ownership for a canonical model destination."""
from __future__ import annotations

import errno
import hashlib
import os
import stat
import tempfile
from pathlib import Path


class DestinationBusyError(RuntimeError):
    pass


def _is_link(path: Path) -> bool:
    try:
        info = path.lstat()
    except FileNotFoundError:
        return False
    return stat.S_ISLNK(info.st_mode) or bool(
        getattr(info, "st_file_attributes", 0)
        & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0)
    )


def _lock_path(destination: Path) -> Path:
    dest = Path(os.path.abspath(destination))
    for ancestor in reversed((dest, *dest.parents)):
        if _is_link(ancestor):
            raise OSError(errno.ELOOP, "model destination contains a link or reparse point", str(ancestor))
    canonical = os.path.normcase(str(dest.resolve(strict=False)))
    digest = hashlib.sha256(canonical.encode("utf-8", "surrogatepass")).hexdigest()
    owner = str(os.getuid()) if hasattr(os, "getuid") else hashlib.sha256(
        os.environ.get("USERNAME", os.environ.get("USER", "user")).encode()
    ).hexdigest()[:16]
    shared_root = Path(tempfile.gettempdir()) / "corridorkey-model-locks"
    for ancestor in reversed((shared_root, *shared_root.parents)):
        if _is_link(ancestor):
            raise OSError(errno.ELOOP, "model lock path contains a link or reparse point", str(ancestor))
    lock_root = shared_root / owner
    lock_root.mkdir(parents=True, exist_ok=True, mode=0o700)
    if _is_link(lock_root) or not lock_root.is_dir():
        raise OSError(errno.ELOOP, "model lock directory is not a real directory", str(lock_root))
    if os.name != "nt":
        info = lock_root.stat()
        if info.st_uid != os.getuid():
            raise PermissionError(errno.EACCES, "model lock directory belongs to another user", str(lock_root))
        if stat.S_IMODE(info.st_mode) & 0o077:
            os.chmod(lock_root, 0o700)
    return lock_root / f"{digest}.lock"


class DestinationLock:
    """Kernel-owned shared reader or exclusive writer lock; lock files persist."""

    def __init__(self, destination: str | Path, *, shared: bool = False):
        self.destination = Path(destination)
        self.shared = shared
        self.path: Path | None = None
        self._file = None
        self._overlapped = None
        self._fcntl = None  # kept from acquire: imports fail during interpreter shutdown

    def acquire(self) -> "DestinationLock":
        path = self.path = _lock_path(self.destination)
        path.parent.mkdir(parents=True, exist_ok=True)
        if _is_link(path):
            raise OSError(errno.ELOOP, "model lock path is a link or reparse point", str(path))
        flags = os.O_CREAT | os.O_RDWR | getattr(os, "O_NOFOLLOW", 0)
        fd = os.open(path, flags, 0o600)
        self._file = os.fdopen(fd, "r+b", buffering=0)
        try:
            if os.name == "nt":
                self._windows_lock()
            else:
                import fcntl

                self._fcntl = fcntl
                mode = fcntl.LOCK_SH if self.shared else fcntl.LOCK_EX
                fcntl.flock(fd, mode | fcntl.LOCK_NB)
        except OSError as exc:
            self._file.close()
            self._file = None
            if exc.errno in (errno.EACCES, errno.EAGAIN, errno.EDEADLK) or getattr(exc, "winerror", None) in (33, 36):
                raise DestinationBusyError(f"model destination is in use: {self.destination}") from exc
            raise
        return self

    def _windows_lock(self) -> None:
        import ctypes
        from ctypes import wintypes
        import msvcrt

        class OVERLAPPED(ctypes.Structure):
            _fields_ = [
                ("Internal", ctypes.c_size_t), ("InternalHigh", ctypes.c_size_t),
                ("Offset", wintypes.DWORD), ("OffsetHigh", wintypes.DWORD),
                ("hEvent", wintypes.HANDLE),
            ]

        overlapped = OVERLAPPED()
        flags = 0x1 | (0 if self.shared else 0x2)  # fail immediately; exclusive for writers
        handle = msvcrt.get_osfhandle(self._file.fileno())
        ok = ctypes.WinDLL("kernel32", use_last_error=True).LockFileEx(
            wintypes.HANDLE(handle), wintypes.DWORD(flags), wintypes.DWORD(0),
            wintypes.DWORD(1), wintypes.DWORD(0), ctypes.byref(overlapped)
        )
        if not ok:
            error = ctypes.get_last_error()
            raise OSError(error, "LockFileEx failed", str(self.path), None, error)
        self._overlapped = overlapped

    def close(self) -> None:
        if self._file is None:
            return
        try:
            if os.name == "nt" and self._overlapped is not None:
                import ctypes
                from ctypes import wintypes
                import msvcrt

                ctypes.WinDLL("kernel32", use_last_error=True).UnlockFileEx(
                    wintypes.HANDLE(msvcrt.get_osfhandle(self._file.fileno())),
                    wintypes.DWORD(0), wintypes.DWORD(1), wintypes.DWORD(0),
                    ctypes.byref(self._overlapped),
                )
            elif self._fcntl is not None:
                self._fcntl.flock(self._file.fileno(), self._fcntl.LOCK_UN)
        finally:
            self._file.close()
            self._file = None
            self._overlapped = None
            self._fcntl = None

    def __enter__(self) -> "DestinationLock":
        return self.acquire()

    def __exit__(self, *_exc) -> None:
        self.close()

    def __del__(self):
        self.close()
