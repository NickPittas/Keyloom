# Network discovery: validation record (Keyloom 2.4.0)

Scope: only the changed networking (automatic per-user service, discovery, Clients controls). Rendering was not re-run.
No secrets, invitation codes or tokens appear here.

| Gate | Result | Evidence |
|---|---|---|
| G1 source contracts | PASS | `tests/coordinator` + `tests/test_packaging.py`: 162 passed; `cargo test --offline --lib` (isolated): 178 passed. Covers 64-hex pin sent (not 16), wrong leaf rejected before auth, failover only before request bytes, no replay after lost reply, own-host queue offers no submitters, invitation routes match the pin, virtual NICs are not self. |
| G2 builds | PASS | Linux 2.4.0-9c57457dc0 (glibc floor 2.30, launch validation OK, psutil 7.2.2 present). Windows native build on Kamino: see "Candidates". |
| G3 live networking | SPLIT: see "G3 sub-gates" | All live sub-gates PASS after the user's LAN-only ufw rules (TCP 8443:8452 and UDP 44554 from 192.168.100.0/24); only dispatch is STATIC. Endor certificate a4e4c609...6f76dab unchanged. |
| G4 Clients GUI | PASS (narrow) | Real GUI on Endor: scan with Cancel, Start/Stop/Remove(confirm)/Refresh buttons, Use invitation reveals save/browse/create/import, name shows host name. Private screenshots in `~/.cache/keyloom-gate-shots`. |
| G5 docs | PASS | `docs/network-rendering.md` "Limits" matches the above. |

## G3 sub-gates
| Sub-gate | Result | Evidence |
|---|---|---|
| Endor -> Kamino discovery (LAN + Tailscale), one PC, 64-hex pin | PASS | Scan from Endor lists Kamino-3 once with LAN and Tailscale routes reachable. |
| Per-route authenticated read-only queries to Kamino (LAN, Tailscale v4, v6) | PASS | hello + installations over each route, pinned fingerprint 99a03044. |
| Kamino -> Endor over **Tailscale** TCP 8443 | PASS | Test-NetConnection True; invitation redeem succeeded over this route. |
| Kamino -> Endor over **LAN** TCP 8443 (192.168.100.21) | **PASS** (was BLOCKED until the user added LAN-only ufw rules) | Re-run after the rules: `Test-NetConnection 192.168.100.21 -Port 8443` -> TcpTestSucceeded True (exit 0); installed Kamino client with its saved Endor identity, `https://192.168.100.21:8443` only: authenticated `hello` fingerprint a4e4c609 (pinned) and `installations` returned 4 records. Before the rules this route timed out. |
| Kamino -> Endor UDP 44554 broadcast discovery | **PASS** | Installed Kamino `udp_probe` (3 s, exit 0) received a reply whose UDP **source address is 192.168.100.21** carrying fingerprint a4e4c609 / name endor-omarchy (also replies from Kamino itself on 192.168.100.152 and 192.168.195.119). That is a real UDP responder on Endor over the LAN, independent of Tailscale. The full scan then listed endor-omarchy once with LAN `https://192.168.100.21:8443` TLS-reachable plus both Tailscale routes (sources: broadcast_ifaces 2, tailscale ok, neighbors ok, 5.1 s, not partial). Note the LAN TLS probe alone is not UDP proof; the UDP source line above is. |
| Checkbox join, invitation-only rejection, file create/import/redeem, Refresh, Stop/Start/Remove (Endor and Kamino), same certificates | PASS | See sections above. |
| Exact-host dispatch, local-queue receive | STATIC | Rust unit tests only; rendering not re-run, Kamino receiver app offline. |

### Endor administrator step (done by the user)
The rules were these (password required, do not disable or broaden the firewall):
```
sudo ufw allow from 192.168.100.0/24 to any port 8443:8452 proto tcp comment 'Keyloom service (LAN)'
sudo ufw allow from 192.168.100.0/24 to any port 44554 proto udp comment 'Keyloom discovery (LAN)'
sudo ufw status numbered
```
Bounded proof from Kamino (PowerShell) after that, then rescan on Endor's peers:
```
Test-NetConnection 192.168.100.21 -Port 8443 -WarningAction SilentlyContinue | Select TcpTestSucceeded
```
UDP is only proven by a Keyloom scan on Kamino listing endor-omarchy with a lan route.

## Endor (installed upgrade 2.2.0 -> 2.4.0)
- Backup of settings, credentials and network state taken first; no active jobs (5 succeeded, 4 failed, 1 cancelled, none running).
- Certificate fingerprint `a4e4c609...6f76dab` unchanged across both upgrades and across Remove/Start; database, tokens and saved connections untouched.
- Service came up dual-stack on TCP 8443 and UDP 44554, advertising LAN and Tailscale (IPv4+IPv6) once each, named `endor-omarchy`.
- Scan sees this PC marked "self" (excluded from the list) with loopback, LAN and Tailscale routes all reachable.
- Stop -> stopped, no process; Start -> running again. Use invitation on -> service policy invitation-only; off -> open again (persisted in settings and `service.json`).
- Remove (needs a second click) -> unit removed, tombstone set, certificate/database kept; app restart did NOT reinstall; Start cleared the tombstone and restored the service.
- Bugs found and fixed here: coordinator client crashed (`Bad file descriptor`) after a fully read reply; Clients showed "Keyloom PC" instead of the host name when `HOSTNAME` was not exported.
- Not verified live: Refresh outcome message, controls during ReScan beyond Cancel being shown, file-based invitation join, policy reinstall on a remote PC.

