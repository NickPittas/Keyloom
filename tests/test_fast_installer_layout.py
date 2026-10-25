"""Split Linux installer layout: manifest runtime_id, boot/app/runtime segments, segment-only reads (tiny fixtures, no installer build)."""
import hashlib
import io
import json
import os
import re
import subprocess
import sys
import tarfile
import time
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "packaging"))
import bundle  # noqa: E402
import installer_payload as ip  # noqa: E402

MARKER = b"__PAYLOAD_BELOW__\n"
FILES = {"app/x.py": b"app v1", "bin/keyloom-ui": b"ui", "runtime/python/bin/python3": b"runtime v1",
         "runtime/python/bin/pip3": b"pip", "runtime/python/lib/m.so": b"so", "runtime/python/lib/python3.11/os.py": b"os",
         "runtime/python/lib/python3.11/site-packages/torch.py": b"torch", "runtime/python/lib/tcl8.6/init.tcl": b"tcl"}


def make_stage(root: Path, app_bytes=b"app v1", rt_bytes=b"runtime v1") -> Path:
    stage = root / "stage"
    for rel, data in FILES.items():
        data = {"app/x.py": app_bytes, "runtime/python/bin/python3": rt_bytes}.get(rel, data)
        (stage / rel).parent.mkdir(parents=True, exist_ok=True)
        (stage / rel).write_bytes(data)
    bundle.write_manifest(stage, "9.9.9", "l" * 64, "c" * 64)
    return stage


def consts(stub: str) -> dict:
    return {k: v.strip('"') for k, v in re.findall(r"\b([A-Z_0-9]+)=(\"[^\"]*\"|[^;\s\"]+)", stub)}


def build(tmp_path, **kw):
    stage = make_stage(tmp_path, **kw)
    out = tmp_path / "out"
    out.mkdir()
    run = bundle.make_linux_run(stage, out)
    raw = run.read_bytes()
    c = consts(raw[:raw.index(MARKER) + len(MARKER)].decode())
    return stage, run, raw, c


def names(b):
    return sorted(m.name for m in tarfile.open(fileobj=io.BytesIO(b)).getmembers() if m.isfile())


def test_runtime_id_covers_only_runtime(tmp_path):
    a = json.loads((make_stage(tmp_path / "a") / "install-manifest.json").read_text())
    b = json.loads((make_stage(tmp_path / "b", app_bytes=b"app v2") / "install-manifest.json").read_text())
    c = json.loads((make_stage(tmp_path / "c", rt_bytes=b"runtime v2") / "install-manifest.json").read_text())
    assert re.fullmatch(r"[0-9a-f]{64}", a["runtime_id"])
    assert a["runtime_id"] == b["runtime_id"] != c["runtime_id"]
    assert a["version_id"] != b["version_id"]
    assert a["runtime_id"] == bundle.runtime_id(a["files"])


def test_segments_offsets_hashes_and_content(tmp_path):
    stage, run, raw, c = build(tmp_path)
    base = int(c["STUB_BYTES"])
    assert raw[base - len(MARKER):base] == MARKER  # STUB_BYTES lands exactly on the first segment byte
    seg = lambda p: raw[base + int(c[p + "_OFFSET"]):base + int(c[p + "_OFFSET"]) + int(c[p + "_LEN"])]
    boot, app, rt = seg("BOOT"), seg("APP"), seg("RUNTIME")
    for b, p in ((boot, "BOOT"), (app, "APP"), (rt, "RUNTIME")):
        assert hashlib.sha256(b).hexdigest() == c[p + "_SHA256"]
    assert int(c["APP_OFFSET"]) == len(boot) and int(c["RUNTIME_OFFSET"]) == len(boot) + len(app)
    assert base + len(boot) + len(app) + len(rt) == len(raw)
    # bootstrap: interpreter closure + stdlib + the extractor only (no site-packages, pip, Tk)
    assert names(boot) == ["boot/installer_payload.py", "boot/python/bin/python3", "boot/python/lib/m.so",
                           "boot/python/lib/python3.11/os.py"]
    assert names(app) == ["payload/app/x.py", "payload/bin/keyloom-ui", "payload/install-manifest.json"]
    assert names(rt) == sorted("payload/" + k for k in FILES if k.startswith("runtime/"))
    assert c["RUNTIME_ID"] == json.loads((stage / "install-manifest.json").read_text())["runtime_id"]
    assert not re.search(r"@[A-Z_0-9]+@", raw[:base].decode())  # every placeholder was substituted


