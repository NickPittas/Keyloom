# Network client executable plan — 2026-10-23

Status: revised by Astra after checking Fable's review against the current dirty tree. Execution resumed by user instruction with parallel implementation workers. Every execution item remains unchecked until its required evidence exists. `network-client-implementation-plan.md` remains the product contract; this document supplies implementation order, dependencies and installed acceptance. The user's later instructions control evidence order where the contract's earlier slice/test instructions differ.

## Delivery and evidence rules

- Execute independent work in parallel with explicit file ownership. Use Sonnet 5.5 medium for substantial or multi-feature implementation; GLM 5.3 Flash or Haiku 5.5 medium for small bounded implementation. Coordinate shared interfaces before editing overlapping callers.
- Send completed features to Opus 5.5 medium only after actual live verification has been attempted and recorded. Never commission reviews of individual tasks or fixes. Create tests only after the relevant feature is fully implemented, finished and verified live; tests are not acceptance evidence.
- Deliver full installed Linux and Windows clients: local workflows, named installation mappings, preserved shot overrides, frozen submissions, authenticated server mode, hardware eligibility, monitoring, job control/recovery and cross-machine Hint → Key → Deliver. A Key-only pilot is not delivery.
- After authorization resumes: finish the whole feature implementation → perform actual installed live testing, recording pass or failure → review the whole feature. No microreviews. No automated tests created or run before implementation is finished and live-tested; later regression checks never establish acceptance.
- Build/package work follows implementation. Installed acceptance first establishes local operation on both OSes, then the first real received Key, then the other OS's received Key, then complete pipelines. These are observation checkpoints, not excuses to postpone missing implementation until after a demo.
- Preserve the dirty tree and existing projects, models, settings and outputs. No resets, stashes, discarded changes or unsolicited commits. Record binary/package SHA-256, source-content identity and protocol/catalog identity: HEAD alone cannot identify this tree.
- Updated user authorization: this PC is unoccupied and its real desktop, monitors and GPU may be used for actual live verification. This supersedes the earlier live-desktop restriction for this PC. Use the real installed runtime, real models and actual GPU rendering; do not substitute isolated/software-rendered displays, fake workers or synthetic environments as acceptance. Authorization for other machines' interactive desktops must follow their own established availability. Preserve existing projects, outputs and settings; use a dedicated real acceptance project/output folder. Automated tests, if later permitted after implementation and live verification, remain subject to rust-ui/AGENTS.md isolation requirements.
- Acceptance uses real models, actual GPU inference and actual machines. Fake workers, source-checkout runs and historical SSH smoke results do not qualify. Server and a full client may co-locate; three distinct machines are not a product requirement. At least two distinct participating machines must actually exchange work, including installed Linux and Windows receivers.
- Only shared storage is required; no file-transfer service, frame-splitting, cloud system or inbound client SSH dependency. Existing local features and interactive operations remain available.
- Bound observations by phase, not an invented universal five-minute no-frame timeout. Before each run record startup/model-load, inference, publication, export and shutdown budgets derived from the chosen clip/model and available historical timings. Check logs, worker/process state, coordinator events and outputs at most 60 seconds apart; no frames during model load alone is not failure. At a budget overrun collect evidence, report failure/inconclusive and use graceful cancellation; never infer a stopped writer from elapsed time or force-release ownership.

## 1. Resolve deployment prerequisites from existing records

Depends on: renewed execution authorization before any machine access or run. This inventory is repository evidence, **not a current connectivity or installation check**.

