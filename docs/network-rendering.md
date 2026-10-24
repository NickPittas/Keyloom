# Keyloom network rendering

Keyloom — powered by CorridorKey can send work to other Windows and Linux PCs on your LAN or Tailscale network. Network setup is in **Clients**; no terminal commands are needed.

Every installation can submit work, receive work, and host a network. One PC hosts each network's shared machine list and queue. All PCs that join that network see all its members. The host can render too. Creating a separate host creates a separate network; it does not merge two queues.

## 1. Choose the host

Use a PC that will normally stay on. Normal setup needs no terminal. In **Clients → Network setup → Host a network on this PC**:

1. Enter an **Address** that **every PC can actually reach**, and a **Port** (normally `8443`). Use the LAN IPv4 address if all PCs can route to it; if they cannot (different subnets, VPN, remote sites), use the host's Tailscale IP instead. The host's certificate is pinned to this address, so changing the address later means a new identity: choose it deliberately.
2. Tick **Trusted network: allow joining by address** if joining by IP and port is acceptable on your trusted LAN/Tailscale network. Leave it off to require an invitation file.
3. Press **Install and start**.
4. On the host PC, press **Join this network on this computer** (give it a **Name**), then tick **Accept work when GPU idle** if the host should render too.

**Host service and client are different processes.** Installing the service creates a per-user service: a `systemd --user` unit on Linux, or a per-user logon task on Windows (with an HKCU Run entry as fallback if Windows refuses the task without elevation). It starts when you sign in, needs no administrator rights, and runs separately from the Keyloom window, so quitting the GUI does not stop it. Use **Stop** to stop it or **Remove** (click twice) to remove its sign-in entry; its local database keeps the network's identity and members. The service only coordinates the machine list and queue; it does not render. Rendering is done by that PC's Keyloom client, which must be running. Turn on [background running](user-guide.md#background-running) to keep the client alive when its window is closed.

The first run may bring up an OS firewall prompt: allow it on your trusted/private network only. On Linux Keyloom does not open the firewall for you; if a firewall blocks the port, permit it for your LAN/Tailscale peers yourself.

![Clients and network setup](screenshots/native/13-pairing.png)

## 3. Use an invitation instead (optional)

A connected member opens **Invite other PCs**, chooses **Save invitation to**, and presses **Create invitation**. Give that **same file** to each PC you want to add; on each PC pick it under **Invitation file (optional)** and press **Join with invitation**.

The invitation points to the same network and includes its certificate identity. It is reusable for seven days. Every joining PC receives its own private credentials and sees the shared machine list. It is not a connection only to the PC that saved the file. Keep the invitation private while it is valid.

Previously created one-use invitations still keep their original expiry and one-use rule.

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
| Cannot join by address | The host service must be running, permit address joining, and be reachable through the firewall. |
| Invitation rejected | Check expiry. Older invitations may be single-use. Create a new reusable invitation from the UI. |
| Different or incomplete machine list | Check that every PC joined the same host/network, rather than creating separate networks. |
| PC visible but not receiving | Enable receiving work, keep its client running, and confirm its GPU is idle and models are ready. |
| Shared files unavailable | Open the share as the desktop user and test the mapping in Keyloom. |
| Certificate identity error | Check the host/address. Keep the existing network state; recreating it creates a different identity. |

Pairing credentials and the service database stay on local disk, outside projects. Keep their tokens private. Renaming the app preserves the existing settings and model locations.
