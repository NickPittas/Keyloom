# Network client implementation plan — 2026-10-23

Status: implementation started. This document records the user's agreed product contract and supersedes the SSH/worker-role deployment assumptions in the earlier readiness audit and standalone release plan. Existing local behavior and installed user data must remain working.

## Product contract

- Every client is a full CorridorKey installation. It can open projects, render locally, submit work and optionally receive network work.
- One installation runs server mode and owns the durable job queue. Participating clients connect outward to its authenticated application API. No inbound SSH requirement on clients.
- Each installation owns named Path Mapping settings: shared location name to a local mount, drive or UNC root. The receiving client resolves logical project/media/hint/output references using its own mappings.
- From Batch, any connected installation can submit one shot, selected shots or all shots, with explicit client selection or Automatic placement.
- Submitted jobs contain complete frozen settings. A preset name is descriptive metadata, never an instruction to look up a potentially different preset on another machine.
- Settings precedence is preset base, then explicit shot overrides, then permitted hardware adaptation. A fixed shot override such as Temporal Window Off must be honored. Returning to Batch, saving and reopening must preserve it.
- The main app always displays current CPU/RAM/GPU/VRAM monitoring, and each registered client exposes the same monitoring in the app.
- Hardware-dependent controls are marked. Auto/Adaptive permits the documented choices; Fixed remains a requirement. Automatic placement excludes incompatible clients with a reason. No silent resolution/model/workflow downgrade.
- Shared storage is the first release scope. File transfer, cloud infrastructure and distributing frames of a single shot across machines are not required for this release.

## 1. Per-installation Path Mapping — in progress

Add a discoverable Path Mapping page using existing Settings persistence and picker plumbing. Rows contain shared location name and local root, with Add/Remove, Browse, a preview and explicit access test. Existing SSH-target maps remain readable and untouched; do not reinterpret them as machine-owned named roots automatically.

Introduce the smallest reusable logical path representation and encoder/resolver. Relative path components must reject traversal and absolute-path injection; names must be unambiguous, duplicate mappings rejected and local roots validated using local platform semantics. Resolve under the selected root. Do not parse a foreign drive path as if it were a local POSIX path. If overlapping local roots are allowed, define deterministic longest-root encoding and reject ambiguous cases.

Tests: old settings load unchanged; new mappings save/reload; successful fixture reference round-trip; spaces/Unicode; missing/duplicate locations; traversal; whole-component root matching; supported native path cases. Access tests must use owned temporary files and never clobber or recursively delete user content. Native Windows tests remain a separate acceptance requirement.

Delivery: working local settings feature and path contracts ready for API use, with no network listener enabled.

## 2. Shot overrides and frozen submission settings

Trace current draft/session/preset behavior before changing it. Reuse existing clip persistence and submission objects. Distinguish explicit user overrides from inherited preset values; a full draft with defaults is not sufficient provenance by itself. Do not infer legacy intent by blindly comparing to today's defaults.

Make a shot-level edit persist through Back, project save/reopen and Batch preset resolution. Batch preview and actual submission must use the same resolution logic for key, hint and delivery stages. Merely selecting a Batch preset must preserve explicit shot overrides. Replacing shot overrides requires an explicit user action.

Expose an override indication and Reset to preset. Preserve source-bound state: frame ranges, interpretation, annotations, hint references and destinations must never be copied indiscriminately from another shot or a preset.

Create an immutable submission snapshot including preset values and per-shot overrides, requested outputs/ranges and required model identities. Later preset/shot changes cannot mutate an existing queued request. Existing saved project data needs a conservative compatibility path that preserves effective behavior.

Tests: change Temporal Window to Off, Back, save/reopen, choose a Batch preset with a different temporal window, and verify the queued snapshot remains Off; another unmodified shot inherits the preset; changing the preset after enqueue leaves the request unchanged. Cover full pipeline and legacy preset callers, not only a new helper.

## 3. Versioned API, registration and one-shot execution

Define the minimal shared protocol: stable installation identity, protocol/build compatibility, client capabilities, authenticated registration, job preflight/submit/accept, status/events, cancel and result query. Reuse existing worker commands/events and scheduler transitions wherever possible; avoid parallel execution implementations.

Clients establish an outbound connection to the server; assignments and progress use that connection. Establish server authentication, pairing and authorization before remote execution. Do not expose arbitrary shell commands, unrestricted filesystem writes or credential-bearing diagnostics. Reviewed implementation choice: outbound HTTPS claim/report polling, a bundled Python link process for the Rust app, and a Python coordinator backed by SQLite on its local disk. No WebSocket or new Rust networking dependency is required for the first slice. The link carries messages; it does not become a second render scheduler.

