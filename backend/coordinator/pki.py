"""TLS identity, invitations and private credential files.
Trust model: the invitation (delivered through a channel the user verifies) carries the server certificate
PEM *and* its SHA-256 fingerprint. The client checks the fingerprint offline, then uses that certificate as
the only trust anchor with hostname checking. No CERT_NONE, no trust-on-first-use."""
import datetime
import functools
import hashlib
import hmac
import ipaddress
import json
import os
import re
import ssl
import stat
import subprocess
from urllib.parse import urlsplit

from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.x509.oid import ExtendedKeyUsageOID, NameOID


class SecurityError(Exception):
    """A credential/trust precondition failed. Never retried or bypassed."""


class OwnerBusy(SecurityError):
    """Another live process already holds this exclusive lock."""


class PairedNotSaved(SecurityError):
    """The server accepted the pairing but the credentials could not be stored: the invitation is spent."""

    def __init__(self, installation_id, why):
        super().__init__(f"paired as {installation_id} but the credentials could not be saved ({why}); the invitation is USED and "
                         f"the token is lost. Ask the server owner to revoke {installation_id} and issue a new invitation.")
        self.installation_id = installation_id


class PairingUncertain(SecurityError):
    """The request may or may not have reached the server before the connection failed."""

    def __init__(self, why):
        super().__init__(f"pairing failed in transit ({why}); the invitation MAY have been used. Nothing was retried. "
                         "If a new attempt is refused, ask the server owner to revoke the installation and issue a new invitation.")


_IS_WINDOWS = os.name == "nt"
_windows_run = functools.partial(subprocess.run, creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))


# ---- private files -------------------------------------------------------------------------------

def _is_principal(head, who):
    """icacls prints `<path> DOMAIN\\user:(F)` on the first line and `   DOMAIN\\user:(F)` after it: the principal is the
    exact trailing name, preceded by whitespace or nothing (so `PC\\bob` never matches `PC\\bobby`). Case-insensitive like NTFS."""
    h, w = head.rstrip().lower(), who.lower()
    return h == w or (h.endswith(w) and h[-len(w) - 1].isspace())


def _windows_verify(path):
    """Read-only: the ACL must contain exactly one entry, the current user. Anything unverifiable fails closed."""
    try:
        who = _windows_run(["whoami"], capture_output=True, text=True, check=True).stdout.strip()
        out = _windows_run(["icacls", str(path)], capture_output=True, text=True, check=True).stdout
    except (OSError, subprocess.SubprocessError) as e:
        raise SecurityError(f"cannot verify the access list of {path} ({type(e).__name__}); refusing to use it") from None
    aces = [ln for ln in out.splitlines() if ":(" in ln]
    if not who or len(aces) != 1 or not _is_principal(aces[0].partition(":(")[0], who):
        raise SecurityError(f"{path} is accessible to other users; refusing to use it")


def protect_private(path):
    if _IS_WINDOWS:
        try:
            who = _windows_run(["whoami"], capture_output=True, text=True, check=True).stdout.strip()
            _windows_run(["icacls", str(path), "/inheritance:r", "/grant:r", f"{who}:F"], capture_output=True, text=True, check=True)
        except (OSError, subprocess.SubprocessError) as e:
            raise SecurityError(f"could not restrict {path} to the current user ({type(e).__name__})") from None
        _windows_verify(path)
    else:
        os.chmod(path, 0o600)
        check_private(path)


def check_private(path):
    """Run on EVERY read of a credential/invitation/key file, not just at creation."""
    if _IS_WINDOWS:
        _windows_verify(path)
    elif os.stat(path).st_mode & (stat.S_IRWXG | stat.S_IRWXO):
        raise SecurityError(f"{path} is accessible to other users; refusing to use it (chmod 600)")


def write_private_new(path, data):
    """Create (never overwrite), restrict, verify, and only then write the secret."""
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        protect_private(path)
        os.write(fd, data.encode())
        os.fsync(fd)
    except BaseException:
        os.close(fd)
        os.unlink(path)  # our own just-created, still-empty or half-written file
        raise
    os.close(fd)


