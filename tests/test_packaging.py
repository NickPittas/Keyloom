"""Installer lifecycle tests on tiny fake payloads (no network, no models, no real runtime)."""
import json
import os
import re
import subprocess
import sys
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / "packaging"))
import bundle  # noqa: E402
import lifecycle  # noqa: E402

FAKE_SETUP = '''
import json, os, sys
ok = not os.path.exists(os.environ["CK_FAIL_FLAG"]) if "CK_FAIL_FLAG" in os.environ else True
op = "verify" if "--verify" in sys.argv else "download"
if "--check" in sys.argv:  # nothing is present yet: the download below fetches the artifact
    print(json.dumps({"event": "status", "artifact": "x", "kind": "weights", "present": False}))
    op, ok = "check", True
print(json.dumps({"event": "artifact-start", "artifact": "x"}))
print(json.dumps({"event": "op-done", "op": op, "ok": ok}))
sys.exit(0 if ok else 1)
'''
SELFCHECK = 'import json,sys\nprint(json.dumps({"ok": True}))\nsys.exit(int(__import__("os").path.exists(sys.argv[1] + "/../BAD")))\n'


def make_payload(root: Path, name="payload", tag="a") -> Path:
    p = root / name
    (p / "app/packaging").mkdir(parents=True)
    (p / "app/scripts").mkdir()
    (p / "bin").mkdir()
    (p / "share").mkdir()
    (p / "runtime/python/bin").mkdir(parents=True)
    py = p / "runtime/python/bin/python3"
    py.write_text(f'#!/bin/sh\nexec {sys.executable} "$@"\n')
    py.chmod(0o755)
    (p / "app/packaging/selfcheck.py").write_text(SELFCHECK)
    (p / "app/packaging/lifecycle.py").write_text((REPO / "packaging/lifecycle.py").read_text())
    (p / "app/scripts/setup_models.py").write_text(FAKE_SETUP)
    # stand-in firewall helper: the real one is covered by test_installer_firewall.py and must not run sudo/ufw here
    (p / "app/packaging/firewall.py").write_text("import sys, pathlib\npathlib.Path(sys.argv[3]).write_text('{}')\n")
    ui = p / "bin/keyloom-ui"  # stand-in UI: reports paths inside its own version dir, like --self-check
    ui.write_text('#!/bin/sh\nD=$(cd "$(dirname "$0")/.." && pwd -P)\n'
                  'printf \'{"ok":true,"installed":true,"python":"%s","repo":"%s"}\\n\' "$D/runtime/python/bin/python3" "$D/app"\n'
                  f"# {tag}\n")
    ui.chmod(0o755)
    (p / "share/keyloom.png").write_bytes(b"png")
    bundle.descriptor(p, "9.9")
    bundle.write_manifest(p, "9.9", "lock", "cat")
    return p


@pytest.fixture
def env(tmp_path, monkeypatch):
    monkeypatch.setenv("HOME", str(tmp_path / "home"))
    monkeypatch.setenv("XDG_DATA_HOME", str(tmp_path / "xdg"))
    monkeypatch.delenv("CK_FAIL_FLAG", raising=False)
    return tmp_path


def run(*argv):
    return lifecycle.main(list(argv))


def install(env, payload, prefix, models="skip"):
    return run("--prefix", str(prefix), "install", "--payload", str(payload), "--models", models)


def test_install_update_rollback_unicode_spaces(env):
    prefix = env / "dest dir ü"
    p1 = make_payload(env, "p1", "a")
    v1 = json.loads((p1 / "install-manifest.json").read_text())["version_id"]
    assert install(env, p1, prefix) == 0  # models skipped is a success; only a requested install that fails is 3
    r = lifecycle.read_receipt(prefix)
    assert r["current"] == v1  # a failed network step never blocks activation (it is recorded in the receipt)
    assert r["models"]["ready"] is False
    assert os.readlink(prefix / "current") == f"versions/{v1}"
    desk = Path(os.environ["XDG_DATA_HOME"]) / "applications/keyloom.desktop"
    assert str(prefix / f"versions/{v1}/bin/keyloom-ui") in desk.read_text()
    assert "env -u PYTHONPATH -u PYTHONHOME" in desk.read_text()
    assert (prefix / "bin/keyloom-setup").stat().st_mode & 0o100

    p2 = make_payload(env, "p2", "b")
    v2 = json.loads((p2 / "install-manifest.json").read_text())["version_id"]
    assert v1 != v2
    install(env, p2, prefix)
    r = lifecycle.read_receipt(prefix)
    assert (r["current"], r["previous"]) == (v2, v1)
    assert run("--prefix", str(prefix), "rollback") == 0
    assert lifecycle.read_receipt(prefix)["current"] == v1
    assert os.readlink(prefix / "current") == f"versions/{v1}"


