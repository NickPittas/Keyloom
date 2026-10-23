"""Regression tests for the round-1 review findings."""
import os
import socket
import ssl
import threading
import time

import pytest

from backend.coordinator import pki
from backend.coordinator.__main__ import main
from backend.coordinator.client import Client, CoordinatorError
from backend.coordinator.schema import ApiError
from backend.coordinator.server import CoordinatorServer
from backend.coordinator.store import SESSION_TTL, Store

from .conftest import CAPS, SPEC, claim_body, make_inst, paired_client, session, sid, spec
from .test_store import ev, setup_pair


# ---- 1. no starvation in claim -------------------------------------------------------------------

def test_claim_not_starved_by_many_targeted_or_ineligible_jobs(store, clock):
    sub, a = setup_pair(store, clock)
    b, _ = make_inst(store, "B")
    store.register(b, session(b, "s2", "GPU-bbbbbbbb"))
    for i in range(250):  # more than the old 200-row window, all reserved for A
        store.submit(sub, {"submission_id": sid(1000 + i), "spec": SPEC, "target": a["id"]})
    for i in range(250):  # and many B cannot run (missing map / model digest)
        store.submit(sub, {"submission_id": sid(2000 + i), "spec": spec(clip_root="shared:nowhere/x"), "target": None})
        store.submit(sub, {"submission_id": sid(3000 + i), "spec": spec(require={"protocol": 1, "models": {"other": None}}), "target": None})
    ok = store.submit(sub, {"submission_id": sid(9), "spec": spec(clip_id="runnable"), "target": None})["job_id"]
    off = store.claim(b, claim_body(clock, "s2", "c2"))["offer"]
    assert off and off["job_id"] == ok


# ---- 2. absolute connection deadline -------------------------------------------------------------

@pytest.fixture
def quick_server(tmp_path, store):
    pem, key = pki.generate_identity(["127.0.0.1"])
    c, k = tmp_path / "cert.pem", tmp_path / "key.pem"
    pki.write_private_new(str(k), key)
    c.write_text(pem)
    srv = CoordinatorServer(("127.0.0.1", 0), pki.server_context(str(c), str(k)), store, conn_deadline=0.8, max_connections=2)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    yield srv, f"https://127.0.0.1:{srv.server_address[1]}", pem
    srv.shutdown()
    srv.server_close()


def _free(srv, n=2, wait=4):
    end = time.time() + wait
    while time.time() < end:
        if srv.slots._value == n:
            return True
        time.sleep(0.05)
    return False


def test_stalled_handshakes_are_cut_off_and_slots_return(store, quick_server):
    srv, url, pem = quick_server
    socks = [socket.create_connection(("127.0.0.1", srv.server_address[1])) for _ in range(2)]  # never start TLS
    time.sleep(0.2)
    assert srv.slots._value == 0
    assert _free(srv), "deadline did not free the slots"
    [s.close() for s in socks]
    creds, _ = paired_client(store, url, pem)
    assert Client(creds).installations()["installations"]  # normal calls fine afterwards


def test_trickled_headers_are_cut_off(store, quick_server):
    srv, url, pem = quick_server
    host, port, ctx = pki.verified_trust(url, pem, pki.fingerprint(pem))
    s = ctx.wrap_socket(socket.create_connection((host, port), 5), server_hostname=host)
    s.sendall(b"GET /v1/installations HTTP/1.1\r\n")
    start, cut = time.time(), False
    try:
        while time.time() - start < 5:
            s.sendall(b"X")  # one byte per 0.15 s never completes a header line, resetting any per-read timeout
            time.sleep(0.15)
    except OSError:
        cut = True
    assert cut and time.time() - start < 3
    s.close()
    assert _free(srv)
    creds, _ = paired_client(store, url, pem)
    assert Client(creds).installations()


def test_deadline_does_not_break_fast_requests_or_recycle_sockets(store, quick_server):
    srv, url, pem = quick_server
    creds, _ = paired_client(store, url, pem)
    c = Client(creds)
    for _ in range(30):
        c.installations()
    time.sleep(1.2)  # every timer has been cancelled; nothing fires late
    assert srv.slots._value == 2 and not srv._guards
    c.installations()


