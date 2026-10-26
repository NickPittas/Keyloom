"""`python -m backend.coordinator revoke|abandon`. Explicit paths only: this never touches real app config.
Joining is done from the Clients page (`/v1/join`); this CLI is operator maintenance only.

Operator (server STOPPED; takes the same lock, refuses while the server runs):
  python -m backend.coordinator revoke --state-dir D inst_...
  python -m backend.coordinator abandon --state-dir D --reason "PC died" --i-understand-writer-not-stopped att_...
No token is ever accepted on the command line."""
import argparse
import os
import sys

from . import pki
from .client import CoordinatorError
from .schema import ApiError
from .store import Store


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
