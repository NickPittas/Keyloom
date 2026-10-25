import io
import json
import os
import stat
import subprocess
import sys

import pytest

from backend.coordinator import client as cl
from backend.coordinator import pki
from backend.netlink import Bridge

from ..coordinator.specs import CAPS, SPEC
from .conftest import call, connected, invite

GPU = {"uuid": "GPU-aaaaaaaa", "name": "RTX", "vram_total_mb": 24000, "ram_total_mb": 64000}
SUBMISSION = "submission-0000001"


def run(bridge, data):
    out = io.BytesIO()
    rc = bridge.run(io.BytesIO(data), out)
    return rc, [json.loads(x) for x in out.getvalue().splitlines()]


# ---- protocol hygiene (no network) ---------------------------------------------------------------

class Fake:
    def __init__(self, creds, timeout):
        self.calls = []

    def installations(self):
        self.calls.append("installations")
        return {"installations": []}

    def job(self, job_id):
        self.calls.append(("job", job_id))
        raise __import__("backend.coordinator.client", fromlist=["x"]).CoordinatorError(404, "unknown_job", "no such job SECRETTOKEN")


def test_framing_errors_never_crash_or_leak():
    b = Bridge()
    big = b'{"id":"a","op":"x","args":{"p":"' + b"x" * (1 << 20) + b'"}}\n'
    rc, out = run(b, b"{broken\n" + big + b'[1]\n{"id":"ok1","op":"nope"}\n{"id":"../x","op":"job"}\n'
                  b'{"id":"a","op":"job","extra":1}\n{"id":"b","op":"installations"}\n{"id":"c","op":"close"}\n{"id":"d","op":"job"}\n')
    assert rc == 0
    assert out[0] == {"event": "ready", "protocol": 1}
    codes = [(o["id"], o["error"]["code"]) for o in out[1:-1]]
    assert codes == [(None, "bad_request"), (None, "too_large"), (None, "bad_request"), ("ok1", "unknown_op"),
                     (None, "bad_request"), ("a", "bad_request"), ("b", "not_connected")]
    assert out[-1] == {"id": "c", "ok": True, "result": {}}  # nothing after close is read
    assert len(out) == 9


def test_oversize_without_newline_is_bounded_and_resyncs():
    rc, out = run(Bridge(), b"x" * (3 << 20) + b'\n{"id":"z","op":"installations"}\n')
    assert [o.get("id", "ready") for o in out[1:]] == [None, "z"] and out[1]["error"]["code"] == "too_large"


def test_eof_without_close_and_partial_line():
    assert run(Bridge(), b"")[0] == 0
    rc, out = run(Bridge(), b'{"id":"q","op":"installations"}')  # final line lacks newline
    assert out[1]["id"] == "q"


def test_ids_cannot_inject_url_parts_and_stats_cannot_override_session():
    b = Bridge(client_cls=Fake)
    b.client, b.session_id = Fake(None, 1), "s" * 32
    for bad in ("../../v1/pair", "a b", "x/y", "a" * 81, "a?b=1", ""):
        assert call(b, "job", {"job_id": bad})["error"]["code"] == "bad_request"
    assert call(b, "stats", {"session_id": "x" * 20, "seq": 1, "measured_age_ms": 0})["error"]["code"] == "bad_request"
    assert call(b, "job", {"job_id": "job_1", "url": "https://evil"})["error"]["code"] == "bad_request"
    assert b.client.calls == []


def test_error_messages_redact_secrets():
    b = Bridge(client_cls=Fake)
    b.client, b.session_id = Fake(None, 1), "s" * 32
    b.secrets.add("SECRETTOKEN")
    r = call(b, "job", {"job_id": "job_1"})
    assert r["error"]["code"] == "unknown_job" and "SECRETTOKEN" not in json.dumps(r)


# ---- real TLS loopback ---------------------------------------------------------------------------

def test_pair_never_echoes_token_and_files_are_private(env):
    b = Bridge()
    creds = str(env["dir"] / "c.json")
    r = call(b, "pair", {"invitation_path": invite(env), "credentials_path": creds, "name": "pc"})
    token = pki.read_credentials(creds)["token"]
    assert r["ok"] and r["result"]["installation_id"] and "token" not in r["result"] and token not in json.dumps(r)
    c = call(b, "connect", {"credentials_path": creds})
    assert token not in json.dumps(c)
    if os.name != "nt":
        for p in (creds, creds + ".session"):
            assert stat.S_IMODE(os.stat(p).st_mode) == 0o600
    again = call(b, "pair", {"invitation_path": invite(env, "i2.json"), "credentials_path": creds, "name": "pc"})
    assert again["error"]["code"] == "io" and pki.read_credentials(creds)["token"] == token  # never clobbers
    assert env["store"].create_pairing()  # and the second code was not spent
    expired = env["dir"] / "old.json"
    pki.write_invitation(str(expired), env["url"], env["pem"], "AAAAA-AAAAA", 1)
    r = call(b, "pair", {"invitation_path": str(expired), "credentials_path": str(env["dir"] / "n.json"), "name": "x"})
    assert r["error"]["code"] == "security" and "AAAAA" not in json.dumps(r)