# ---- 3. pairing never burns a code on an unusable destination ------------------------------------

def _invite(store, tls, tmp_path, name="inv.json"):
    _, url, pem = tls
    p = str(tmp_path / name)
    pki.write_invitation(p, url, pem, store.create_pairing(), 4_000_000_000)
    return p


def test_cli_pair_checks_destination_before_redeeming(store, tls_server, tmp_path):
    inv = _invite(store, tls_server, tmp_path)
    existing = tmp_path / "exists.json"
    existing.write_text("keep me")
    assert main(["pair", "--invitation", inv, "--credentials", str(existing), "--name", "n"]) == 1
    assert existing.read_text() == "keep me"
    if os.name != "nt" and os.geteuid() != 0:
        ro = tmp_path / "ro"
        ro.mkdir(mode=0o500)
        assert main(["pair", "--invitation", inv, "--credentials", str(ro / "c.json"), "--name", "n"]) == 1
    good = str(tmp_path / "good.json")
    assert main(["pair", "--invitation", inv, "--credentials", good, "--name", "n"]) == 0  # same code still unspent
    assert Client(pki.read_credentials(good)).installations()["installations"]


def test_cli_pair_failed_redemption_leaves_no_file(store, tls_server, tmp_path):
    _, url, pem = tls_server
    inv = str(tmp_path / "bad.json")
    pki.write_invitation(inv, url, pem, "AAAAA-AAAAA", 4_000_000_000)  # unknown code -> 403
    dest = tmp_path / "c.json"
    assert main(["pair", "--invitation", inv, "--credentials", str(dest), "--name", "n"]) == 1
    assert not dest.exists()


# ---- 4/5. operator commands ----------------------------------------------------------------------

def _state(tmp_path, clock):
    d = tmp_path / "state"
    d.mkdir()
    return d, Store(d / "coordinator.sqlite3", clock)


def test_abandon_marks_failed_never_requeues_and_fences_events(tmp_path, clock, capsys):
    d, s = _state(tmp_path, clock)
    sub, rec = setup_pair(s, clock)
    j = s.submit(sub, {"submission_id": sid(1), "spec": SPEC, "target": None})["job_id"]
    off = s.claim(rec, claim_body(clock))["offer"]
    a = off["attempt_id"]
    s.start(rec, a, {"session_id": sid("s1"), "effective_plan": {}})
    s.close()
    base = ["--state-dir", str(d)]
    with pytest.raises(SystemExit):  # explicit acknowledgement is mandatory
        main(["abandon", *base, "--reason", "x", a])
    assert main(["abandon", *base, "--reason", "PC died", "--i-understand-writer-not-stopped", "att_unknown"]) == 1
    lock = pki.OwnerLock(str(d / "coordinator.lock"))  # a running server holds this
    assert main(["abandon", *base, "--reason", "PC died", "--i-understand-writer-not-stopped", a]) == 1
    del lock
    assert main(["abandon", *base, "--reason", "PC died", "--i-understand-writer-not-stopped", a]) == 0
    assert "not requeued" in capsys.readouterr().out
    s = Store(d / "coordinator.sqlite3", clock)
    v = s.job_view(sub, j)
    assert v["state"] == "failed" and "NOT proven stopped" in v["reason"] and "PC died" in v["reason"]
    assert s.claim(rec, claim_body(clock, claim_n="c2"))["offer"] is None  # never re-offered
    with pytest.raises(ApiError) as e:
        s.report(rec, a, {"session_id": sid("s1"), "events": [ev(1)]})
    assert e.value.code == "attempt_terminal"
    with pytest.raises(ApiError) as e:
        s.abandon(a, "again")
    assert e.value.status == 409


