#!/usr/bin/env python3
"""Foreground command supervisor: explicit deadline, line streaming, 1 s heartbeat, whole-tree cleanup.

    python packaging/run_monitored.py --deadline 10 [--label NAME] [--phase NAME] -- cmd arg...

Output is `[hh:mm:ss +elapsed] ...` lines on stdout, flushed. Child stdout+stderr are merged and streamed as `| line`.
A `heartbeat` line every second proves only that THIS supervisor is alive and how long the child has been quiet; it is
NOT child progress. Exit: child's code (signal N -> 128+N), 124 on deadline (tree killed), 127 if it cannot start,
2 on bad usage. The argv and the environment are never printed (only --label, default: the program's basename), but
the child's own output is passed through verbatim and cannot be redacted.
Default (non-interactive): stdin is /dev/null and the child gets its own session. --interactive (Linux only; refused on
Windows): the child inherits stdin AND stays in this process's session/process group, so it keeps the controlling
terminal and can ask for a password on /dev/tty itself (sudo, UFW). This tool never reads, forwards, logs or stores
input. Its stdout/stderr are still piped (an unterminated prompt shows within 1 s). Untested here with a real tty.
Cleanup on deadline, SIGTERM/SIGHUP/Ctrl-C and after exit: every descendant found through /proc PPid, even after
setsid(), is SIGSTOPped, then SIGKILLed; in default mode the child's own process group too. In interactive mode there
is NO killpg (that group is the caller's terminal group), so only that child's own descendants are signalled. A
descendant already reparented to init before the scan cannot be found; this is not a container. The already-reaped
child pid itself is never signalled.
Windows: UNTESTED best effort (CREATE_NEW_PROCESS_GROUP + `taskkill /T /F`); a warning is printed, do not rely on it.
No background monitor: the supervisor is this one foreground process.
"""
from __future__ import annotations

import argparse
import os
import signal
import subprocess
import sys
import threading
import time

IS_WIN = os.name == "nt"


def kill_tree(p: subprocess.Popen, own_group: bool = True) -> None:
    try:
        if IS_WIN:
            subprocess.run(["taskkill", "/PID", str(p.pid), "/T", "/F"], capture_output=True, timeout=5)
        else:
            seen: set[int] = set()
            skip = {p.pid} if p.returncode is not None else set()  # reaped: its pid may already belong to someone else
            for _ in range(5):  # SIGSTOP what we find so it cannot fork away, rescan until the tree is stable
                pids = tree_pids(p.pid) - seen - skip
                if not pids:
                    break
                seen |= pids
                for pid in pids:
                    _signal(pid, signal.SIGSTOP)
            for pid in seen:
                _signal(pid, signal.SIGKILL)
            if own_group:  # group leader == p.pid (start_new_session); interactive children share OUR group: never killpg
                os.killpg(p.pid, signal.SIGKILL)
    except (OSError, subprocess.SubprocessError):
        pass


def _signal(pid: int, sig: int) -> None:
    try:
        os.kill(pid, sig)
    except OSError:
        pass


def tree_pids(root: int) -> set[int]:
    """root and all its descendants by /proc PPid, regardless of session/process group (Linux)."""
    kids: dict[int, list[int]] = {}
    for name in os.listdir("/proc"):
        if name.isdigit():
            try:
                with open(f"/proc/{name}/stat", "rb") as f:
                    ppid = int(f.read().rsplit(b")", 1)[1].split()[1])  # after "(comm)": state ppid ...
            except (OSError, ValueError, IndexError):
                continue
            kids.setdefault(ppid, []).append(int(name))
    out, todo = set(), [root]
    while todo:
        pid = todo.pop()
        if pid not in out:
            out.add(pid)
            todo += kids.get(pid, [])
    return out


