# CorridorKey 2.2.0 release record

Published as separate Linux and Windows downloads in the private repository.

| Platform | Payload identity | Packaging |
|---|---|---|
| Linux x86-64 | `2.2.0-a263fbf35a` | self-extracting installer, three numeric parts for GitHub |
| Windows x86-64 | `2.2.0-bcc0720114` | per-user Setup exe and two required disk slices |

The suffix is the content identity of the complete staged payload, not a Git commit. Both payloads were built from the release source before the documentation commit. The installer manifests record individual file sizes and SHA256 hashes; release checksum files cover every downloadable installer part.

## Checked on the existing PCs

- Linux and Windows native release builds, installed runtime self-checks, and packaged imports passed. Linux all-ELF ABI scan reports glibc 2.30; Windows app-local VC runtime copies are version 14.44.35211.0.
- On both platforms, start in background, normal close to background, second launch restoring the same process, and explicit Quit were checked. The network client continued reporting telemetry while hidden.
- Windows was checked using the actual CorridorKey top-level window, filtered by PID and title; its hidden event-loop window is not a visibility indicator. Python helper consoles no longer appear over the GUI.
- The earlier accepted Windows/Linux renders, batch assignment, stages, cancellation and resource bars remain recorded in [network client live evidence](design/rust-ui/network-client-live-20261024.md). They were not repeated for packaging.
- The screenshots use the Kessel documentation/test projects. The batch-progress screenshot is reused from the earlier real run. Opening the batch confirmation for documentation was cancelled without submission.
- New background/login settings default to off. Temporary live-check settings were restored after capture.

## Limits

These are existing-PC checks, not clean-machine certification. A clean Windows and Linux Mint install remains for user confirmation. NVIDIA drivers and Linux desktop graphics libraries belong to the operating system; models download separately. No new automated tests or full render walkthrough were run for this release pass. Quit any hidden copy before upgrading or uninstalling.

See [installation](install.md), [single-shot and batch guide](user-guide.md), and [multi-PC setup](network-rendering.md).