def test_full_flow_through_bridge_and_session_is_stable(env):
    sub, _ = connected(env, "submitter")
    rec, creds = connected(env, "receiver")
    sess = call(rec, "connect", {"credentials_path": creds})["result"]["session_id"]
    assert call(rec, "register", {"build": "t", "gpu": GPU})["ok"]
    j = call(sub, "submit", {"submission_id": SUBMISSION, "spec": SPEC, "target": None})["result"]
    assert call(sub, "submit", {"submission_id": SUBMISSION, "spec": SPEC, "target": None})["result"] == j
    clash = call(sub, "submit", {"submission_id": SUBMISSION, "spec": {**SPEC, "clip_id": "other"}, "target": None})
    assert clash["error"]["code"] == "submission_conflict"
    res = {"sampled_at": int(__import__("time").time()), "free_vram_mb": 1, "free_ram_mb": 1}
    off = call(rec, "claim", {"claim_id": "claim-00000000001", "capabilities": CAPS, "resources": res})["result"]["offer"]
    assert off["job_id"] == j["job_id"] and off["spec"] == SPEC
    assert call(rec, "start", {"attempt_id": off["attempt_id"], "effective_plan": {"a": 1}})["result"]["state"] == "started"
    ev = {"attempt_id": off["attempt_id"], "events": [{"seq": 1, "type": "progress", "data": {"done": 1, "total": 2}}]}
    assert call(rec, "events", ev)["ok"] and call(rec, "events", ev)["ok"]  # replay is idempotent
    assert call(rec, "stats", {"seq": 1, "measured_age_ms": 5, "cpu_percent": 1.5})["result"]["accepted"]
    assert call(sub, "cancel", {"job_id": j["job_id"]})["result"]["cancel_requested"]
    assert call(sub, "job", {"job_id": j["job_id"]})["result"]["state"] == "started"
    assert len(call(sub, "job_events", {"job_id": j["job_id"]})["result"]["events"]) == 1
    names = {i["name"] for i in call(sub, "installations")["result"]["installations"]}
    assert {"submitter", "receiver"} <= names
    # restart of the app/bridge: the old bridge ends first (its lock is released), then the same session id is reused
    run(rec, b'{"id":"c","op":"close"}\n')
    again = Bridge()
    assert call(again, "connect", {"credentials_path": creds})["result"]["session_id"] == sess
    assert call(again, "register", {"build": "t", "gpu": GPU})["ok"]
    ev2 = {"attempt_id": off["attempt_id"], "events": [{"seq": 2, "type": "terminal", "data": {"state": "cancelled"}}]}
    assert call(again, "events", ev2)["ok"]
    r = call(again, "reject", {"attempt_id": off["attempt_id"], "code": "busy", "message": ""})
    assert r["error"]["code"] == "attempt_not_offered"


def test_coordinator_and_transport_errors_pass_through(env):
    b, creds = connected(env)
    assert call(b, "claim", {"claim_id": "claim-00000000002", "capabilities": CAPS, "resources": None})["error"]["code"] == "stale_session"
    assert call(b, "start", {"attempt_id": "att_nope", "effective_plan": {}})["error"]["code"] in ("stale_session", "unknown_attempt")
    assert call(b, "submit", {"submission_id": "s", "spec": {}, "target": None})["error"]["code"] == "invalid"
    d = json.loads(open(creds).read())
    d["token"] = "x" * 43
    bad = env["dir"] / "badtoken.json"
    pki.write_credentials(str(bad), d["url"], d["certificate_pem"], d["fingerprint_sha256"], d["installation_id"], d["token"])
    b2 = Bridge()
    assert call(b2, "connect", {"credentials_path": str(bad)})["ok"]  # connect is local; the server decides
    r = call(b2, "installations")
    assert r["error"]["code"] == "unauthorized" and "x" * 43 not in json.dumps(r)
    other_pem, _ = pki.generate_identity(["127.0.0.1"])
    d["certificate_pem"], d["fingerprint_sha256"] = other_pem, pki.fingerprint(other_pem)
    forged = env["dir"] / "forged.json"
    pki.write_credentials(str(forged), d["url"], d["certificate_pem"], d["fingerprint_sha256"], d["installation_id"], "t" * 43)
    b3 = Bridge()
    assert call(b3, "connect", {"credentials_path": str(forged)})["ok"]
    assert call(b3, "installations")["error"]["code"] == "security"  # rejected certificate is never "retryable"