def test_failed_activation_keeps_previous_and_discards_new(env):
    prefix = env / "p"
    p1 = make_payload(env, "p1", "a")
    install(env, p1, prefix)
    cur = lifecycle.read_receipt(prefix)["current"]
    p2 = make_payload(env, "p2", "b")
    (p2 / "bin/keyloom-ui").write_text("#!/bin/sh\nexit 1\n")  # the UI self-check fails ...
    bundle.write_manifest(p2, "9.9", "lock", "cat")  # ... and the bundle itself is consistent
    vid2 = json.loads((p2 / "install-manifest.json").read_text())["version_id"]
    assert install(env, p2, prefix) == 2
    assert lifecycle.read_receipt(prefix)["current"] == cur
    assert os.readlink(prefix / "current") == f"versions/{cur}"
    assert not (prefix / "versions" / vid2).exists()


def test_corrupt_payload_rejected_before_switch(env):
    prefix = env / "p"
    p = make_payload(env)
    (p / "share/keyloom.png").write_text("tampered")
    assert install(env, p, prefix) == 2
    assert not (prefix / "current").exists()


def test_repair_restores_damaged_file_from_payload(env):
    prefix = env / "p"
    install(env, make_payload(env, "p1"), prefix)
    cur = lifecycle.read_receipt(prefix)["current"]
    ui = prefix / "versions" / cur / "share/keyloom.png"
    ui.write_text("damaged")
    assert lifecycle.verify_bundle(prefix / "versions" / cur) == [("changed", "share/keyloom.png")]
    assert run("--prefix", str(prefix), "repair") == 2  # no payload: reports, does not guess
    assert run("--prefix", str(prefix), "repair", "--payload", str(make_payload(env, "p2"))) == 0
    assert lifecycle.verify_bundle(prefix / "versions" / cur) == []


def test_models_ready_only_when_download_and_verify_pass(env, monkeypatch):
    prefix = env / "p"
    assert install(env, make_payload(env), prefix, models="full") == 0
    assert lifecycle.read_receipt(prefix)["models"]["ready"] is True
    flag = env / "fail"
    flag.write_text("")
    monkeypatch.setenv("CK_FAIL_FLAG", str(flag))
    assert run("--prefix", str(prefix), "models") == 3
    assert lifecycle.read_receipt(prefix)["models"]["ready"] is False


def test_uninstall_removes_only_owned_and_keeps_user_data(env):
    prefix = env / "p"
    install(env, make_payload(env), prefix)
    models = env / "home/models"
    models.mkdir(parents=True)
    (models / "w.safetensors").write_text("w")
    (prefix / "user-note.txt").write_text("mine")  # foreign file inside prefix survives
    assert run("--prefix", str(prefix), "uninstall") == 0
    assert (models / "w.safetensors").exists() and (prefix / "user-note.txt").exists()
    assert not (prefix / "versions").exists() and not (prefix / "receipt.json").exists()
    assert not (Path(os.environ["XDG_DATA_HOME"]) / "applications/keyloom.desktop").exists()


def test_uninstall_reports_incomplete_when_a_staging_tree_cannot_be_removed(env, monkeypatch):
    prefix = env / "p"
    install(env, make_payload(env), prefix)
    real = lifecycle.try_rmtree
    monkeypatch.setattr(lifecycle, "try_rmtree", lambda p: False if p.name.startswith(".staging.") else real(p))
    (prefix / ".staging.x").mkdir()
    assert run("--prefix", str(prefix), "uninstall") == 1


def test_uninstall_refuses_without_receipt(env):
    d = env / "other"
    d.mkdir()
    (d / "keep").write_text("x")
    assert run("--prefix", str(d), "uninstall") == 2
    assert (d / "keep").exists()


def test_shortcut_text_quoting():
    e = lifecycle.desktop_entry(Path('/a b/"x"$y/keyloom-ui'), None)
    assert '"/a b/\\\\"x\\\\"\\\\$y/keyloom-ui" %F' in e
    s = lifecycle.windows_shortcut_script(Path("C:/Start/it's.lnk"), Path("C:/Ünï/ui.exe"), Path("C:/"), None)
    assert "it''s.lnk" in s and "Ünï" in s


def test_linux_run_installer_checksum_and_extract(env):
    stage = make_payload(env, "stage")
    run_file = bundle.make_linux_run(stage, env)
    out = env / "x"
    ok = subprocess.run(["sh", str(run_file), "--extract-only", str(out)], capture_output=True, text=True, timeout=10)
    assert ok.returncode == 0 and (out / "payload/install-manifest.json").is_file()
    data = bytearray(run_file.read_bytes())
    data[-5] ^= 0xFF
    run_file.write_bytes(bytes(data))
    bad = subprocess.run(["sh", str(run_file), "--extract-only", str(env / "y")], capture_output=True, text=True, timeout=10)
    assert bad.returncode != 0 and "checksum" in bad.stdout + bad.stderr  # failures are logged to stdout