def test_revoke_cli_lock_and_gpu_reidentity_blocked_until_abandon(tmp_path, clock):
    d, s = _state(tmp_path, clock)
    sub, rec = setup_pair(s, clock)
    s.submit(sub, {"submission_id": sid(1), "spec": SPEC, "target": None})
    a = s.claim(rec, claim_body(clock))["offer"]["attempt_id"]
    s.start(rec, a, {"session_id": sid("s1"), "effective_plan": {}})
    s.close()
    lock = pki.OwnerLock(str(d / "coordinator.lock"))
    assert main(["revoke", "--state-dir", str(d), rec["id"]]) == 1  # server running
    del lock
    assert main(["revoke", "--state-dir", str(d), rec["id"]]) == 0
    assert main(["revoke", "--state-dir", str(d), "inst_nope"]) == 1
    s = Store(d / "coordinator.sqlite3", clock)
    assert s.job_view(sub, s.db.execute("SELECT job_id FROM jobs").fetchone()[0])["state"] == "started"  # still fenced
    new, _ = make_inst(s, "reinstalled")
    with pytest.raises(ApiError) as e:  # the old writer may be live: the physical GPU is not handed over
        s.register(new, session(new, "s7"))
    assert e.value.code == "gpu_identity_conflict"
    s.close()
    assert main(["abandon", "--state-dir", str(d), "--reason", "checked", "--i-understand-writer-not-stopped", a]) == 0
    s = Store(d / "coordinator.sqlite3", clock)
    s.register(new, session(new, "s7"))


# ---- offered attempts of vanished owners ---------------------------------------------------------

def test_expired_offer_is_revoked_requeued_and_late_start_refused(store, clock):
    sub, rec = setup_pair(store, clock)
    other, _ = make_inst(store, "other")
    store.register(other, session(other, "s2", "GPU-bbbbbbbb"))
    j = store.submit(sub, {"submission_id": sid(1), "spec": SPEC, "target": None})["job_id"]
    first = claim_body(clock)
    off = store.claim(rec, first)["offer"]
    clock.t += SESSION_TTL - 5
    store.claim(other, claim_body(clock, "s2", "c2"))  # owner still within its session window: nothing reassigned
    assert store.job_view(sub, j)["attempt_id"] == off["attempt_id"]
    clock.t += 10  # owner silent beyond the session TTL
    store.register(other, session(other, "s2", "GPU-bbbbbbbb"))  # keep the other installation's own session alive
    new = store.claim(other, claim_body(clock, "s2", "c3"))["offer"]
    assert new["job_id"] == j and new["attempt_id"] != off["attempt_id"]
    with pytest.raises(ApiError) as e:  # the stale owner coming back late cannot start the revoked attempt
        store.start(rec, off["attempt_id"], {"session_id": sid("s1"), "effective_plan": {}})
    assert e.value.status == 409 and e.value.code == "attempt_not_offered"
    assert store.claim(rec, first)["offer"]["state"] == "revoked"  # original claim id reports revoked


def test_expiry_never_touches_started_attempts(store, clock):
    sub, rec = setup_pair(store, clock)
    j = store.submit(sub, {"submission_id": sid(1), "spec": SPEC, "target": None})["job_id"]
    off = store.claim(rec, claim_body(clock))["offer"]
    store.start(rec, off["attempt_id"], {"session_id": sid("s1"), "effective_plan": {}})
    clock.t += 100 * SESSION_TTL
    v = store.job_view(sub, j)
    assert v["state"] == "started" and v["attempt_id"] == off["attempt_id"]


def test_revoke_requeues_offered_but_not_started(store, clock):
    sub, rec = setup_pair(store, clock)
    j = store.submit(sub, {"submission_id": sid(1), "spec": SPEC, "target": None})["job_id"]
    store.claim(rec, claim_body(clock))
    assert store.revoke(rec["id"]) == []
    assert store.job_view(sub, j)["state"] == "queued"


# ---- netlink-review follow-ups: PKI readers, Windows read-time ACL, non-finite JSON ----------------

import json as _json
import subprocess as _sp


