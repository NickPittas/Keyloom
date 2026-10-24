"""Canonical installer build orchestration, on fake phases (no real build, network, runtime or install)."""
import hashlib
import json
import sys
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / "packaging"))
import build_release as br  # noqa: E402

VID = "9.9.9-abc1234567"


def sums(out: Path, name: str, files: list[Path]):
    (out / name).write_text("".join(f"{hashlib.sha256(f.read_bytes()).hexdigest()}  {f.name}\n" for f in files))


def fake_linux_outputs(out: Path, online=True):
    run = out / f"Keyloom-{VID}-linux-x86_64.run"
    run.write_bytes(b"offline-bytes")
    sums(out, run.name + ".sha256", [run])
    parts = [out / (run.name + ".part001")]
    parts[0].write_bytes(run.read_bytes())
    sums(out, run.name + ".parts.sha256", parts)
    if online:
        on = out / f"Keyloom-online-{VID}-linux-x86_64.run"
        on.write_bytes(b"online")
        sums(out, on.name + ".sha256", [on])


@pytest.fixture
def env(tmp_path, monkeypatch):
    ff = tmp_path / "ff"
    ff.mkdir()
    for t in ("ffmpeg", "ffprobe"):
        (ff / t).write_text("x")
    ui = tmp_path / "ui-bin"
    cfg = tmp_path / "cfg.json"
    cfg.write_text(json.dumps({"ffmpeg_dir": str(ff), "wheel_cache": str(tmp_path / "wc"), "runtime_dir": str(tmp_path / "rt")}))
    calls = []

    def fake_run(self, phase, cmd, timeout, cwd=None, env=None):
        calls.append((phase, cmd))
        self.phases.append({"phase": phase, "exit": 0})
        if phase == "ui":
            ui.write_bytes(b"\x7fELF....")
        elif phase == "stage":
            st = self.dir / "stage"
            st.mkdir()
            (st / "install-manifest.json").write_text(json.dumps(
                {"version_id": VID, "app_version": "9.9.9", "platform": br.PLATFORM, "lock_sha256": "l"}))
            (self.dir / "VERSION_ID").write_text(VID)
        elif phase == "offline":
            fake_linux_outputs(self.dir, online=False)
        elif phase == "online":
            on = self.dir / f"Keyloom-online-{VID}-linux-x86_64.run"
            on.write_bytes(b"online")
            sums(self.dir, on.name + ".sha256", [on])

    monkeypatch.setattr(br.Build, "run", fake_run)
    monkeypatch.setattr(br, "IS_WIN", False)
    monkeypatch.setattr(br, "PLATFORM", "linux-x86_64")
    monkeypatch.setattr(br, "ui_binary_path", lambda cfg: ui)
    monkeypatch.setattr(br.bundle, "validate_reused_runtime", lambda p: "lockhash")
    monkeypatch.setattr(br, "source_fingerprint", lambda: ("fp", 1))
    monkeypatch.setattr(br, "tool_version", lambda cmd: "v")
    return tmp_path, cfg, calls


def build(tmp, cfg):
    rc = br.main(["--config", str(cfg), "--out-root", str(tmp / "out")])
    (run,) = (tmp / "out").iterdir()
    return rc, run, json.loads((run / "build-result.json").read_text())


def test_success_builds_stage_once_and_both_installers_from_it(env):
    tmp, cfg, calls = env
    rc, run, res = build(tmp, cfg)
    assert rc == 0 and res["status"] == "ok" and (run / "build.log").is_file()
    assert [p for p, _ in calls] == ["ui", "stage", "helper-load", "offline", "online"]
    assert calls[2][1][-1] == str(run / "stage")  # helper-load gate runs on the same stage, before any installer
    for phase, cmd in calls[3:]:
        assert cmd[cmd.index("--stage") + 1] == str(run / "stage")
    assert res["payload"]["version_id"] == VID and res["source"]["fingerprint_sha256"] == "fp"
    names = {a["name"] for a in res["artifacts"]}
    assert f"Keyloom-{VID}-linux-x86_64.run" in names and f"Keyloom-online-{VID}-linux-x86_64.run" in names
    assert all(len(a["sha256"]) == 64 for a in res["artifacts"])