def test_end_to_end_stub_install(env):
    stage = make_payload(env, "stage")
    run_file = bundle.make_linux_run(stage, env)
    prefix = env / "stub prefix"
    r = subprocess.run(["sh", str(run_file), "--prefix", str(prefix), "--models", "skip"], capture_output=True,
                       text=True, timeout=12, env=dict(os.environ))
    assert r.returncode == 0, r.stderr  # app installed; models skipped is a success
    assert lifecycle.read_receipt(prefix)["current"]
    assert not list(prefix.glob(".staging.*"))


def test_iss_rendering(env):
    stage = make_payload(env, "stage")
    meta = env / "runtime-meta.json"
    meta.write_text("{}")
    iss = bundle.render_iss(stage, env, meta).read_text(encoding="utf-8-sig")
    assert "@" not in re.sub(r"\{.*?\}|[\w.]+@[\w.]+", "", iss.replace("@VERSION_ID@", ""))
    assert "PrivilegesRequired=lowest" in iss and "--models" in iss
    low = iss.lower().replace("no admin or automatic firewall rule", "").replace("(no admin or automatic firewall rule)", "")
    assert "name: firewall;" in low and "--firewall yes" in low and "--firewall skip" in low  # the task picks yes or skip; rules are the lifecycle's (UAC) step
    assert "exitprocess(0)" in low and "prepareruntime;" in low and "runtimechecked" in low  # exit-0 no-op; runtime decided once per install


@pytest.mark.parametrize("name", ["linux-x86_64", "windows-x86_64"])
def test_locks_are_hashed_and_pinned(name):
    t = (REPO / "packaging/locks" / f"{name}.txt").read_text()
    assert "torch==2.9.1+cu128" in t and "torchvision==0.24.1+cu128" in t
    assert "numpy==1.26" in t and "--hash=sha256:" in t
    assert "sam-2 @ git+https://github.com/facebookresearch/sam2.git@2b90b9f5" in t


def test_descriptor_is_accepted_by_runtime_contract(env, monkeypatch):
    from backend import install_paths
    stage = make_payload(env, "stage")
    monkeypatch.setenv("CK_INSTALL", str(stage / "keyloom-install.json"))
    d = install_paths.discover()
    assert d.python == stage / "runtime/python/bin/python3" and d.helper == stage / "app"


def snap(prefix):
    return sorted(str(x.relative_to(prefix)) for x in prefix.rglob("*") if ".staging" not in str(x))


def test_failure_after_pointer_work_never_leaves_dangling_current(env):
    prefix = env / "a"
    install(env, make_payload(env, "a1", "a"), prefix)
    v1 = lifecycle.read_receipt(prefix)["current"]
    apps = Path(os.environ["XDG_DATA_HOME"]) / "applications"
    for f in apps.iterdir():
        f.unlink()
    apps.rmdir()
    apps.write_text("not a dir")  # shortcut write must fail
    p2 = make_payload(env, "a2", "b")
    v2 = json.loads((p2 / "install-manifest.json").read_text())["version_id"]
    assert install(env, p2, prefix) == 2
    assert (prefix / "current").exists() and os.readlink(prefix / "current") == f"versions/{v1}"
    assert (prefix / "current.txt").read_text() == v1 and lifecycle.read_receipt(prefix)["current"] == v1
    assert not (prefix / "versions" / v2).exists() and not (prefix / ".txn.json").exists()


@pytest.mark.parametrize("same_version", [False, True])
def test_interrupt_leaves_no_orphans_and_keeps_old(env, monkeypatch, same_version):
    prefix = env / "b"
    p1 = make_payload(env, "b1", "a")
    install(env, p1, prefix)
    v1 = lifecycle.read_receipt(prefix)["current"]
    p2 = make_payload(env, "b2", "a" if same_version else "b")
    if same_version:  # an intact identical build is a no-op now; a damaged one takes the replace-with-backup path
        (prefix / "versions" / v1 / "share/keyloom.png").write_text("damaged")
    monkeypatch.setattr(lifecycle, "run_light_check", lambda *a, **k: (_ for _ in ()).throw(KeyboardInterrupt()))
    assert install(env, p2, prefix) == 130
    assert lifecycle.read_receipt(prefix)["current"] == v1
    assert sorted(d.name for d in (prefix / "versions").iterdir()) == [v1]
    assert lifecycle.verify_bundle(prefix / "versions" / v1) == ([("changed", "share/keyloom.png")] if same_version else [])
    assert not (prefix / ".txn.json").exists()
    assert run("--prefix", str(prefix), "uninstall") == 0
    assert not prefix.exists()