def run(argv: list[str], deadline: float, label: str, phase: str = "run", out=None, cwd=None, env=None,
        log=None, info: dict | None = None, interactive: bool = False) -> int:
    """Library entry (build_release.Build.run). `log`: extra text file receiving every line (kept even if the console
    breaks); `info["timed_out"]` tells a real deadline kill from a child that itself exited 124."""
    out = out or sys.stdout
    t0 = time.monotonic()
    lock = threading.Lock()
    last_out = [t0]
    partial = [""]  # output without a newline yet: shown at the next heartbeat so a prompt/progress bar is not invisible

    def say(msg: str) -> None:
        line = f"[{time.strftime('%H:%M:%S')} +{time.monotonic() - t0:5.1f}s] {msg}\n"
        with lock:
            for f in (log, out):
                if f is not None:
                    f.write(line)
                    f.flush()

    say(f"start label={label!r} phase={phase} deadline={deadline:g}s")
    if IS_WIN:
        say("WARNING: Windows process-tree cleanup is untested best effort")
    if interactive and IS_WIN:
        say("REFUSED: --interactive is not supported on Windows (terminal/job handling unproven)")
        return 127
    own = not interactive  # interactive: same session/group as the caller, so the controlling tty stays usable
    kw = {"creationflags": subprocess.CREATE_NEW_PROCESS_GROUP} if IS_WIN else {"start_new_session": own}
    try:
        p = subprocess.Popen(argv, cwd=cwd, env=env, stdin=None if interactive else subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, **kw)
    except OSError as e:  # strerror only: it may not embed argv
        say(f"FAILED to start label={label!r}: {e.strerror or type(e).__name__}")
        return 127

    def pump() -> None:
        fd = p.stdout.fileno()
        while chunk := os.read(fd, 65536):  # chunks, not readline(): a newline-less writer never blocks the heartbeat
            last_out[0] = time.monotonic()
            with lock:
                partial[0] += chunk.decode("utf-8", "replace")
                *lines, partial[0] = partial[0].replace("\r\n", "\n").split("\n")
            for ln in lines:
                say("| " + ln)
        with lock:
            rest, partial[0] = partial[0], ""
        if rest:
            say("| " + rest)

    reader = threading.Thread(target=pump, daemon=True)
    reader.start()
    rc = None
    try:
        while rc is None:
            left = deadline - (time.monotonic() - t0)
            if left <= 0:
                say(f"DEADLINE {deadline:g}s exceeded phase={phase}: killing process tree")
                kill_tree(p, own)
                p.wait()
                rc = 124
                if info is not None:
                    info["timed_out"] = True
                break
            try:
                rc = p.wait(timeout=min(1.0, left))
            except subprocess.TimeoutExpired:
                with lock:
                    rest, partial[0] = partial[0], ""
                if rest:
                    say("| " + rest + "  [unterminated line]")
                say(f"heartbeat phase={phase} running, quiet {time.monotonic() - last_out[0]:.0f}s (supervisor alive; not child progress)")
    except BaseException:
        kill_tree(p, own)
        p.wait()
        raise
    kill_tree(p, own)  # leftover descendants of a finished child would hold the pipe open
    reader.join(2)
    if rc < 0:
        rc = 128 - rc
    say(f"{'DEADLINE FAILED' if info and info.get('timed_out') or (info is None and rc == 124) else 'exit'} code={rc} label={label!r} elapsed={time.monotonic() - t0:.1f}s")
    return rc


def main(args: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--deadline", type=float, required=True, help="hard seconds limit (required, >0)")
    ap.add_argument("--label", help="safe display name (default: basename of program); argv is never printed")
    ap.add_argument("--phase", default="run", help="static phase text shown in heartbeats")
    ap.add_argument("--interactive", action="store_true", help="child inherits stdin and keeps this terminal/session (no new session, no killpg); default: /dev/null + own session")
    ap.add_argument("cmd", nargs=argparse.REMAINDER, help="-- program and args")
    a = ap.parse_args(args)
    cmd = a.cmd[1:] if a.cmd[:1] == ["--"] else a.cmd
    if not cmd or a.deadline <= 0:
        ap.error("need --deadline > 0 and a command after --")
    if not IS_WIN:  # run() kills the child tree on any BaseException, so turn termination signals into one
        for sig in (signal.SIGTERM, signal.SIGHUP):
            signal.signal(sig, lambda n, _f: sys.exit(128 + n))
    try:
        return run(cmd, a.deadline, a.label or os.path.basename(cmd[0]), a.phase, interactive=a.interactive)
    except KeyboardInterrupt:
        return 130


if __name__ == "__main__":
    sys.exit(main())