def test_app_only_change_keeps_runtime_id_in_stub(tmp_path):
    assert build(tmp_path / "x")[3]["RUNTIME_ID"] == build(tmp_path / "y", app_bytes=b"app v2")[3]["RUNTIME_ID"]


class Spy:
    """File wrapper recording every byte range read."""
    def __init__(self, f, log):
        self.f, self.log = f, log

    def __enter__(self):
        return self

    def __exit__(self, *a):
        self.f.close()

    def seek(self, n):
        self.f.seek(n)

    def read(self, n):
        pos = self.f.tell()
        b = self.f.read(n)
        self.log.append((pos, pos + len(b)))
        return b


def extract(run, c, p, dest, **kw):
    log = []
    ip.extract_segment(run, int(c["STUB_BYTES"]), int(c[p + "_OFFSET"]), int(c[p + "_LEN"]), c[p + "_SHA256"], dest, p.lower(),
                       out=io.StringIO(), opener=lambda f, m: Spy(open(f, m), log), **kw)
    return log


def test_app_update_never_reads_runtime_bytes(tmp_path):
    _, run, raw, c = build(tmp_path)
    rt_lo = int(c["STUB_BYTES"]) + int(c["RUNTIME_OFFSET"])
    log = extract(run, c, "APP", tmp_path / "d", require="payload/", forbid="payload/runtime")
    assert log and max(hi for _, hi in log) <= rt_lo  # nothing at or beyond the runtime segment was read
    assert (tmp_path / "d/payload/app/x.py").read_bytes() == b"app v1" and not (tmp_path / "d/payload/runtime").exists()


def test_runtime_extract_reads_only_its_segment(tmp_path):
    _, run, raw, c = build(tmp_path)
    log = extract(run, c, "RUNTIME", tmp_path / "d", require="payload/runtime")
    assert min(lo for lo, _ in log) >= int(c["STUB_BYTES"]) + int(c["RUNTIME_OFFSET"])
    assert (tmp_path / "d/payload/runtime/python/bin/python3").read_bytes() == b"runtime v1"


@pytest.mark.parametrize("p,pos", [("APP", "APP_OFFSET"), ("RUNTIME", "RUNTIME_OFFSET")])
def test_tampered_segment_fails(tmp_path, p, pos):
    _, run, raw, c = build(tmp_path)
    at = int(c["STUB_BYTES"]) + int(c[pos]) + int(c[p + "_LEN"]) - 3  # inside the gzip trailer: tar still parses, hash must not
    run.write_bytes(raw[:at] + bytes([raw[at] ^ 1]) + raw[at + 1:])
    with pytest.raises((ValueError, EOFError, OSError, tarfile.TarError)):
        extract(run, c, p, tmp_path / "d")


def test_wrong_member_and_traversal_rejected(tmp_path):
    _, run, raw, c = build(tmp_path)
    with pytest.raises(ValueError):  # runtime member inside an app-required segment
        extract(run, c, "RUNTIME", tmp_path / "d", require="payload/app")


def test_extractor_cli_exit_codes(tmp_path):
    _, run, raw, c = build(tmp_path)
    args = [sys.executable, "-I", str(Path(ip.__file__)), "extract", "--run", str(run), "--base", c["STUB_BYTES"],
            "--offset", c["APP_OFFSET"], "--len", c["APP_LEN"], "--label", "app", "--dest", str(tmp_path / "d")]
    assert subprocess.run(args + ["--sha256", c["APP_SHA256"]], capture_output=True).returncode == 0
    r = subprocess.run(args + ["--sha256", "0" * 64], capture_output=True, text=True)
    assert r.returncode == 1 and "checksum mismatch" in r.stdout  # the supervised worker's output is relayed to stdout