| Role/candidate | Repository evidence | Still to establish before deployment |
| --- | --- | --- |
| Current Linux build/client machine | `local-workflow-acceptance.md`: RTX 4090; launcher `~/.local/opt/CorridorKey/bin/corridorkey`; older installer under `~/.cache/ck-dist-ready-1023/` | Current identity, chosen server role, execution account, usable private GUI session and current package/model identity |
| Additional Linux client | `remote-validation.md`: `mustafar-omarchy`, `npittas@100.126.185.76`; RTX 5090 GPU 0 and RTX 3050 GPU 1; `/mnt/Projects` mapped to `//100.122.195.120/Projects`, historically proven same share as `//192.168.100.10/Projects` | Access still authorized/usable, full installer deployment (old `~/ck-linux-smoke` is not one), selected GPU UUID, current mount rights and private session |
| Windows client | `remote-validation.md`: `Kamino-3`, `nick@192.168.100.152`, RTX 4090; `local-workflow-acceptance.md`: installed `C:\Users\Nick\AppData\Local\Programs\CorridorKey`, launcher `bin\corridorkey.cmd`, installer set under `C:\Users\Nick\ck-local-build-1023\dist4` | Correlate installation with host/current build; execution account, private GUI session, shared-storage authentication and accessible drive/UNC root. Old `\\192.168.100.10\Projects` access was unresolved |

- [ ] Reconcile these records with repository deployment/configuration artifacts before asking the user for missing facts. Historical SSH addresses are discovery/deployment leads only, not the new execution transport. Do not reuse old sockets, assume credentials still work or convert old SSH maps automatically.
- [ ] Record the chosen server installation, advertised API hostname/address, bind/interface/port, trusted invitation delivery, allowed peers and exact firewall requirement. Select local-disk coordinator state storage, never SQLite-on-NAS. No listener/firewall change on ordinary local startup.
- [ ] Resolve the intended server and receiving lifecycle: app-open, login startup or boot service. Existing UI receiving is app-open; manual-only server operation and boot services are neither silently imposed nor silently excluded. Record supported lifecycle, account/storage/model access, startup/shutdown/update behavior and any product decision needing confirmation.
- [ ] Fill each installation's named shared roots, verified common storage identity, source/hint/output/manifest access, local scratch/model roots and GPU choice. Windows share authentication blocks acceptance until the actual execution account can access it; never guess passwords.
- [ ] Select owned, non-user-media acceptance clips: a short self-contained sequence for Key, a multi-shot representative-resolution Hint → Key → Deliver case, and a movie/imported-hint case for reference handling. Record exact range, outputs, model hashes and new output folders before runs.
- [ ] Write the machine-specific build/install/launch/pair/run procedure, observation budgets and evidence destinations. Ask only for fields still absent after this inventory; unavailable machines or private sessions block installed acceptance, not this plan edit.

Deliverable/done: a filled deployment/run sheet with actual hosts, accounts, lifecycle decision, mappings and commands, or an explicit list of unresolved fields. No claim of buildability or live access from document inspection.

## 2. Remove frame-loop dependence from all execution advancement

Depends on: lifecycle decision in §1. Code: `rust-ui/src/ui/app.rs` (`ai_tick`, `net_tick`, close handling), `ui/ai.rs` (`tick`, target changes, batch resolution, task-end handling, network submission), `scheduler.rs`, `remote.rs`, `worker.rs`, `netlink.rs`, `metrics.rs`.

Verified gap: App update calls both ticks. `AiUi::tick` invokes `Scheduler::tick`, which drains worker messages, polls preflight, dispatches Key/tasks and pumps locks. `Remote::tick` drives claims, start acknowledgements, progress, cancel and terminal reporting. Moving only network polling while sending scheduling requests back through App does not fix this. Worker stdout reader threads do not perform those transitions. Whether compositor suppression caused the reported stall is still unproven; a running worker may publish frames even while controller processing stalls.