def test_world_readable_credentials_refused(env):
    if os.name == "nt":
        pytest.skip("posix modes")
    _, creds = connected(env)
    os.chmod(creds, 0o644)
    assert call(Bridge(), "connect", {"credentials_path": creds})["error"]["code"] == "security"


def test_persistent_subprocess_over_pipes(env, tmp_path):
    repo = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    creds = str(env["dir"] / "p.json")
    proc = subprocess.Popen([sys.executable, "-m", "backend.netlink", "--timeout", "5"], cwd=repo, text=True,
                            env={"PYTHONPATH": repo, "PYTHONDONTWRITEBYTECODE": "1", "PATH": os.environ["PATH"]},
                            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)

    def ask(rid, op, args=None):
        proc.stdin.write(json.dumps({"id": rid, "op": op, "args": args or {}}) + "\n")
        proc.stdin.flush()
        return json.loads(proc.stdout.readline())

    try:
        assert json.loads(proc.stdout.readline())["event"] == "ready"
        assert ask("1", "pair", {"invitation_path": invite(env, "pp.json"), "credentials_path": creds, "name": "p"})["ok"]
        assert ask("2", "connect", {"credentials_path": creds})["ok"]
        assert ask("3", "installations")["result"]["installations"][0]["name"] == "p"  # same process kept its client
        token = pki.read_credentials(creds)["token"]
        proc.stdin.close()  # EOF, no close op
        assert proc.wait(10) == 0
        assert token not in proc.stdout.read() + proc.stderr.read()
    finally:
        proc.kill()
    help_ = subprocess.run([sys.executable, "-m", "backend.netlink", "-h"], cwd=repo, capture_output=True, text=True,
                           env={"PYTHONPATH": repo, "PYTHONDONTWRITEBYTECODE": "1", "PATH": os.environ["PATH"]})
    assert help_.returncode == 0 and "Protocol v1" in help_.stdout


# ---- round-1 review regressions ------------------------------------------------------------------

def _creds_file(path, **over):
    d = {"version": 1, "url": "https://127.0.0.1:9", "certificate_pem": "P", "fingerprint_sha256": "f",
         "installation_id": "inst_1", "token": "TOKEN_0123456789abcdef", **over}
    open(path, "w").write(json.dumps(d))
    os.chmod(path, 0o600)


@pytest.mark.parametrize("field,value", [("token", 12345), ("token", None), ("token", ""), ("token", ["a"]), ("installation_id", 7),
                                         ("url", None), ("token", "a.b")])
def test_non_string_credentials_are_structured_security_errors(tmp_path, field, value):
    p = str(tmp_path / "c.json")
    _creds_file(p, **{field: value})
    before = open(p).read()
    b = Bridge()
    r = call(b, "connect", {"credentials_path": p})
    assert r["error"]["code"] == "security" and "Traceback" not in json.dumps(r)
    assert open(p).read() == before and not os.path.exists(p + ".session")
    assert call(b, "installations")["error"]["code"] == "not_connected"  # the helper keeps serving


@pytest.mark.parametrize("value", [12345, None, ""])
def test_bad_invitation_code_types(env, value):
    p = env["dir"] / "i.json"
    pki.write_invitation(str(p), env["url"], env["pem"], "AAAAA-AAAAA", 4_000_000_000)
    d = json.loads(p.read_text())
    d["code"] = value
    p.write_text(json.dumps(d))
    dest = env["dir"] / "d.json"
    b = Bridge()
    r = call(b, "pair", {"invitation_path": str(p), "credentials_path": str(dest), "name": "x"})
    assert r["error"]["code"] == "security" and not dest.exists()
    assert b._redact(12345) == "12345"  # redaction itself can no longer throw


def test_pair_readonly_directory_leaves_code_unused(env):
    if os.name == "nt" or os.geteuid() == 0:
        pytest.skip("posix permissions")
    ro = env["dir"] / "ro"
    ro.mkdir(mode=0o500)
    inv = invite(env)
    r = call(Bridge(), "pair", {"invitation_path": inv, "credentials_path": str(ro / "c.json"), "name": "x"})
    assert r["error"]["code"] == "io"
    good = str(env["dir"] / "ok.json")
    assert call(Bridge(), "pair", {"invitation_path": inv, "credentials_path": good, "name": "x"})["ok"]  # code was never spent