def test_missing_online_artifact_is_not_green(env, monkeypatch):
    tmp, cfg, _ = env
    orig = br.Build.run

    def run(self, phase, cmd, timeout, cwd=None, env=None):
        if phase != "online":
            return orig(self, phase, cmd, timeout, cwd, env)
        self.phases.append({"phase": phase, "exit": 0})  # child "succeeded" but wrote nothing
    monkeypatch.setattr(br.Build, "run", run)
    rc, _, res = build(tmp, cfg)
    assert rc == 1 and res["status"] == "failed" and res["artifacts"] == []


def test_source_change_during_build_fails(env, monkeypatch):
    tmp, cfg, _ = env
    seq = iter(["a", "b"])
    monkeypatch.setattr(br, "source_fingerprint", lambda: (next(seq), 1))
    rc, _, res = build(tmp, cfg)
    assert rc == 1 and "changed during the build" in res["error"]


def test_invalid_config_still_logs_and_fails(tmp_path):
    cfg = tmp_path / "bad.json"
    cfg.write_text('{"ffmpeg": "typo"}')
    rc = br.main(["--config", str(cfg), "--out-root", str(tmp_path / "out")])
    (run,) = (tmp_path / "out").iterdir()
    res = json.loads((run / "build-result.json").read_text())
    assert rc == 1 and res["status"] == "failed" and "unknown keys" in res["error"] and "FAILED" in (run / "build.log").read_text()


def test_preflight_only_builds_nothing(env):
    tmp, cfg, calls = env
    rc = br.main(["--config", str(cfg), "--out-root", str(tmp / "out"), "--preflight-only"])
    assert rc == 0 and calls == []


def test_child_failure_and_timeout_are_nonzero_and_logged(tmp_path):
    b = br.Build(tmp_path, 100)
    with pytest.raises(br.BuildError, match="exit code 3"):
        b.run("x", [sys.executable, "-c", "print('boom'); raise SystemExit(3)"], 30)
    with pytest.raises(br.BuildError, match="timed out"):
        b.run("y", [sys.executable, "-c", "import time; time.sleep(60)"], 1)
    b.log.close()
    assert "boom" in (tmp_path / "build.log").read_text()
    assert [p["exit"] for p in b.phases] == [3, "timeout"]


def test_windows_slices_must_be_contiguous_and_bounded(tmp_path, monkeypatch):
    monkeypatch.setattr(br, "IS_WIN", True)
    stem = f"Keyloom-{VID}-windows-x86_64-setup"
    (tmp_path / f"{stem}-1.bin").write_bytes(b"x")
    (tmp_path / f"{stem}-3.bin").write_bytes(b"x")  # slice 2 missing
    with pytest.raises(br.BuildError, match="contiguous"):
        br.verify_artifacts(tmp_path, VID, True)
    (tmp_path / f"{stem}-3.bin").unlink()
    monkeypatch.setattr(br, "SLICE", 0)
    (tmp_path / f"{stem}-2.bin").write_bytes(b"x")  # oversized slice
    with pytest.raises(br.BuildError, match="exceeds"):
        br.verify_artifacts(tmp_path, VID, True)


def test_checksum_mismatch_rejected(env):
    tmp, _, _ = env
    fake_linux_outputs(tmp, online=True)
    (tmp / f"Keyloom-{VID}-linux-x86_64.run").write_bytes(b"corrupt")
    with pytest.raises(br.BuildError, match="checksum"):
        br.verify_artifacts(tmp, VID, True)


@pytest.mark.parametrize("win", [False, True])
def test_worker_dispatches_by_platform(tmp_path, monkeypatch, win):
    seen = []
    monkeypatch.setattr(br, "IS_WIN", win)
    for n in ("make_windows_exe", "make_online_exe", "make_linux_run", "make_online_run", "split_linux_run"):
        monkeypatch.setattr(br.bundle, n, lambda *a, _n=n, **k: seen.append(_n) or tmp_path)
    for step in ("offline", "online"):
        br.main(["--_step", step, "--stage", str(tmp_path), "--out", str(tmp_path), "--wheel-cache", str(tmp_path), "--split"])
    assert seen == (["make_windows_exe", "make_online_exe"] if win else ["make_linux_run", "split_linux_run", "make_online_run"])


