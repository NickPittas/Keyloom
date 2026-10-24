# Keyloom 2.3.0 release record

Keyloom — powered by CorridorKey. The payload source is commit `e7da0e28ba7d72a912bb3f915dc2149eff9cbf54`; later documentation updates do not change these payloads. The repository and release downloads remain private.

## Changes

- Retain connections to multiple hosts while hosting a network on this PC. Each connection has independent connect, receive-work and default-placement controls.
- Preserve existing PC names until **Save** is pressed. Saved name changes reach connected hosts immediately and disconnected hosts on reconnect.
- Keep separate job histories and credentials per network, with a shared GPU reservation so hosts cannot assign conflicting work.
- Recover certificate-address mistakes using the covered addresses shown in the interface, without deleting the network identity.
- Offer an online installer alongside the full offline installer. The online version bundles Keyloom and fetches large dependencies from public package sources; installation needs no GitHub sign-in. Matching installed dependency files are reused after verification.

Models remain downloads managed by the app. NVIDIA display drivers remain managed by the operating system. Projects, settings, model stores and network identities are preserved during an update.

## Payloads

| Platform | Payload identity | Installer choices |
|---|---|---|
| Linux x86-64 | `2.3.0-1b3b546391` | Online `.run`, or full offline numeric `.run` parts |
| Windows x86-64 | `2.3.0-662dcec4eb` | Online exe + bin, or offline exe + every bin |

Sizes and SHA256 values are recorded in [the asset manifest](release-2.3.0-assets.json). Keep each Windows installer’s matching bin files beside its exe.

## Verification

Linux and Windows native 2.3.0 release builds compile. The Linux UI has a glibc 2.30 floor. Multi-host code was reviewed, and the findings were corrected. These are build and source-review results; simultaneous multi-host behavior has not been verified live on the installed applications.

Both online and offline installers were built on their target operating systems. On Linux the online installer’s actual payload was unpacked, its pinned public wheel downloads restored the omitted libraries, and all 34,381 installed manifest entries verified. This was payload reconstruction in a build folder; it did not activate an installation or alter desktop shortcuts, settings or projects. Windows public wheel mapping passed byte-for-byte build checks and both Inno installers compiled; Windows installation, locks and repair behavior remain unverified live.

No approved render workflows were repeated. Clean Windows and Linux Mint installations remain for user confirmation. NVIDIA display drivers and Linux desktop libraries belong to the OS, and models remain downloads through the app. The online installer requires no GitHub sign-in, account or token when it runs; it downloads only pinned public dependencies.

Quit Keyloom, including a hidden copy, before updating. Install over the existing folder without uninstalling. See [installation](install.md), [workflow guide](user-guide.md) and [network setup](network-rendering.md).