def test_parse_json_rejects_nonfinite_exponent_as_400():
    from backend.coordinator.schema import parse_json
    for raw in (b'{"a":1e400}', b'{"a":-1e999}', b'{"a":[1e400]}', b'{"a":NaN}', b'{"a":Infinity}', b'{"a":1e5000000}'):
        with pytest.raises(ApiError) as e:
            parse_json(raw)
        assert e.value.status == 400
    assert parse_json(b'{"a":1e300,"b":0.5}') == {"a": 1e300, "b": 0.5}


def test_non_finite_over_http_is_400_not_500(store, tls_server):
    from .test_http import raw
    _, url, pem = tls_server
    creds, _ = paired_client(store, url, pem)
    b = f"{creds['installation_id']}.{creds['token']}"
    st, body = raw(url, pem, "POST", "/v1/stats", b'{"seq":1,"measured_age_ms":0,"cpu_percent":1e400,"session_id":"x"}',
                   {"Content-Type": "application/json"}, b)
    assert st == 400


@pytest.mark.parametrize("field,value", [("token", 5), ("token", None), ("token", ""), ("installation_id", 1), ("certificate_pem", [])])
def test_credential_reader_types(tmp_path, field, value):
    p = str(tmp_path / "c.json")
    pki.write_credentials(p, "https://127.0.0.1:1", "PEM", "ff", "inst_1", "SECRET-TOKEN-VALUE-0123")
    d = _json.loads(open(p).read())
    d[field] = value
    open(p, "w").write(_json.dumps(d))
    with pytest.raises(pki.SecurityError) as e:
        pki.read_credentials(p)
    assert "SECRET-TOKEN-VALUE-0123" not in str(e.value)


def _fake_icacls(state):
    def run(cmd, **kw):
        if cmd[0] == "whoami":
            return _sp.CompletedProcess(cmd, 0, "PC\\me\n", "")
        if cmd[0] == "icacls" and len(cmd) == 2:
            if state.get("broken"):
                raise FileNotFoundError("icacls")
            extra = "  BUILTIN\\Users:(R)\n" if state.get("widened") else ""
            return _sp.CompletedProcess(cmd, 0, "f PC\\me:(F)\n" + extra, "")
        return _sp.CompletedProcess(cmd, 0, "", "")
    return run


def test_windows_read_time_acl_validation(tmp_path, monkeypatch):
    state = {}
    monkeypatch.setattr(pki, "_IS_WINDOWS", True)
    monkeypatch.setattr(pki, "_windows_run", _fake_icacls(state))
    p = str(tmp_path / "c.json")
    pki.write_credentials(p, "https://127.0.0.1:1", "PEM", "ff", "inst_1", "TOKEN_0123456789abcdef")  # protected at creation
    assert pki.read_credentials(p)["token"] == "TOKEN_0123456789abcdef"
    state["widened"] = True  # ACL widened AFTER creation
    with pytest.raises(pki.SecurityError):
        pki.read_credentials(p)
    with pytest.raises(pki.SecurityError):
        pki.check_private(p)
    state.update(widened=False, broken=True)  # cannot verify -> fail closed, never a silent no-op
    with pytest.raises(pki.SecurityError) as e:
        pki.read_credentials(p)
    assert "cannot verify" in str(e.value)
    inv = str(tmp_path / "i.json")
    state.update(broken=False)
    pki.write_invitation(inv, "https://127.0.0.1:1", pki.generate_identity(["127.0.0.1"])[0], "AAAAA-AAAAA", 4_000_000_000)
    state["widened"] = True
    with pytest.raises(pki.SecurityError):
        pki.read_invitation(inv)


def test_cli_pair_save_failure_message(store, tls_server, tmp_path, monkeypatch, capsys):
    _, url, pem = tls_server
    inv = str(tmp_path / "i.json")
    pki.write_invitation(inv, url, pem, store.create_pairing(), 4_000_000_000)
    monkeypatch.setattr(pki, "fill_reserved", lambda *a: (_ for _ in ()).throw(OSError(28, "full")))
    dest = tmp_path / "c.json"
    assert main(["pair", "--invitation", inv, "--credentials", str(dest), "--name", "n"]) == 1
    err = capsys.readouterr().err
    assert "USED" in err and "revoke inst_" in err and not dest.exists()


