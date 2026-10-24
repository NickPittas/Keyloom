# Keyloom network rendering

Keyloom — powered by CorridorKey can send work to other Windows and Linux PCs on your LAN or Tailscale network. Network setup is in **Clients**; no terminal commands are needed.

Every installation hosts its own network service, can submit work, and can receive work. There is nothing to type: no host address, port or "Add network" form.

**This PC** (top of **Clients**) shows the PC name, the addresses the service answers on (every LAN, Tailscale and ZeroTier interface at once), whether it is running, and four buttons: **Start**, **Stop**, **Remove** (click twice) and **Refresh**. **Save** next to the name is the only thing that changes the global PC name; it is sent to every connected PC.

**Other PCs** lists the Keyloom PCs found on the network. Keyloom scans once when Clients is first opened and again only when you press **ReScan** (there is an elapsed timer and **Cancel**; a scan is cut off after 35 s). Each row shows the name, all of the PC's addresses, the network type (LAN, Tailscale, ZeroTier), reachability, and status. Tick the checkbox to use that PC; untick it to stop using it (saved files, attempts and jobs are kept). A PC saved earlier but not found in the last scan stays in the list as *not found*. Rows are told apart by certificate fingerprint (hover for the full value), never by name, so two PCs may have the same name.

## 1. Hosting happens automatically

The first time Keyloom starts it installs this PC's per-user service (a `systemd --user` unit on Linux, a per-user logon task on Windows, with an HKCU Run entry as fallback) and joins this PC to its own network with **Accept work when GPU idle** on. The service starts at sign-in, needs no administrator rights, and runs separately from the Keyloom window. It only coordinates the machine list and queue; it does not render. Rendering is done by that PC's Keyloom client, which must be open or running in [background mode](user-guide.md#background-running). The **Receiver** line therefore reports the service coordinator only: *offline* means the app is not receiving, even if the service is up.

- **Stop** stops the service and Keyloom does not restart it during that run. It returns with **Start** or the next OS sign-in.
- **Remove** removes the service and remembers that you did; Keyloom does not install it again until you press **Start**. Its database keeps the network identity, so starting again changes nothing for other PCs.
- Every request has a 45 s limit. If it passes, the state shows as *unknown*; press **Refresh** before trying again.

The first run may show an OS firewall prompt: allow it on your trusted/private network only. On Linux Keyloom does not open the firewall for you.

![Clients and network setup](screenshots/native/13-pairing.png)

## 2. Use other PCs

Tick a PC in **Other PCs**. If its owner allows joining without an invitation, Keyloom connects to it using the certificate fingerprint the scan showed; a changed certificate is refused. A PC that requires an invitation shows *Needs invitation* (see below). Addresses are refreshed on every scan, so a PC that moves between LAN and Tailscale keeps working without re-joining. Connections created before this version keep working and keep offering every member PC; new ones offer only the PC you ticked.

Under **Connection details (advanced)** each saved connection keeps its own **Connect**, **Accept work when GPU idle**, **Default for Batch** and **Remove** controls. **Default for Batch** chooses the preferred network for automatic placement. Assigning a particular PC keeps that network and PC as the destination. Keyloom accepts work from one network at a time so separate hosts cannot reserve the same GPU concurrently. **Remove** is unavailable while that connection has unfinished work or an unresolved attempt.

## 3. Use an invitation instead (optional)

Tick **Use invitation** under **This PC**. Other PCs can then join only with an invitation file: choose **Save invitation to**, press **Create invitation**, and give that same file to each PC. On each of those PCs tick **Use invitation**, pick the file under **Import invitation** and press **Join with invitation**. Leave the box off and any PC that can reach this one on your LAN, Tailscale or ZeroTier network may join by address. The change applies to the running service within a second and does not interrupt jobs; services that already had a policy keep it until you change this box.

The invitation points to the same network and includes its certificate identity. It is reusable for seven days; keep it private while it is valid. Previously created one-use invitations keep their original rule.

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

1. Tick clips and use **Assign selected to → Apply to N**, or choose the PC directly on each clip card.
2. Choose a workflow preset and press **Key selected N clips…**.
3. Review the range, stages, outputs and **Render client** for each shot.
4. Leave **Run on** as **Each shot's assigned client**, or choose a network PC for this entire batch.
5. Press **Queue N clips**.

![Batch confirmation](screenshots/native/14-batch-confirm.png)

A shot's own In/Out, input colour and hint prompts are retained. Each whole shot runs on one receiving PC through its reviewed Hint, Key and Deliver stages; a shot is not split across PCs. Network work writes new results. Resume and Replace stay local.

## 6. Watch the Batch queue

![Queue with machine meters and stages](screenshots/native/15-batch-progress.png)

The queue groups work by PC and shows its CPU, RAM, GPU and VRAM bars alongside stage progress. Monitor and cancel work here; **Clients** is for availability and setup. The example above is from the earlier real two-PC run, reused without repeating a render.

Cancelling is cooperative and keeps frames already written. An offline PC keeps its assignment. When the host is unavailable, new network work waits; local work can continue. A job already running remains owned by its receiver and is not reassigned to another PC.

## Troubleshooting

| Symptom | Check |
|---|---|
| Service will not start | Read the UI error: address/port in use, unavailable user service manager, or permissions. Do not start a second host on the same port. |
| Cannot join / *Needs invitation* | The other PC's service must be running, reachable through its firewall, and have **Use invitation** off; otherwise import an invitation. ReScan if its addresses changed. |
| Invitation rejected | Check expiry. Older invitations may be single-use. Create a new reusable invitation from the UI. |
| Different or incomplete machine list | Check that every PC joined the same host/network, rather than creating separate networks. |
| PC visible but not receiving | Enable receiving work, keep its client running, and confirm its GPU is idle and models are ready. |
| Shared files unavailable | Open the share as the desktop user and test the mapping in Keyloom. |
| PC missing from the list | ReScan. Check that Keyloom is running there and the PCs share a network (LAN, Tailscale or ZeroTier). Do not delete a service folder: that would create a different network and orphan every joined PC. |

Pairing credentials and the service database stay on local disk, outside projects. Keep their tokens private. Renaming the app preserves the existing settings and model locations.

## Limits

- Every PC must run Keyloom 2.4 or newer to be found; earlier versions have no discovery and must be updated (or joined with an invitation file).
- The OS firewall must allow the Keyloom service (TCP 8443-8452 and UDP 44554); Keyloom never opens ports itself.
- Discovery uses LAN broadcast plus Tailscale's peer list and known addresses. Networks that block broadcast or Tailscale ACLs that block the ports hide PCs from the list; join those with an invitation.
- The background service is separate from the Keyloom window: **Stop** keeps it stopped until **Start** or the next sign-in.
- If the host's identity was revoked, Keyloom reports "identity revoked" and leaves your state untouched. There is no automatic re-issue: restore the host database or recover the identity manually; a new invitation alone does not restore it.
