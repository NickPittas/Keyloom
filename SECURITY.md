# Security Policy

## Reporting a vulnerability

Report Keyloom vulnerabilities through this repository's [private security advisory form](https://github.com/NickPittas/Keyloom/security/advisories/new) when private reporting is available. If it is unavailable, ask the repository maintainer for a private reporting channel without posting exploit details publicly.

Do not send Keyloom reports to the upstream project's email address or assume its maintainers support this fork. Do not open public issues containing vulnerability details, tokens, certificates, credentials, private paths or media.

Include:

- Affected Keyloom version, operating system and installer payload ID.
- A description, impact and minimal reproduction using non-sensitive fixtures.
- Relevant redacted logs and whether the issue involves the desktop app, worker, installer or network service.

No acknowledgment or remediation deadline is guaranteed. Coordinate disclosure privately where possible.

## Scope

- Native Rust UI and Python workers/model-management helpers.
- Local file handling, subprocesses and project/output ownership.
- Network coordinator, discovery, credentials and shared-path handling.
- Linux and Windows installer generation, dependency verification and activation/rollback.

Report vulnerabilities solely in third-party dependencies or externally hosted model weights to their maintainers. For integration issues affecting Keyloom, include the dependency name and version in a private Keyloom report as well.

## Network and installation considerations

Keyloom can expose a per-user network service. The installer configures hosting automatically unless explicitly removed; rendering still requires the app to be open or running in background mode. Restrict reachability to trusted networks and review installer firewall prompts. This is not a claim that the application has no network-facing components.

The **Let other PCs join this PC** control governs new joins. Existing logins require separate retirement when access should be revoked. Treat pairing credentials, service databases and certificate identities as sensitive. See [network setup and access controls](docs/network-rendering.md).

Online installers download pinned public dependencies and verify them during installation; model downloads are separate. Do not load untrusted model files, replace a managed runtime in place or bypass installer checks. Public availability does not establish additional redistribution permissions; see [Licence and sharing](README.md#licence-and-sharing).

Keep personal footage and credentials out of issue reports. Existing user data must not be used as a destructive test fixture. Contributor test isolation and command-safety requirements are documented in [AGENTS.md](AGENTS.md) and [rust-ui/AGENTS.md](rust-ui/AGENTS.md).
