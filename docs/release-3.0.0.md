# Keyloom 3.0.0 release

Keyloom — powered by CorridorKey. Source commit `3ec5df0bd5fcd902118f3c4165e7b8f102c199d5`; later documentation updates do not change the payloads. Only the online installers are distributed. Source and release assets are public; redistribution permissions are described in the [README](../README.md#licence-and-sharing).

## Before updating

Quit Keyloom first, **including the copy in the tray** (tray icon > **Quit**). The installer stops with code 7 if Keyloom is still running. Install over the existing folder without uninstalling. Projects, settings, models and network identities are kept.

## Changes since 2.5.0

- **New pages:** new Start, Board, Shot, Machines, Models and Settings pages.
- **Board:** the Queue panel is always visible and shows per-machine and per-stage progress. Drag its edge to resize it. Jobs sent to other machines show running, completed or failed, with the reason, in Activity and on the shot card.
- **Much faster Board** with projects on network drives (Windows SMB).
- **Quit, Close and tray** work as expected, including Ctrl+Q on non-Latin keyboard layouts.
- **Shot page:** a drawing tool column beside the frame (SAM points, paint, erase, box, and Chroma colour pick); resizable panels; output selection in Deliver; playback controls always visible.
- **Models page:** **Repair** re-downloads a model and its dependencies and never deletes your files. Models already on disk are found without downloading them again.
- **Installer cleanup** of unused model copies: old Keyloom folders and unused Hugging Face cache files, including the shared blob store. It never follows links or junctions and never touches models in use.
- Fixes to the batch dialog, shortcuts (Ctrl+Shift+1..5, Ctrl+N/O/S/I) and viewer toolbar layout.

## Payloads

| Platform | Payload identity | Installer |
|---|---|---|
| Linux x86-64 | `3.0.0-9fd666602f` | Online `.run` |
| Windows x86-64 | `3.0.0-f6b35b1a00` | Online `.exe` + `-1.bin` |

Sizes and SHA256 values are in [the asset manifest](release-3.0.0-assets.json). Keep the Windows `.bin` beside the `.exe`.

| File | SHA256 |
|---|---|
| `Keyloom-online-3.0.0-9fd666602f-linux-x86_64.run` | `382996808578c69dcd153548ac4557c687902e3595c7876a24fe1047ecda7c0f` |
| `Keyloom-online-3.0.0-f6b35b1a00-windows-x86_64.exe` | `455454494da584c19716bf0168fa8e53eb03d4544478b80b09e0eff31af18a17` |
| `Keyloom-online-3.0.0-f6b35b1a00-windows-x86_64-1.bin` | `40d0058a24a13b0bd05bfce1170f47a5bff2d07a74a196e394ac7be63759d5e2` |

Releases: tags `v3.0.0-windows` and `v3.0.0-linux` on [GitHub](https://github.com/NickPittas/Keyloom/releases).

## Verification

Verified live on Linux and on Windows: the new Start, Board, Shot, Machines, Models and Settings pages; the Queue panel with per-machine and per-stage progress; jobs on other machines; Quit, Close and tray; the Shot drawing tools; Deliver output selection; Models Repair and finding models already on disk; and Board speed with projects on a network drive. The installer model cleanup ran on both systems (on Windows it found nothing to remove); its handling of links was checked on Linux with symbolic links, and Windows junctions were not exercised.

The online installer needs no GitHub sign-in or account; it downloads only pinned public dependencies. NVIDIA display drivers remain managed by the operating system, and models are downloaded from the app. See [installation](install.md), [workflow guide](user-guide.md) and [network setup](network-rendering.md).