def test_pair_save_failure_after_redeem_is_distinct(env, monkeypatch):
    inv, dest = invite(env), env["dir"] / "d.json"

    def boom(path, data):
        raise OSError(28, "No space left")
    monkeypatch.setattr(pki, "fill_reserved", boom)
    r = call(Bridge(), "pair", {"invitation_path": inv, "credentials_path": str(dest), "name": "x"})
    assert r["error"]["code"] == "paired_not_saved" and "inst_" in r["error"]["message"] and "revoke" in r["error"]["message"]
    assert not dest.exists()  # no half-written secret
    monkeypatch.undo()
    assert call(Bridge(), "pair", {"invitation_path": inv, "credentials_path": str(dest), "name": "x"})["error"]["code"] == "pairing_rejected"


def test_pair_transport_failure_says_maybe_used_and_cert_failure_says_unsent(env, monkeypatch):
    inv, dest = invite(env), env["dir"] / "d.json"
    monkeypatch.setattr(cl.Client, "pair", classmethod(lambda c, i, n, e=None: (_ for _ in ()).throw(TimeoutError())))
    r = call(Bridge(), "pair", {"invitation_path": inv, "credentials_path": str(dest), "name": "x"})
    assert r["error"]["code"] == "pair_uncertain" and "MAY" in r["error"]["message"] and not dest.exists()
    monkeypatch.undo()
    other, _ = pki.generate_identity(["127.0.0.1"])
    forged = env["dir"] / "forged-inv.json"
    pki.write_invitation(str(forged), env["url"], other, env["store"].create_pairing(), 4_000_000_000)
    r = call(Bridge(), "pair", {"invitation_path": str(forged), "credentials_path": str(dest), "name": "x"})
    assert r["error"]["code"] == "security" and "nothing was sent" in r["error"]["message"] and not dest.exists()


@pytest.mark.parametrize("exc,code", [(__import__("socket").gaierror(-2, "Name or service not known"), "transport"),
                                      (OSError(113, "No route to host"), "transport"), (TimeoutError(), "transport"),
                                      (ConnectionResetError(), "transport")])
def test_network_oserrors_are_transport_not_io(exc, code):
    class Down(Fake):
        def installations(self):
            raise exc
    b = Bridge(client_cls=Down)
    b.client, b.session_id = Down(None, 1), "s" * 32
    assert call(b, "installations")["error"]["code"] == code


def test_local_file_errors_stay_io(tmp_path):
    r = call(Bridge(), "connect", {"credentials_path": str(tmp_path / "missing.json")})
    assert r["error"]["code"] == "io"


# ---- review round 2 -------------------------------------------------------------------------------

def test_second_bridge_on_same_credentials_is_busy_until_eof_or_close(env):
    first, creds = connected(env, "dup")
    sess = call(first, "connect", {"credentials_path": creds})  # same bridge, same credentials: fine, lock kept
    assert sess["ok"]
    second = Bridge()
    r = call(second, "connect", {"credentials_path": creds})
    assert r["error"]["code"] == "busy" and "already in use" in r["error"]["message"] and "already owns" not in r["error"]["message"]
    assert second.client is None and call(second, "installations")["error"]["code"] == "not_connected"
    assert call(first, "installations")["ok"]  # the working connection is undisturbed
    rc, _ = run(first, b"")  # EOF releases
    assert rc == 0 and first.lock is None
    third = Bridge()
    r3 = call(third, "connect", {"credentials_path": creds})
    assert r3["ok"] and r3["result"]["session_id"] == sess["result"]["session_id"]  # stable session across the handover
    run(third, b'{"id":"x","op":"close"}\n')
    assert call(Bridge(), "connect", {"credentials_path": creds})["ok"]


def test_failed_or_replacing_connect_keeps_locks_straight(env, tmp_path):
    a, creds_a = connected(env, "a")
    b, creds_b = connected(env, "b")
    broken = str(env["dir"] / "broken.json")
    _creds_file(broken, token="")
    assert call(a, "connect", {"credentials_path": broken})["error"]["code"] == "security"
    assert call(a, "installations")["ok"]  # working connection survived the failed connect
    r = call(a, "connect", {"credentials_path": creds_b})  # b's bridge still holds those credentials
    assert r["error"]["code"] == "busy" and call(a, "installations")["ok"]
    a.release()
    b.release()
    assert call(a, "connect", {"credentials_path": creds_b})["ok"]  # replace: switch a from nothing to b
    assert call(a, "connect", {"credentials_path": creds_a})["ok"]  # then to a: b's lock must have been released
    assert call(b, "connect", {"credentials_path": creds_b})["ok"]
    assert call(Bridge(), "connect", {"credentials_path": creds_b})["error"]["code"] == "busy"