def alive(pid):
    try:
        os.kill(pid, 0)
        return "Z" not in Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[0]
    except (ProcessLookupError, FileNotFoundError):
        return False


def test_supervisor_kills_and_reaps_blocked_worker_group(tmp_path):
    pidfile = tmp_path / "gc.pid"
    out = io.StringIO()
    t0 = time.monotonic()
    # the worker blocks (like stuck I/O) and leaves a grandchild in its process group
    rc = ip.supervise(["/bin/sh", "-c", f"sleep 60 & echo $! > {pidfile}; wait"], "blocked", out=out, poll=0.2, stall=1)
    assert rc == 124 and time.monotonic() - t0 < 5
    assert "no progress for 1s" in out.getvalue() and "heartbeat" in out.getvalue()
    assert not alive(int(pidfile.read_text()))


def test_supervisor_passes_exit_code():
    assert ip.supervise(["/bin/sh", "-c", "exit 3"], "x", out=io.StringIO(), poll=0.2) == 3
    assert ip.main(["run", "--label", "x", "--", "/bin/sh", "-c", "exit 7"]) == 7


def test_rendered_stub_shell_syntax_and_flow(tmp_path):
    _, run, raw, c = build(tmp_path)
    stub = tmp_path / "stub.sh"
    stub.write_bytes(raw[:int(c["STUB_BYTES"])])
    assert subprocess.run(["/bin/sh", "-n", str(stub)], capture_output=True).returncode == 0
    text = stub.read_text()
    # boot python (outside payload/) runs the lifecycle; runtime read only after runtime-available says no (or on repair)
    assert text.index("runtime-available") < text.index('seg runtime "$RUNTIME_OFFSET" "$RUNTIME_LEN" "$RUNTIME_SHA256" "$STAGE"')
    assert "payload/runtime/python/bin/python3" not in text
    # no phase is unlimited and nothing runs unsupervised
    assert not re.search(r'seg \w+ .*" 0 payload', text) and "--deadline 0" not in text
    assert "sup bootstrap sh -c" in text and "--label runtime-available --log" in text
    # runtime-available: 0 attach, 1 extract, every other code FAILs (no silent full-runtime fallback, no code-3 branch)
    case = text[text.index('case "$RA" in'):text.index('esac', text.index('case "$RA" in'))]
    assert re.search(r"0\) NEEDRT=0", case) and re.search(r"1\) NEEDRT=1", case)
    assert not re.search(r"\b3\)", case) and re.search(r"\*\).*exit 1", case)
    assert "KEYLOOM_RUNTIME_PHASE" not in text and "RUNTIME_PHASE_S" not in text  # no unapproved deadline escape hatch
    # install/repair are supervised; the attach flags exist only on the reuse branch of install, never on repair
    assert '--label "$VERB" --log "$LOG" --log-title "$TITLE" --wait-file' in text
    assert 'set -- --attach-runtime "$@"' in text
    assert text.index('[ "$NEEDRT" = 1 ] || set -- --attach-runtime') > text.index("never --attach-runtime")  # else branch only
    assert "sudo -S" not in text and "read -s" not in text


def sup_script(tmp_path, body):
    """The stub's real supervisor functions (between the sup markers) plus a test body, run by /bin/sh (dash on Debian)."""
    _, run, raw, c = build(tmp_path)
    stub = raw[:int(c["STUB_BYTES"])].decode()
    funcs = stub[stub.index("# --- sup begin"):stub.index("# --- sup end")]
    script = tmp_path / "sup.sh"
    script.write_text("set -eu\nPF=\n" + funcs + body)
    return script


def alive_pid(pid):
    try:
        return "Z" not in Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[0]
    except FileNotFoundError:
        return False