Network mode is opt-in. No listener or firewall change is part of ordinary local startup. Bind/interface, encryption, allowed peers, credential storage and exact firewall behavior must be concrete before any host configuration changes. Keep pairing credentials out of project files and logs.

One-shot acceptance: full installed client registers; the receiving client resolves all inputs/hints/output destinations using its own mappings, performs preflight and reports its effective plan before running a real isolated fixture; status/results reach the submitting app. The server never resolves a client filesystem path or launches that client's worker. Missing paths/models and incompatible versions produce actionable errors.

### Reviewed first API contract

- Python coordinator owns durable submissions, placement and attempt transitions in local SQLite transactions. It never independently runs workers. The server installation may render only by registering its own ordinary receiving client.
- Receiving Rust Scheduler remains the sole local executor. Local and network submissions share the same GPU owner. Polling for assignments does not reserve an idle GPU; acceptance/start must atomically reserve it in the local scheduling path.
- Initial operations: expiring/rate-limited one-time pairing; authenticated session registration; idempotent frozen submission; idle-client claim; preflight result and start acknowledgment; ordered progress/terminal events; cancellation; submitter event/status queries.
- Repeating a submission ID with the same content returns its original result. Reusing that ID with different content fails. Claim retries return the same attempt. Repeated event sequence numbers do not duplicate state transitions; event reports from old attempts are refused.
- Stable installation identity is tied to individual credentials; identify a physical GPU using its UUID where supported. Do not treat two addresses as two GPUs. Initial pilot can require one enabled GPU per installation; unsupported identity must be reported rather than silently promising deduplication.
- Transport uses an explicitly trusted server certificate and per-installation token. A fingerprint alone cannot bootstrap CERT_REQUIRED: the pairing invitation must also supply the certificate/trust material through the same user-verified channel (or use an already trusted CA). Verify the supplied fingerprint before trusting it, validate the intended endpoint and never disable certificate verification to retrieve the certificate. Final trust flow requires focused tests.
- Tokens live in a separate user-private credential store, never ordinary Settings/project files/logs; restrict Linux mode and verify Windows user access at creation. Do not defer access protection while distributing credentials. Server stores token hashes and supports revocation.
- Bound request/event sizes, active connections, polling durations and authentication attempts. A shared logical job spec carries output patterns/configuration, not per-frame output arrays; receiver expands it with the existing builder. Resolve embedded external source/hint references too. Unhandled/unmapped dependencies fail preflight; do not read submitter-local paths by accident or rewrite shared clip metadata to make it work.
- Preflight is write-free except explicitly owned access probes. Server start acknowledgment is an attempt-state commit point. A started or disconnected attempt stays owned/fenced; no heartbeat timeout authorizes reassignment. Offered-but-never-started assignments may be revoked with compare-and-set; a late start must be refused.
- Keep stable logical job identity separate from attempt identity. Verify Resume/manifest/lock semantics before putting attempt IDs into worker job IDs. For the first vertical slice, explicitly refuse network Resume/Replace if ownership continuity is not established; local supported modes remain unchanged.
- First acceptance is one real Key shot with frozen overrides from one submitting installation on one receiving installation. Hint/Deliver and multi-GPU registration follow; they are required for the final release, not claimed by the first Key-only slice.

## 4. Hardware-aware execution plans

Registration reports GPU identity/capabilities, total VRAM, RAM, installed models and supported routes. Dynamic resources are sampled again when claiming/starting a job. A stale free-memory sample is not a reservation.

The receiver resolves requested settings into a per-attempt effective plan using media dimensions/frame count, route/model, actual resources and the requested adaptation policy. Report requested/effective values and reasons to server/UI, and record the effective plan with the attempt. Never rewrite the preset or shot overrides.

Reuse existing Auto VRAM selection and effective-profile recording, but audit every supported route. Only validated execution-only fallbacks can be automatic for fixed jobs. Adaptive temporal changes must be explicit in the request. A batch that requires consistent temporal settings pins a common profile and excludes clients that cannot honor it.

Resume matches the recorded effective profile. Migration to another GPU must not mix incompatible partial outputs. Insufficient resources remain visible and actionable; any retry is bounded and retains output ownership.