def stage_with(tmp_path, firewall_src):
    pk = tmp_path / "stage/app/packaging"
    pk.mkdir(parents=True)
    (pk / "lifecycle.py").write_text((REPO / "packaging/lifecycle.py").read_text())
    if firewall_src is not None:
        (pk / "firewall.py").write_text(firewall_src)
    return tmp_path / "stage"


def gate(tmp_path, firewall_src):
    b = br.Build(tmp_path, 100)
    try:
        br.helper_load_gate(b, stage_with(tmp_path, firewall_src), sys.executable, 60)
    finally:
        b.log.close()
    return (tmp_path / "build.log").read_text()


def test_helper_gate_passes_valid_helper(tmp_path):
    side_effect = tmp_path / "ran"
    src = f"def run(m, p): open({str(side_effect)!r}, 'w')\ndef remove(r, m): open({str(side_effect)!r}, 'w')\ndef main(a=None): open({str(side_effect)!r}, 'w')\n"
    assert "load ok" in gate(tmp_path, src)
    assert not side_effect.exists()  # imported only; run/remove never called


@pytest.mark.parametrize("src,msg", [
    (None, "missing or lacks"),
    ("import no_such_dependency_xyz\ndef run(m,p): pass\ndef remove(r,m): pass\n", "ModuleNotFoundError"),
    ("def run(m, p): pass\ndef remove(r, m): pass\n", "lacks run"),  # no main()
])
def test_helper_gate_fails_on_bad_helper(tmp_path, src, msg):
    with pytest.raises(br.BuildError, match="exit code"):
        gate(tmp_path, src)
    assert msg in (tmp_path / "build.log").read_text()


# ---- monitored child execution (Build.run -> run_monitored) -----------------------------------------------------

def test_monitored_run_streams_to_console_and_log_without_argv_echo(tmp_path, capsys):
    b = br.Build(tmp_path, 100)
    b.run("x", [sys.executable, "-c", "import sys; print('out-line'); print('err-line', file=sys.stderr)", "--token=s3cret"], 30)
    b.log.close()
    log, console = (tmp_path / "build.log").read_text(), capsys.readouterr().out
    for text in (log, console):
        assert "| out-line" in text and "| err-line" in text and "exit code=0" in text
        assert "s3cret" not in text
    assert [p["exit"] for p in b.phases] == [0]


def test_unterminated_output_and_quiet_child_do_not_block_heartbeat(tmp_path, capsys):
    b = br.Build(tmp_path, 100)
    b.run("q", [sys.executable, "-u", "-c", "import time; print('prompt>', end='', flush=True); time.sleep(2.3)"], 30)
    b.log.close()
    out = capsys.readouterr().out
    assert "| prompt>  [unterminated line]" in out
    assert "heartbeat phase=q" in out and "not child progress" in out
    assert "heartbeat" in (tmp_path / "build.log").read_text()


def test_phase_budget_is_capped_by_the_ordinary_command_gate(tmp_path, monkeypatch):
    assert br.ORDINARY_GATE == 10  # the user's current gate; the next test shrinks it only to keep the test fast
    monkeypatch.setattr(br, "ORDINARY_GATE", 1)
    b = br.Build(tmp_path, 100)
    with pytest.raises(br.BuildError, match="timed out after 1s"):
        b.run("long", [sys.executable, "-c", "import time; time.sleep(60)"], 7200)  # configured budget 2 h
    assert b.phases[-1]["exit"] == "timeout"


def test_child_exiting_124_is_a_failure_not_a_timeout(tmp_path):
    b = br.Build(tmp_path, 100)
    with pytest.raises(br.BuildError, match="exit code 124"):
        b.run("z", [sys.executable, "-c", "raise SystemExit(124)"], 30)
    assert b.phases[-1]["exit"] == 124
