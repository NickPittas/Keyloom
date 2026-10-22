"""Mock selfcheck for win_smoke_worker timeout bookkeeping. Local only (no ssh, no GPU, no GUI).

Run: timeout 60 python3 rust-ui/tools/test_win_smoke_worker.py
Verifies: per-read 15s cap, absolute monotonic stage deadlines, fail-fast on terminal/rejected,
assert_exit failing on nonzero/'STILL RUNNING', cancel bookkeeping, and the required --stage-budget.
"""
import importlib.util
import queue
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).parent
spec = importlib.util.spec_from_file_location("wsw", HERE / "win_smoke_worker.py")
wsw = importlib.util.module_from_spec(spec)
spec.loader.exec_module(wsw)


class FakeStdin:
    def __init__(self): self.sent = []
    def write(self, s): self.sent.append(s)
    def flush(self): pass
    def close(self): pass


class FakeProc:
    def __init__(self, rc=0): self.stdin, self.rc = FakeStdin(), rc
    def poll(self): return self.rc
    def wait(self, timeout=None): return self.rc
    def kill(self): pass


class SpyQ:  # records the timeout read() actually asked for
    def __init__(self, items=None): self.items, self.asked = list(items or []), []
    def get(self, timeout=None):
        self.asked.append(timeout)
        if self.items:
            return self.items.pop(0)
        raise queue.Empty


def worker(items=None, rc=0):
    import json
    w = object.__new__(wsw.Worker)
    w.q, w.events, w.err, w.t0 = SpyQ([json.dumps(i) for i in (items or [])]), [], [], time.time()
    w.p = FakeProc(rc)
    return w


def expect(kind, fn):
    try:
        fn()
    except kind:
        return
    except Exception as e:  # noqa: BLE001
        raise AssertionError(f"expected {kind.__name__}, got {type(e).__name__}: {e}")
    raise AssertionError(f"expected {kind.__name__}, nothing raised")


# 1. read() clamps any requested timeout to WAIT_CAP (even when it then times out)
w = worker()
expect(wsw.Timeout, lambda: w.read(99))
assert w.q.asked[-1] <= wsw.WAIT_CAP, w.q.asked[-1]
print("ok 1 read clamps to WAIT_CAP =", wsw.WAIT_CAP)

# 2. read() raises Timeout on silence
expect(wsw.Timeout, lambda: worker().read(0.05))
print("ok 2 read Timeout on silence")

# 3. read_until fails fast on rejected (never spins to the deadline)
w = worker([{"event": "rejected", "code": "exists"}])
end = time.monotonic() + 30
expect(SystemExit, lambda: w.read_until(lambda e: e["event"] == "accepted", end))
print("ok 3 read_until fails fast on rejected")

# 4. read_until raises Timeout when the absolute monotonic budget is exhausted
w = worker([])
end = time.monotonic() + 0.2
expect(wsw.Timeout, lambda: w.read_until(lambda e: e["event"] == "accepted", end))
print("ok 4 read_until Timeout on budget exhaustion")

# 5. drain_terminal: budget exhausted -> Timeout, quickly (monotonic deadline honoured)
w = worker([])
end = time.monotonic() + 0.2
t = time.monotonic()
expect(wsw.Timeout, lambda: w.drain_terminal(end))
assert time.monotonic() - t < 2.0, "deadline overshot"
print("ok 5 drain_terminal Timeout bounded by monotonic deadline")

# 6. drain_terminal returns the terminal done event
w = worker([{"event": "progress", "done": 1}, {"event": "done", "outcome": "cancelled"}])
e = w.drain_terminal(time.monotonic() + 5)
assert e["event"] == "done" and e["outcome"] == "cancelled", e
print("ok 6 drain_terminal returns terminal done")

# 7. assert_exit fails on nonzero and on 'STILL RUNNING' (never reports green)
expect(AssertionError, lambda: worker(rc=1).assert_exit("x"))
expect(AssertionError, lambda: worker(rc="STILL RUNNING after 15s").assert_exit("x"))
assert worker(rc=0).assert_exit("x") == 0
print("ok 7 assert_exit fails nonzero / still-running, passes 0")

# 8. run_job: cancel sent at threshold, terminal returned, send recorded on the real pipe
items = [{"event": "accepted"}, {"event": "progress", "phase": "publish", "done": 2},
         {"event": "done", "outcome": "cancelled"}]
w = worker(items)
j = {"job_id": "w2", "outputs": {}, "range": {}, "manifest_dir": "x", "clip": {}}
e = w.run_job(j, time.monotonic() + 5, cancel_at=2)
assert e["outcome"] == "cancelled", e
assert any('"cmd": "cancel"' in s for s in w.p.stdin.sent), w.p.stdin.sent
print("ok 8 run_job cancel bookkeeping + terminal return")

# 9. run_job: budget exhaustion -> Timeout
w = worker([])
expect(wsw.Timeout, lambda: w.run_job({"job_id": "z"}, time.monotonic() + 0.1))
print("ok 9 run_job Timeout on budget exhaustion")

# 10. real parser: GPU stage without --stage-budget must error (rc 2) before any ssh use
r = subprocess.run([sys.executable, str(HERE / "win_smoke_worker.py"), "--sock", "x",
                    "--host", "h", "--task", "t", "--python", "p", "--models", "m",
                    "--stage", "job"], capture_output=True, text=True, timeout=30)
assert r.returncode == 2 and "--stage-budget" in r.stderr, (r.returncode, r.stderr[-200:])
print("ok 10 parser rejects GPU stage without --stage-budget")

# 11. launch_command: byte-identical production launcher by default; prefix only with --models-env
class _A: pass
a = _A(); a.task, a.python, a.gpu, a.models, a.models_env = "T", "P", 0, "M", False
bare = wsw.launch_command(a)
assert bare == wsw.remote_command("T" + chr(92) + "repo", "P", 0), bare
assert "set CORRIDORKEY" not in bare
a.models_env = True
assert wsw.launch_command(a).startswith("set CORRIDORKEY_V2_MODELS_DIR=M&& ")
print("ok 11 launch_command bare production default, prefix only with --models-env")

print("SELFTEST PASS (11 checks)")