def test_malformed_query_string_is_400_not_500(store, tls_server):
    from .test_http import raw
    _, url, pem = tls_server
    creds, _ = paired_client(store, url, pem)
    b = f"{creds['installation_id']}.{creds['token']}"
    for q in ("?a&&=", "?after=x", "?after=1&after", "?bogus=1", "?limit=0"):
        st, _ = raw(url, pem, "GET", f"/v1/jobs/job_x/events{q}", b"", None, b)
        assert st == 400, q


def test_windows_acl_principal_must_match_exactly(tmp_path, monkeypatch):
    def run_as(user, acl_user):
        def run(cmd, **kw):
            if cmd[0] == "whoami":
                return _sp.CompletedProcess(cmd, 0, user + "\n", "")
            if len(cmd) == 2:
                return _sp.CompletedProcess(cmd, 0, f"C:\\x\\c.json {acl_user}:(F)\n", "")
            return _sp.CompletedProcess(cmd, 0, "", "")
        return run
    monkeypatch.setattr(pki, "_IS_WINDOWS", True)
    p = str(tmp_path / "f")
    monkeypatch.setattr(pki, "_windows_run", run_as("PC\\bob", "PC\\bob"))
    pki.check_private(p)
    monkeypatch.setattr(pki, "_windows_run", run_as("PC\\BOB", "pc\\bob"))  # NTFS names are case-insensitive
    pki.check_private(p)
    for other in ("PC\\bobby", "OTHER\\bob", "xPC\\bob"):
        monkeypatch.setattr(pki, "_windows_run", run_as("PC\\bob", other))
        with pytest.raises(pki.SecurityError):
            pki.check_private(p)
    monkeypatch.setattr(pki, "_windows_run", run_as("PC\\john smith", "PC\\john smith"))  # names with spaces
    pki.check_private(p)


# ---- final nits: pairing reply validation, single-valued query parameters -----------------------

@pytest.mark.parametrize("iid,tok", [("inst_ok", "a.b" + "c" * 20), ("in.st", "c" * 30), ("", "c" * 30), ("inst_ok", "short"),
                                     ("inst_ok", "c" * 200), (5, "c" * 30), ("inst_ok", None), ("inst_ok", "has space " + "c" * 20)])
def test_pair_rejects_unusable_credentials_and_never_succeeds(tls_server, store, tmp_path, monkeypatch, iid, tok):
    from backend.coordinator import client as cl
    _, url, pem = tls_server
    inv_path = str(tmp_path / "i.json")
    pki.write_invitation(inv_path, url, pem, store.create_pairing(), 4_000_000_000)
    monkeypatch.setattr(cl.Client, "_call", lambda self, *a, **k: {"installation_id": iid, "token": tok})
    dest = tmp_path / "c.json"
    with pytest.raises(pki.PairingUncertain):
        cl.redeem_to_file(pki.read_invitation(inv_path), str(dest), "n")
    assert not dest.exists()


def test_valid_server_issued_credentials_pass_every_check(store, tmp_path, tls_server):
    _, url, pem = tls_server
    creds, _ = paired_client(store, url, pem)
    assert pki.valid_installation_id(creds["installation_id"]) and pki.valid_token(creds["token"])
    p = str(tmp_path / "c.json")
    pki.write_credentials(p, creds["url"], creds["certificate_pem"], creds["fingerprint_sha256"], creds["installation_id"], creds["token"])
    assert pki.read_credentials(p)["token"] == creds["token"]


def test_duplicate_query_parameters_are_400(store, tls_server):
    from .test_http import raw
    _, url, pem = tls_server
    creds, _ = paired_client(store, url, pem)
    b = f"{creds['installation_id']}.{creds['token']}"
    for q in ("?after=1&after=2", "?limit=5&limit=5", "?after=", "?after=1&limit=2&after=3"):
        assert raw(url, pem, "GET", f"/v1/jobs/job_x/events{q}", b"", None, b)[0] == 400, q
    assert raw(url, pem, "GET", "/v1/jobs/job_x/events?after=1&limit=2", b"", None, b)[0] == 404  # well-formed: reaches the job lookup


