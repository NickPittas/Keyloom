"""Linux installer step: let this PC's trusted networks reach the Keyloom network service through an ACTIVE UFW.

Stdlib only; used by lifecycle.py for the offline and the online installer alike (one common path).
Only UFW is managed. It is never installed, enabled or reconfigured, and no rule is added without the user's yes.
Only the `ufw` command is elevated (sudo asks for the password on the real terminal; it is never captured or logged).
Rules added here carry the comment `Keyloom installer ...` and are recorded in the receipt; only those are ever removed.
Rules the user added themselves are read, counted as coverage, and never changed.
"""
from __future__ import annotations

import contextlib
import ipaddress
import os
from pathlib import Path
import re
import select
import shlex
import shutil
import subprocess
import sys
import time

TCP_RANGE = "8443:8452"  # automatic port choice in backend/network_service.py
UDP_PORT = "44554"       # discovery, backend/discovery.py
COMMENT = {"tcp": "Keyloom installer: service", "udp": "Keyloom installer: discovery"}
OWNER_PREFIX = "Keyloom installer"
# Same exclusions as backend/discovery.py: container/VM bridges are never trusted networks.
VIRTUAL = re.compile(r"(docker|br-|veth|virbr|vboxnet|vethernet)", re.I)
CGNAT = ipaddress.ip_network("100.64.0.0/10")
TS6 = ipaddress.ip_network("fd7a:115c:a1e0::/48")
MAX_NETWORKS = 16
CONSENT_SECONDS = 120
SUDO_SECONDS = 180  # includes typing the password
SUDO_PROMPT = "[sudo] password for %p (Keyloom firewall rules): "


def say(msg: str) -> None:
    print(msg, flush=True)


