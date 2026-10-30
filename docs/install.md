# Installing Keyloom — powered by CorridorKey

Keyloom installs for your own user account. The networking rework configures the per-user host automatically across detected interfaces; an explicit **Remove** choice is preserved. In **Clients**, use **ReScan** and check the PCs you want to send work to. The host coordinates discovery and queues; rendering needs Keyloom open or running in background mode. See [Network rendering](network-rendering.md).

## What you need

| | |
|---|---|
| Windows | Windows 10 or 11, 64-bit. A clean-machine install has not been certified yet. |
| Linux | 64-bit, glibc 2.30 or newer. Linux Mint 22.x is the expected target; it has not been tested on a clean machine. |
| GPU | NVIDIA GPU with the vendor driver installed in the operating system |
| Linux libraries | OpenGL/EGL, X11 or Wayland client libraries, xkbcommon, fontconfig, freetype, glib, dbus, libstdc++. The installer checks these first and prints an `apt` hint on Debian/Ubuntu/Mint if something is missing. |
| Internet | Needed to download the installer from the public GitHub release, for missing runtime dependencies (PyPI / PyTorch / NVIDIA), and for the models. No GitHub sign-in is needed to run the installer. |
| Disk | About 9 GiB for the app (allow roughly 18 GiB during an update), plus room for your selected models (the **full** profile is very large) and your project media. Projects can live on any drive. |

You do **not** need to install Python, Rust, FFmpeg, the CUDA toolkit or the Visual C++ runtime. The online installer supplies Python, FFmpeg, CUDA user-space libraries and the required Visual C++ runtime, downloading missing public dependencies when necessary; no compiler or toolkit is needed. A missing NVIDIA driver only produces a warning during install, because models can still be downloaded, but you cannot render without it.