# ---- invalid Unicode at the trust boundary -------------------------------------------------------

def _escaped(spec_dict, old, new):
    import json as j
    return j.dumps(spec_dict).replace(old, new).encode()


def test_parse_json_rejects_lone_surrogates_everywhere_but_accepts_pairs():
    from backend.coordinator.schema import parse_json
    for raw in (b'{"a":"\\ud800"}', b'{"\\udc00":1}', b'{"a":["x",{"b":"\\ude00z"}]}', b'{"a":"\\ud83d"}', b'{"a":"\\ud83dx"}',
                b'{"a":"\\ude9e\\ud834"}'):  # reversed pair is two lone surrogates
        with pytest.raises(ApiError) as e:
            parse_json(raw)
        assert e.value.status == 400, raw
    assert parse_json(b'{"a":"\\ud834\\udd1e","\\ud834\\udd1e":1}') == {"a": "\U0001d11e", "\U0001d11e": 1}
    assert parse_json('{"a":"é\\n\\t"}'.encode())["a"] == "é\n\t"  # ordinary text, newlines included, untouched


def test_text_and_shared_refs_reject_surrogates_directly():
    from backend.coordinator.schema import shared_ref, text
    with pytest.raises(ApiError):
        text("a\ud800b", "x")
    with pytest.raises(ApiError):
        shared_ref("shared:root/a\ud800", "x")
    assert text("clip 𝄞 é", "x")


def test_http_surrogates_are_400_and_server_keeps_serving(store, tls_server, clock):
    from .test_http import raw
    _, url, pem = tls_server
    sub, _ = paired_client(store, url, pem)
    rec, _ = paired_client(store, url, pem)
    sb, rb = (f"{c['installation_id']}.{c['token']}" for c in (sub, rec))
    js = {"Content-Type": "application/json"}
    import json as j
    good = {"submission_id": sid(1), "spec": SPEC, "target": None}
    poisoned = j.dumps(good).replace('"clip-1"', '"\\ud800"').encode()
    assert raw(url, pem, "POST", "/v1/jobs", poisoned, js, sb)[0] == 400
    keyed = j.dumps({**good, "spec": {**SPEC, "preset_name": "x"}}).replace('"preset_name"', '"\\udc00"').encode()
    assert raw(url, pem, "POST", "/v1/jobs", keyed, js, sb)[0] == 400
    pair_ok = j.dumps(good).replace('"clip-1"', '"\\ud834\\udd1e"').encode()  # valid astral pair
    st, body = raw(url, pem, "POST", "/v1/jobs", pair_ok, js, sb)
    assert st == 200 and body["state"] == "queued"
    assert store.db.execute("SELECT count(*) FROM jobs").fetchone()[0] == 1  # nothing from the poisoned requests
    # events / start / stats strings too
    Client(rec).register(session(None))
    off = Client(rec).claim(claim_body(clock))["offer"]
    assert off["spec"]["clip_id"] == "\U0001d11e"  # round-trips; offer is encodable
    sess = sid("s1")
    start_bad = j.dumps({"session_id": sess, "effective_plan": {"k": "x"}}).replace('"x"', '"\\ud800"').encode()
    assert raw(url, pem, "POST", f"/v1/attempts/{off['attempt_id']}/start", start_bad, js, rb)[0] == 400
    Client(rec).start(off["attempt_id"], {"session_id": sess, "effective_plan": {"ok": 1}})
    ev_bad = j.dumps({"session_id": sess, "events": [{"seq": 1, "type": "note", "data": {"message": "m"}}]}).replace('"m"', '"\\ud800"').encode()
    assert raw(url, pem, "POST", f"/v1/attempts/{off['attempt_id']}/events", ev_bad, js, rb)[0] == 400
    assert Client(rec).report(off["attempt_id"], {"session_id": sess, "events": [
        {"seq": 1, "type": "note", "data": {"message": "ok \U0001d11e"}}]})["last_seq"] == 1