def reserve_private(path):
    """Claim a NEW private destination (exclusive, protected, verified) before anything irreversible happens."""
    os.close(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600))
    try:
        protect_private(path)
    except BaseException:
        os.unlink(path)
        raise


def release_reserved(path):
    """Drop a reservation we made and never filled (still empty)."""
    if os.path.exists(path) and os.path.getsize(path) == 0:
        os.unlink(path)


def fill_reserved(path, data):
    check_private(path)
    with open(path, "r+", encoding="utf-8") as f:
        f.write(data)
        f.flush()
        os.fsync(f.fileno())


def read_private(path):
    check_private(path)
    with open(path, encoding="utf-8") as f:
        return f.read()


class OwnerLock:
    """Exclusive, non-blocking process lock (flock / msvcrt). Held until close() or process exit."""

    def __init__(self, path, what="coordinator"):
        self.path, self.f = path, open(path, "a+")
        try:
            if os.name == "nt":
                import msvcrt
                msvcrt.locking(self.f.fileno(), msvcrt.LK_NBLCK, 1)
            else:
                import fcntl
                fcntl.flock(self.f, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError:
            self.f.close()
            raise OwnerBusy(f"another {what} already owns {path}") from None

    def close(self):
        if self.f:
            self.f.close()  # closing the descriptor releases the lock on both platforms
            self.f = None


# ---- certificates --------------------------------------------------------------------------------

def _san(host):
    try:
        return x509.IPAddress(ipaddress.ip_address(host))
    except ValueError:
        return x509.DNSName(host)


def generate_identity(hosts, days=365):
    """Self-signed P-256 certificate whose SAN lists exactly the explicit endpoint hosts."""
    if not hosts or any(h in ("0.0.0.0", "::", "") for h in hosts):
        raise SecurityError("certificate needs explicit endpoint host names/addresses, not a wildcard bind address")
    key = ec.generate_private_key(ec.SECP256R1())
    name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "CorridorKey coordinator")])
    now = datetime.datetime.now(datetime.timezone.utc)
    pub = key.public_key()
    cert = (x509.CertificateBuilder().subject_name(name).issuer_name(name).public_key(pub)
            .serial_number(x509.random_serial_number()).not_valid_before(now - datetime.timedelta(minutes=5))
            .not_valid_after(now + datetime.timedelta(days=days))
            .add_extension(x509.SubjectAlternativeName([_san(h) for h in hosts]), False)
            .add_extension(x509.BasicConstraints(ca=True, path_length=0), True)  # self-signed anchor; strict verifiers want this
            .add_extension(x509.KeyUsage(True, False, False, False, False, True, False, False, False), True)
            .add_extension(x509.ExtendedKeyUsage([ExtendedKeyUsageOID.SERVER_AUTH]), False)
            .add_extension(x509.SubjectKeyIdentifier.from_public_key(pub), False)
            .sign(key, hashes.SHA256()))
    return (cert.public_bytes(serialization.Encoding.PEM).decode(),
            key.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8, serialization.NoEncryption()).decode())


def fingerprint(cert_pem):
    der = x509.load_pem_x509_certificate(cert_pem.encode()).public_bytes(serialization.Encoding.DER)
    return hashlib.sha256(der).hexdigest()


def cert_covers_host(cert_pem, host):
    san = x509.load_pem_x509_certificate(cert_pem.encode()).extensions.get_extension_for_class(x509.SubjectAlternativeName).value
    try:
        return ipaddress.ip_address(host) in san.get_values_for_type(x509.IPAddress)
    except ValueError:
        return host.lower() in [n.lower() for n in san.get_values_for_type(x509.DNSName)]


def server_context(cert_path, key_path):
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.minimum_version = ssl.TLSVersion.TLSv1_2
    ctx.load_cert_chain(cert_path, key_path)
    return ctx


