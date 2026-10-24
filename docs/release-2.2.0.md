# Keyloom 2.2.0 release record

Keyloom — powered by CorridorKey. Separate Linux and Windows installers are being prepared for the private repository; final asset identities are listed in the release asset manifest.

## Changed setup verified on the existing PCs

- Linux and Windows native release builds compile. Linux UI targets glibc 2.30. The bundled runtimes include Python, inference libraries, FFmpeg and the Windows app-local Visual C++ runtime.
- Linux: installed and started the network service from Clients; joined the host locally; created a private invitation; stopped and restarted the service; explicitly quit and reopened the GUI while the host service remained active.
- Windows: installed the per-user logon service from Clients, allowed the OS firewall prompt, joined the host locally, created an invitation, stopped and removed the service. Recreated the temporary network using its reachable Tailscale address.
- Two separate Linux PCs joined that Windows network using the **same invitation file**. The member lists showed all three PCs connected. No render was submitted during this setup check.
- The setup check exposed a Linux unit WorkingDirectory quoting error and a disconnected-client request backlog on Windows. Both are fixed. The backlog fix limits outstanding status requests and enters reconnect backoff when membership refresh fails.
- Code review confirmed that older network certificates keep their identity when moving to the UI-managed service. Local joining uses loopback transport while retaining the advertised hostname and pinned certificate for TLS verification.

## Earlier accepted behavior

The existing-PC checks for standalone runtime imports, background close/reopen, second-launch activation and explicit Quit were completed before the Keyloom rename. The earlier Windows/Linux renders, Batch assignment, stages, cancellation and resource bars remain recorded in [network client live evidence](design/rust-ui/network-client-live-20261024.md). They were not repeated for packaging or the network setup change.

The guides use actual application screenshots from the Kessel documentation/test projects. Workflow captures from before the rename retain the CorridorKey title; their controls are unchanged. The Batch progress image comes from the earlier real multi-PC render. No production footage was modified for documentation.

## Limits

These are existing-PC checks, not clean-machine certification. A clean Windows or Linux Mint installation remains for user confirmation. NVIDIA drivers and Linux desktop graphics libraries belong to the operating system; models download separately. No new automated tests or full render walkthrough were run in this release pass. Quit any hidden copy before upgrading or uninstalling. Hosting starts at user sign-in; it is not a boot-before-login system service. A network has one host and no automatic host failover.

See [installation](install.md), [single-shot and Batch guide](user-guide.md), and [multi-PC setup](network-rendering.md).