Tests: mixed GPU capabilities/memory fixtures, changed available memory at start, explicit Off, fixed incompatible settings, permitted Auto adaptation, no-fit case, safe OOM outcome and Resume on a different client. Real calibration is required on participating GPUs before memory estimates are claimed reliable.

### Always-visible local and per-client monitoring — added by user

Display system CPU utilization, RAM used/total, GPU utilization and VRAM used/total in the main app on Board, clip workspace, Settings and Clients. Keep the summary compact and stable as numbers change; use the existing global chrome/footer rather than a separate monitoring window. Identify the rendering GPU and provide per-GPU readings for multiple GPUs. The Clients page shows equivalent statistics per installation, including while idle.

Collect in the background at roughly one-to-two-second intervals. Use existing lightweight system/NVML facilities, without importing torch, allocating CUDA contexts or loading models. Reuse one persistent sampler, not a process per frame. Counters fail independently; missing drivers, permissions or unsupported utilization show unavailable, never a fake zero. CPU utilization uses actual sample deltas. Samples use a clear byte/percentage schema and validated finite values.

Remote clients report latest telemetry over their authenticated outbound session, separate from job progress. Server only accepts a client's own current-session reports, keeps the latest sample and exposes receipt/sample age. Sequence/receipt timing prevents replay from making old values look fresh. No history database or graphs are required. Disconnected/stale values are labeled; a local counter read does not establish a GPU memory reservation for scheduling.

Tests use fixture counters and a fake sampling helper: first-sample warmup, deltas, missing/error counters, multi-GPU identity, stale/disconnected samples and helper cleanup. Private rendered QA checks the always-visible display on main pages and narrow layouts. Real Windows/NVML sampling and network telemetry remain explicit installed-pilot acceptance items. No temperatures, fan speeds or process inventory are required.

## 5. Batch routing, persistence and recovery

Persist stable job IDs and frozen settings from the first API submission. Retrying the same submission ID must not create another job. Server assignment owns attempt IDs; the worker acknowledges claims and reports authoritative state. A lost connection never proves a writer stopped.

Extend existing dependency scheduling to supported remote Hint -> Key -> Deliver stages, with per-stage eligibility and clear reasons. Preserve local interactive operations where required. Client selection in Batch must describe its actual effect on the whole pipeline.

Persist queued/running/terminal records and event positions. Reconnect queries/replays state and reconciles manifests. UI close/reopen must not erase ownership. Coordinator/worker restart and NAS loss must not duplicate publication, steal locks or overwrite completed output.

Start by assigning whole shots, respecting one active owner per physical GPU. Multiple addresses for the same installation/GPU must not produce duplicate slots. Local standalone rendering and network receiving must share GPU ownership rather than competing silently.

Tests: one/selected/all clips; mixed overrides; dependencies; cancel/resume; idempotent submission; reconnect; worker/server/UI restart; stale worker events; duplicate LAN/overlay registration; inaccessible NAS; output collision and safe recovery.

## 6. Installed pilot acceptance and rollout

Use the existing full installer/runtime/catalog/lifecycle, adding network role configuration without requiring a checkout, remote Python path or SSH server. Define whether receiving is app-open/login startup or boot service; test storage/model access under the actual execution account. Boot services cannot assume interactive drive mappings or login-only credentials.

Qualify one clean installed Linux client and one clean installed Windows client against the server. Record exact build/protocol/catalog identities, GPU/runtime/storage configuration, settings and outputs. Run representative-resolution multi-shot pipelines plus disconnect/reboot/cancel/resume/update/rollback scenarios. Keep projects, outputs and models on uninstall.

Only after this installed pilot passes should we distribute to the rest of the network. Synthetic unit tests, a source checkout or old SSH smoke results are not installation acceptance.

## Execution and evidence rules

Parent coordinates; Sonnet 5.5 medium implements, GLM 5.3 Flash low researches and Opus 5.5 medium independently reviews concrete revisions. Preserve the dirty working tree: no stash, reset, checkout, discarded edits or unsolicited commits.

All tests use rust-ui/tools/isolated-test.sh; rendered checks use tools/virtual-display-test.sh with private fixtures/config and mocked pickers. Never test on the live desktop. No live network/credentials/firewall/NAS changes are necessary for the foundational slices.

Each slice reports actual checks and limitations. Isolated tests, private rendered QA, native Windows, installed artifacts, real network and real GPU evidence are distinct. Existing unrelated failures must be named, not hidden by a blanket pass claim.
