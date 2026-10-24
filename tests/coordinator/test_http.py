"""Real TLS on loopback ephemeral ports."""
import http.client
import json
import socket
import ssl

import pytest

from backend.coordinator import pki
from backend.coordinator.client import Client, CoordinatorError

from .conftest import CAPS, SPEC, claim_body, paired_client, session, sid


def raw(url, cert_pem, method, path, body=b"", headers=None, bearer=None):
    host, port, ctx = pki.verified_trust(url, cert_pem, pki.fingerprint(cert_pem))
    c = http.client.HTTPSConnection(host, port, context=ctx, timeout=5)
    h = dict(headers or {})
    if bearer:
        h["Authorization"] = f"Bearer {bearer}"
    c.request(method, path, body, h)
    r = c.getresponse()
    out = (r.status, json.loads(r.read() or b"{}"))
    c.close()
    return out


def test_end_to_end_over_tls(store, clock, tls_server):
    _, url, pem = tls_server
    sub, _ = paired_client(store, url, pem)
    rec, _ = paired_client(store, url, pem)
    sub, rec = Client(sub), Client(rec)
    rec.register(session(None))
    j = sub.submit(sid(1), SPEC)
    assert sub.submit(sid(1), SPEC) == j
    off = rec.claim(claim_body(clock))["offer"]
    assert off["job_id"] == j["job_id"] and off["spec"] == SPEC
    rec.start(off["attempt_id"], {"session_id": sid("s1"), "effective_plan": {"ok": True}})
    rec.report(off["attempt_id"], {"session_id": sid("s1"), "events": [{"seq": 1, "type": "terminal", "data": {"state": "succeeded"}}]})
    assert sub.job(j["job_id"])["state"] == "succeeded"
    assert sub.job_events(j["job_id"])["events"][0]["data"]["state"] == "succeeded"
    rec.stats({"session_id": sid("s1"), "seq": 1, "measured_age_ms": 10, "cpu_percent": 3})
    assert any(i["stats"] for i in sub.installations()["installations"])
    with pytest.raises(CoordinatorError) as e:
        rec.job(j["job_id"])
    assert e.value.status == 404


def test_wrong_token_and_throttle(store, tls_server):
    srv, url, pem = tls_server
    creds, _ = paired_client(store, url, pem)
    bad = Client({**creds, "token": "x" * 43})
    for _ in range(10):
        with pytest.raises(CoordinatorError) as e:
            bad.installations()
        assert e.value.status == 401
    with pytest.raises(CoordinatorError) as e:
        bad.installations()
    assert e.value.status == 429


def test_wrong_certificate_and_fingerprint_refused(store, tls_server):
    _, url, pem = tls_server
    creds, inv = paired_client(store, url, pem)
    other_pem, _ = pki.generate_identity(["127.0.0.1"])
    with pytest.raises(pki.SecurityError):  # fingerprint does not match supplied cert
        Client({**creds, "certificate_pem": other_pem})
    forged = {**creds, "certificate_pem": other_pem, "fingerprint_sha256": pki.fingerprint(other_pem)}
    with pytest.raises((ssl.SSLError, pki.SecurityError)):  # self-consistent but not what the server presents
        Client(forged).installations()
    # identity is the pinned leaf, independent of SAN/address (automatic multi-address)
    with pytest.raises(pki.SecurityError):
        pki.endpoint("http://127.0.0.1:1")
    with pytest.raises(pki.SecurityError):
        pki.generate_identity(["0.0.0.0"])


def test_pairing_reuse_and_rate_limit(store, tls_server):
    srv, url, pem = tls_server
    creds, inv = paired_client(store, url, pem)
    with pytest.raises(CoordinatorError) as e:
        Client.pair(inv, "again")
    assert e.value.status == 403
    inv2 = {**inv, "code": "AAAAA-AAAAA"}
    for _ in range(9):
        with pytest.raises(CoordinatorError):
            Client.pair(inv2, "x")
    with pytest.raises(CoordinatorError) as e:
        Client.pair(inv2, "x")
    assert e.value.status == 429