The source repository and linked installer assets are public. Availability is not additional redistribution permission or upstream approval: see [Licence and sharing](../README.md#licence-and-sharing). Public redistribution of the bundled installers has not been cleared.

## Download the latest online installer

Only **online installers** are distributed, and only the latest release is retained on GitHub. There is no offline-installer download. Quit Keyloom with **File → Quit** before updating; install over the existing folder without uninstalling.

### Windows 2.5.0

Download **all three files** from the [Windows release](https://github.com/NickPittas/Keyloom/releases/tag/v2.5.0-windows) into the **same folder**:

- `Keyloom-online-2.5.0-5804bedd5c-windows-x86_64.exe`
- `Keyloom-online-2.5.0-5804bedd5c-windows-x86_64-1.bin`
- `Keyloom-online-2.5.0-5804bedd5c-windows-x86_64.exe.sha256`

Check both installer files in PowerShell:

```powershell
Get-Content .\Keyloom-online-2.5.0-5804bedd5c-windows-x86_64.exe.sha256 | ForEach-Object {
  $hash, $name = $_ -split '\s+', 2
  $got = (Get-FileHash -Algorithm SHA256 -LiteralPath $name).Hash
  if ($got -ine $hash) { throw "SHA256 mismatch: $name" }
  "OK  $name"
}
```

Every line must say `OK`. Then double-click the `.exe`; keep the `.bin` beside it. The default install folder is `%LOCALAPPDATA%\Programs\Keyloom`. Download models from the app after installation.

### Linux 2.5.0

Download the `.run` and its `.sha256` file from the [Linux release](https://github.com/NickPittas/Keyloom/releases/tag/v2.5.0-linux). In the download folder:

```sh
sha256sum -c Keyloom-online-2.5.0-7560ea9aff-linux-x86_64.run.sha256
chmod +x Keyloom-online-2.5.0-7560ea9aff-linux-x86_64.run
./Keyloom-online-2.5.0-7560ea9aff-linux-x86_64.run
```

The default install folder is `~/.local/opt/Keyloom`. Use `--prefix DIR` to choose another folder or `--repair` to repair an existing install. See [Linux firewall](#linux-firewall-ufw) for `--firewall yes|skip` and `--no-firewall`. Do not run two installers on the same folder at once.

## How the online installer works

It **contains Keyloom itself** (the application, its own Python runtime and FFmpeg) and leaves out only the very large public dependencies, mainly PyTorch and the NVIDIA CUDA user-space libraries. Missing dependencies are downloaded during installation as hash-pinned wheels from the public PyPI / PyTorch / NVIDIA package servers. The first installation may download several GiB; unchanged dependencies can be reused during updates. Models are downloaded separately from the app.

**Getting it vs. installing it.** Download the installer from the public release links above. Running it needs **no GitHub access, sign-in, token, client id or release lookup**: its dependency downloads use anonymous HTTPS. If everything is already installed and unchanged, it can verify that installation without network access; this does not make it an offline installer for a fresh PC.

- **Payload validation.** Downloaded wheels are checked against pinned sizes and SHA-256 values, and extracted files must match the install manifest. New application files are checked before activation. An unchanged runtime may be trusted from an existing installation's recorded identity rather than rehashed; **Repair** fully verifies it. Activation handles OS-library checks, shortcuts, the receipt and rollback.
- **First install:** downloads only the wheels (several GiB; the size is printed before it starts), assembles a complete copy under the install folder and installs it.
- **Update:** finds the existing install from its `receipt.json`. Files identical to the current or previous version are copied from the installed copy, the rest comes from the installer, and a wheel is downloaded only if a file it provides is still missing. If this exact build is already installed and every file verifies, it says so and exits without any network access.
- **Fast update (unchanged runtime):** when the new build's `runtime_id` (a SHA-256 over its `runtime/` manifest entries) equals that of the runtime in an install Keyloom itself activated (this folder, or the one being taken over), the update links that runtime directory by reference. No runtime file is read, hashed, copied or downloaded; only the changed application files are staged and checked. "Already installed" is one local check (application files hashed once, the runtime is trusted from the install record, not re-read) and needs no network. If an existing runtime cannot be reused safely, the installer assembles a fresh runtime from its payload and required downloads; an invalid manifest stops installation. The trust is a same-user install record, not protection against the account owner.
- **Runtime changed or first install:** only the new runtime is extracted and, if files are missing, downloaded. While a wheel downloads, verifies or extracts, a line is printed every second with the phase, bytes done/total, speed and elapsed time; `WAITING` means no new bytes arrived in that second (liveness, not progress). A silent server fails after 20 s (`stalled: no data for 20s`, or `connect timed out`), the download resumes on retry, and a failing local step is never repeated unchanged.
- **Logs:** every install appends to `<prefix>/install.log` (header per step; all installer output and failure reasons). The app writes service errors to `<settings>/network/service.log` and shows them as selectable text in Clients. While a declared wait (UAC, sudo, download) is active the console prints its reason once and then every 20 s, plus `wait finished`.
- **No install-time limit:** there is no time budget for an install. A step is stopped, rolled back and reported (exit `124`, nothing activated) only when it shows no output at all for 5 minutes. Only an actual wheel download and your sudo password (Linux) or UAC prompt (Windows) for the firewall step are declared waits through `KEYLOOM_WAIT_FILE`, which the wrapper must export. Independent work runs on one bounded pool of 8 workers (never more than 16): wheel downloads and file copies. Existing-install discovery checks candidates in a fixed order. Each wheel is handled by exactly one worker, symlinks are created only after all file workers finish, and results never depend on completion order. The console shows, every second, bytes, MiB/s, active and finished workers, phase and elapsed time. Fewer wheels are fetched at once when free disk space is tight, and any error cancels the remaining work before anything is activated. Each wheel transfer runs in its own short-lived helper process, so a stop (an error elsewhere or Ctrl-C) kills a stuck connection immediately instead of waiting for a network timeout; partial downloads are kept and resume on the next run.
- **Repair:** `--repair` rebuilds the current version the same way and fully re-verifies every file, including the runtime.
- **Resume:** an interrupted download resumes from `.staging.dl` in the install folder (kept up to 6 hours, removed after a successful install).
- **Keep:** projects, settings, credentials, network identities and certificates, jobs and models live outside the install folder and are never touched. Models and the NVIDIA display driver are **not** installed by the installer; the app offers models in **Settings → Models & runtime…**. A previously verified model state is kept on update. No compiler is needed on the PC.
- **Quit Keyloom first.** The installer holds the same per-user instance lock as the app for the whole run. If Keyloom (even hidden) is running, it stops with exit code 4; choose **File → Quit** and run it again. It never launches Keyloom.
- **Install folder:** Linux uses the default above or `--prefix DIR`. Windows uses `/PREFIX=DIR`, then a valid folder recorded by an earlier installer, then `%LOCALAPPDATA%\Programs\Keyloom`. It has no folder-selection page.

Options of the shared client (`packaging/online.py`), normally supplied by the wrapper:

| Option | Meaning |
|---|---|
| `--payload-dir PATH` | The bundled slim stage (with the original `install-manifest.json`) |
| `--sources PATH` | `wheel-sources.json`: version, and the public wheel URLs with size, SHA-256 and the files each provides |
| `--prefix DIR` | Install somewhere else (default as above); an existing install there is detected by its receipt |
| `--repair` | Rebuild and verify the current version |
| `--firewall yes\|skip`, `--no-firewall` | Configure or skip Linux firewall rules: see [Linux firewall](#linux-firewall-ufw). |

The shared client's exit codes are `0` installed or already up to date, `2` failed, `4` Keyloom is running, and `130` interrupted. The wrapper can additionally report a stall (`124`) or a setup-specific failure. Allow free space for the new version plus the wheels being downloaded, and room for the activation step: about three times the size of the app on first install (see *What you need*).

## Windows firewall

Windows allows incoming connections per **program path**. Each Keyloom version runs its network service from its own
`pythonw.exe`, so a new version (or the move to the `Keyloom` folder) needs its own Allow rule or Windows shows a
"Python" prompt and other PCs cannot reach this one until it is answered.

- The installer first **reads** the firewall (no administrator needed). If an enabled inbound Allow rule already exists for the
  new `pythonw.exe` (yours or ours), nothing is added.
- Otherwise, if the **Configure Windows Firewall rules for the network service** task is checked (the default), Windows shows **one administrator (UAC) prompt** (the elevated PowerShell runs hidden) and exactly two rules are added:
  TCP 8443-8452 and UDP 44554 for that `pythonw.exe`, Private and Public profiles, only from the local subnet, Tailscale
  (`100.64.0.0/10`, `fd7a:115c:a1e0::/48`) and ZeroTier (`192.168.195.0/24`). The rules are named `Keyloom network ... (<version>)`,
  grouped `Keyloom`, and recorded in the install receipt. The firewall is never disabled and no other rule is touched.
- An unchecked task (`/MERGETASKS="!firewall"` for unattended installs, which otherwise show the UAC prompt) or a cancelled UAC never adds rules: the installer prints the exact
  PowerShell command (run it as Administrator) and the status stays `pending`.
- Rules from older versions stay (they are not ours to delete); uninstall offers to remove only the recorded rules.

## Linux firewall (UFW)

The Linux online installer runs this step after the app is installed. It matters only if **UFW is installed and enabled**; otherwise nothing is done and no `sudo` is used. The installer never installs, enables or disables a firewall, never adds a blanket allow rule and never touches `sudoers`.

- **What it opens:** TCP 8443-8452 (the network service picks a port in that range) and UDP 44554 (discovery), only *from* the networks this PC is on right now: its private LAN subnets, ZeroTier subnets, and Tailscale (`100.64.0.0/10` and `fd7a:115c:a1e0::/48`). Loopback, public, link-local, multicast, and Docker/VM bridge adapters (`docker*`, `br-*`, `veth*`, `virbr*`, `vboxnet*`, `vEthernet*`) are never included. Every network is validated before anything is elevated.
- **How it runs:** no question is asked; firewall rules are configured by default. It lists the networks, then `sudo` asks for your password at the terminal (never in chat, arguments, environment or logs), reads `ufw status`, and adds only the rules that are missing. Only `ufw` runs as root. The read also needs `sudo`, because UFW only shows its rules to root.
- **Your own rules count.** A rule you added (any comment) that already allows these ports from a network is enough, and is never changed or removed. Rules the installer adds are commented `Keyloom installer: service` / `Keyloom installer: discovery` and listed in the receipt (`firewall.owned`). Reinstalling or updating asks nothing again while the networks are the same.
- **No terminal, `--firewall skip` / `--no-firewall` (skips the step), wrong password, sudo timeout:** the app is still installed. The installer prints `NETWORK FIREWALL SETUP PENDING` with the exact `sudo ufw ...` commands and saves them in `receipt.json` (`firewall.status: "pending"`); `install-result.json` carries the status. The online installer's success code does not imply that network firewall setup is complete. Other PCs may not reach this one until you run them. Silent or scripted installs have no terminal, so they never prompt or hang; pass `--firewall skip` to make that explicit.
- **Not covered:** firewalld (reported `unsupported`) and raw iptables/nftables rules are not managed or inspected. Network readiness is never claimed in those cases; the installer says so.
- **Uninstall** (`keyloom-setup uninstall`) offers to remove only the rules recorded as added by the installer, after asking and with `sudo`. If you decline or there is no terminal, it prints the delete commands and leaves them. `repair` (installer `--repair` or `keyloom-setup repair`) runs the same step again; rollback never changes the firewall. Rules for networks you leave stay until uninstall.

Fallback by hand (replace the network with yours, e.g. `192.168.100.0/24`):

```sh
sudo ufw allow from 192.168.100.0/24 to any port 8443:8452 proto tcp comment 'Keyloom service (LAN)'
sudo ufw allow from 192.168.100.0/24 to any port 44554 proto udp comment 'Keyloom discovery (LAN)'
```

The Windows installers are unchanged by this step.

## Keep each rendering client running

After installation, launch Keyloom on **every PC that will render shots**, not just the submitting PC. Leave the app open, or enable background mode and keep it running after closing its window. **File → Quit** or terminating the app stops that PC from receiving work.

The daemon/service can remain running when the app has quit, so other PCs may still discover it. **Daemon-only visibility does not mean the PC can accept or render shots.** Check its receiver status and Clients controls before submitting work; see [network rendering](network-rendering.md).

## Models

**No model weights are bundled with the installers.** After installation, Keyloom's model manager downloads selected models from their respective creators' published repositories or storage endpoints, or reuses verified existing files. **Each model is subject to its creator's licence and terms**, which may differ between code and weights and may restrict commercial use, access or redistribution. Keyloom does not grant additional rights. See the [model-creator attributions and source links](../README.md#model-creators-and-download-terms) before downloading or using models.

Models are **not** downloaded by the online installer. Open **Settings → Models & runtime…** (see the [User guide](user-guide.md#models)) to install the required models or the **full profile** of every model the app can use. The full download is very large; downloads are resumable and verified. You can also use `keyloom-setup models` from the installed `bin` folder.

## Install result and exit codes

The shared online client returns `0` when installed or already up to date, `2` on failure, `4` if Keyloom is still running, and `130` if interrupted. The wrapper can also report `124` for a stalled step or a setup-specific error. Models are downloaded separately from the app.

The setup/model-management commands can additionally report model readiness:

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
- **Repair:** Linux `…run --repair`; Windows run the online `.exe` with `/REPAIR=1`. Installed setup commands also offer repair.
- **Uninstall:** use `keyloom-setup uninstall`. The Windows online installer does not create a Windows *Settings → Apps* entry.

Installed helpers:

```powershell
# Windows PowerShell; adjust the folder for a custom installation.
& "$env:LOCALAPPDATA\Programs\Keyloom\bin\keyloom-setup.cmd" status
& "$env:LOCALAPPDATA\Programs\Keyloom\bin\keyloom-setup.cmd" models
& "$env:LOCALAPPDATA\Programs\Keyloom\bin\keyloom-setup.cmd" uninstall
```

```sh
# Linux; adjust the folder for a custom installation.
~/.local/opt/Keyloom/bin/keyloom-setup status
~/.local/opt/Keyloom/bin/keyloom-setup models
~/.local/opt/Keyloom/bin/keyloom-setup uninstall
```

Projects, outputs, models, the download cache and your settings live **outside** the install folder and are kept through repair, update and uninstall.

### Old CorridorKey installs

Keyloom installs only into its own folder: `%LOCALAPPDATA%\Programs\Keyloom` on Windows, `~/.local/opt/Keyloom` on Linux, or a
folder you choose with `/PREFIX` or `--prefix`. It never adopts, reads, writes or deletes a CorridorKey folder, so an old
CorridorKey install is left exactly as it was. Remove it by hand when you no longer need it.

Settings live only in `%APPDATA%\keyloom` on Windows and `~/.config/keyloom` on Linux. Keyloom does not read the old
`corridorkey-native` settings folder and does not migrate it, so settings start fresh.

### Reinstalling and updating never resets anything

- Running the installer for the **exact build that is already installed and verified** changes nothing: no new
  version, no shortcut, service, setting or model touched. It checks the installation before downloading anything.
  Use `repair` to restore damaged files.
- Your settings file (including keys this version does not know), custom model and data folders, network name,
  connections, "hosting removed" choice, certificates and jobs are never written by an installer.
- Updating reuses the active version's compatible runtime, existing files and wheel cache. Nothing is `pip`-installed
  on your machine.
- The online installer never downloads models. Use the app's Models page or `keyloom-setup models`. If the model catalog changed,
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