def test_lock_released_when_process_exits(env):
    repo = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    b, creds = connected(env, "x")
    b.release()
    proc = subprocess.Popen([sys.executable, "-m", "backend.netlink"], cwd=repo, text=True,
                            env={"PYTHONPATH": repo, "PYTHONDONTWRITEBYTECODE": "1", "PATH": os.environ["PATH"]},
                            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    try:
        proc.stdout.readline()
        proc.stdin.write(json.dumps({"id": "1", "op": "connect", "args": {"credentials_path": creds}}) + "\n")
        proc.stdin.flush()
        assert json.loads(proc.stdout.readline())["ok"]
        assert call(Bridge(), "connect", {"credentials_path": creds})["error"]["code"] == "busy"  # a live process holds it
        proc.kill()
        proc.wait(10)
        assert call(Bridge(), "connect", {"credentials_path": creds})["ok"]  # killed: the OS released it
    finally:
        proc.kill()


def _canned(monkeypatch, status, body):
    """Replace only the wire exchange with one canned HTTP reply; the client's own parsing and checks still run."""
    monkeypatch.setattr(cl, "exchange", lambda urls, *a, **k: (urls[0], status, body))


@pytest.mark.parametrize("status,body", [(200, b"\xff\xfe not utf8"), (200, b"[1,2]"), (200, b"null"), (200, b"not json"),
                                         (500, b"{}"), (500, b'{"error":"str"}'), (500, b'{"error":{"code":5}}'), (500, b"\xff")])
def test_malformed_replies_are_transport(env, monkeypatch, status, body):
    b, _ = connected(env, "m")
    _canned(monkeypatch, status, body)
    r = call(b, "installations")
    assert r["error"]["code"] == "transport", r


def test_malformed_pairing_reply_is_uncertain(env, monkeypatch):
    _canned(monkeypatch, 200, b'{"installation_id":5}')
    dest = env["dir"] / "d.json"
    r = call(Bridge(), "pair", {"invitation_path": invite(env), "credentials_path": str(dest), "name": "x"})
    assert r["error"]["code"] == "pair_uncertain" and not dest.exists()


def test_non_ascii_reply_near_the_size_limit_is_raw_utf8_and_not_expanded():
    class Big(Fake):
        def installations(self):
            # a coordinator body just under 1 MiB on the wire: every é was escaped as \u00e9 (6 bytes) there
            return {"text": "é" * ((1 << 20) // 6 - 64), "astral": "𝄞" * 100}
    b = Bridge(client_cls=Big)
    b.client, b.session_id = Big(None, 1), "s" * 32
    out = io.BytesIO()
    b.run(io.BytesIO(b'{"id":"a","op":"installations"}\n'), out)
    ready, reply = out.getvalue().splitlines()
    assert len(reply) < (1 << 20) + 4096 and "\\u00e9".encode() not in reply and "é".encode() in reply
    assert json.loads(reply)["result"]["astral"] == "𝄞" * 100


def test_bridge_rejects_surrogates_and_keeps_serving_and_never_crashes_on_a_poisoned_reply():
    class Poison(Fake):
        def installations(self):
            return {"text": "a\ud800b"}  # e.g. an old poisoned row from a coordinator without the new check
    b = Bridge(client_cls=Poison)
    b.client, b.session_id = Poison(None, 1), "s" * 32
    data = (b'{"id":"a","op":"submit","args":{"submission_id":"x","spec":{"clip_id":"\\ud800"},"target":null}}\n'
            b'{"id":"b","op":"installations"}\n{"id":"c","op":"job","args":{"job_id":"job_1"}}\n')
    rc, out = run(b, data)
    assert rc == 0 and [o.get("id") for o in out[1:]] == [None, "b", "c"]  # one answer each, bridge still alive (an unreadable line has no id)
    assert out[1]["error"]["code"] == "bad_request"
    assert out[2]["error"]["code"] == "internal" and "invalid Unicode" in out[2]["error"]["message"]
    assert out[3]["error"]["code"] == "unknown_job" or out[3]["ok"] is False