- [ ] Establish a single non-render-driven owner/driver for scheduler and network execution state. Select the smallest ownership/channel arrangement after tracing the listed callers; Fable's shared scheduler plus one 100 ms thread is a candidate, not a mandated design. A repaint request alone is not an execution clock.
- [ ] Move every required advancement onto that driver: target application/worker startup, pending local/network submissions, preflight results, slot reservation, dispatch/dependencies, worker messages, publication/completion bookkeeping, hint locks, cancellation, ownership persistence, progress/terminal outbox retries, reconnect and telemetry forwarding. No transition needed to finish a queued workflow may await a UI frame.
- [ ] Keep egui, texture/preview updates and visible clip/manifest refresh on the UI thread. Deliver snapshots/events to it; UI refresh may wait for visibility, but execution and authoritative state may not. Preserve task-end effects currently inside `AiUi::tick`, rather than moving only `sched.tick()`.
- [ ] Serialize local Queue and receiver claims through the same physical-GPU ownership path. Cover the interval between claim, preflight and coordinator start acknowledgement; recheck/reserve before start so a newly queued local job cannot race it. Do not create a second executor.
- [ ] Stop new claims when receiving is disabled or shutdown begins. Honor the chosen lifecycle and existing graceful-close behavior; drain cancellation/terminal state, retain unresolved ownership and stop/join owned bridge/controller processes without relying on redraws.

Deliverable/done: all advancement paths have a non-UI driver and one GPU owner; installed covered/minimized acceptance in §8 proves behavior. No runtime stall diagnosis is claimed yet.

## 3. Complete settings resolution before submission integration

Depends on: existing local persistence contracts. Code: `presets.rs`, `shot_domains.rs`, `pipeline.rs`, `workflow.rs`, `ui/ai.rs`, `ui/app.rs`, `ui/app/hint.rs`, `ui/app/project_shell.rs` under `rust-ui/src/`.

Already present, awaiting installed verification: per-field route/delivery override markers, Batch refresh queue, merged delivery writes with markers/preset base, route persistence helpers and failed-delivery-save handling. Do not relabel these as missing solely because an earlier plan listed them.

- [ ] Trace preset base → marked shot fields → permitted adaptation for Key, Hint and Deliver; use the same resolved object for confirmation, local Queue and frozen network submission. Preserve effective legacy values and source-bound range/colour/annotations/hint references/destinations.
- [ ] Complete only uncovered callers: refresh in-memory clips after Batch Use preset values and queue-time writes; atomically save values with their override/provenance markers; advance saved baselines only on success and preserve failed edits for retry.
- [ ] Retain override indication, Reset to preset and Use preset values. Neither navigating Back nor merely selecting a Batch preset clears a shot override.

Deliverable/done: a consistent snapshot/persistence path for all three stages. Installed UI evidence in §8 must show Temporal Window Off survives Back/save/reopen/different Batch preset, unmodified shots inherit, explicit resets work and post-submit edits do not change queued settings.

## 4. Finish installed server, pairing, mappings and monitoring

Depends on: §1 lifecycle and §2 driver. Code: `backend/coordinator/{__main__,pki,server,store,schema}.py`, `backend/netlink.py`, `backend/system_metrics.py`, `backend/worker/gpu_identity.py`; `rust-ui/src/{remote,netlink,metrics,shared_paths,settings}.rs`, `ui/{settings,clients,metrics_bar}.rs`, `ui/app.rs`; `packaging/{bundle,lifecycle}.py`, Windows installer template.

Already present: invitation pairing/certificate verification; private credentials; UUID probing/registration; claim/start/event/cancel/stats API; per-installation mappings; Clients UI and telemetry helpers. These need integration/acceptance, not rebuilding. The coordinator README's “not wired” heading is older than the current `remote.rs` wiring.

