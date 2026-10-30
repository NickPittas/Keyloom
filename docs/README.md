# Keyloom documentation

## Current guides

- [Installation and latest downloads](install.md): Linux/Windows **online installers only**, setup, repair and uninstall.
- [User guide](user-guide.md): native UI, models, workflows, Clients and the Machines/Waiting queue.
- [Network rendering](network-rendering.md): discovery, access controls, shared storage and Automatic scheduling.
- [Pascal GPU compatibility](PASCAL-MANUAL-INSTALL.md): current limitations; old runtime-swap instructions do not apply.
- [Changelog](../CHANGELOG.md), [contributing](../CONTRIBUTING.md) and [security reporting](../SECURITY.md).

The source repository and linked release assets are public. That is not a grant of additional installer redistribution rights or upstream endorsement; see [Licence and sharing](../README.md#licence-and-sharing). Only the latest release's online installers are retained on GitHub. Older tags/source history remain available, but older release pages and binaries are not current downloads.

## Developer references

- [Native application](../rust-ui/README.md)
- [Installer build tooling](../packaging/README.md): canonical native builds can generate internal full/offline artifacts; those are **not** distributed offline installers.
- [Coordinator protocol](../backend/coordinator/README.md)
- [Repository instructions](../AGENTS.md) and [test isolation](../rust-ui/AGENTS.md)

## Historical and archived material

- [Upstream README snapshot](upstream-readme.md) and all files under `docs/i18n/` are legacy EZ-CorridorKey documentation, **not translated Keyloom 2.5.0 guides**. Their updater, Docker/macOS, old installation paths and screenshots are historical. Use the current English guides above.
- [2.2.0](release-2.2.0.md) and [2.3.0](release-2.3.0.md) release records, with their adjacent asset manifests, preserve the original build/distribution evidence. They do not indicate that those downloads or offline installers are offered now.
- The dated branding, discovery and stability validation reports record specific snapshots. They do not establish general clean-machine or performance certification.
- Files under `docs/design/rust-ui/` are engineering plans, contracts, proposals, audits and acceptance/review records. Read their recorded date and scope: an old “pending”, “deferred”, private-distribution assumption or Qt reference is not a statement about current Keyloom. In particular, the standalone release plan is not the current installation guide or redistribution clearance.
- Root `PLAN-*.md` files are historical implementation plans, not instructions to run setup/updater commands.

Historical evidence is intentionally preserved rather than rewritten to claim checks that were never performed. Third-party documentation and licences in vendored model/engine directories describe those components, not Keyloom's release policy.
