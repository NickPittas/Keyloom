"""Manual smoke (NOT a pytest test): production Windows launcher -> real worker over ssh.

Driver runs on Linux and uses an already-authenticated ssh ControlMaster socket:
    python3 win_smoke_worker.py --sock /tmp/ck-windows-smoke-ssh --host nick@192.168.100.152 \
        --task 'C:\\Users\\Nick\\ck-wsmoke-1022a' --python <remote python.exe> --models <remote models dir> \
        [--expect-sha256 <corridorkey_v2 sha>] [--gpu 0]
`<task>\\repo` must hold the worker checkout; everything is written below `<task>\\work`.
Launch string is byte-identical to rust-ui/src/worker.rs `remote_command` (Windows): one encoded PowerShell.
"""
import base64, json, queue, subprocess, sys, threading, time, argparse

FIXTURE_SRC = r'''
import os, sys
os.environ["OPENCV_IO_ENABLE_OPENEXR"] = "1"
import cv2, numpy as np
root = sys.argv[1]; N, A, B, S = 12, 3, 9, 256
os.makedirs(root + r"\Frames", exist_ok=True); os.makedirs(root + r"\AlphaHint", exist_ok=True)
yy, xx = np.mgrid[0:S, 0:S]
for i in range(N):
    s = "shot_%d" % (1001 + i)
    if not A <= i <= B:
        for d in ("Frames", "AlphaHint"):
            open(root + "\\%s\\%s.png" % (d, s), "wb").write(b"POISON")
        continue
    disc = ((xx - (90 + 8 * i)) ** 2 + (yy - 128) ** 2) < 50 ** 2
    img = np.zeros((S, S, 3), np.uint8); img[...] = (40, 200, 40); img[disc] = (60, 90, 200)
    cv2.imwrite(root + "\\Frames\\%s.png" % s, img)
    cv2.imwrite(root + "\\AlphaHint\\%s.png" % s, (disc * 255).astype(np.uint8))
print("fixture ok")
'''
N, A, B = 12, 3, 9
STEMS = [f"shot_{1001 + i}" for i in range(N)]


def ps_quote(s): return "'" + s.replace("'", "''") + "'"


def remote_command(repo, python, gpu):
    script = (f"Set-Location -LiteralPath {ps_quote(repo)} -ErrorAction Stop; "
              f"& {ps_quote(python)} -m backend.worker --gpu {gpu}; exit $LASTEXITCODE")
    enc = base64.b64encode(script.encode("utf-16-le")).decode()
    return f"powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand {enc}"


def make_job(args, jid, out, mode="new", sha=None):
    j = lambda *p: "\\".join(p)
    root = j(args.task, "work", "proj", "clips", "clip1")
    outs = {o: [j(out, o.capitalize(), f"{s}.{args.fmt}") for s in STEMS[A:B + 1]] for o in ("fg", "matte")}
    cfg = dict(fg_enabled=True, fg_format=args.fmt, matte_enabled=True, matte_format=args.fmt, comp_enabled=False,
               comp_format="png", processed_enabled=False, processed_format="exr", exr_compression="piz")
    params = dict(input_is_linear=False, use_alpha_hint=False, hint_quality=0.95, carry_hint=True,
                  carry_hint_quality=0.95, vram_profile="auto", despill_strength=0.5, cutout_linear=False,
                  hint_mode="none", temporal_window="auto")
    return {"job_id": jid, "clip_id": "smoke/clip1", "clip_root": root,
            "range": {"in_point": A, "out_point": B, "in_stem": STEMS[A], "out_stem": STEMS[B]},
            "params": params, "output_config": cfg, "hint_keyframes_abs": None, "outputs": outs,
            "manifest_dir": out, "mode": mode, "publish": "final", "path_map": [],
            "require": {"protocol": 1, "models": {"corridorkey_v2": sha}}}


SSH_TIMEOUT = 15  # every non-worker remote command


