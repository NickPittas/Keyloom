# Installing Keyloom — powered by CorridorKey

Keyloom installs for your own user account. The networking rework configures the per-user host automatically across detected interfaces; an explicit **Remove** choice is preserved. In **Clients**, use **ReScan** and check the PCs you want to send work to. The host coordinates discovery and queues; rendering needs Keyloom open or running in background mode. See [Network rendering](network-rendering.md). This automatic setup is under development and is not part of the published 2.3.0 installers below.

## What you need

| | |
|---|---|
| Windows | Windows 10 or 11, 64-bit. A clean-machine install has not been certified yet. |
| Linux | 64-bit, glibc 2.30 or newer. Linux Mint 22.x is the expected target; it has not been tested on a clean machine. |
| GPU | NVIDIA GPU with the vendor driver installed in the operating system |
| Linux libraries | OpenGL/EGL, X11 or Wayland client libraries, xkbcommon, fontconfig, freetype, glib, dbus, libstdc++. The installer checks these first and prints an `apt` hint on Debian/Ubuntu/Mint if something is missing. |
| Internet | Needed to obtain the installer from its owner (the source repository is private; no GitHub sign-in is needed to run it), for the online installer's wheel downloads (PyPI / PyTorch / NVIDIA), and for the models |
| Disk | About 9 GiB for the app (allow roughly 18 GiB during an update), plus room for the **full** model set (large) and your project media. Projects can live on any drive. |

You do **not** need to install Python, Rust, FFmpeg, the CUDA toolkit or the Visual C++ runtime. The app includes Python, FFmpeg, CUDA user-space libraries and the required Visual C++ runtime; no compiler or toolkit is needed. A missing NVIDIA driver only produces a warning during install, because models can still be downloaded, but you cannot render without it.

The installers are for private use. The licence is non-commercial (CC-BY-NC-SA-4.0) and public redistribution is not cleared.

## Choose an installer

Use the **online installer** for a smaller download and to reuse unchanged dependencies on existing PCs. Use the **offline installer** when the target PC cannot download dependencies. Both install the same Keyloom build. Quit Keyloom with **File → Quit** before updating; install over the existing folder without uninstalling.