- [ ] Provide a supported installed coordinator/server-mode entry point using the bundled interpreter/helper tree and persistent local user-data state. Existing lifecycle shims expose only app/setup; choose app integration or lifecycle-managed launcher consistent with §1. Cover Linux/Windows startup, shutdown, update/rollback and uninstall preservation; no checkout, system Python or remote Python configuration required.
- [ ] Keep URL/certificate in the invitation/credential flow. Do not add a duplicate endpoint setting or put tokens in settings/projects. Existing settings may store the credential **path**, not the secret; default is `network/credentials.json` beside settings. Verify installed platform paths and native Windows private-file ACL behavior; preserve identity across reinstall/update.
- [ ] Carry verified rendering GPU UUID (respect device selection), application/protocol compatibility and actual operation/model capabilities through registration/claims. Show why registration or receiving is unavailable. Multiple addresses must not create duplicate GPU slots.
- [ ] Finish discoverable named Path Mapping Add/Remove/Browse/preview/access-test behavior using `shared_paths.rs` and existing settings persistence. Keep old SSH maps untouched. Reject ambiguous/missing/escaping references, use local platform path semantics and app-owned temporary access probes; never modify user assets during probing.
- [ ] Keep background CPU/RAM/GPU/VRAM samples visible on Board, workspace, Settings and Clients, including idle clients and selected/per-GPU identity. Preserve independent unavailable/stale states, sample age and authenticated own-session reporting; counters are not GPU reservations.
- [ ] Verify bundle contents once: `bundle.py` already copies the whole backend tree, including these modules. Add packaging entries only for actual new entry points/assets, not redundant module copies.

Deliverable/done: opt-in installed server/client configuration and complete monitoring/mapping UI. Pair/reopen/reinstall, real UUID and idle/stale telemetry observations belong to §8.

## 5. Finish the frozen Key path and hardware eligibility

Depends on: §§2–4. Code: `rust-ui/src/submission.rs`, `shared_paths.rs`, `ui/ai.rs` (Batch confirmation/outbox), `remote.rs`, `scheduler.rs`; coordinator `schema.py`, `store.py`, `server.py`; `backend/v2_temporal.py`, `backend/corridorkey_v2.py` and existing worker preflight.

Already present: Key-only `JobSpec`, logical references, receiver `submission::preflight`, stable submission/job/attempt identifiers, persisted submitted-job/event state, scheduler submission and coordinator progress/cancel handling. Current claim filtering considers kinds/maps/model identities, not a complete hardware policy. The backend's over-budget fixed-window estimate currently warns; it is not proof of a placement exclusion policy.

- [ ] Freeze complete confirmed settings, ranges, outputs, hint selection, required model identities and explicit fixed/adaptive intent. Display receiver/Automatic semantics and resolved destination before Queue; retain immutable submission IDs for retries and visible rejection reasons.
- [ ] Encode every reference with sender mappings and resolve with receiver mappings before execution. Reuse receiver preflight for source/hint availability, model identity, range identity, no-clobber output/manifest and actual write capability. Return the offending reference/path and reason to the submitter.
- [ ] Complete hardware-dependent control labels and eligibility for explicit selection and Automatic: reuse documented model/runtime constraints and temporal estimates, record actual effective adaptation, and never silently change a fixed window/model/resolution/workflow. Define policy for estimates versus hard incompatibility before turning warnings into exclusions. Unsupported candidates need visible reasons, not just `no_eligible_job`.
- [ ] Reserve the local slot before committing start; persist acknowledged ownership successfully before worker launch. Current `remote::write_private` discards I/O errors, so an attempt-file call is not sufficient durability. Surface persistence failure and leave the acknowledged attempt fenced rather than launch without recoverable ownership.
- [ ] Run the preflighted Key through the existing scheduler/worker; forward authoritative progress/publication/terminal results, update the submitter without page reopening and expose the resulting manifest/output location. Preserve idempotent submission and ordered event replay.

Deliverable/done: complete installed Key path and enforceable, visible eligibility; real local/received jobs and local-versus-network contention are observed in §8, not simulated here.

## 6. Implement full Hint and Deliver routing, not a Key-only release

Depends on: §§2–5; complete both gates before live testing begins. Code: `rust-ui/src/submission.rs`, `pipeline.rs`, `tasks.rs`, `hints.rs`, `ui/ai.rs`, `ui/ai/tasks.rs`, `remote.rs`, `scheduler.rs`; coordinator `schema.py`, `store.py`, `server.py`; existing `backend/worker` dispatch and `backend/service/task_job.py`/hint/export services.