class Timeout(Exception):
    pass


def ssh(args, cmd, stdin=None, timeout=SSH_TIMEOUT):
    return subprocess.run(["ssh", "-S", args.sock, "-o", "BatchMode=yes", args.host, cmd], input=stdin,
                          capture_output=True, text=True, timeout=timeout)


def owned_pids(args):
    """PIDs of python.exe started from OUR copied interpreter only (never ComfyUI's)."""
    ps = ("Get-CimInstance Win32_Process -Filter \"Name='python.exe'\" | "
          f"? {{ $_.ExecutablePath -eq '{args.python}' }} | % {{ $_.ProcessId }}")
    enc = base64.b64encode(ps.encode("utf-16-le")).decode()
    r = ssh(args, f"powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand {enc}")
    return [int(t) for t in r.stdout.split() if t.isdigit()]


def cleanup(args):
    for pid in owned_pids(args):
        ssh(args, f"taskkill /F /PID {pid}")
        print("killed owned pid", pid, file=sys.stderr)


class Worker:
    def __init__(self, args):
        self.args = args
        # `set` is cmd.exe syntax (OpenSSH's default shell here): test-only way to point the stock launcher at the models.
        remote = f"set CORRIDORKEY_V2_MODELS_DIR={args.models}&& {remote_command(args.task + chr(92) + 'repo', args.python, args.gpu)}"
        self.p = subprocess.Popen(["ssh", "-S", args.sock, "-o", "BatchMode=yes", args.host, remote],
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.err, self.events, self.t0 = [], [], time.time()
        self.q = queue.Queue()
        threading.Thread(target=lambda: [self.q.put(l) for l in self.p.stdout] or self.q.put(None), daemon=True).start()
        threading.Thread(target=lambda: self.err.extend(self.p.stderr), daemon=True).start()

    def tail(self):
        return "".join(self.err[-30:])[-2500:]

    def read(self, timeout):
        try:
            line = self.q.get(timeout=timeout)
        except queue.Empty:
            raise Timeout(f"no worker line for {timeout}s; stderr tail:\n{self.tail()}")
        if line is None:
            raise SystemExit("worker closed stdout; stderr tail:\n" + self.tail())
        e = json.loads(line); e["_t"] = round(time.time() - self.t0, 2); self.events.append(e)
        print("EV", e["_t"], line[:160].rstrip(), file=sys.stderr, flush=True)
        return e

    def send(self, obj):
        self.p.stdin.write(json.dumps({"v": 1, **obj}) + "\n"); self.p.stdin.flush()

    def run_job(self, j, idle=30, total=120, cancel_at=None):
        """idle: max silence between events; total: wall cap for the job."""
        self.send({"cmd": "key", "job": j}); end = time.time() + total
        while True:
            e = self.read(min(idle, max(1, end - time.time())))
            if cancel_at and e["event"] == "progress" and e.get("phase") == "publish" and e["done"] >= cancel_at:
                self.send({"cmd": "cancel", "job_id": j["job_id"]}); cancel_at = None
            if e["event"] in ("done", "rejected"):
                return e
            if time.time() > end:
                raise Timeout(f"job exceeded {total}s; stderr tail:\n{self.tail()}")

    def wait_exit(self, timeout=15):
        try:
            return self.p.wait(timeout)
        except subprocess.TimeoutExpired:
            return "STILL RUNNING after %ss" % timeout


def diagnose(args):
    """Bounded snapshot of what the owned worker is doing (no assumptions from silence)."""
    ps = ("Get-CimInstance Win32_Process -Filter \"Name='python.exe'\" | "
          f"? {{ $_.ExecutablePath -eq '{args.python}' }} | % {{ $p=Get-Process -Id $_.ProcessId; "
          "'pid={0} cpu_s={1:N1} ws_mb={2:N0} threads={3}' -f $p.Id,$p.CPU,($p.WorkingSet64/1MB),$p.Threads.Count }; "
          "nvidia-smi --query-gpu=memory.used,utilization.gpu --format=csv,noheader")
    enc = base64.b64encode(ps.encode("utf-16-le")).decode()
    return ssh(args, f"powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand {enc}").stdout.strip()


def main():
    ap = argparse.ArgumentParser()
    for k in ("sock", "host", "task", "python", "models"): ap.add_argument("--" + k, required=True)
    ap.add_argument("--expect-sha256"); ap.add_argument("--gpu", type=int, default=0)
    ap.add_argument("--fmt", choices=("exr", "png"), default="exr", help="png when OpenEXR/Imath are not installed remotely")
    ap.add_argument("--stage", choices=("hello", "job", "full"), default="hello",
                    help="hello: hello + stdin EOF exit only (no model); job: + one key job; full: everything")
    ap.add_argument("--job-total", type=int, default=120)
    args = ap.parse_args(); rep = {}
    w = None
    try:
        r = ssh(args, f'"{args.python}" - {args.task}\\work\\proj\\clips\\clip1', FIXTURE_SRC)
        assert "fixture ok" in r.stdout, r
        out = args.task + "\\work\\out" + str(int(time.time()))  # fresh folder per run
        w = Worker(args); h = w.read(20); assert h["event"] == "hello", h
        rep["hello"] = {k: h[k] for k in ("host", "gpu", "capabilities", "worker_rev", "models")}
        sha = h["models"]["corridorkey_v2"].get("sha256")
        if args.expect_sha256: assert sha == args.expect_sha256, ("model sha mismatch", sha)
        assert h["capabilities"]["key"], w.tail()
        if args.stage == "hello":
            w.p.stdin.close(); rep["eof_idle_exit"] = w.wait_exit()
        else:
            d1 = w.run_job(make_job(args, "w1", out + "\\v001", sha=sha), total=args.job_total)
            rep["job1"] = {k: d1.get(k) for k in ("outcome", "message", "error_kind", "published", "expected", "manifest_complete")}
            assert d1["outcome"] == "complete", (d1, w.tail())
            if args.stage == "full":
                d2 = w.run_job(make_job(args, "w2", out + "\\v002"), cancel_at=2)
                rep["job2_cancel"] = {k: d2.get(k) for k in ("outcome", "published", "manifest_complete")}
                assert d2["outcome"] == "cancelled", d2
                d3 = w.run_job(make_job(args, "w3", out + "\\v001"))
                rep["job3_noclobber"] = {k: d3.get(k) for k in ("event", "code")}
                assert d3["event"] == "rejected" and d3["code"] == "exists", d3
            w.p.stdin.close(); rep["eof_idle_exit"] = w.wait_exit()
            if args.stage == "full":
                w2 = Worker(args); w2.read(20)
                w2.send({"cmd": "key", "job": make_job(args, "w4", out + "\\v004")})
                while w2.read(30)["event"] != "accepted": pass
                w2.p.stdin.close(); rep["eof_midjob_exit"] = w2.wait_exit()
                rep["eof_midjob_last"] = w2.events[-1].get("event"), w2.events[-1].get("outcome")
        rep["stderr_tail"] = w.tail()[-800:]
    except Timeout as e:
        print("TIMEOUT:", e, file=sys.stderr)
        try: print("DIAG:", diagnose(args), file=sys.stderr)
        except Exception as x: print("diag failed", x, file=sys.stderr)
        rep["timeout"] = str(e)[:300]
    finally:
        if w and w.p.poll() is None:
            try: w.p.stdin.close()
            except OSError: pass
            if w.wait_exit(10) != 0 and w.p.poll() is None: w.p.kill()
        cleanup(args)
        rep["leftover_owned_pids"] = owned_pids(args)
    print(json.dumps(rep, indent=2, default=str))


if __name__ == "__main__":
    main()
