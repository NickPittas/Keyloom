"""Self is the loopback-verified service, never an address collision; virtual adapters are not advertised; pair uses matching routes."""

from backend import discovery, netlink
from backend.coordinator import pki

FP, OTHER = "a" * 64, "b" * 64


def test_virtual_adapters_not_advertised(monkeypatch):
    monkeypatch.setattr(discovery, "interfaces", lambda: [
        ("docker0", "172.17.0.1", None, None), ("br-1f2e", "172.18.0.1", None, None), ("veth9", "172.19.0.1", None, None),
        ("virbr0", "192.168.122.1", None, None), ("vboxnet0", "192.168.56.1", None, None), ("vEthernet (WSL)", "172.20.0.1", None, None),
        ("eth0", "192.168.1.5", None, None), ("tailscale0", "100.64.0.9", None, None), ("ztabc", "10.147.1.2", None, None)])
    assert [e["url"] for e in discovery.local_endpoints(8443)] == [
        "https://192.168.1.5:8443", "https://10.147.1.2:8443", "https://100.64.0.9:8443"]






def test_hello_info_uses_given_url_not_stale_credentials(monkeypatch):
    seen = {}
    monkeypatch.setattr(pki, "read_credentials", lambda p: {"url": "https://127.0.0.1:1111", "certificate_pem": "", "fingerprint_sha256": FP})

    class C:
        def __init__(self, creds, *a, **kw): seen["url"] = creds["url"]
        def hello(self): raise OSError()
    monkeypatch.setattr(netlink, "Client", C)
    netlink.Bridge()._hello_info("x", (), True, "https://127.0.0.1:2222")
    assert seen["url"] == "https://127.0.0.1:2222"