Verified gap: `JobSpec::validate` accepts only Key, receiver claims advertise only Key, and coordinator validation/storage filtering are Key-specific. Remote preflight scans self-contained clip `Frames`/`Input` and `AlphaHint`; it refuses movies/external source metadata. It does not encode arbitrary external hint references. This is broader than a missing UI button and narrower than proof that every external-hint case currently produces a specific refusal.

### Hint implementation gate

- [ ] Extend the frozen/versioned envelope and server validation/storage/capability filtering with supported saved Hint routes, exact settings/models, required annotations/keyframes, logical inputs/outputs and dependency identities. Define compatibility/refusal for older Key-only peers and stored records; do not silently reinterpret them.
- [ ] Resolve source/hint/annotation references at the receiver, then reuse existing Hint task construction, locking and publication. Queue Key only after authoritative successful Hint completion; propagate stage failure/cancel and preserve local interactive authoring where required.
- [ ] Handle mapped movie/external source and imported-hint workflows through existing normalization/import paths or explicit receiver-resolved references, never sender absolute paths buried in `clip.json`. Keep precise refusal naming the unsupported source/reference until its route is implemented. The first self-contained Key case is an intermediate restriction, not permission to remove product-supported shared-source/hint workflows from final scope.

### Deliver implementation gate

- [ ] Extend the same envelope, validation, preflight and receiver plan with frozen delivery products/container/settings, source manifests, ranges and mapped destinations. Reuse existing export tasks and atomic/no-clobber publication, not a separate render engine.
- [ ] Connect Hint → Key → Deliver with existing dependency semantics and per-stage progress/results/cancellation. Make one-shot, selected-shot and all-shot Batch use the same builder. Explicit receiver and Automatic must evaluate the whole workflow; retain whole-shot placement rather than distributing frames.
- [ ] Remove Key-only UI claims once support exists. Explain exclusions for unsupported operations/hardware without silently dropping Hint/Deliver or changing saved settings.

Deliverable/done: both stage contracts, receiver execution and whole-workflow placement are implemented. Separate actual Hint and Deliver evidence plus a combined multi-machine pipeline are mandatory in §8.

## 7. Complete job control and recovery using existing ownership rules

Depends on: §§2–6. Code: `rust-ui/src/remote.rs` persisted submission/cursor/attempt files and owned-attempt state, `scheduler.rs` worker-stop/lost-writer handling, `worker.rs`, coordinator `store.py` and `__main__.py`, existing manifest/lock recovery and lifecycle code.

Already present: cancellation requests, event sequence/idempotency, unresolved-owned-attempt UI and operator `abandon`. `_operate` uses the same owner lock as `serve`, so operator commands refuse while the server holds it. `abandon` marks failure; it neither stops a writer, unlocks storage nor automatically requeues.

- [ ] Complete queued cancellation and active worker cancellation across all stages. Show cancel-requested until authoritative terminal/stop evidence; no duplicate terminal or premature slot release.
- [ ] Reconcile submitter close/reopen from stable submission/job IDs and persisted cursor. Reconcile receiver reopen against owned attempt and actual worker/manifest state; unresolved writers stay held, not silently relaunched or forgotten. Honor graceful-close choices and surface interrupted work explicitly.
- [ ] Restore coordinator durable queue/ownership after restart; reconnect/replay without duplicate launch/publication. Complete network resume using existing local manifest/resume semantics and explicit ownership/recovery decisions, since current network schema accepts only `new`.
- [ ] Reuse operator `abandon` for confirmed lost attempts: check machine/writer state, stop server, resolve its record, restart server, then reconcile local ownership/storage through existing recovery rules. Do not turn local “forget” into authorization to overwrite/reassign.
- [ ] Preserve fences and visible failure on NAS loss, worker restart and output collision. Implement only required recovery for these ordinary contract scenarios; no speculative fault catalogue.