@contextlib.contextmanager
def user_wait(reason: str):
    """Pause the installer supervisor's automatic clock (KEYLOOM_WAIT_FILE, inherited from lifecycle) ONLY while the human
    answers a question or sudo/UAC runs. The file exists exactly as long as the block does and is removed on success, failure
    and Ctrl-C. The automatic probes around it (UFW state, interfaces) stay on the clock. Nothing is read or stored from the user."""
    f = os.environ.get("KEYLOOM_WAIT_FILE")
    made = False
    try:
        if f:
            try:
                fd = os.open(f, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
                with os.fdopen(fd, "w", encoding="utf-8") as h:
                    h.write(reason)
                made = True
            except OSError:
                pass  # no pause rather than a failed install
        yield
    finally:
        if made:
            try:
                os.unlink(f)
            except OSError:
                pass


# ---- what to allow ---------------------------------------------------------------------------------

def trusted_networks(links: list) -> list[str]:
    """CIDRs derived from `ip -j addr show up` output: private LAN and ZeroTier subnets, Tailscale's own ranges.
    Loopback, link-local, public, multicast, unspecified and container/VM adapters never qualify."""
    found: set = set()
    for link in links if isinstance(links, list) else []:
        name = str(link.get("ifname", ""))
        if not name or name == "lo" or VIRTUAL.match(name):
            continue
        for info in link.get("addr_info") or []:
            try:
                ip = ipaddress.ip_address(str(info.get("local", "")))
                plen = int(info.get("prefixlen"))
            except (ValueError, TypeError):
                continue
            if ip.is_loopback or ip.is_link_local or ip.is_unspecified or ip.is_multicast:
                continue
            n = name.lower()
            if n.startswith("tailscale"):
                net = CGNAT if ip in CGNAT else TS6 if ip in TS6 else None
            elif ip.is_private and not ip.is_global and plen >= (8 if ip.version == 4 else 48):
                net = ipaddress.ip_network(f"{ip}/{plen}", strict=False)
            else:
                net = None
            if net is not None:
                found.add(net)
    return [str(n) for n in sorted(found, key=lambda n: (n.version, n))][:MAX_NETWORKS]


def valid_cidr(text) -> bool:
    try:
        return str(ipaddress.ip_network(text, strict=True)) == text
    except (ValueError, TypeError):
        return False


def allow_args(cidr: str, proto: str) -> list[str]:
    if not valid_cidr(cidr) or proto not in COMMENT:
        raise ValueError("invalid firewall rule")
    return ["allow", "from", cidr, "to", "any", "port", TCP_RANGE if proto == "tcp" else UDP_PORT,
            "proto", proto, "comment", COMMENT[proto]]


def delete_args(cidr: str, proto: str) -> list[str]:
    return ["--force", "delete"] + allow_args(cidr, proto)[:-2]


def manual_commands(cidrs: list[str], verb=allow_args) -> list[str]:
    return ["sudo ufw " + " ".join(shlex.quote(a) for a in verb(c, p)) for c in cidrs for p in ("tcp", "udp")]


# ---- reading the firewall --------------------------------------------------------------------------

def ufw_path() -> str | None:
    return shutil.which("ufw", path=os.environ.get("PATH", "") + ":/usr/sbin:/sbin:/usr/local/sbin")


def ufw_enabled() -> bool | None:
    """True/False from the world-readable config; None when it cannot be read without root."""
    try:
        for line in open("/etc/ufw/ufw.conf", encoding="utf-8"):
            if line.strip().upper().startswith("ENABLED="):
                return line.split("=", 1)[1].strip().lower() == "yes"
    except OSError:
        pass
    return None


def firewalld_active() -> bool:
    try:
        return subprocess.run(["systemctl", "is-active", "--quiet", "firewalld"], timeout=10).returncode == 0
    except (OSError, subprocess.SubprocessError):
        return False


def interface_links() -> list:
    ip = shutil.which("ip", path=os.environ.get("PATH", "") + ":/usr/sbin:/sbin")
    if not ip:
        return []
    import json
    try:
        r = subprocess.run([ip, "-j", "addr", "show", "up"], capture_output=True, text=True, timeout=10)
        return json.loads(r.stdout) if r.returncode == 0 else []
    except (OSError, subprocess.SubprocessError, ValueError):
        return []


def _span(token: str, proto: str):
    """(lo, hi) of a `ufw status` To column such as 8443:8452/tcp; None for anything not understood (= not covered)."""
    m = re.fullmatch(r"(\d+)(?::(\d+))?(?:/(tcp|udp))?(?: \(v6\))?", token.strip())
    if not m or (m.group(3) or proto) != proto:
        return None
    lo = int(m.group(1))
    return lo, int(m.group(2) or lo)


def parse_status(text: str) -> list[dict]:
    """Rules of `ufw status` as {to, from, comment}; only plain inbound ALLOW rules without an interface clause."""
    rules = []
    for raw in text.splitlines():
        body, _, comment = raw.partition("#")
        cols = re.split(r"\s{2,}", body.strip())
        if len(cols) != 3 or cols[1] not in ("ALLOW", "ALLOW IN"):
            continue
        src = cols[2].replace("(v6)", "").strip()
        if src.lower() == "anywhere":
            net = None
        else:
            try:
                net = ipaddress.ip_network(src, strict=False)
            except ValueError:
                continue
        rules.append({"to": cols[0], "net": net, "any": src.lower() == "anywhere", "comment": comment.strip()})
    return rules


def covered(rules: list[dict], cidr: str, proto: str) -> bool:
    """Is every address of `cidr` already allowed in on the needed port(s)? Anywhere-sources only cover with the right family."""
    want = ipaddress.ip_network(cidr)
    lo, hi = (8443, 8452) if proto == "tcp" else (44554, 44554)
    for r in rules:
        span = _span(r["to"], proto)
        if not span or span[0] > lo or span[1] < hi:
            continue
        if r["any"] and ("(v6)" in r["to"]) == (want.version == 6):
            return True
        n = r["net"]
        if n is not None and n.version == want.version and want.subnet_of(n):
            return True
    return False


def missing(rules: list[dict], cidrs: list[str]) -> list[tuple[str, str]]:
    return [(c, p) for c in cidrs for p in ("tcp", "udp") if not covered(rules, c, p)]


# ---- elevated ufw ----------------------------------------------------------------------------------

def sudo_ufw(ufw: str, args: list[str], capture: bool = False):
    """(returncode, stdout) of `sudo ufw ARGS` with the password prompt on the real terminal; (None, '') on timeout."""
    cmd = ([] if os.geteuid() == 0 else ["sudo", "-p", SUDO_PROMPT, "--"]) + [ufw] + args
    p = subprocess.Popen(cmd, stdout=subprocess.PIPE if capture else None, text=True)
    try:
        with user_wait("sudo password prompt for UFW rules"):
            out, _ = p.communicate(timeout=SUDO_SECONDS)
        return p.returncode, out or ""
    except subprocess.TimeoutExpired:
        p.terminate()
        try:
            p.wait(10)
        except subprocess.TimeoutExpired:
            p.kill()
            p.wait()
        return None, ""
    except BaseException:
        p.kill()
        p.wait()
        raise


def interactive() -> bool:
    return sys.stdin.isatty()  # stdout is the installer supervisor's pipe


def ask(question: str) -> bool:
    """Y/n on the terminal with a deadline; silence or EOF is a no."""
    print(f"{question} [Y/n] ", end="", flush=True)
    with user_wait("waiting for your answer about firewall rules"):
        ready = select.select([sys.stdin], [], [], CONSENT_SECONDS)[0]
    if not ready:
        print()
        return False
    return sys.stdin.readline().strip().lower() in ("", "y", "yes")


# ---- install ---------------------------------------------------------------------------------------

def run(mode: str, prior: dict | None) -> dict | None:
    """Make sure the trusted networks can reach the service. Returns the receipt record (None: not applicable here).
    status: ok | not_needed | pending | unsupported. Never raises for an ordinary failure; never changes anything unasked."""
    if sys.platform.startswith("win"):
        return run_windows(mode, prior)
    if not sys.platform.startswith("linux"):
        return None
    owned = list((prior or {}).get("owned") or [])
    rec = {"owned": owned, "networks": [], "commands": [], "checked_at": time.strftime("%Y-%m-%dT%H:%M:%S%z")}

    def done(status, detail, commands=()):
        rec.update(status=status, detail=detail, commands=list(commands))
        return rec

    ufw = ufw_path()
    if not ufw:
        if firewalld_active():
            return done("unsupported", "firewalld is active; only UFW is managed here. Allow TCP 8443-8452 and UDP 44554 "
                        "from your trusted networks in firewalld yourself.")
        return done("not_needed", "UFW is not installed (other firewalls such as raw iptables/nftables are not inspected)")
    if ufw_enabled() is False:
        return done("not_needed", "UFW is installed but not enabled; it was left that way")
    nets = trusted_networks(interface_links())
    rec["networks"] = nets
    if not nets:
        return done("pending", "no private LAN, ZeroTier or Tailscale network is up; connect one and run the installer again")
    cmds = manual_commands(nets)
    if (prior or {}).get("status") == "ok" and set(nets) <= set(prior.get("networks") or []):
        # Already settled for these networks: no sudo, nothing changed. The old "added N rule(s)" text described the earlier
        # install, so it is replaced; the recorded owned rules stay.
        return dict(prior, checked_at=rec["checked_at"], commands=[],
                    detail="UFW rules recorded by an earlier install still cover these networks; nothing was added or changed")
    if mode == "skip":
        return done("pending", "firewall setup was skipped (--firewall skip); network access from other PCs is not confirmed", cmds)
    if not interactive():
        return done("pending", "no terminal to ask for permission; network access from other PCs is not confirmed", cmds)
    say("\nNetwork firewall (UFW is active)\n"
        "  Keyloom lets other PCs find and use this one over TCP 8443-8452 and UDP 44554.\n"
        "  The installer can allow exactly that, only from these networks of this PC:\n    " + "\n    ".join(nets) + "\n"
        "  It needs sudo to read UFW and add what is missing; nothing else runs as root. UFW stays enabled; existing rules\n"
        "  are never changed. Rules added are commented 'Keyloom installer ...' and removed again on uninstall.")
    if not ask("Check UFW and add the missing rules (sudo password prompt follows)?"):
        return done("pending", "firewall setup was declined; network access from other PCs is not confirmed", cmds)
    rc, out = sudo_ufw(ufw, ["status"], capture=True)
    if rc != 0:
        return done("pending", "UFW could not be read with sudo" + (" (timed out)" if rc is None else ""), cmds)
    todo = missing(parse_status(out), nets)
    if not todo:
        return done("ok", "UFW already allows the Keyloom ports from these networks; no rule was added")
    for i, (cidr, proto) in enumerate(todo):
        rc, _ = sudo_ufw(ufw, allow_args(cidr, proto))
        if rc != 0:
            return done("pending", "adding a UFW rule failed" + (" (timed out)" if rc is None else "") + "; "
                        f"{len(todo) - i} rule(s) still missing", [_one(c, p) for c, p in todo[i:]])
        if cidr not in owned:
            owned.append(cidr)
    return done("ok", f"added {len(todo)} UFW rule(s); UFW stays enabled")


def _one(cidr: str, proto: str) -> str:
    return "sudo ufw " + " ".join(shlex.quote(a) for a in allow_args(cidr, proto))


# ---- uninstall -------------------------------------------------------------------------------------

# ---- Windows: program-specific Allow rules for the installed pythonw.exe (the network service) -----------------------------
# Windows firewall rules are tied to the program path. A new version/prefix has a new pythonw.exe, which would make Windows ask
# the user ("Python" prompt) and, unanswered, leave the PC unreachable. The installer checks (read-only, no admin) whether the
# path is already allowed and otherwise offers ONE elevated step (UAC) that adds exactly these rules, records them, and never
# touches rules it did not create.
WIN_GROUP = "Keyloom"
WIN_REMOTE = ["LocalSubnet", "100.64.0.0/10", "fd7a:115c:a1e0::/48", "192.168.195.0/24"]  # LAN, Tailscale, ZeroTier
WIN_PROFILES = ["Private", "Public"]
WIN_RULES = (("tcp", "8443-8452", "service"), ("udp", "44554", "discovery"))


def win_ps(script: str, timeout: int = 60) -> tuple[int, str]:
    r = subprocess.run(["powershell", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script],
                       capture_output=True, text=True, timeout=timeout, creationflags=0x08000000)
    return r.returncode, r.stdout


def win_program() -> str:
    return str(Path(sys.executable).with_name("pythonw.exe"))


def win_version_name(program: str) -> str:
    parts = re.split(r"[\\/]", program)  # ...\versions\<id>\runtime\python\pythonw.exe
    return parts[-4] if len(parts) >= 4 else "Keyloom"


def win_rule_specs(program: str) -> list[dict]:
    return [{"name": f"Keyloom network {what} ({win_version_name(program)})", "program": program, "protocol": proto.upper(),
             "ports": ports, "remote": WIN_REMOTE, "profiles": WIN_PROFILES} for proto, ports, what in WIN_RULES]


def win_allowed(program: str) -> bool | None:
    """True when an enabled inbound Allow rule already exists for exactly this program (any rule, ours or the user's)."""
    q = ("$p=" + _ps(program) + "; $f=Get-NetFirewallApplicationFilter -ErrorAction SilentlyContinue | "
         "Where-Object { $_.Program -ieq $p }; $n=0; foreach($a in $f){ $r=$a | Get-NetFirewallRule; "
         "if($r.Enabled -eq 'True' -and $r.Direction -eq 'Inbound' -and $r.Action -eq 'Allow'){ $n++ } }; Write-Output $n")
    try:
        rc, out = win_ps(q)
        return int(out.strip().splitlines()[-1]) > 0 if rc == 0 and out.strip() else None
    except (OSError, subprocess.SubprocessError, ValueError):
        return None


def _ps(text: str) -> str:
    return "'" + text.replace("'", "''") + "'"


def win_add_script(specs: list[dict]) -> str:
    parts = []
    for r in specs:
        parts.append(f"New-NetFirewallRule -DisplayName {_ps(r['name'])} -Group {_ps(WIN_GROUP)} -Direction Inbound -Action Allow "
                     f"-Program {_ps(r['program'])} -Protocol {r['protocol']} -LocalPort {r['ports']} "
                     f"-Profile {','.join(r['profiles'])} -RemoteAddress {','.join(r['remote'])} | Out-Null")
    return "; ".join(parts)


def win_remove_script(owned: list[dict]) -> str:
    return "; ".join(f"Get-NetFirewallRule -DisplayName {_ps(r['name'])} -ErrorAction SilentlyContinue | "
                     f"Where-Object {{ ($_ | Get-NetFirewallApplicationFilter).Program -ieq {_ps(r['program'])} }} | Remove-NetFirewallRule"
                     for r in owned)


def win_elevated(script: str) -> bool:
    """One UAC prompt (the user sees exactly the rules) -> run the script elevated and wait. False if declined/failed."""
    ps = ("$p=Start-Process powershell -Verb RunAs -Wait -PassThru -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-Command',"
          + _ps(script) + "; exit $p.ExitCode")
    try:
        with user_wait("Windows administrator (UAC) approval for firewall rules"):
            return win_ps(ps, timeout=600)[0] == 0
    except (OSError, subprocess.SubprocessError):
        return False


def win_ask(question: str) -> bool:
    import threading
    answer = []
    t = threading.Thread(target=lambda: answer.append(input(f"{question} [Y/n] ")), daemon=True)
    t.start()
    with user_wait("waiting for your answer about firewall rules"):
        t.join(CONSENT_SECONDS)
    return bool(answer) and answer[0].strip().lower() in ("", "y", "yes")


def run_windows(mode: str, prior: dict | None) -> dict | None:
    program = win_program()
    specs = win_rule_specs(program)
    owned = [r for r in (prior or {}).get("owned") or [] if isinstance(r, dict)]
    rec = {"owned": owned, "program": program, "commands": [], "checked_at": time.strftime("%Y-%m-%dT%H:%M:%S%z")}

    def done(status, detail, commands=()):
        rec.update(status=status, detail=detail, commands=list(commands))
        return rec

    manual = ["powershell (as Administrator): " + win_add_script(specs)]
    allowed = win_allowed(program)
    if allowed:
        return done("ok", "Windows Firewall already allows this version's pythonw.exe; nothing was added or changed")
    if allowed is None:
        return done("pending", "Windows Firewall could not be read", manual)
    if mode == "skip" or not sys.stdin or not sys.stdin.isatty():
        return done("pending", "Windows Firewall has no Allow rule for this version's pythonw.exe (setup skipped or no terminal); "
                    "other PCs may be blocked until it is added, and Windows may show a 'Python' prompt", manual)
    say("\nWindows Firewall\n  This version of Keyloom runs its network service as\n    " + program + "\n"
        "  Windows only allows incoming connections per program path, so the new path needs its own rules:\n"
        "  TCP 8443-8452 and UDP 44554, Private and Public networks, from the local subnet, Tailscale and ZeroTier only.\n"
        "  Windows will ask for administrator approval once. Nothing else is changed; rules from older versions stay.")
    if not win_ask("Add these rules now?"):
        return done("pending", "firewall setup was declined", manual)
    if not win_elevated(win_add_script(specs)):
        return done("pending", "the elevated step was cancelled or failed", manual)
    if win_allowed(program) is not True:
        return done("pending", "the rules were not found after the elevated step", manual)
    have = {(r.get("name"), r.get("program")) for r in owned}
    owned.extend(r for r in specs if (r["name"], r["program"]) not in have)
    return done("ok", f"added {len(specs)} Windows Firewall rule(s) for this version's pythonw.exe")


def remove_windows(rec: dict | None, mode: str) -> str:
    owned = [r for r in (rec or {}).get("owned") or [] if isinstance(r, dict) and r.get("name") and r.get("program")]
    if not owned:
        return ""
    cmd = "powershell (as Administrator): " + win_remove_script(owned)
    left = "Keyloom firewall rules were left in Windows Firewall. Remove them with:\n  " + cmd
    if mode == "skip" or not sys.stdin or not sys.stdin.isatty():
        return left
    say("\nThe installer added Windows Firewall rules for: " + ", ".join(sorted({r["program"] for r in owned})) + ". Only those are removed.")
    if not win_ask("Remove them now (administrator approval follows)?"):
        return left
    return "Keyloom firewall rules removed from Windows Firewall." if win_elevated(win_remove_script(owned)) else left


def remove(rec: dict | None, mode: str) -> str:
    """Delete only the rules this installer added (receipt `owned`). Returns a message; never raises for ordinary failures."""
    if sys.platform.startswith("win"):
        return remove_windows(rec, mode)
    owned = [c for c in (rec or {}).get("owned") or [] if valid_cidr(c)]
    if not owned or not sys.platform.startswith("linux"):
        return ""
    cmds = manual_commands(owned, delete_args)
    left = "Keyloom firewall rules were left in UFW. Remove them with:\n  " + "\n  ".join(cmds)
    ufw = ufw_path()
    if not ufw:
        return ""
    if mode == "skip" or not interactive():
        return left
    say("\nThe installer added UFW rules for " + ", ".join(owned) + " (TCP 8443-8452, UDP 44554). Only those are removed.")
    if not ask("Remove them now (sudo password prompt follows)?"):
        return left
    rc, out = sudo_ufw(ufw, ["status"], capture=True)
    if rc != 0:
        return left
    rules = parse_status(out)
    for cidr in owned:
        for proto in ("tcp", "udp"):
            ours = [r for r in rules if r["net"] == ipaddress.ip_network(cidr)
                    and _span(r["to"], proto) == ((8443, 8452) if proto == "tcp" else (44554, 44554))
                    and (not r["comment"] or r["comment"].startswith(OWNER_PREFIX))]
            if ours and sudo_ufw(ufw, delete_args(cidr, proto))[0] != 0:
                return left
    return "Keyloom firewall rules removed from UFW."


def main(argv=None) -> int:
    """`firewall.py MODE PRIOR_JSON OUT`: run() in this interpreter; the record goes to OUT (lifecycle.firewall_step)."""
    import json
    mode, prior, out = (argv if argv is not None else sys.argv[1:])
    if mode not in ("ask", "skip"):
        return 64
    rec = run(mode, json.loads(prior) or None)
    with open(out, "w", encoding="utf-8") as f:
        json.dump(rec, f)
    return 0


if __name__ == "__main__":
    sys.exit(main())