def client_context(cert_pem, expected_fingerprint):
    """The supplied certificate is the sole trust anchor, after its fingerprint matched the invitation."""
    if not hmac.compare_digest(fingerprint(cert_pem), expected_fingerprint.lower()):
        raise SecurityError("certificate does not match the invitation fingerprint")
    ctx = ssl.create_default_context(cadata=cert_pem)  # explicit cadata: system roots are not loaded
    ctx.check_hostname, ctx.verify_mode = True, ssl.CERT_REQUIRED
    ctx.minimum_version = ssl.TLSVersion.TLSv1_2
    return ctx


def endpoint(url):
    u = urlsplit(url)
    if u.scheme != "https" or not u.hostname or u.username or u.password or u.path not in ("", "/") or u.query or u.fragment:
        raise SecurityError("server URL must be exactly https://host[:port]")
    return u.hostname, u.port or 443


def verified_trust(url, cert_pem, expected_fingerprint):
    """Offline checks shared by pairing and every later connection."""
    host, port = endpoint(url)
    ctx = client_context(cert_pem, expected_fingerprint)
    if not cert_covers_host(cert_pem, host):
        raise SecurityError(f"certificate does not cover {host}; ask the server owner for an invitation naming that address")
    return host, port, ctx


# ---- invitation / credentials --------------------------------------------------------------------

def write_invitation(path, url, cert_pem, code, expires_at):
    host, _ = endpoint(url)
    if not cert_covers_host(cert_pem, host):
        raise SecurityError(f"server certificate does not cover {host}")
    write_private_new(path, json.dumps({"version": 1, "url": url, "certificate_pem": cert_pem,
                                        "fingerprint_sha256": fingerprint(cert_pem), "code": code, "expires_at": expires_at}))


_ID_OK = re.compile(r"[A-Za-z0-9_-]{1,80}\Z")
_TOKEN_OK = re.compile(r"[A-Za-z0-9_-]{16,128}\Z")


def valid_installation_id(v):
    """The bearer is `<installation_id>.<token>` split at the FIRST dot, so neither part may contain one; the server
    issues `inst_<32 hex>` and a 43-char urlsafe token, both inside these sets."""
    return isinstance(v, str) and _ID_OK.match(v) is not None


def valid_token(v):
    return isinstance(v, str) and _TOKEN_OK.match(v) is not None


def _strings(d, kind, *keys):
    for k in keys:
        if not isinstance(d[k], str) or not 1 <= len(d[k]) <= 8192:
            raise SecurityError(f"{kind} field {k!r} must be a non-empty string")


def _load(path, kind):
    try:
        d = json.loads(read_private(path))
    except ValueError:
        raise SecurityError(f"{kind} file is not valid JSON; it was left untouched") from None
    return d


def read_invitation(path):
    d = _load(path, "invitation")
    need = {"version", "url", "certificate_pem", "fingerprint_sha256", "code", "expires_at"}
    if not isinstance(d, dict) or set(d) != need or d["version"] != 1 or isinstance(d["expires_at"], bool) \
            or not isinstance(d["expires_at"], (int, float)):
        raise SecurityError("not a valid pairing invitation")
    _strings(d, "invitation", "url", "certificate_pem", "fingerprint_sha256", "code")
    return d


def write_credentials(path, url, cert_pem, fp, installation_id, token):
    write_private_new(path, json.dumps({"version": 1, "url": url, "certificate_pem": cert_pem, "fingerprint_sha256": fp,
                                        "installation_id": installation_id, "token": token}))


def credentials_json(url, cert_pem, fp, installation_id, token):
    return json.dumps({"version": 1, "url": url, "certificate_pem": cert_pem, "fingerprint_sha256": fp,
                       "installation_id": installation_id, "token": token})


def read_credentials(path):
    d = _load(path, "credential")
    need = {"version", "url", "certificate_pem", "fingerprint_sha256", "installation_id", "token"}
    if not isinstance(d, dict) or set(d) != need or d["version"] != 1:
        raise SecurityError("credential file is corrupt or from a newer build; it was left untouched")
    _strings(d, "credential", "url", "certificate_pem", "fingerprint_sha256", "installation_id", "token")
    if not valid_installation_id(d["installation_id"]) or not valid_token(d["token"]):
        raise SecurityError("credential file is corrupt; it was left untouched")
    return d
