# Keyloom network rendering

Keyloom — powered by CorridorKey can send work to other Windows and Linux PCs on your LAN or Tailscale network. Network setup is in **Clients**; no terminal commands are needed.

Every installation hosts its own network service, can submit work, and can receive work. There is nothing to type: no host address, port or "Add network" form.

**This PC** (top of **Clients**) shows the PC name, the addresses the service answers on (every LAN, Tailscale and ZeroTier interface at once), whether it is running, and four buttons: **Start**, **Stop**, **Remove** (click twice) and **Refresh**. **Save** next to the name is the only thing that changes the global PC name; it is sent to every connected PC.

**Other PCs** lists discovered PCs you have not joined yet. Keyloom scans once when Clients is first opened and again only when you press **ReScan** (there is an elapsed timer and **Cancel**; a scan is cut off after 35 s). Each row shows the name, addresses, network type (LAN, Tailscale, ZeroTier), reachability and join status. Tick an available PC to join it. Once joined, it moves to the **Clients** table below; it is not duplicated in the discovery list. PCs are identified by certificate, never by name, so two PCs may have the same name.

## 1. Hosting happens automatically

The first time Keyloom starts it installs this PC's per-user service (a `systemd --user` unit on Linux, a per-user logon task on Windows, with an HKCU Run entry as fallback) and joins this PC to its own network with **Accept work** on. The service starts at sign-in, needs no administrator rights, and runs separately from the Keyloom window. It only coordinates the machine list and queue; it does not render. Rendering is done by that PC's Keyloom client, which must be open or running in [background mode](user-guide.md#background-running). The **Receiver** line therefore reports the service coordinator only: *offline* means the app is not receiving, even if the service is up.

- **Stop** stops the service and Keyloom does not restart it during that run. It returns with **Start** or the next OS sign-in.
- **Remove** removes the service and remembers that you did; Keyloom does not install it again until you press **Start**. Its database keeps the network identity, so starting again changes nothing for other PCs.
- Every request has a 45 s limit. If it passes, the state shows as *unknown*; press **Refresh** before trying again.

The first run may show an OS firewall prompt: allow it on your trusted/private network only. On Linux Keyloom does not open the firewall for you.

![Clients page with service controls, discovery and the unified client table](screenshots/native/12-clients.png)

*Current UI, captured in an isolated session with simulated PCs, addresses and hardware. This is a documentation example, not a live network or performance measurement.*

## 2. Use other PCs

Tick an available PC in **Other PCs**. Its owner must enable **Let other PCs join this PC**. Keyloom connects using the certificate fingerprint the scan showed; a changed certificate is refused. Addresses are refreshed on each scan, so moving between LAN and Tailscale does not require re-joining. New connections offer the PC you selected; older connections may still offer every member of their network.

The **Clients** table puts **This computer** and joined network PCs together. It shows the client name, connection kind, address selection, status, reported GPU and network ID. Drag a column header's right edge to resize it; scroll horizontally if necessary.

| Control | What it does |
|---|---|
| **Check workers** | Starts/probes local workers so their GPU and capabilities can be reported. **Details** shows model identity and a **Recheck** control when idle. |
| **Address** | **Automatic** chooses the connection address; select a listed address to prefer LAN, Tailscale or another available route. |
| **Connect** | Enables or disables this connection without deleting saved files, attempts or jobs. |
| **Accept work** | On a network PC's row, allows **Automatic** to send shots to that PC. On this computer's own connection, allows this PC to receive work from other PCs. |
| **Default for Batch job** | Chooses the preferred network for batch submissions that do not specify a PC. It does not pin every Automatic shot to that PC. |
| **Remove** | Removes a network connection; unavailable while unfinished work or an unresolved attempt would be orphaned. |

Expand **Default assignment** to choose the session's default placement: **Automatic** or a particular available client. Each shot can still have its own assignment. Keyloom accepts work from one network at a time so separate hosts cannot reserve the same GPU concurrently.

## 3. Control who can join this PC

Under **This PC**, **Let other PCs join this PC** controls new address-based joins. Turn it off to refuse new joins; it does not disconnect PCs that already joined or interrupt their jobs. Turn it on only on a trusted network. The current Clients page has no invitation-file creation or import controls.

Expand **PCs registered on this PC** to inspect existing logins, when they joined, when they were last seen and whether they are online or in use. **Refresh** reloads the list. **Retire** asks for confirmation and revokes a selected login; that PC must join again before using this PC. This PC's own login and logins in use cannot be retired. **Retire all unused** selects only offline, unused logins other than this PC's own.

## 4. Map shared storage on each PC

Footage and outputs live on shared storage that each receiving user can access. Keyloom sends a shared location name plus a relative path; each PC resolves that name under its own local drive or mount.

Open **Settings → Path mapping** on every PC:

![Path mapping](screenshots/native/10-path-mapping.png)

| Field | Linux example | Windows example |
|---|---|---|
| Shared location name | `Kessel` | `Kessel` |
| Folder on this PC | `/mnt/Kessel` | `R:\` |

Map the shared **root** (here `R:\` on Windows is the same storage as `/mnt/Kessel` on Linux). Use the **same logical name on every PC**; only the local folder differs. Projects under that root then resolve everywhere. Production footage can sit on a read-only share, but any location where a receiver writes (new results, and hint assets for steps that build them) must be writable: **Test read and write** shows what this PC can do. Then press **Apply settings**. Invalid names or non-absolute paths block Apply. Saving a mapping does not prove the drive is currently reachable; check the access result before rendering.

The mappings must cover both project source and output folders. If they are on different shares, add both locations. Windows mapped drives belong to the signed-in user. Connect them in that user's desktop session; a system account or SSH session may not see them.

## 5. Choose where each shot runs

On the Batch page:

1. Tick clips and use **Assign N selected to → Apply to N**, or choose the destination directly on each clip card. **All** selects every clip.
2. Choose a workflow preset (or current settings) and press **Key selected N clips…**.
3. Keyloom checks the shots and queues them directly when no decisions are needed. Otherwise, **Review batch** lists the affected shots: choose **OK** to accept a proposed action or **Skip** to leave a shot out, then **Queue N clips**.

**Automatic** waits for a suitable free PC. **This computer** or a named PC fixes the destination. A shot's own In/Out, input colour and hint prompts are retained. Each whole shot runs on one receiving PC through its Hint, Key and Deliver stages; a shot is not split across PCs. Network work writes new results. Resume and Replace stay local.

The destination checks models, shared paths, free output paths and the frame contract before accepting a job. Source-only clips are prepared on the receiving PC. A PC refuses a second attempt while it still owns one, even if the link drops.

## 6. Watch the Batch queue

The Batch board's **Queue** has two tabs: **Machines** for work assigned to PCs, and **Waiting (N)** for Automatic shots that have not been assigned yet. **Clients** remains the place for availability and setup.

### Machines

![Machines tab with computer meters and ordered pipeline stages](screenshots/native/15-batch-progress.png)

*Isolated documentation example: one simulated pipeline is running while three shots wait for a free PC. Hardware readings are fixture values, not measured performance.*

Each PC has a collapsible section with CPU, RAM, GPU and VRAM bars. Unavailable statistics are shown as unavailable, not zero. The current shot shows its ordered **Pre-matte**, **Hint**, **Keying** and **Export** stages as applicable. A stage marked **Waiting** is waiting for its dependency in that assigned pipeline; it is not an unassigned shot in the Waiting tab. Expand **Details** for local job diagnostics and recovery controls.

Collapse a PC to see its current shot and overall status on the header. Finished, failed and cancelled runs move into its collapsed **Earlier (N)** log. **Clear finished** clears finished entries and logs without hiding running work or deleting rendered output. History for removed PCs remains under **History** until cleared.

Cancelling running work is cooperative and keeps frames already written. A job already running remains owned by its receiver and is not reassigned while it may still be writing. When **Send again** is available for a submission that has never been attempted, it returns the shot to the end of Automatic's waiting list; an uncertain submission instead stays under reconciliation.

### Waiting list

![Waiting tab with three Automatic shots and Pause, Resume and Cancel controls](screenshots/native/16-batch-waiting.png)

*Isolated example: Studio_03 is paused; the other shots can be assigned when a suitable PC becomes free.*

**Automatic** keeps confirmed shots in one ordered waiting list rather than splitting the batch between PCs in advance. Each free PC takes one eligible shot; when it finishes, it can take the next. Network candidates must be online with both **Connect** and **Accept work** enabled. Automatic requires fresh, known GPU readings with both GPU utilization and VRAM use **below 50%**; a busy PC or unknown/stale readings are not treated as free.

- **Why:** explains why shots are still waiting.
- **Pause** holds that shot in the waiting list without assigning a PC. Other eligible shots can continue.
- **Resume** makes a paused shot eligible again; it does not pause or resume a running renderer.
- **Cancel** removes that waiting shot; **Cancel all** removes all waiting shots, including paused ones, without cancelling work already assigned to PCs.
- If an Automatic run fails, its shot returns to the end of the list and waits for another suitable PC. It will not retry a PC on which it already failed in this session. If every suitable PC has failed, the shot stays waiting rather than being silently dropped.

A shot assigned explicitly to a PC is not load-balanced in advance by Automatic. An offline destination keeps its assignment; local work can continue if a network host is unavailable.

## Troubleshooting

| Symptom | Check |
|---|---|
| Service will not start | Read the UI error: address/port in use, unavailable user service manager, or permissions. Do not start a second host on the same port. |
| Cannot join / *Join not allowed* | The other PC's service must be running, reachable through its firewall, and have **Let other PCs join this PC** enabled. ReScan if its addresses changed. |
| Different or incomplete machine list | Check the joined connections in **Clients**. Each PC hosts its own service; new connections offer the PC you selected, not necessarily every member of its network. |
| PC visible but Automatic does not use it | Check **Connect** and **Accept work** on its Clients row, keep its app running, and check the Queue's **Waiting** tab for the reason. GPU and VRAM readings must be known, fresh and below 50%. |
| Shared files unavailable | Open the share as the desktop user and test the mapping in Keyloom. |
| PC missing from the list | ReScan. Check that Keyloom is running there and the PCs share a network (LAN, Tailscale or ZeroTier). Do not delete a service folder: that would create a different network and orphan every joined PC. |

Pairing credentials and the service database stay on local disk, outside projects. Keep their tokens private. Renaming the app preserves the existing settings and model locations.

## Limits

- Every PC must run Keyloom 2.4 or newer to be found; earlier versions have no discovery and must be updated.
- The OS firewall must allow the Keyloom service (TCP 8443-8452 and UDP 44554); Keyloom never opens ports itself.
- Discovery uses LAN broadcast plus Tailscale's peer list and known addresses. Networks that block broadcast or Tailscale ACLs that block the ports can hide PCs from the list; correct reachability and ReScan. The current UI has no manual-address or invitation-file fallback.
- The background service is separate from the Keyloom window: **Stop** keeps it stopped until **Start** or the next sign-in.
- If the host's identity was revoked, Keyloom reports "identity revoked" and leaves your state untouched. There is no automatic re-issue: restore the host database or recover the identity manually; joining again alone does not restore it.