def test_framing_and_limits(store, tls_server):
    _, url, pem = tls_server
    creds, _ = paired_client(store, url, pem)
    b = f"{creds['installation_id']}.{creds['token']}"
    js = {"Content-Type": "application/json"}
    assert raw(url, pem, "POST", "/v1/claim", b"{}", {"Content-Type": "text/plain"}, b)[0] == 415
    assert raw(url, pem, "POST", "/v1/claim", b"{not json", js, b)[0] == 400
    assert raw(url, pem, "POST", "/v1/claim", b'{"a":1,"a":2}', js, b)[0] == 400
    assert raw(url, pem, "POST", "/v1/claim", b'{"a":NaN}', js, b)[0] == 400
    assert raw(url, pem, "POST", "/v1/claim", b"[]", js, b)[0] == 400
    assert raw(url, pem, "POST", "/v1/claim", b"[" * 5000, js, b)[0] == 400
    assert raw(url, pem, "POST", "/v1/claim", b"{" * 40 + b"}" * 40, js, b)[0] == 400
    assert raw(url, pem, "GET", "/v1/installations", b"", None, None)[0] == 401
    assert raw(url, pem, "GET", "/nope", b"", None, b)[0] == 404
    assert raw(url, pem, "PUT", "/v1/claim", b"", None, b)[0] == 405
    assert raw(url, pem, "POST", "/v1/jobs/abc/cancel", b'{"x":1}', js, b)[0] in (400, 404)


def tls_raw(url, pem, data):
    host, port, ctx = pki.verified_trust(url, pem, pki.fingerprint(pem))
    s = ctx.wrap_socket(socket.create_connection((host, port), 5), server_hostname=host)
    s.sendall(data)
    s.settimeout(5)
    out = s.recv(4096)
    s.close()
    return out.split(b" ", 2)[1]


def test_raw_framing_attacks(store, tls_server):
    _, url, pem = tls_server
    creds, _ = paired_client(store, url, pem)
    auth = f"Authorization: Bearer {creds['installation_id']}.{creds['token']}\r\n".encode()
    base = b"POST /v1/claim HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\n" + auth
    assert tls_raw(url, pem, base + b"Transfer-Encoding: chunked\r\n\r\n0\r\n\r\n") == b"400"
    assert tls_raw(url, pem, base + b"Content-Length: 2\r\nContent-Length: 2\r\n\r\n{}") == b"400"
    assert tls_raw(url, pem, base + b"Content-Length: +2\r\n\r\n{}") == b"400"
    assert tls_raw(url, pem, base + b"\r\n") == b"411"
    assert tls_raw(url, pem, base + b"Content-Length: 99999999\r\n\r\n") == b"413"
    assert tls_raw(url, pem, b"GET /v1/installations HTTP/1.1\r\nHost: x\r\nContent-Length: 2\r\n" + auth + b"\r\n{}") == b"400"


def test_plain_client_cannot_talk_http(store, tls_server):
    srv, url, pem = tls_server
    s = socket.create_connection(("127.0.0.1", srv.server_address[1]), 5)
    s.sendall(b"GET /v1/installations HTTP/1.1\r\nHost: x\r\n\r\n")
    s.settimeout(5)
    try:
        data = s.recv(1024)
    except OSError:
        data = b""
    assert not data.startswith(b"HTTP/")


def test_started_attempt_survives_disconnect(store, clock, tls_server):
    _, url, pem = tls_server
    sub, rec = Client(paired_client(store, url, pem)[0]), Client(paired_client(store, url, pem)[0])
    rec.register(session(None))
    j = sub.submit(sid(1), SPEC)
    off = rec.claim(claim_body(clock))["offer"]
    rec.start(off["attempt_id"], {"session_id": sid("s1"), "effective_plan": {}})
    clock.t += 10_000  # disconnected for a long time
    assert sub.job(j["job_id"])["state"] == "started"
    assert [i for i in sub.installations()["installations"] if not i["connected"]]
