import threading

import pytest

from backend.coordinator import pki
from backend.coordinator.server import CoordinatorServer
from backend.coordinator.store import Store
from backend.netlink import Bridge


@pytest.fixture
def env(tmp_path):
    store = Store(tmp_path / "c.sqlite3")
    pem, key = pki.generate_identity(["127.0.0.1"])
    c, k = tmp_path / "cert.pem", tmp_path / "key.pem"
    pki.write_private_new(str(k), key)
    c.write_text(pem)
    srv = CoordinatorServer(("127.0.0.1", 0), pki.server_context(str(c), str(k)), store)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    url = f"https://127.0.0.1:{srv.server_address[1]}"
    d = tmp_path / "client"
    d.mkdir(mode=0o700)
    yield {"store": store, "url": url, "pem": pem, "dir": d}
    srv.shutdown()
    srv.server_close()
    store.close()


def invite(env, name="inv.json", admin=False):
    p = env["dir"] / name
    pki.write_invitation(str(p), env["url"], env["pem"], env["store"].create_pairing(admin), 4_000_000_000)
    return str(p)


def call(bridge, op, args=None, rid="r1"):
    import json
    msg = {"id": rid, "op": op}
    if args is not None:
        msg["args"] = args
    resp, _ = bridge.handle(json.dumps(msg).encode())
    return resp


def connected(env, name="pc"):
    """A bridge that has paired and connected a fresh installation; returns (bridge, creds_path)."""
    b = Bridge()
    creds = str(env["dir"] / f"{name}.json")
    assert call(b, "pair", {"invitation_path": invite(env, name + "-inv.json"), "credentials_path": creds, "name": name})["ok"]
    assert call(b, "connect", {"credentials_path": creds})["ok"]
    return b, creds