Deliverable/done: recovery paths exist for the live scenarios below; disconnect never means a writer stopped and completed outputs are never overwritten implicitly.

## 8. Build, install and live-test the completed feature

Depends on: implementation §§2–7 complete; §1 environment facts/access and execution authorization resolved. No automated regression tests yet. Installation's built-in integrity/launch checks are packaging behavior, not acceptance evidence for network/rendering.

- [ ] Build/package natively using `packaging/README.md`: Linux `packaging/linux/build-ui-old-glibc.sh`, pinned FFmpeg, then `python packaging/bundle.py linux --out <dir> --ffmpeg-dir <bin> --ui-binary rust-ui/target-oldglibc/x86_64-unknown-linux-gnu/release/corridorkey-ui`; Windows `python packaging/bundle.py windows --out <dir> --ffmpeg-dir <bin> --build-ui`. Resolve actual paths/tool availability before execution, and record source/binary/installer/runtime/catalog hashes. Keep Windows installer `.exe` and payload `.bin` together.
- [ ] Install full packages through normal installers on the chosen Linux and Windows clients, not old worker checkouts. Preserve existing installations/data; use owned acceptance projects/output destinations. Launch from outside the checkout with bundled runtime and no system-Python dependence.
- [ ] Perform real local import/Hint/Key/Deliver on each installed OS, including model readiness, saved shot state and actual output inspection. Historical acceptance narrows investigation but does not pass the current candidate. Record any failure, not a blanket pass.
- [ ] Start the selected installed server mode; pair both clients from trusted invitations and reopen to reconnect. Compare reported GPU UUID against actual selected-device identity (for example `nvidia-smi -L`), verify mapped storage with an owned marker and inspect live/idle/unavailable/stale monitoring on all required pages.
- [ ] Run first real received Key on the selected Linux receiver, then Windows. Record submitting/receiving installations, job/attempt IDs, frozen/effective settings, manifests, frame count/readability and final UI state. Queue local work while a network offer is pending and observe at most one active job per physical GPU.
- [ ] In a safe private compositor session, receive a job while covered, and cover a running job through subsequent dispatch/stage completion. Observe worker publications **and** coordinator event/terminal advancement without uncovering. Use a workload spanning at least two minutes where practical; a shorter completed run proves only its observed interval. Exercise minimized separately where supported; record unsupported compositor behavior rather than equating it with covered. Inspect logs/driver progress to establish or reject the original stall diagnosis.
- [ ] Exercise installed shot-settings sequences from §3, including confirmed queue values, reset actions and a post-submit edit. Use only private-session GUI interaction, never the live desktop.
- [ ] Hint gate: generate a real supported remote Hint on each OS, inspect its masks/manifest and the dependent Key's actual use of it. Exercise the selected movie/imported-hint route through the implemented normalization/reference path. Precise refusal of an unmapped source is required, but is not a passing full pipeline.
- [ ] Deliver gate: export actual remote Key outputs on each OS, inspect container/product/range and readable final media. Run one-shot, selected and all-shot submissions; run a representative-resolution multi-shot Batch using both Linux and Windows receivers with all Hint → Key → Deliver stages observed. Exercise explicit placement and Automatic. Include fixed Off preservation and a demonstrably incompatible candidate with an exclusion reason; do not invent an unsupported Temporal Window or deliberately exhaust VRAM to manufacture that case.
- [ ] Exercise queued cancel, running cancel, interrupted-job resume and submitter close/reopen during a live job: the same job/attempt returns, with no resubmission. Exercise receiver “Stop work, then quit” and reopen: preserved partial results plus authoritative cancelled/held state, not duplicate execution. For an abrupt receiver/worker restart, any unproven writer remains held until resolved.
- [ ] Exercise a reversible API interruption: stop only the owned acceptance coordinator for 30 seconds while a receiver is rendering, then restart with the same state directory. Record that this combines connection loss and server restart; it does not prove cable/NAS failure. Existing ownership survives, buffered events reconcile and no second attempt starts. Avoid firewall/physical-cable manipulation.
- [ ] Exercise NAS loss using a dedicated acceptance mount/session only, with a recorded reversible disconnect/remount procedure for that OS; never unmount a production share. Observe visible failure/held ownership and safe recovery without overwrites. If no safe dedicated mount is available, record this acceptance prerequisite as blocked, not passed.
- [ ] Exercise the agreed lifecycle across receiver/coordinator reboot and installed update/rollback, then reconnect and inspect retained identities/jobs/settings/models/projects/outputs. Verify uninstall preservation on the owned acceptance installation, including credentials/state outside version directories. Do not reboot a working user machine without authorization.

