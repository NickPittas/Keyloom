"""packaging/run_monitored.py: tiny fixture children only; each test finishes in a few seconds."""
import os
import re
import subprocess
import sys
import time
from pathlib import Path

import pytest

TOOL = Path(__file__).resolve().parents[1] / "packaging" / "run_monitored.py"


def mon(deadline, code, *extra, label="fixture", env=None):
    p = subprocess.run([sys.executable, str(TOOL), "--deadline", str(deadline), "--label", label, *extra,
                        "--", sys.executable, "-c", code],
                       capture_output=True, text=True, timeout=10, env=env)
    return p.returncode, p.stdout


def test_success_streams_lines_and_final_status():
    rc, out = mon(5, "import sys; print('hello'); print('err', file=sys.stderr)")
    assert rc == 0
    assert "| hello" in out and "| err" in out
    assert re.search(r"exit code=0 label='fixture' elapsed=", out)


def test_failure_exit_code_is_propagated():
    rc, out = mon(5, "raise SystemExit(7)")
    assert rc == 7 and "exit code=7" in out


def test_silent_child_gets_heartbeats_labelled_not_progress():
    rc, out = mon(6, "import time; time.sleep(2.5)", "--phase", "quiet-phase")
    assert rc == 0
    beats = re.findall(r"heartbeat phase=quiet-phase running, quiet \d+s \(supervisor alive; not child progress\)", out)
    assert len(beats) >= 2


def test_deadline_kills_descendants_and_fails():
    code = ("import subprocess,sys,time;"
            "c=subprocess.Popen([sys.executable,'-c','import time;time.sleep(60)']);"
            "print('GC',c.pid,flush=True);time.sleep(60)")
    t = time.monotonic()
    rc, out = mon(2, code)
    assert rc == 124 and "DEADLINE" in out
    assert time.monotonic() - t < 8
    pid = int(re.search(r"\| GC (\d+)", out).group(1))
    for _ in range(20):  # reaped by init may lag a moment
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            break
        time.sleep(0.1)
    else:
        pytest.fail("grandchild survived the deadline")


def test_descendant_holding_pipe_after_child_exit_is_cleaned():
    code = ("import subprocess,sys;"
            "subprocess.Popen([sys.executable,'-c','import time;time.sleep(60)']);print('done')")
    t = time.monotonic()
    rc, out = mon(8, code)
    assert rc == 0 and time.monotonic() - t < 6


def test_no_secret_in_env_or_argv_display():
    env = {**os.environ, "KEYLOOM_SECRET_TOKEN": "env-s3cret"}
    p = subprocess.run([sys.executable, str(TOOL), "--deadline", "5", "--label", "safe", "--",
                        sys.executable, "-c", "pass", "--token=argv-s3cret"],
                       capture_output=True, text=True, timeout=10, env=env)
    assert "s3cret" not in p.stdout + p.stderr and "--token" not in p.stdout
    assert "label='safe'" in p.stdout


def test_missing_command_reports_without_argv():
    p = subprocess.run([sys.executable, str(TOOL), "--deadline", "5", "--", "/nonexistent/x", "--pw=s3cret"],
                       capture_output=True, text=True, timeout=10)
    assert p.returncode == 127 and "s3cret" not in p.stdout


def test_deadline_is_required():
    p = subprocess.run([sys.executable, str(TOOL), "--", sys.executable, "-c", "pass"],
                       capture_output=True, text=True, timeout=10)
    assert p.returncode == 2


def gone(pid):
    for _ in range(30):
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return True
        time.sleep(0.1)
    return False


# a grandchild that moves itself into ANOTHER session (killpg on the child's group cannot reach it)
SETSID_TREE = ("import subprocess,sys,time;"
               "c=subprocess.Popen([sys.executable,'-c','import time;time.sleep(60)'],start_new_session=True);"
               "print('GC',c.pid,flush=True);time.sleep(60)")


def test_deadline_kills_descendant_in_nested_new_session():
    rc, out = mon(2, SETSID_TREE)
    assert rc == 124
    assert gone(int(re.search(r"\| GC (\d+)", out).group(1))), "nested-session grandchild survived"


def test_sigterm_to_supervisor_cleans_tree_and_exits_143():
    p = subprocess.Popen([sys.executable, "-u", str(TOOL), "--deadline", "8", "--", sys.executable, "-c", SETSID_TREE],
                         stdout=subprocess.PIPE, text=True)
    pid = None
    for line in p.stdout:
        m = re.search(r"\| GC (\d+)", line)
        if m:
            pid = int(m.group(1))
            break
    assert pid, "fixture did not report its grandchild"
    p.terminate()
    assert p.wait(timeout=5) == 143
    assert gone(pid), "grandchild survived SIGTERM of the supervisor"


def test_interactive_keeps_callers_session_and_cleans_only_own_descendants():
    code = "import os; print('SID', os.getsid(0), 'PGID', os.getpgid(0), flush=True)"
    base = [sys.executable, str(TOOL), "--deadline", "5"]
    here = f"SID {os.getsid(0)} PGID {os.getpgid(0)}"
    assert here in subprocess.run([*base, "--interactive", "--", sys.executable, "-c", code],
                                  capture_output=True, text=True, timeout=10).stdout
    assert here not in subprocess.run([*base, "--", sys.executable, "-c", code],
                                      capture_output=True, text=True, timeout=10).stdout
    # deadline in interactive mode: grandchild dies, and the supervisor (same group) survives to report 124 -> no killpg
    p = subprocess.run([sys.executable, str(TOOL), "--deadline", "2", "--interactive", "--", sys.executable, "-c", SETSID_TREE],
                       capture_output=True, text=True, timeout=10)
    assert p.returncode == 124 and "DEADLINE FAILED" in p.stdout
    assert gone(int(re.search(r"\| GC (\d+)", p.stdout).group(1)))


def test_stdin_is_devnull_by_default_and_forwarded_when_interactive():
    code = "import sys; print('got', repr(sys.stdin.readline()))"
    base = [sys.executable, str(TOOL), "--deadline", "5"]
    p = subprocess.run([*base, "--", sys.executable, "-c", code], input="secret-pw\n", capture_output=True, text=True, timeout=10)
    assert "got ''" in p.stdout
    p = subprocess.run([*base, "--interactive", "--", sys.executable, "-c", code], input="answer\n", capture_output=True, text=True, timeout=10)
    assert "got 'answer\\n'" in p.stdout