def test_recover_cleans_crash_leftovers(env):
    prefix = env / "c"
    install(env, make_payload(env, "c1", "a"), prefix)
    v1 = lifecycle.read_receipt(prefix)["current"]
    orphan = prefix / "versions" / "9.9-deadbeef00"
    orphan.mkdir()
    (orphan / "f").write_text("x")
    lifecycle.atomic_write_json(prefix / ".txn.json", {"version": "9.9-deadbeef00", "backup": None})
    old = prefix / "versions" / f"{v1}.old-abcdef"
    old.mkdir()
    (prefix / "current").unlink()  # pointer lost too
    lifecycle.recover(prefix)
    assert sorted(d.name for d in (prefix / "versions").iterdir()) == [v1]
    assert os.readlink(prefix / "current") == f"versions/{v1}" and not (prefix / ".txn.json").exists()


def test_locked_failed_version_is_deferred_not_lost(env, monkeypatch):
    prefix = env / "d"
    install(env, make_payload(env, "d1", "a"), prefix)
    p2 = make_payload(env, "d2", "b")
    (p2 / "bin/keyloom-ui").write_text("#!/bin/sh\nexit 1\n")
    bundle.write_manifest(p2, "9.9", "lock", "cat")
    v2 = json.loads((p2 / "install-manifest.json").read_text())["version_id"]
    monkeypatch.setattr(lifecycle, "try_rmtree", lambda p: False)  # Windows: our own exe is locked
    assert install(env, p2, prefix) == 2
    assert (prefix / ".txn.json").exists() and (prefix / "versions" / v2).exists()
    monkeypatch.undo()
    lifecycle.recover(prefix)  # later run (other interpreter) finishes the cleanup
    assert not (prefix / "versions" / v2).exists() and not (prefix / ".txn.json").exists()


def test_tampered_receipt_cannot_aim_deletes_outside(env):
    prefix = env / "e"
    install(env, make_payload(env, "e1"), prefix)
    victim = env / "victim"
    victim.mkdir()
    (victim / "precious").write_text("x")
    for bad in ("../../victim", "..", "a/b", "/etc", ""):
        r = lifecycle.read_receipt(prefix)
        r["versions"].append(bad)
        lifecycle.atomic_write_json(prefix / "receipt.json", r)
        assert run("--prefix", str(prefix), "uninstall") == 2
        r["versions"].remove(bad)
        lifecycle.atomic_write_json(prefix / "receipt.json", r)
    assert (victim / "precious").exists()


def test_uv_layout_with_minor_symlink_and_exact_version(env, monkeypatch):
    def fake_run(cmd, check=True, **k):
        if cmd[1:3] == ["python", "install"]:
            d = Path(cmd[cmd.index("--install-dir") + 1])
            full = d / "cpython-3.11.16-linux-x86_64-gnu"
            (full / "bin").mkdir(parents=True)
            (full / "bin/python3").write_text("")
            os.symlink(full, d / "cpython-3.11-linux-x86_64-gnu")
        else:
            calls.append(cmd)
            if "--target" in cmd:
                so = Path(cmd[cmd.index("--target") + 1]) / "sam2/_C.so"
                so.parent.mkdir(parents=True, exist_ok=True)
                so.write_bytes(b"x")
    calls = []
    monkeypatch.setattr(bundle.subprocess, "run", fake_run)
    monkeypatch.setattr(bundle, "cuda128_home", lambda tc=None: Path("/tc/cuda"))
    monkeypatch.setattr(bundle, "gcc13_dir", lambda tc=None: Path("/tc/gcc"))
    st = env / "st"
    st.mkdir()
    bundle.stage_runtime(st, "3.11.16")
    assert (st / "runtime/python/bin/python3").is_file() and not (st / "runtime/_py").exists()
    assert not any(p.is_symlink() for p in (st / "runtime").rglob("*"))
    pip = calls[0]
    assert "https://download.pytorch.org/whl/cu128" in pip and "https://pypi.org/simple" in pip
    with pytest.raises(SystemExit):
        bundle.stage_runtime(env / "st", "3.11")


def test_model_args_preserve_spaces_and_globs(env):
    stage = make_payload(env, "stage")
    (stage / "app/scripts/setup_models.py").write_text(
        'import json,sys,os\nopen(os.environ["ARGLOG"],"a").write(json.dumps(sys.argv[1:])+"\\n")\n'
        'print(json.dumps({"event":"status","artifact":"x","kind":"weights","present":False}))\n'
        'print(json.dumps({"event":"op-done","ok":True}))\n')
    bundle.write_manifest(stage, "9.9", "lock", "cat")
    run_file = bundle.make_linux_run(stage, env)
    log = env / "args.log"
    r = subprocess.run(["sh", str(run_file), "--prefix", str(env / "pre fix"), "--models", "full",
                        "--model-arg", "--hf-cache", "--model-arg", "/tmp/my cache/*"], capture_output=True,
                       text=True, timeout=12, env=dict(os.environ, ARGLOG=str(log)))
    assert r.returncode == 0, r.stderr
    for line in log.read_text().splitlines():
        assert line and json.loads(line)[-2:] == ["--hf-cache", "/tmp/my cache/*"]