## Kamino (installed upgrade 2.2.0 -> 2.4.0-332ca72d8d, authorized)
- Before: installer exe and both slices matched their `.sha256` list; no jobs in any network database; private backup of settings, credentials and network state at `C:\Users\Nick\keyloom-kamino-backup-pre24`. Silent per-user install with `/TASKS=!models` (no model download), exit 0, 5m40s.
- Certificate fingerprint `99a03044...0f99a199` identical before (backup) and after. Service runs from the new version (`pythonw.exe`, TCP 8443 dual-stack + UDP 44554), name `Kamino-3`, startup task `CorridorKey Network Service` kept.
- Windows created an automatic inbound **Block** rule for the new `pythonw.exe` (nobody could answer the prompt over SSH), so the first scan found nothing. Fix applied from the elevated SSH session: removed only that program's two auto Block rules and added two program-specific Allow rules (TCP, and UDP 44554), scoped to LocalSubnet, Tailscale 100.64.0.0/10 and fd7a:115c:a1e0::/48, ZeroTier 192.168.195.0/24, profiles Private+Public. The firewall stays on in all profiles. **Every new version path triggers this again**; the per-user installer cannot add the rule itself (no admin), so each upgrade needs one Allow click on Kamino or the same two rules.
- Endor scan: Kamino-3 listed once with 64-hex pin, LAN + 2 Tailscale routes reachable (3-9 ms) and a ZeroTier route unreachable from Endor (Endor has no ZeroTier). Clients row: "Available / receiver offline" (the receiver app was not running on Kamino).
- GUI: ticking the Kamino-3 checkbox joined it ("Joined. Your other connections stay connected."); settings keep the local-host connection (`receive` true) and add Kamino (`receive` false, enabled). During a rescan Cancel is shown, Start/Stop/Remove/Refresh, the invitation checkbox and the checked row stay usable and the previous result stays visible.
- Authenticated read-only `hello` + `installations` against the Kamino pinned identity over each route separately (LAN, Tailscale IPv4, Tailscale IPv6): all succeeded with fingerprint 99a03044. No NIC was disabled.
- Invitation-only: with the Endor service set to invitation-only, an address join to it was refused (`404 not_found`) and no credentials were written; policy restored to open afterwards.
- Cleaned up: scratch tasks `kl24build`/`kl24inst`/`kl24tmp` deleted, scratch state removed. The older Block rule for the `dist24\stage` python (created by the scratch test) was left in place.

## Invitation, Refresh and service lifecycle (live)
- Endor GUI: Use invitation reveals the controls; **Create invitation** wrote a private (0600) file, "Invitation saved. It can be reused for 7 days by any PC; keep the file private."
- Endor GUI import of that file: "This computer was already on that network; keeping its connection." No new credentials or connections were created (same-certificate identity reused, nothing redeemed).
- Kamino (installed Windows code, netlink `pair`) redeemed the file for real: the client refused the file until its Windows ACL was restricted to the current user; the LAN route timed out before any request byte (Endor's own `ufw` blocks LAN TCP 8443 from other PCs; Tailscale is open) and pairing failed over to the Tailscale route supplied as an extra route (`reused:false`, new installation). Without the extra route the pair attempt failed with a transport timeout and an explicit "invitation MAY have been used" message; nothing was retried. The invitation file itself lists only the LAN address, so on a PC whose firewall blocks the LAN route a file join needs a scan-provided route (the GUI passes those) or a firewall allow.
- Cleanup: test installation revoked, the shared invitation removed from the database, test files and credentials deleted on both PCs, Use invitation back off. Endor certificate unchanged.
- Refresh: the button re-reads service status without a message. With the service stopped behind the GUI's back the panel kept saying "Running" until Refresh was pressed, then showed "Stopped ... (it also starts at sign-in)"; Start restored it.
- Kamino service via the installed bridge: stop (installed, not running) -> start (running) -> remove (not installed, tombstone) -> start (running); certificate `99a03044...` identical throughout, startup task back to Running, tombstone cleared, Endor rediscovers it.
- Endor firewall: `ufw` blocked LAN TCP 8443 until the user added the LAN-only allow rules; afterwards TCP and UDP both verified (G3 sub-gates table).

## Candidates
- Linux: `2.4.0-9c57457dc0` (installed on Endor; hardlinked copy of the stage kept at `~/.cache/keyloom-discovery-linux-2.4.0c/stage-keep`).
- Windows: `2.4.0-332ca72d8d` native build on Kamino (launch validation passed), installer `C:\Users\Nick\ck-local-build-1023\dist24\Keyloom-2.4.0-332ca72d8d-windows-x86_64-setup.exe` (sha256 `cea66315...c7bd67a92`, plus two `-N.bin` slices); psutil 7.2.2 bundled. Installed on Kamino (see above).

## Remaining blocked gates
1. Not exercised live: live job dispatch to Kamino (static unit coverage only; receiver app offline and rendering intentionally not re-run).
2. The release payload does not yet include the separately owned unified build script and packaging changes; rebuild both platforms from the final tree before release.
3. Address column in Clients wraps narrowly with long IPv6 addresses (cosmetic).