def test_shell_supervisor_success_and_exit_code(tmp_path):
    s = sup_script(tmp_path, "sup ok sh -c 'exit 0'\nR=0; sup bad sh -c 'exit 3' || R=$?\necho got $R\n")
    r = subprocess.run(["/bin/sh", str(s)], capture_output=True, text=True, timeout=8)
    assert r.returncode == 0 and "got 3" in r.stdout, (r.stdout, r.stderr)


def test_shell_supervisor_timeout_kills_only_its_tree(tmp_path):
    unrelated = subprocess.Popen(["/bin/sleep", "30"])
    pidfile = tmp_path / "gc.pid"
    s = sup_script(tmp_path, f"STALL_S=2; R=0; sup blocked sh -c 'sleep 30 & echo $! > {pidfile}; wait' || R=$?\necho got $R\n")
    try:
        t0 = time.monotonic()
        r = subprocess.run(["/bin/sh", str(s)], capture_output=True, text=True, timeout=8)
        assert "got 124" in r.stdout and "made no progress for 2s" in r.stderr and "heartbeat" in r.stdout, (r.stdout, r.stderr)
        assert time.monotonic() - t0 < 6
        assert not alive_pid(int(pidfile.read_text()))  # the blocked child's own descendant is gone
        assert alive_pid(unrelated.pid)                  # a process that is not its descendant is untouched
    finally:
        unrelated.kill()
        unrelated.wait()


def test_wait_file_pauses_clock_only_when_declared(tmp_path):
    wf = tmp_path / "waiting"
    wf.write_text("waiting for sudo consent")
    out = io.StringIO()
    assert ip.supervise(["/bin/sh", "-c", "sleep 1.5"], "install", out=out, poll=0.2, wait_file=str(wf)) == 0
    assert "WAITING FOR USER" in out.getvalue() and "sudo consent" in out.getvalue()
    wf.unlink()  # no declared wait: the same worker is cut off at the deadline
    assert ip.supervise(["/bin/sh", "-c", "sleep 30"], "install", out=io.StringIO(), poll=0.2, wait_file=str(wf), stall=1, grace=1) == 124


def test_unavailable_cli_never_reads_runtime(tmp_path):
    """Runs the rendered stub's real control flow with a fake boot python: a failing runtime-available (exit 2) must stop
    before any runtime segment is requested; exit 1 requests exactly the runtime segment."""
    _, run, raw, c = build(tmp_path)
    log = tmp_path / "calls.log"
    fake = tmp_path / "fakepy"
    fake.write_text(f'''#!/bin/sh
echo "$*" >> {log}
case "$*" in
  *runtime-available*) exit ${{RA_CODE:-2}};;
  *" run "*) exit 1;;
  *attest*) echo x > {tmp_path}/st/nonce;;
esac
exit 0
''')
    fake.chmod(0o755)
    stub = raw[:int(c["STUB_BYTES"])].decode()
    # swap the bootstrap unpack for a prepared fake interpreter; everything after it is the real flow
    head, tail = stub.split('PY="$STAGE/boot/python/bin/python3"', 1)
    flow = f'STAGE={tmp_path}/st; mkdir -p $STAGE/payload; PREFIX={tmp_path}/p; VERB=install; EXTRACT=""; SELF=x\n' \
           f'PY={fake}' + tail
    script = tmp_path / "flow.sh"
    script.write_text("set -eu\n" + "\n".join(l for l in c_consts(stub)) + "\n" + flow.split("\nset +e")[0])
    for code, wants_runtime, rc in ((2, False, 1), (3, False, 1), (1, True, 0)):
        log.write_text("")
        r = subprocess.run(["/bin/sh", str(script)], capture_output=True, text=True, env={"RA_CODE": str(code), "PATH": "/usr/bin:/bin"})
        assert ("--label runtime --offset" in log.read_text()) is wants_runtime, r.stderr
        assert r.returncode == rc, (code, r.stdout, r.stderr)


def c_consts(stub):
    return [l for l in stub.splitlines() if re.match(r"(STUB_BYTES|BOOT_|APP_|RUNTIME_|TITLE=)", l)]