def test_repair_default_skips_models_and_handles_names_and_links(env):
    prefix = env / "r"
    p = make_payload(env, "p1")
    (p / "weird name (1).txt").write_text("w")
    os.symlink("share/keyloom.png", p / "lnk")
    bundle.write_manifest(p, "9.9", "lock", "cat")
    install(env, p, prefix)
    cur = lifecycle.read_receipt(prefix)["current"]
    v = prefix / "versions" / cur
    (v / "weird name (1).txt").unlink()
    (v / "lnk").unlink()
    (v / "lnk").write_text("regular file now")
    pay = make_payload(env, "p2")
    (pay / "weird name (1).txt").write_text("w")
    os.symlink("share/keyloom.png", pay / "lnk")
    bundle.write_manifest(pay, "9.9", "lock", "cat")
    assert run("--prefix", str(prefix), "repair", "--payload", str(pay)) == 0  # no --models: nothing downloaded
    assert lifecycle.verify_bundle(v) == []


def test_env_is_scrubbed_for_children_and_shims(env, monkeypatch):
    monkeypatch.setenv("PYTHONPATH", "/evil")
    assert "PYTHONPATH" not in lifecycle.clean_env() and lifecycle.clean_env()["PYTHONNOUSERSITE"] == "1"
    prefix = env / "s"
    install(env, make_payload(env), prefix)
    shim = (prefix / "bin/keyloom").read_text()
    assert "unset PYTHONPATH PYTHONHOME" in shim and "PYTHONNOUSERSITE=1" in shim


def test_glibc_floor_refuses_old_system(env, monkeypatch):
    p = make_payload(env)
    bundle.write_manifest(p, "9.9", "lock", "cat", glibc="9.99")
    monkeypatch.setattr(lifecycle, "host_glibc", lambda: (2, 31))
    assert install(env, p, env / "g") == 2 and not (env / "g/versions").exists() or not list((env / "g/versions").iterdir())


def test_min_glibc_scans_elf_symbol_versions(env):
    d = env / "elf"
    d.mkdir()
    (d / "a").write_bytes(b"\x7fELF" + b"\0GLIBC_2.17\0GLIBC_2.35\0GLIBC_2.9\0")
    (d / "b.txt").write_bytes(b"GLIBC_9.9\0")  # not ELF: ignored
    assert bundle.min_glibc(d) == "2.35"


def test_iss_has_no_wholesale_deletes_and_checks_exit_and_root(env):
    iss = (REPO / "packaging/windows/keyloom.iss.in").read_text()
    assert 'Name: "{app}\\versions"' in iss and "filesandordirs; Name: \"{app}\\versions\"" not in iss
    assert "filesandordirs; Name: \"{app}\\bin\"" not in iss
    assert "Rc <> 0" in iss and "IsDriveRoot" in iss and "RemoveBackslashUnlessRoot" in iss


def test_desktop_exec_escapes_percent_and_backslash_two_layers():
    e = lifecycle.desktop_entry(Path("/a%b/c\\d/ui"), None, Path("/p"))
    exec_line = [l for l in e.splitlines() if l.startswith("Exec=")][0]
    assert '"/a%%b/c\\\\\\\\d/ui"' in exec_line  # % doubled; one literal backslash = 4 in the file
    assert f"X-Keyloom-Owner={lifecycle.owner_id(Path('/p'))}" in e


def test_uninstall_leaves_foreign_versions_dirs_and_foreign_desktop_file(env):
    prefix = env / "u"
    install(env, make_payload(env), prefix)
    foreign = prefix / "versions/draft-2024"
    foreign.mkdir()
    (foreign / "essay.txt").write_text("precious")
    desk = Path(os.environ["XDG_DATA_HOME"]) / "applications/keyloom.desktop"
    desk.write_text("[Desktop Entry]\nName=someone else's\n")  # not written by this prefix
    assert run("--prefix", str(prefix), "uninstall") == 0
    assert (foreign / "essay.txt").exists() and desk.exists()


