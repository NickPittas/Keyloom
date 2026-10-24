# Installing Keyloom — powered by CorridorKey

Keyloom installs for your own user account. Installing the app does not enable network hosting. Use **Clients** to install and start the optional per-user network service, or join an existing network. See [Network rendering](network-rendering.md).

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
4. Setup installs to `%LOCALAPPDATA%\Programs\CorridorKey`, checks the bundled runtime and adds a Start Menu shortcut. Open Keyloom and download the models you need from **Models & runtime…**.

If a problem appears, the **previous** version is restored, or on a first install everything is removed.

Command-line helpers (PowerShell):

```powershell
& "$env:LOCALAPPDATA\Programs\CorridorKey\bin\corridorkey-setup.cmd" status
& "$env:LOCALAPPDATA\Programs\CorridorKey\bin\corridorkey-setup.cmd" models
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
./Keyloom-2.3.0-1b3b546391-linux-x86_64.run --models skip
```

It installs to `~/.local/opt/CorridorKey` by default, adds a desktop entry, installs the `corridorkey`, `corridorkey-setup` and `corridorkey-coordinator` commands under `~/.local/opt/CorridorKey/bin`, and leaves model downloads to the app when run with `--models skip`.

Options:

| Option | Meaning |
|---|---|
| `--prefix DIR` | Install somewhere else. The folder must be new/empty or an existing CorridorKey install. |
| `--models skip` | Install the app only; models later with `corridorkey-setup models`. |
| `--repair` | Restore damaged program files from this installer (models are re-verified only with `--models full`). |

Do not run two installers on the same folder at once.

Later commands:

```sh
~/.local/opt/CorridorKey/bin/corridorkey-setup status --verify
~/.local/opt/CorridorKey/bin/corridorkey-setup models
~/.local/opt/CorridorKey/bin/corridorkey-setup repair
~/.local/opt/CorridorKey/bin/corridorkey-setup rollback
~/.local/opt/CorridorKey/bin/corridorkey-setup uninstall
```

## Online installer

Recommended when internet access is available. Alongside the offline installers above, an **online installer** exists for Linux and Windows. It **contains Keyloom itself** (the application, its own Python runtime and FFmpeg) and leaves out only the very large public dependencies, mainly PyTorch and the NVIDIA CUDA user-space libraries. Those are downloaded during installation, each as a hash-pinned wheel from the public PyPI / PyTorch / NVIDIA package servers. The result is byte-for-byte the same version (`version_id`) and the same installed files as the full offline installer.

**Getting it vs. installing it.** The source repository is private, so you receive the online installer from the owner **once, manually** (the same way as the offline installer). Running it needs **no GitHub access, sign-in, token, client id or release lookup**: the only connections are anonymous HTTPS downloads of the pinned public wheels, over HTTPS only. If everything is already installed and unchanged, it works completely offline.

- **Every file is verified.** Each file of the finished install is checked against the install manifest (SHA-256 and size) before anything is activated; every downloaded wheel is checked against its pinned size and SHA-256, and only the entries the manifest names are extracted. The normal installer step then runs (OS-library check, validation, shortcuts, receipt, rollback).
- **First install:** downloads only the wheels (several GiB; the size is printed before it starts), assembles a complete copy under the install folder and installs it.
- **Update:** finds the existing install from its `receipt.json`. Files identical to the current or previous version are copied from the installed copy, the rest comes from the installer, and a wheel is downloaded only if a file it provides is still missing. If this exact build is already installed and every file verifies, it says so and exits without any network access.
- **Repair:** `--repair` rebuilds the current version the same way and restores damaged files.
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

Exit codes: `0` installed or already up to date, `2` failed (the previous install is untouched), `4` Keyloom is running, `130` interrupted. Allow free space for the new version plus the wheels being downloaded, and room for the activation step: about three times the size of the app on first install (see *What you need*).

## Models

Keyloom installs the **full** model profile: every model the app can use. They are *not* inside the installer and the download can be very large and slow. It is resumable and every file is checked afterwards. The app also offers **Settings → Models & runtime…** (see the [User guide](user-guide.md#models)) to install, verify and repair them.

## Install result and exit codes

These codes apply to the Linux installer and setup command. Windows Setup can complete successfully while models are skipped or incomplete; read its final notice and resume with the model command above.

| Exit code | Meaning |
|---|---|
| 0 | Installed and all models are ready |
| 3 | The app is installed, but models are missing, skipped or incomplete. Run `corridorkey-setup models` to finish. Some workflows stay unavailable until then. |
| 2 | Refused or failed; nothing was left half-installed |
| 130 | Interrupted (Ctrl-C). The previous state is restored. |

If the download is interrupted, run the command again. It resumes.

## Updating, repairing, uninstalling

- **Quit Keyloom first.** Closing the window only *hides* it when background mode is on (see [Settings → Interface](user-guide.md#background-running)). Choose **File → Quit**, and check your task list for a hidden older copy (there is no tray icon) before you update.
- **Update:** run the new installer. Two versions are kept (current and previous) so a failed update rolls back.
- **Repair:** Linux `…run --repair`, or `corridorkey-setup repair`. On Windows, uninstall and reinstall.
- **Uninstall:** Windows *Settings → Apps*, or Linux `corridorkey-setup uninstall`.

Projects, outputs, models, the download cache and your settings live **outside** the install folder and are kept through repair, update and uninstall.

## Starting the app

- Windows: Start Menu → Keyloom.
- Linux: your application menu → Keyloom, or `~/.local/opt/CorridorKey/bin/corridorkey`.

Optional: have Keyloom start hidden when you sign in (needed for always-on network rendering). See [Background running](user-guide.md#background-running).