- [Windows 2.3.0 downloads](https://github.com/NickPittas/Keyloom/releases/tag/v2.3.0-windows): `Keyloom-online-2.3.0-662dcec4eb-windows-x86_64.exe` and its checksum file. Keep any online `.bin` slices listed in that release beside the exe, then double-click it.
- [Linux 2.3.0 downloads](https://github.com/NickPittas/Keyloom/releases/tag/v2.3.0-linux): `Keyloom-online-2.3.0-1b3b546391-linux-x86_64.run` and its checksum file. In the download folder:

  ```sh
  sha256sum -c Keyloom-online-2.3.0-1b3b546391-linux-x86_64.run.sha256
  chmod +x Keyloom-online-2.3.0-1b3b546391-linux-x86_64.run
  ./Keyloom-online-2.3.0-1b3b546391-linux-x86_64.run
  ```

The online installer leaves models to the app. Its first installation may download several GiB of public dependencies; an update downloads a wheel only when required files cannot be reused.

## Windows offline installer

GitHub limits file size, so the installer comes in slices: one `.exe` plus several `.bin` files (each up to 1900 MiB), and a `.sha256` file.

The Windows payload is `2.3.0-662dcec4eb`; the Linux payload is `2.3.0-1b3b546391`. These suffixes identify the bundled contents.

Download the [Windows release](https://github.com/NickPittas/Keyloom/releases/tag/v2.3.0-windows).

1. Download **all** of these into the **same folder**:
   - `Keyloom-2.3.0-662dcec4eb-windows-x86_64-setup.exe`
   - every `Keyloom-2.3.0-662dcec4eb-windows-x86_64-setup-1.bin`, `-2.bin`, … (all of them)
   - `Keyloom-2.3.0-662dcec4eb-windows-x86_64-setup.exe.sha256`
2. Check the files. In PowerShell, inside that folder:

   ```powershell
   Get-Content .\Keyloom-2.3.0-662dcec4eb-windows-x86_64-setup.exe.sha256 | ForEach-Object {
     $hash, $name = $_ -split '\s+', 2
     $got = (Get-FileHash -Algorithm SHA256 -LiteralPath $name).Hash
     if ($got -ine $hash) { throw "SHA256 mismatch: $name" }
     "OK  $name"
   }
   ```

   Every line must say `OK`. Stop if PowerShell reports an error; re-download the missing or mismatched file.
3. Double-click the `.exe`. Untick **Download and verify ALL models now**; download models from the app after installation.
4. Setup installs to `%LOCALAPPDATA%\Programs\Keyloom`, checks the bundled runtime and adds a Start Menu shortcut. Open Keyloom and download the models you need from **Models & runtime…**.

If a problem appears, the **previous** version is restored, or on a first install everything is removed.

Command-line helpers (PowerShell):

```powershell
& "$env:LOCALAPPDATA\Programs\Keyloom\bin\keyloom-setup.cmd" status
& "$env:LOCALAPPDATA\Programs\Keyloom\bin\keyloom-setup.cmd" models
```

## Linux offline installer

Download the [Linux release](https://github.com/NickPittas/Keyloom/releases/tag/v2.3.0-linux).

The release is a single self-extracting `.run` file. If it is hosted on GitHub it is split into parts of up to 1900 MiB. Where a complete `.run` is already available (for example on a local file share), skip the reassembly steps and go straight to *Run the installer*.

### Download and reassemble the parts

Put these files in one folder:

- `Keyloom-2.3.0-1b3b546391-linux-x86_64.run.part001`, `.part002`, `.part003` (every part)
- `Keyloom-2.3.0-1b3b546391-linux-x86_64.run.parts.sha256` (checksums of the parts)
- `Keyloom-2.3.0-1b3b546391-linux-x86_64.run.sha256` (checksum of the whole file)

```sh
cd ~/Downloads/keyloom
sha256sum -c Keyloom-2.3.0-1b3b546391-linux-x86_64.run.parts.sha256
cat Keyloom-2.3.0-1b3b546391-linux-x86_64.run.part[0-9][0-9][0-9] > Keyloom-2.3.0-1b3b546391-linux-x86_64.run
sha256sum -c Keyloom-2.3.0-1b3b546391-linux-x86_64.run.sha256
chmod +x Keyloom-2.3.0-1b3b546391-linux-x86_64.run
```

Use the numeric pattern `part[0-9][0-9][0-9]`. A plain `part*` would also match the `.parts.sha256` file and corrupt the result. Each check must print `OK`.

### Run the installer

```sh
./Keyloom-2.4.0-<id>-linux-x86_64.run
```

It installs to `~/.local/opt/Keyloom` by default, adds a desktop entry, installs the `keyloom`, `keyloom-setup` and `keyloom-coordinator` commands under `~/.local/opt/Keyloom/bin`, and leaves model downloads to the app when run with `--models skip`.

Options:

| Option | Meaning |
|---|---|
| `--prefix DIR` | Install somewhere else. The folder must be new/empty or an existing Keyloom install. |
| `--models full` | Explicitly download and verify ALL models now. The default is `skip`: installing, updating or repairing never downloads models. Models come from the app's Models page or `keyloom-setup models`. |
| `--repair` | Restore damaged program files from this installer (models are re-verified only with `--models full`). |
| `--firewall yes\|skip` | `yes` (default): if UFW is active, use `sudo` (a password prompt follows, no question first) for the network firewall rules ([below](#linux-firewall-ufw)). `skip`: do not touch the firewall; the step is left pending. `ask` is accepted and means `yes`. |
| `--no-firewall` | Same as `--firewall skip`. |

Do not run two installers on the same folder at once.

Later commands:

```sh
~/.local/opt/Keyloom/bin/keyloom-setup status --verify
~/.local/opt/Keyloom/bin/keyloom-setup models
~/.local/opt/Keyloom/bin/keyloom-setup repair
~/.local/opt/Keyloom/bin/keyloom-setup rollback
~/.local/opt/Keyloom/bin/keyloom-setup uninstall
```

## Online installer

Recommended when internet access is available. Alongside the offline installers above, an **online installer** exists for Linux and Windows. It **contains Keyloom itself** (the application, its own Python runtime and FFmpeg) and leaves out only the very large public dependencies, mainly PyTorch and the NVIDIA CUDA user-space libraries. Those are downloaded during installation, each as a hash-pinned wheel from the public PyPI / PyTorch / NVIDIA package servers. The result is byte-for-byte the same version (`version_id`) and the same installed files as the full offline installer.

**Getting it vs. installing it.** The source repository is private, so you receive the online installer from the owner **once, manually** (the same way as the offline installer). Running it needs **no GitHub access, sign-in, token, client id or release lookup**: the only connections are anonymous HTTPS downloads of the pinned public wheels, over HTTPS only. If everything is already installed and unchanged, it works completely offline.

- **Every file is verified.** Each file of the finished install is checked against the install manifest (SHA-256 and size) before anything is activated; every downloaded wheel is checked against its pinned size and SHA-256, and only the entries the manifest names are extracted. The normal installer step then runs (OS-library check, validation, shortcuts, receipt, rollback).
- **First install:** downloads only the wheels (several GiB; the size is printed before it starts), assembles a complete copy under the install folder and installs it.
- **Update:** finds the existing install from its `receipt.json`. Files identical to the current or previous version are copied from the installed copy, the rest comes from the installer, and a wheel is downloaded only if a file it provides is still missing. If this exact build is already installed and every file verifies, it says so and exits without any network access.
- **Fast update (unchanged runtime):** when the new build's `runtime_id` (a SHA-256 over its `runtime/` manifest entries) equals that of the runtime in an install Keyloom itself activated (this folder, or the one being taken over), the update links that runtime directory by reference. No runtime file is read, hashed, copied or downloaded; only the changed application files are staged and checked. "Already installed" is one local check (application files hashed once, the runtime is trusted from the install record, not re-read) and needs no network. If the same runtime exists but cannot be reused safely, a normal update stops and asks for **Repair** instead of silently spending minutes copying it. The trust is a same-user install record, not protection against the account owner.
- **Runtime changed or first install:** only the new runtime is extracted and, if files are missing, downloaded. While a wheel downloads, verifies or extracts, a line is printed every second with the phase, bytes done/total, speed and elapsed time; `WAITING` means no new bytes arrived in that second (liveness, not progress). A silent server fails after 20 s (`stalled: no data for 20s`, or `connect timed out`), the download resumes on retry, and a failing local step is never repeated unchanged.
- **Logs:** every install appends to `<prefix>/install.log` (header per step; all installer output and failure reasons). The app writes service errors to `<settings>/network/service.log` and shows them as selectable text in Clients. While a declared wait (UAC, sudo, download) is active the console prints its reason once and then every 20 s, plus `wait finished`.
- **No install-time limit:** there is no time budget for an install. A step is stopped, rolled back and reported (exit `124`, nothing activated) only when it shows no output at all for 5 minutes. Only an actual wheel download and your sudo password (Linux) or UAC prompt (Windows) for the firewall step are declared waits through `KEYLOOM_WAIT_FILE`, which the wrapper must export. Independent work runs on one bounded pool of 8 workers (never more than 16): wheel downloads, file copies and the read-only discovery probes (existing-install candidates and installed manifests). Each wheel is handled by exactly one worker, symlinks are created only after all file workers finish, and results never depend on completion order. The console shows, every second, bytes, MiB/s, active and finished workers, phase and elapsed time. Fewer wheels are fetched at once when free disk space is tight, and any error cancels the remaining work before anything is activated. Each wheel transfer runs in its own short-lived helper process, so a stop (an error elsewhere or Ctrl-C) kills a stuck connection immediately instead of waiting for a network timeout; partial downloads are kept and resume on the next run.
- **Repair:** `--repair` rebuilds the current version the same way and fully re-verifies every file, including the runtime.
- **Resume:** an interrupted download resumes from `.staging.dl` in the install folder (kept up to 6 hours, removed after a successful install).
- **Keep:** projects, settings, credentials, network identities and certificates, jobs and models live outside the install folder and are never touched. Models and the NVIDIA display driver are **not** installed by the installer; the app offers models in **Settings → Models & runtime…**. A previously verified model state is kept on update. No compiler is needed on the PC.
- **Quit Keyloom first.** The installer holds the same per-user instance lock as the app for the whole run. If Keyloom (even hidden) is running, it stops with exit code 4; choose **File → Quit** and run it again. It never launches Keyloom.
- **Install folder:** the default (see above), or `--prefix`. On Windows the wrapper detects an existing install, including one in a custom folder chosen in the Inno Setup dialog, and updates that one.

Options of the shared client (`packaging/online.py`), normally supplied by the wrapper:

| Option | Meaning |
|---|---|
| `--payload-dir PATH` | The bundled slim stage (with the original `install-manifest.json`) |
| `--sources PATH` | `wheel-sources.json`: version, and the public wheel URLs with size, SHA-256 and the files each provides |
| `--prefix DIR` | Install somewhere else (default as above); an existing install there is detected by its receipt |
| `--repair` | Rebuild and verify the current version |
| `--firewall yes\|skip`, `--no-firewall` | Same as the offline installer: see [Linux firewall](#linux-firewall-ufw). |

Exit codes: `0` installed or already up to date, `2` failed (the previous install is untouched), `4` Keyloom is running, `130` interrupted. Allow free space for the new version plus the wheels being downloaded, and room for the activation step: about three times the size of the app on first install (see *What you need*).

## Windows firewall

Windows allows incoming connections per **program path**. Each Keyloom version runs its network service from its own
`pythonw.exe`, so a new version (or the move to the `Keyloom` folder) needs its own Allow rule or Windows shows a
"Python" prompt and other PCs cannot reach this one until it is answered.

- The installer first **reads** the firewall (no administrator needed). If an enabled inbound Allow rule already exists for the
  new `pythonw.exe` (yours or ours), nothing is added.
- Otherwise, if the **Configure Windows Firewall rules** task is checked (the default; both installers), Windows shows **one administrator (UAC) prompt** (the elevated PowerShell runs hidden) and exactly two rules are added:
  TCP 8443-8452 and UDP 44554 for that `pythonw.exe`, Private and Public profiles, only from the local subnet, Tailscale
  (`100.64.0.0/10`, `fd7a:115c:a1e0::/48`) and ZeroTier (`192.168.195.0/24`). The rules are named `Keyloom network ... (<version>)`,
  grouped `Keyloom`, and recorded in the install receipt. The firewall is never disabled and no other rule is touched.
- An unchecked task (`/MERGETASKS="!firewall"` for unattended installs, which otherwise show the UAC prompt) or a cancelled UAC never adds rules: the installer prints the exact
  PowerShell command (run it as Administrator) and the status stays `pending`.
- Rules from older versions stay (they are not ours to delete); uninstall offers to remove only the recorded rules.

## Linux firewall (UFW)

Both Linux installers (offline and online) run the same step after the app is installed. It matters only if **UFW is installed and enabled**; otherwise nothing is done and no `sudo` is used. The installer never installs, enables or disables a firewall, never adds a blanket allow rule and never touches `sudoers`.

- **What it opens:** TCP 8443-8452 (the network service picks a port in that range) and UDP 44554 (discovery), only *from* the networks this PC is on right now: its private LAN subnets, ZeroTier subnets, and Tailscale (`100.64.0.0/10` and `fd7a:115c:a1e0::/48`). Loopback, public, link-local, multicast, and Docker/VM bridge adapters (`docker*`, `br-*`, `veth*`, `virbr*`, `vboxnet*`, `vEthernet*`) are never included. Every network is validated before anything is elevated.
- **How it runs:** no question is asked; firewall rules are configured by default. It lists the networks, then `sudo` asks for your password at the terminal (never in chat, arguments, environment or logs), reads `ufw status`, and adds only the rules that are missing. Only `ufw` runs as root. The read also needs `sudo`, because UFW only shows its rules to root.
- **Your own rules count.** A rule you added (any comment) that already allows these ports from a network is enough, and is never changed or removed. Rules the installer adds are commented `Keyloom installer: service` / `Keyloom installer: discovery` and listed in the receipt (`firewall.owned`). Reinstalling or updating asks nothing again while the networks are the same.
- **No terminal, `--firewall skip` / `--no-firewall` (skips the step), wrong password, sudo timeout:** the app is still installed. The installer prints `NETWORK FIREWALL SETUP PENDING` with the exact `sudo ufw ...` commands and saves them in `receipt.json` (`firewall.status: "pending"`); `install-result.json` carries the status. The exit code does not change (0/3 still describe the models). Other PCs may not reach this one until you run them. Silent or scripted installs have no terminal, so they never prompt or hang; pass `--firewall skip` to make that explicit.
- **Not covered:** firewalld (reported `unsupported`) and raw iptables/nftables rules are not managed or inspected. Network readiness is never claimed in those cases; the installer says so.
- **Uninstall** (`keyloom-setup uninstall`) offers to remove only the rules recorded as added by the installer, after asking and with `sudo`. If you decline or there is no terminal, it prints the delete commands and leaves them. `repair` (installer `--repair` or `keyloom-setup repair`) runs the same step again; rollback never changes the firewall. Rules for networks you leave stay until uninstall.

Fallback by hand (replace the network with yours, e.g. `192.168.100.0/24`):

```sh
sudo ufw allow from 192.168.100.0/24 to any port 8443:8452 proto tcp comment 'Keyloom service (LAN)'
sudo ufw allow from 192.168.100.0/24 to any port 44554 proto udp comment 'Keyloom discovery (LAN)'
```

The Windows installers are unchanged by this step.

## Models

Keyloom installs the **full** model profile: every model the app can use. They are *not* inside the installer and the download can be very large and slow. It is resumable and every file is checked afterwards. The app also offers **Settings → Models & runtime…** (see the [User guide](user-guide.md#models)) to install, verify and repair them.

## Install result and exit codes

These codes apply to the Linux installer and setup command. Windows Setup can complete successfully while models are skipped or incomplete; read its final notice and resume with the model command above.

| Exit code | Meaning |
|---|---|
| 0 | Installed and all models are ready |
| 3 | The app is installed, but models are missing, skipped or incomplete. Run `keyloom-setup models` to finish. Some workflows stay unavailable until then. |
| 2 | Refused or failed; nothing was left half-installed |
| 130 | Interrupted (Ctrl-C). The previous state is restored. |

If the download is interrupted, run the command again. It resumes.

## Updating, repairing, uninstalling

- **Quit Keyloom first.** Closing the window only *hides* it when background mode is on (see [Settings → Interface](user-guide.md#background-running)). Choose **File → Quit**, and check your task list for a hidden older copy (there is no tray icon) before you update.
- **Update:** run the new installer. Two versions are kept (current and previous) so a failed update rolls back.
- **Repair:** Linux `…run --repair`, or `keyloom-setup repair`. On Windows, uninstall and reinstall.
- **Uninstall:** Windows *Settings → Apps*, or Linux `keyloom-setup uninstall`.

Projects, outputs, models, the download cache and your settings live **outside** the install folder and are kept through repair, update and uninstall.

### Old CorridorKey installs

Keyloom installs only into its own folder: `%LOCALAPPDATA%\Programs\Keyloom` on Windows, `~/.local/opt/Keyloom` on Linux, or a
folder you choose with `/PREFIX` or `--prefix`. It never adopts, reads, writes or deletes a CorridorKey folder, so an old
CorridorKey install is left exactly as it was. Remove it by hand when you no longer need it.

Settings live only in `%APPDATA%\keyloom` on Windows and `~/.config/keyloom` on Linux. Keyloom does not read the old
`corridorkey-native` settings folder and does not migrate it, so settings start fresh.

### Reinstalling and updating never resets anything

- Running the installer for the **exact build that is already installed and verified** changes nothing: no new
  version, no shortcut, service, setting or model touched. It prints "already installed and verified". The Linux
  offline installer checks this before unpacking; the Windows installer checks it before writing files; the online
  installer checks it before downloading. Use `repair` to restore damaged files.
- Your settings file (including keys this version does not know), custom model and data folders, network name,
  connections, "hosting removed" choice, certificates and jobs are never written by an installer.
- Updating reuses the active version's runtime files when they are byte-identical (Linux offline: hard links, counted
  in `install-result.json` as `runtime_reuse`; online: the existing files and wheel cache). Nothing is `pip`-installed
  on your machine.
- Models are never downloaded by install/update/repair unless you ask (`--models full`, the Windows "Download ALL
  models" box which is off by default, the app's Models page, or `keyloom-setup models`). If the model catalog changed,
  your existing models are kept and marked "unverified" until you re-check them.

### Upgrading from an install made before the Keyloom rename

Projects and outputs stay where they are. Settings and network identity start fresh in `%APPDATA%\keyloom` (or
`~/.config/keyloom` on Linux); the old settings folder is not read. Engine models are looked for in
`%LOCALAPPDATA%\Keyloom\EngineV2` (or `~/.local/share/Keyloom/EngineV2` on Linux), and the old
`%LOCALAPPDATA%\EZ-CorridorKey2\CorridorKeyV2` folder is not read. Copy the models you want across by hand, or download them
from the Models page. The old CorridorKey commands, shortcuts, sign-in entry and network service are not changed, so remove them by hand.

## Starting the app

- Windows: Start Menu → Keyloom.
- Linux: your application menu → Keyloom, or `~/.local/opt/Keyloom/bin/keyloom`.

Optional: have Keyloom start hidden when you sign in (needed for always-on network rendering). See [Background running](user-guide.md#background-running).
