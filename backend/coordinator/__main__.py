"""`python -m backend.coordinator serve|pair`. Explicit paths only: this never touches real app config.

Server (opt-in; binds loopback unless told otherwise):
  python -m backend.coordinator serve --state-dir D --advertise-host nas-pc.lan --host 192.168.1.20 \\
      --invite-out /private/place/invite.json [--invite-admin]
  The invitation file holds the one-time code (valid 10 min) and certificate; move it to the client over a
  channel you trust (USB, scp you verified). It is created mode 0600 and never printed.
Client:
  python -m backend.coordinator pair --invitation invite.json --credentials /private/place/creds.json --name "Edit bay 2"
Operator (server STOPPED; takes the same lock, refuses while serve runs):
  python -m backend.coordinator revoke --state-dir D inst_...
  python -m backend.coordinator abandon --state-dir D --reason "PC died" --i-understand-writer-not-stopped att_...
  Credentials are created 0600 (Windows: owner-only ACL, verified) and refused if they already exist.
No token or code is ever accepted on the command line."""
import argparse
import os
import sys
import time

from . import pki
from .client import CoordinatorError, redeem_to_file
from .schema import ApiError
from .server import CoordinatorServer
from .store import Store


def serve(a):
    os.makedirs(a.state_dir, mode=0o700, exist_ok=True)
    lock = pki.OwnerLock(os.path.join(a.state_dir, "coordinator.lock"))
    cert, key = (os.path.join(a.state_dir, n) for n in ("server-cert.pem", "server-key.pem"))
    if os.path.exists(cert) != os.path.exists(key):
        raise pki.SecurityError("certificate and key files are inconsistent; refusing to regenerate")
    hosts = a.advertise_host or ([a.host] if a.host not in ("0.0.0.0", "::") else [])
    if not os.path.exists(cert):
        cert_pem, key_pem = pki.generate_identity(hosts)
        pki.write_private_new(key, key_pem)
        pki.write_private_new(cert, cert_pem)
    pki.check_private(key)
    cert_pem = open(cert, encoding="utf-8").read()
    store = Store(os.path.join(a.state_dir, "coordinator.sqlite3"))
    server = CoordinatorServer((a.host, a.port), pki.server_context(cert, key), store,
                               advertise_url=f"https://{hosts[0]}:{a.port}" if hosts and a.port else None)
    port = server.server_address[1]
    if a.invite_out:
        if not hosts:
            raise pki.SecurityError("--invite-out needs --advertise-host (or a specific --host)")
        code = store.create_pairing(a.invite_admin, 600)
        pki.write_invitation(a.invite_out, f"https://{hosts[0]}:{port}", cert_pem, code, time.time() + 600)
        print(f"invitation written to {a.invite_out} (expires in 10 minutes)")
    print(f"listening on https://{a.host}:{port}; certificate sha256 {pki.fingerprint(cert_pem)}", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
        store.close()
        del lock


def pair(a):
    inv = pki.read_invitation(a.invitation)
    iid = redeem_to_file(inv, a.credentials, a.name)
    print(f"paired as {iid}; server certificate sha256 {inv['fingerprint_sha256']}; credentials stored in {a.credentials}")


def _operate(state_dir):
    """Operator commands need the server stopped: same lock, same store, never a second writer."""
    if not os.path.isfile(os.path.join(state_dir, "coordinator.sqlite3")):
        raise pki.SecurityError(f"no coordinator database in {state_dir}")
    lock = pki.OwnerLock(os.path.join(state_dir, "coordinator.lock"))
    return lock, Store(os.path.join(state_dir, "coordinator.sqlite3"))


def revoke(a):
    lock, store = _operate(a.state_dir)
    try:
        fenced = store.revoke(a.installation_id)
    finally:
        store.close()
        del lock
    print(f"revoked {a.installation_id}: credentials, session and unstarted offers released; it may be re-paired and its GPU re-registered.")
    for att in fenced:
        print(f"  attempt {att} is STARTED and stays fenced (its writer may still run); resolve it with `abandon` only after you have checked the machine.")


def abandon(a):
    lock, store = _operate(a.state_dir)
    try:
        r = store.abandon(a.attempt_id, a.reason)
    finally:
        store.close()
        del lock
    print(f"attempt {r['attempt_id']} / job {r['job_id']} marked failed. Nothing was stopped or unlocked and the job was not requeued.")


def main(argv=None):
    p = argparse.ArgumentParser(prog="python -m backend.coordinator", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("serve")
    s.add_argument("--state-dir", required=True)
    s.add_argument("--host", default="127.0.0.1")
    s.add_argument("--port", type=int, default=8443)
    s.add_argument("--advertise-host", action="append", help="host/IP clients will use; goes into the certificate SAN")
    s.add_argument("--invite-out")
    s.add_argument("--invite-admin", action="store_true")
    s.set_defaults(fn=serve)
    c = sub.add_parser("pair")
    c.add_argument("--invitation", required=True)
    c.add_argument("--credentials", required=True)
    c.add_argument("--name", required=True)
    c.set_defaults(fn=pair)
    r = sub.add_parser("revoke", help="(server stopped) revoke an installation's credentials")
    r.add_argument("--state-dir", required=True)
    r.add_argument("installation_id")
    r.set_defaults(fn=revoke)
    b = sub.add_parser("abandon", help="(server stopped) mark a lost STARTED attempt failed; never requeues, never stops a writer")
    b.add_argument("--state-dir", required=True)
    b.add_argument("--reason", required=True)
    b.add_argument("--i-understand-writer-not-stopped", action="store_true", required=True,
                   help="acknowledge this neither stops the render process nor releases any lock on the shared storage")
    b.add_argument("attempt_id")
    b.set_defaults(fn=abandon)
    a = p.parse_args(argv)
    try:
        a.fn(a)
    except (pki.SecurityError, CoordinatorError, ApiError, OSError, ValueError) as e:
        print(f"error: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
