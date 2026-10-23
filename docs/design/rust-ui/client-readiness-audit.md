# Network client readiness — 2026-10-23

Source inspection and existing acceptance records. No machines contacted, services installed, firewall rules changed, or tests run for this audit. “Client” means a network processing machine controlled from the desktop.

## Revised product direction — user clarification

This section supersedes the earlier role/SSH proposal and ordering below. Each client is a full CorridorKey installation that can submit and receive work. One installation runs the server/coordinator. Clients connect outward through an authenticated application API; SSH is existing implementation evidence, not the proposed deployment requirement.

Each installation owns its Path Mapping settings: shared location name to local filesystem root. Jobs carry project/clip references using those shared names, resolved by the receiving client. Before accepting work, that client verifies every source/hint and output path. Shared storage remains the first deployment scope.

Any installation can submit one clip, selected clips or all clips from Batch. The server owns durable assignments and receives client progress/results over the established connection. Submissions include a settings snapshot, per-clip overrides, ranges, outputs and stable job identity; a preset name alone is insufficient.

### Hardware-dependent settings and execution policy

The user requires GPU-dependent settings to be flagged and automatically adapted to each receiving PC's GPU and memory. Treat this as part of job eligibility and preflight, not just a UI warning.

- Each client reports GPU identity, supported compute/backend features, total VRAM, current available VRAM, host RAM, installed model identities and supported routes. Refresh dynamic memory at job start; a registration-time sample is not a reservation.
- Flag hardware-sensitive controls in the app. Show Auto/Adaptive or Fixed, the relevant requirement, and requested versus resolved values per assigned client. Record an explanation when adapting. Never rewrite the saved preset with machine-specific choices.
- Resolve an effective execution plan on the receiving client using media dimensions, frame count, route/model and available resources. Report it to the server before rendering; freeze and record it for the attempt.
- Automatically adjust validated execution-only controls such as chunking/batching or supported offload strategies. Do not claim byte-identical output across GPUs; validate equivalence for each implementation.
- Output-affecting settings (notably temporal window) may adapt only when the job requests Auto/Adaptive. A fixed window remains a requirement. Do not silently change resolution, selected model, precision policy or workflow to make a job fit. An incompatible client is ineligible, with a reason; Automatic can choose another eligible client.
- Make adaptive temporal policy explicit: per-clip Auto can resolve differently on different GPUs. A batch requiring a consistent temporal profile must pin one common profile and restrict placement accordingly.
- Resume retains the original recorded effective profile. It cannot splice frames from a newly chosen incompatible profile. A replacement client must match the required profile or the job stays blocked; use a new output version for a changed profile.
- Recheck memory immediately before execution. If memory changes or allocation fails, retry only a bounded, approved execution-only fallback with safe cleanup. No silent quality reduction, duplicate publication or endless retry. If no plan fits, report insufficient resources.
- Qualification covers mixed GPU capacities, busy VRAM, unsupported hardware/model routes, media-size-dependent estimates, fixed versus adaptive settings, and resume on another GPU.

Existing reusable behavior: `CorridorKeyV2/corridorkey/runtime.py::apply_auto_vram` samples GPU memory and selects temporal window/low-VRAM mode using media dimensions and frame count. `backend/service/key_job.py` reports the effective profile and rejects Resume when it differs from the recorded profile. The scheduler also has an optional minimum-total-VRAM check. These do not establish complete network capability negotiation, hardware-sensitive UI or per-route adaptation.

Revised implementation order: (1) per-installation Path Mapping and validation; (2) client registration/API and GPU/model capability reporting; (3) single-clip submission, effective-plan negotiation and progress; (4) batch selection and full supported pipeline routing; (5) persistent reconnect/recovery and clean installed Linux/Windows mixed-GPU acceptance. Define persistent job identity/ownership in the API from the first submission implementation.

## Outcome

The remote Key execution foundation exists. A deployable, unattended client product does not yet exist. Installing the desktop package on another PC does not register a managed network worker.

The user has accepted the main app as working. `local-workflow-acceptance.md` now records installed Linux and Windows runtime/model/workflow evidence; the old NOT PASSED label in `standalone-release-plan.md` predates that evidence. These records do not establish current network-role acceptance.

## What can be reused

- JSON-lines worker protocol with capability/GPU/model reports, progress, cancellation and one job at a time (`backend/worker/__init__.py`).
- SSH process transport for Linux and Windows, including Windows encoded launcher; batch authentication and strict host-key checking (`rust-ui/src/worker.rs`, `Target::ssh_args`).
- Target selection, per-clip assignment, status, explicit retry and lost-writer fencing (`scheduler.rs`, `ui/clients.rs`).
- Per-target shared-storage path maps (`worker.rs`, `jobs.rs`, `ui/settings.rs`).
- Bundled Python, model catalog/provisioning, install manifests, lifecycle and platform installers (`packaging/`, `scripts/setup_models.py`).
- Prior synthetic remote Key smoke on Linux/NAS and Windows/local disk (`remote-validation.md`). This is prior evidence, not rerun in this audit.

## Release gaps and work

### 1. Durable ownership before unattended operation

The scheduler belongs to the desktop. Workers are subprocesses launched over SSH. Closing their input asks them to cancel and exit (`WorkerConn::request_stop`, `Drop`, worker `serve`). A network interruption is therefore not a supported detach/reconnect workflow.