def test_install_refuses_foreign_nonempty_prefix(env):
    d = env / "mydir"
    (d / "versions/draft").mkdir(parents=True)
    (d / "versions/draft/essay.txt").write_text("x")
    assert install(env, make_payload(env), d) == 2
    assert (d / "versions/draft/essay.txt").exists() and not (d / "receipt.json").exists()
    d2 = env / "mydir2"
    d2.mkdir()
    (d2 / "notes.txt").write_text("x")
    assert install(env, make_payload(env, "q"), d2) == 2


def test_first_install_failure_removes_shortcuts_shims_and_retry_works(env, monkeypatch):
    prefix = env / "g"
    monkeypatch.setattr(lifecycle, "prune_versions", lambda *a, **k: (_ for _ in ()).throw(OSError("disk full")))
    assert install(env, make_payload(env), prefix) == 2  # unexpected errors exit 2, not a traceback
    assert not (Path(os.environ["XDG_DATA_HOME"]) / "applications/keyloom.desktop").exists()
    assert not (prefix / "bin").exists() and not (prefix / "receipt.json").exists()
    assert not (prefix / "current").exists() and not list((prefix / "versions").iterdir())
    monkeypatch.undo()
    assert install(env, make_payload(env, "again"), prefix) == 0  # retry into the same folder is accepted


def test_no_payload_failure_is_journaled_and_live_version_is_never_deleted(env, monkeypatch):
    prefix = env / "w"
    install(env, make_payload(env, "w1", "a"), prefix)
    v1 = lifecycle.read_receipt(prefix)["current"]
    p2 = make_payload(env, "w2", "b")
    (p2 / "bin/keyloom-ui").write_text("#!/bin/sh\nexit 1\n")
    bundle.write_manifest(p2, "9.9", "lock", "cat")
    v2 = json.loads((p2 / "install-manifest.json").read_text())["version_id"]
    os.rename(p2, prefix / "versions" / v2)  # what the Windows installer does: files placed in place
    monkeypatch.setattr(lifecycle, "try_rmtree", lambda p: False)
    assert run("--prefix", str(prefix), "install", "--version", v2, "--models", "skip") == 2
    assert json.loads((prefix / ".txn.json").read_text())["version"] == v2  # recoverable later
    monkeypatch.undo()
    lifecycle.recover(prefix)
    assert not (prefix / "versions" / v2).exists() and lifecycle.read_receipt(prefix)["current"] == v1
    # re-activating the live version in place and failing must not delete it
    monkeypatch.setattr(lifecycle, "run_light_check", lambda *a, **k: (_ for _ in ()).throw(lifecycle.InstallError("x")))
    assert run("--prefix", str(prefix), "install", "--version", v1, "--models", "skip") == 2
    assert (prefix / "versions" / v1).is_dir() and lifecycle.verify_bundle(prefix / "versions" / v1) == []


def test_rollback_then_uninstall_removes_everything(env):
    prefix = env / "rb"
    install(env, make_payload(env, "a", "a"), prefix)
    install(env, make_payload(env, "b", "b"), prefix)
    assert run("--prefix", str(prefix), "rollback") == 0
    assert run("--prefix", str(prefix), "uninstall") == 0
    assert not prefix.exists()


def test_windows_deferred_delete_command_removes_running_dir_last():
    cmd = lifecycle.deferred_delete_command([Path("C:/A b/versions/v1")], Path("C:/A b"))
    assert cmd[:2] == ["cmd", "/c"] and 'rmdir /s /q "C:/A b/versions/v1"' in cmd[2]
    assert cmd[2].index("/s /q") < cmd[2].index('rmdir "C:/A b"') and "/s" not in cmd[2].split("&")[-1]


def test_reuse_runtime_requires_provenance_marker(env):
    rt = env / "rt/python"
    rt.mkdir(parents=True)
    ff = env / "ff"
    ff.mkdir()
    for t in ("ffmpeg", "ffprobe"):
        (ff / t).write_text("x")
    ui = env / "ui"
    ui.write_text("x")
    with pytest.raises(SystemExit, match="no .ck-runtime-provenance.json"):
        bundle.build_stage(env / "out", ui_binary=ui, ffmpeg_dir=ff, runtime_dir=rt, smoke=False)


def test_iss_sets_failing_exit_code_and_suppressible_boxes():
    iss = (REPO / "packaging/windows/keyloom.iss.in").read_text()
    assert "GetCustomSetupExitCode" in iss and "FailCode := Rc" in iss and "SuppressibleMsgBox" in iss
    assert "recover" in iss and "already installed" in iss


def test_manifest_ignores_bytecode_so_validation_runs_cannot_invalidate_it(env):
    p = make_payload(env, "pyc")
    (p / "app/__pycache__").mkdir()
    (p / "app/__pycache__/x.cpython-311.pyc").write_bytes(b"1")
    bundle.write_manifest(p, "9.9", "lock", "cat")
    assert not any("__pycache__" in k for k in json.loads((p / "install-manifest.json").read_text())["files"])
    (p / "app/__pycache__/x.cpython-311.pyc").write_bytes(b"changed by python")
    assert lifecycle.verify_bundle(p) == []


