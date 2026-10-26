"""Request boundaries: addresses may change, authenticated mutations must never be replayed."""
import hashlib

import threading

import pytest

from backend.coordinator import client, pki


@pytest.mark.parametrize("response_fails", [False, True])
def test_failover_only_before_request_bytes(monkeypatch, response_fails):
    calls, requests = [], []

    class Socket:
        def getpeercert(self, binary_form=False): return b"pinned leaf"
        def settimeout(self, value): pass
        def close(self): pass

    class Context:
        def wrap_socket(self, raw, **kw): return raw

    class Response:
        status = 200
        def isclosed(self): return False
        def read(self, limit): return b""

    class Connection:
        def __init__(self, host, port): self.host = host
        def request(self, *args): requests.append(self.host)
        def getresponse(self):
            if response_fails:
                raise ConnectionResetError("reply lost after mutation")
            return Response()
        def close(self): pass

    def connect(address, timeout):
        calls.append(address[0])
        if len(calls) == 1:
            raise ConnectionRefusedError()
        return Socket()

    monkeypatch.setattr(client.socket, "create_connection", connect)
    monkeypatch.setattr(client.http.client, "HTTPConnection", Connection)
    args = (["https://192.168.1.2:8443", "https://100.64.1.2:8443", "https://192.168.1.3:8443"],
            Context(), hashlib.sha256(b"pinned leaf").hexdigest(), "POST", "/v1/jobs", b"{}", {}, 5)
    if response_fails:
        with pytest.raises(ConnectionResetError):
            client.exchange(*args)
    else:
        assert client.exchange(*args)[0] == "https://100.64.1.2:8443"
    assert calls == ["192.168.1.2", "100.64.1.2"]
    assert requests == ["100.64.1.2"]


def test_wrong_leaf_is_rejected():
    class Socket:
        def getpeercert(self, binary_form=False): return b"another leaf"

    with pytest.raises(pki.SecurityError):
        pki.check_pin(Socket(), hashlib.sha256(b"pinned leaf").hexdigest())


def test_preferred_address_is_tried_first_and_unknown_is_ignored():
    url, ts = "https://192.168.1.2:8443", "https://100.64.1.2:8443"
    c = object.__new__(client.Client)
    c._lock, c.url, c._known, c.last = threading.Lock(), url, [url, ts], None
    c.prefer(ts)
    assert c._order()[0] == ts
    c.last = None
    c.prefer("https://10.9.9.9:8443")  # not a known address: nothing changes
    assert c.last is None and c._order() == [url, ts]