Move the existing scheduler into the coordinator described in the existing release plan; make the UI reconnect to it. Persist submissions/dependencies/outcomes and reconcile authoritative worker state after process or connection loss. Use stable job IDs, idempotent submission and attempt ownership. Retain current no-lock-stealing behavior; a timeout cannot prove an old writer stopped. Address host/GPU identity so registering LAN and Tailscale addresses cannot dispatch twice onto one GPU.

Acceptance: UI close/reopen, coordinator restart, worker restart, dropped SSH/network, and reboot produce explicit recoverable states without duplicate writers or output replacement. A job's intended behavior on disconnect must be specified and tested.

### 2. Installable worker role

Current packaging installs a desktop app. Settings expect a remote repo folder, Python path, SSH host/user and GPU index. No worker-role service installation or enrollment flow was found.

Extend the existing package/lifecycle with the worker role, reuse its runtime/catalog/models, provide a stable launcher and supported version handshake. Install startup/stop/update/uninstall under a defined account. The machine should not require a checkout, hand-built environment or manual Python path. Report model readiness per route.

Acceptance: fresh Linux and Windows installs, model setup interruption/retry, reboot, update/rollback and uninstall. Services must use the same model/cache/scratch/storage roots as setup and remove only app-owned state.

### 3. Full remote pipeline routing

`scheduler.rs::task_slot` explicitly excludes nonlocal targets. `TaskKind` includes the hint generators, SAM work, extraction/import tasks and video delivery. Remote Key support therefore does not mean remote Hint → Key → Deliver support.

Decide which stages belong on workers and route those explicitly. Preserve local interactive work where appropriate; do not imply all stages follow client assignment until they do. Add mapped task paths and translate returned paths/status to the desktop's view. Respect dependencies, cancellation, output choices and frozen settings.

Acceptance: an assigned remote clip completes its supported pipeline with the coordinator GPU unused for those remote stages; a missing model/route gives an actionable explanation or explicit placement policy.

### 4. Shared-storage proof and Windows credentials

Only `Storage::Shared` is accepted. Transfer mode is explicitly rejected. `KeyJob::mapped` maps clip root, manifest directory and outputs; that alone is not evidence that every embedded source/hint reference is portable. Existing Windows remote validation explicitly leaves NAS authentication and the final bare-launcher GPU job unresolved.

For the first release, use shared NAS storage and defer transfer. Validate every actual source/hint/output path in the worker account/session before dispatch. Cover multiple mapping rows, Windows/POSIX syntax, external sequence references, write permissions and safe publication. NAS access must survive reboot and not rely on a desktop-only mapped drive.

Acceptance: Windows and Linux workers use the same real share under their installed account, with read/write/publication probes, clear permission errors and safe behavior on NAS disconnection. Test both mapping directions and two independent roots.

### 5. Connection setup and model trust

The existing SSH path requires working noninteractive authentication and trusted host keys. The Clients page shows status and Recheck, while technical target setup lives in Settings. There is no completed end-user enrollment flow.

Provide Add client / Test connection using the existing transport where suitable. Distinguish routing, SSH identity/authentication, protocol/version, GPU, models and storage failures. Derive expected model identity from the catalog: current unpinned remote Key jobs use the local worker's digests as reference, so a controller without a ready local GPU needs explicit support. Do not use allow-unverified as the normal installation path.

Acceptance: a fresh worker can be enrolled from the app without editing JSON or guessing repo/Python paths; wrong host key, missing route/model and inaccessible storage fail visibly. LAN and Tailscale address changes retain one worker identity.

### 6. Scoped machine integration

Reuse SSH if it meets the persistent design; do not invent a second network protocol unnecessarily. Installer setup must identify the actual service account and exact inbound requirements. Local-only installs open nothing. Any app-managed firewall rules must be peer/profile/interface scoped and removed on role disable/uninstall; preserve existing SSH/Tailscale configuration.

Acceptance: authorized controller succeeds, unauthorized peer fails; local use survives incomplete network setup. No credential storage in project files or logs.

## Recommended sequence

1. Agree on the first release contract: one controller, installed Linux/Windows workers, shared NAS; whole supported pipelines; documented disconnect behavior. Keep transfer mode outside this release.
2. Extract durable coordinator ownership and recovery using the existing scheduler/protocol.
3. Add worker-role package/startup and catalog-based readiness/trust.
4. Complete remote task routing and shared-storage preflight/mapping.
5. Finish Add client / Test connection with actionable diagnostics.
6. Qualify one clean Linux and one clean Windows worker from installed artifacts, then distribute to the rest of the network.

Role packaging and routing can be developed separately, but network deployment is accepted only after the ownership/recovery and real-storage checks pass. Existing temporary smoke deployments are not substitutes for installer acceptance.

## Pilot acceptance checklist

- Install without a developer checkout; report build/runtime/GPU/route readiness.
- Connect from main app and verify identity; show one slot per physical GPU.
- Read sources/hints and publish outputs through the actual service account on the NAS.
- Run real multi-clip Hint → Key → Deliver at representative resolution; verify output/range/settings.
- Exercise cancel/resume, existing-output refusal, disconnect/reconnect and NAS loss.
- Restart UI, coordinator, worker and machine; prove no duplicate writer and a usable recovery path.
- Update/rollback/uninstall without touching projects, outputs or other applications' environment.

## Limits

This is a source-grounded gap assessment, not a fresh network or GPU run. Earlier acceptance records cover synthetic inputs and older builds. No timeline is estimated until the persistent role contract and first worker platforms are fixed. Existing unrelated local UI test failures are not investigated here.