def _prov(rt, **kw):
    d = {"lock_sha256": bundle.sha256_file(REPO / "packaging/locks" / f"{bundle.PLATFORM}.txt"),
         "recipe": bundle.RUNTIME_RECIPE, "sam2_c_sha256": None}
    d.update(kw)
    (rt / bundle.PROVENANCE).write_text(json.dumps(d))


def test_reuse_runtime_rejects_old_recipe_and_tampered_extension(env):
    rt = env / "rt/python"
    (rt / "lib/python3.11/site-packages/sam2").mkdir(parents=True)
    ff = env / "ff"
    ff.mkdir()
    for t in ("ffmpeg", "ffprobe"):
        (ff / t).write_text("x")
    ui = env / "ui"
    ui.write_text("x")
    _prov(rt, recipe="r2: old, built against cached wheel")
    with pytest.raises(SystemExit, match="different recipe"):
        bundle.build_stage(env / "o1", ui_binary=ui, ffmpeg_dir=ff, runtime_dir=rt, smoke=False)
    so = rt / "lib/python3.11/site-packages/sam2/_C.so"
    so.write_bytes(b"built")
    _prov(rt, sam2_c_sha256="0" * 64)
    with pytest.raises(SystemExit, match="extension differs"):
        bundle.build_stage(env / "o2", ui_binary=ui, ffmpeg_dir=ff, runtime_dir=rt, smoke=False)


def test_sam2_build_env_uses_private_cuda_and_old_host_compiler(env, monkeypatch):
    monkeypatch.setenv("CUDA_HOME", "/opt/cuda")
    monkeypatch.setattr(bundle, "gcc13_dir", lambda tc=None: Path("/tc/gcc13"))
    site = env / "site"
    (site / "nvidia/cublas/include").mkdir(parents=True)
    e = bundle.sam2_build_env(site, Path("/tc/cuda-12.8.1"))
    assert e["CUDA_HOME"] == "/tc/cuda-12.8.1" and e["PATH"].startswith("/tc/cuda-12.8.1/bin")
    assert "-ccbin /tc/gcc13/bin/x86_64-conda-linux-gnu-g++" in e["NVCC_APPEND_FLAGS"]
    assert e["SAM2_BUILD_CUDA"] == "1" and e["SAM2_BUILD_ALLOW_ERRORS"] == "0" and str(site / "nvidia/cublas/include") in e["CPATH"]
    assert "12.8" in "".join(r for r, _ in bundle.CUDA_PARTS) and all(len(h) == 64 for _, h in bundle.CUDA_PARTS)


def test_stage_runtime_builds_sam2_uncached_and_records_provenance(env, monkeypatch):
    calls = []

    def fake_run(cmd, check=True, env=None, **k):
        if cmd[1:3] == ["python", "install"]:
            d = Path(cmd[cmd.index("--install-dir") + 1])
            (d / "cpython-3.11.16-linux-x86_64-gnu/bin").mkdir(parents=True)
            (d / "cpython-3.11.16-linux-x86_64-gnu/bin/python3").write_text("")
        elif "sam-2 @ git+https://github.com/facebookresearch/sam2.git@2b90b9f5ceec907a1c18123530e92e794ad901a4" in cmd:
            calls.append((cmd, env))
            so = Path(cmd[cmd.index("--target") + 1]) / "sam2/_C.so"
            so.parent.mkdir(parents=True, exist_ok=True)
            so.write_bytes(b"ext")
    monkeypatch.setattr(bundle.subprocess, "run", fake_run)
    monkeypatch.setattr(bundle, "cuda128_home", lambda tc=None: Path("/tc/cuda"))
    monkeypatch.setattr(bundle, "gcc13_dir", lambda tc=None: Path("/tc/gcc"))
    st = env / "st"
    st.mkdir()
    bundle.stage_runtime(st, "3.11.16")
    cmd, e = calls[0]
    assert "--no-cache" in cmd and "--no-build-isolation" in cmd and e["CUDA_HOME"] == "/tc/cuda"
    prov = json.loads((st / "runtime/python" / bundle.PROVENANCE).read_text())
    assert prov["recipe"] == bundle.RUNTIME_RECIPE and prov["sam2_c_sha256"] == bundle.hashlib.sha256(b"ext").hexdigest()


def test_selfcheck_imports_sam2_extension_and_probes_gpu_operator():
    t = (REPO / "packaging/selfcheck.py").read_text()
    assert 'importlib.import_module("sam2._C")' in t and "get_connected_componnets" in t and "torch.cuda.is_available()" in t
    assert 'sys.platform != "win32"' in t and '"sam2_c": sam2_c' in t  # required on Linux, honest on Windows


