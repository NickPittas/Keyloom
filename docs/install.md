# Installing Keyloom — powered by CorridorKey

Keyloom installs for your own user account. Installing the app does not enable network hosting. Use **Clients** to install and start the optional per-user network service, or join an existing network. See [Network rendering](network-rendering.md).

## What you need

| | |
|---|---|
| Windows | Windows 10 or 11, 64-bit. A clean-machine install has not been certified yet. |
| Linux | 64-bit, glibc 2.30 or newer. Linux Mint 22.x is the expected target; it has not been tested on a clean machine. |
| GPU | NVIDIA GPU with the vendor driver installed in the operating system |
| Linux libraries | OpenGL/EGL, X11 or Wayland client libraries, xkbcommon, fontconfig, freetype, glib, dbus, libstdc++. The installer checks these first and prints an `apt` hint on Debian/Ubuntu/Mint if something is missing. |
| Internet | Needed to download the installer (if hosted privately, you must be signed in to GitHub) and the models |
| Disk | About 9 GiB for the app (allow roughly 18 GiB during an update), plus room for the **full** model set (large) and your project media. Projects can live on any drive. |

You do **not** need to install Python, Rust, FFmpeg, the CUDA toolkit or the Visual C++ runtime. The app includes Python, FFmpeg, CUDA user-space libraries and the required Visual C++ runtime; no compiler or toolkit is needed. A missing NVIDIA driver only produces a warning during install, because models can still be downloaded, but you cannot render without it.

The installers are for private use. The licence is non-commercial (CC-BY-NC-SA-4.0) and public redistribution is not cleared.

## Windows

GitHub limits file size, so the installer comes in slices: one `.exe` plus several `.bin` files (each up to 1900 MiB), and a `.sha256` file.

`<id>` below stands for the build identifier in the file names of the release you download (the same in every file of one release). Final installer IDs are not published yet.

Download the [Windows release](https://github.com/NickPittas/Keyloom/releases/tag/v2.2.0-windows).

1. Download **all** of these into the **same folder**:
   - `Keyloom-2.2.0-<id>-windows-x86_64-setup.exe`
   - every `Keyloom-2.2.0-<id>-windows-x86_64-setup-1.bin`, `-2.bin`, … (all of them)
   - `Keyloom-2.2.0-<id>-windows-x86_64-setup.exe.sha256`
2. Check the files. In PowerShell, inside that folder:

   ```powershell
   Get-Content .\Keyloom-2.2.0-<id>-windows-x86_64-setup.exe.sha256 | ForEach-Object {
     $hash, $name = $_ -split '\s+', 2
     $got = (Get-FileHash -Algorithm SHA256 -LiteralPath $name).Hash
     if ($got -ine $hash) { throw "SHA256 mismatch: $name" }
     "OK  $name"
   }
   ```

   Every line must say `OK`. Stop if PowerShell reports an error; re-download the missing or mismatched file.
3. Double-click the `.exe`. Keep the "Download and verify ALL models now" option ticked unless you want to do the models later.
4. Setup installs to `%LOCALAPPDATA%\Programs\CorridorKey`, checks that the app starts, adds a Start Menu shortcut and then opens a console window that downloads and verifies the models. Leave that window open until it finishes.

If a problem appears, the **previous** version is restored, or on a first install everything is removed.

Command-line helpers (PowerShell):

```powershell
& "$env:LOCALAPPDATA\Programs\CorridorKey\bin\corridorkey-setup.cmd" status
& "$env:LOCALAPPDATA\Programs\CorridorKey\bin\corridorkey-setup.cmd" models
```

## Linux

Download the [Linux release](https://github.com/NickPittas/Keyloom/releases/tag/v2.2.0-linux).

The release is a single self-extracting `.run` file. If it is hosted on GitHub it is split into parts of up to 1900 MiB. Where a complete `.run` is already available (for example on a local file share), skip the reassembly steps and go straight to *Run the installer*.

### Download and reassemble the parts

Put these files in one folder:

- `Keyloom-2.2.0-<id>-linux-x86_64.run.part001`, `.part002`, `.part003` (every part)
- `Keyloom-2.2.0-<id>-linux-x86_64.run.parts.sha256` (checksums of the parts)
- `Keyloom-2.2.0-<id>-linux-x86_64.run.sha256` (checksum of the whole file)

```sh
cd ~/Downloads/corridorkey
sha256sum -c Keyloom-2.2.0-<id>-linux-x86_64.run.parts.sha256
cat Keyloom-2.2.0-<id>-linux-x86_64.run.part[0-9][0-9][0-9] > Keyloom-2.2.0-<id>-linux-x86_64.run
sha256sum -c Keyloom-2.2.0-<id>-linux-x86_64.run.sha256
chmod +x Keyloom-2.2.0-<id>-linux-x86_64.run
```

Use the numeric pattern `part[0-9][0-9][0-9]`. A plain `part*` would also match the `.parts.sha256` file and corrupt the result. Each check must print `OK`.

### Run the installer

```sh
./Keyloom-2.2.0-<id>-linux-x86_64.run
```

It installs to `~/.local/opt/CorridorKey` by default, adds a desktop entry, installs the `corridorkey`, `corridorkey-setup` and `corridorkey-coordinator` commands under `~/.local/opt/CorridorKey/bin`, then downloads and verifies the full model set.

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