Deliverable/done: per-scenario live pass/failure records, exact installed artifacts, machine/config identities, logs and inspected outputs. Missing access, unsupported private session or a failed pipeline is a concrete incomplete gate. Record live failures before the whole-feature review; automated substitutes cannot turn them into passes.

## 9. Whole-feature review, corrections and final delivery

Depends on: completed implementation and §8 actual live attempts with pass/failure evidence. No source-only review in lieu of an unavailable live attempt, and no per-fix review loop.

- [ ] Review the entire feature and connected callers once as a whole: settings/snapshots, execution ownership, installed lifecycle, authentication/credentials, mappings, hardware placement, telemetry, all stages and recovery. Include observed failures and exact source/artifact identities; no production/security claim from this narrow plan review.
- [ ] Fix concrete implementation/review defects, then repeat affected actual installed workflows. If fixes change packages, regenerate hashes and validate the final package pair with real local and cross-machine full-workflow runs, not an older successful binary.
- [ ] Only after finished implementation and live testing, add/run focused regressions for observed behavior through `rust-ui/tools/isolated-test.sh`; rendered checks use the private-display workflow. Scheduler advancement without UI updates and unmapped-reference refusal are candidates, not mandatory speculative test expansion. Update affected existing fixtures only at this stage.
- [ ] Deliver installable Linux/Windows packages, exact install/server/pair/mapping/recovery instructions, final hashes, live evidence, whole-feature review outcome and explicitly unresolved prerequisites/failures. Broader rollout waits for installed pilot acceptance.

Done when: final full clients demonstrate actual local workflows on both OSes and actual orchestrated full workflows across machines, and the whole-feature review's blocking findings are resolved. An implemented but unverified feature remains unaccepted.

## Fable review disposition (source inspection only)

- **Confirmed:** both scheduler and network progress need a non-frame-loop driver; existing pairing/mappings/registration/Key/control code must not be described as missing; invitation-based credentials should not acquire a duplicate endpoint setting; packaging already copies backend; installed server entry point/lifecycle needs completion; settings precede submission; Hint/Deliver require schema plus receiver changes; recovery should reuse `abandon`; dirty-tree evidence needs hashes.
- **Rejected as an inventory conclusion:** no named additional Linux machine. `remote-validation.md` names `mustafar-omarchy` and `Kamino-3` with GPUs, historical access and storage details. Those are leads, not current installed-client acceptance. Three distinct machines are not required.
- **Qualified:** compositor stall causation is not live-proven; a shared scheduler/100 ms thread is one design option; manual-only server scope is not agreed; no fixed five-minute output timeout; external hints are not represented generally, rather than proven universally rejected by a dedicated check; hardware incompatibility must follow real constraints, not an invented unsupported window. Several proposed shot-state repairs already exist in the dirty tree and need caller/installed verification.
- **Additional concrete gap found:** attempt persistence currently swallows write/rename failures before scheduler submission; §5 requires checked persistence before launching. Full hardware eligibility is also not established by recorded free-memory samples or backend warnings.
- Not audited here: complete coordinator transactions, PKI correctness, Windows ACL implementation or runtime behavior. No builds, tests, apps, remote access or GPU work were performed for this revision.