def test_uninstall_refuses_before_deleting_when_a_receipt_version_is_foreign(env):
    prefix = env / "n"
    install(env, make_payload(env), prefix)
    r = lifecycle.read_receipt(prefix)
    (prefix / "versions/mine-not").mkdir()
    (prefix / "versions/mine-not/f").write_text("x")
    r["versions"].append("mine-not")
    lifecycle.atomic_write_json(prefix / "receipt.json", r)
    assert run("--prefix", str(prefix), "uninstall") == 2
    assert (prefix / "versions" / r["current"]).is_dir() and (prefix / "bin/keyloom").exists()


def _run_selfcheck_with_fake_sam2(tmp, platform, with_c):
    """Run selfcheck.py's sam2._C logic against stub modules; everything else is stubbed to import trivially."""
    code = (REPO / "packaging/selfcheck.py").read_text()
    pre = ("import sys, types\nsys.platform = %r\nfor m in ['torch']:\n    t = types.ModuleType(m); t.__version__='x'; "
           "t.version=types.SimpleNamespace(cuda='12.8'); t.cuda=types.SimpleNamespace(is_available=lambda: False); sys.modules[m]=t\n"
           "import importlib\n_real = importlib.import_module\n"
           "def fake(name, *a, **k):\n    if name == 'sam2._C' and not %r: raise ImportError('no ext')\n    return types.ModuleType(name)\n"
           "importlib.import_module = fake\n") % (platform, with_c)
    # backend.ffmpeg_tools is real; stub its validator
    pre += ("ff = types.ModuleType('backend.ffmpeg_tools.discovery'); ff.validate_ffmpeg_install = lambda: types.SimpleNamespace("
            "ok=True, message='ok', ffmpeg_path=sys.argv[1]+'/f', ffprobe_path=sys.argv[1]+'/p'); sys.modules['backend.ffmpeg_tools.discovery']=ff\n")
    f = tmp / "sc.py"
    f.write_text(pre + code)
    return subprocess.run([sys.executable, str(f), str(tmp)], capture_output=True, text=True, timeout=10)


def test_selfcheck_sam2_c_required_on_linux_optional_and_honest_on_windows(env):
    r = _run_selfcheck_with_fake_sam2(env, "linux", False)
    assert r.returncode == 1 and "sam2._C" in r.stderr
    r = _run_selfcheck_with_fake_sam2(env, "win32", False)
    assert r.returncode == 0 and json.loads(r.stdout)["sam2_c"] is False
    r = _run_selfcheck_with_fake_sam2(env, "linux", True)
    assert r.returncode == 0 and json.loads(r.stdout)["sam2_c"] is True


def test_rejected_reused_runtime_leaves_no_stage(env):
    rt = env / "rt/python"
    rt.mkdir(parents=True)
    _prov(rt, recipe="old")
    ff = env / "ff"
    ff.mkdir()
    ui = env / "ui"
    ui.write_text("x")
    with pytest.raises(SystemExit, match="different recipe"):
        bundle.build_stage(env / "out", ui_binary=ui, ffmpeg_dir=ff, runtime_dir=rt, smoke=False)
    assert not (env / "out/stage").exists()


def test_toolchain_pins_and_incomplete_cache_is_rebuilt(env, monkeypatch):
    assert "13.4.0" in " ".join(bundle.GCC_SPECS) and "/download/2.9.0-0/" in bundle.MAMBA_URL and "latest" not in bundle.MAMBA_URL
    tc = env / "tc"
    (tc / "cuda-12.8.1/bin").mkdir(parents=True)
    (tc / "cuda-12.8.1/bin/nvcc").write_text("half-extracted")  # binary exists but no completion marker
    monkeypatch.setattr(bundle, "toolchain_dir", lambda: tc)

    def no_net(*a, **k):
        raise RuntimeError("download attempted")
    import urllib.request
    monkeypatch.setattr(urllib.request, "urlretrieve", no_net)
    with pytest.raises(RuntimeError, match="download attempted"):
        bundle.cuda128_home(tc)
    assert not (tc / "cuda-12.8.1").exists()  # incomplete tree discarded, not trusted
    g = tc / "mamba/gcc13/bin"
    g.mkdir(parents=True)
    (g / "x86_64-conda-linux-gnu-g++").write_text("#!/bin/sh\necho g++ 13.3.0\n")
    (g / "x86_64-conda-linux-gnu-g++").chmod(0o755)
    (tc / "mamba/gcc13" / bundle.GCC_MARK).write_text(json.dumps({"specs": ["something else"]}))
    with pytest.raises(RuntimeError, match="download attempted"):
        bundle.gcc13_dir(tc)
