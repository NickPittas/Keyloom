# Installed network client observations — 2026-10-24

## Current interaction contract

Batch is the dispatch and observation surface. Its queue groups work by PC and shows each PC's numeric CPU, RAM, GPU and VRAM measurements as compact bars. Every submitted workflow stage has its own progress bar. Cancellation is available in this queue. Clients is a compact availability table with connection/setup controls collapsed below it.

Board, Key and Batch share the existing persisted per-clip assignment. Paired network installations use `network:<installation_id>` in that field; their immutable workflow goes through the coordinator rather than the legacy worker scheduler. Batch defaults to each shot's assignment and retains an explicit whole-batch network override. Offline assignments remain saved. New results only are supported on network clients; Resume/Replace remain local.

Remote stages use the existing ordered note/progress event stream. A next-stage announcement proves preceding dependency stages completed; no percentage is invented for an unknown total. Actual scheduler progress is reported for Hint and Deliver as well as Key. Periodic empty event requests also check cancellation when a stage has no new progress. Recent terminal queue history is saved with its installation, stage results and event cursor. Event snapshots and cursors are saved together, and terminal event pages are drained before polling stops.

## Installed artifacts

- Endor and Mustafar: `2.1.5-6d65d788da`, old-glibc Linux build, measured glibc floor 2.30.
- Kamino: `2.1.5-4b436bd1d2`, native Windows build.
- Payloads passed the existing manifest and launcher validation and were activated through the packaged lifecycle. Existing models were retained; no new model downloads were needed. Latest artifacts were installed as verified payloads rather than recompressing another full Windows installer.

## Targeted live evidence

Real installed desktop sessions and GPUs were used. The dedicated acceptance project is `/mnt/Kessel/CorridorKey_Client_Test_20261023`; Kamino resolves the same shared location through its actual interactive `R:\` drive. Since the user's storage restriction, Projects has only been read.

- A real 40-frame Windows Key job completed with a complete manifest and 80 EXRs. The submitting app displayed 40/40 and refreshed its shot card.
- A real 40-frame GVM → MatAnyone2 → Key → matte H.264 and processed ProRes workflow completed on Kamino. Both delivered videos report 40 frames at 1920×1080.
- Distinct per-clip assignments dispatched to Kamino's RTX 4090 and Mustafar's RTX 5090 and survived reopening the submitting app.
- A coordinator outage lasting approximately 147 seconds recovered without manually restarting the then-running clients. This does not independently prove the prior review's bridge-restart-cap scenario.
- With the new Batch UI, `job_2a734b57d48a489cab01e09eadd23413` showed real Mustafar GPU load and bounded Key progress in Batch. Cancel was clicked there while the job was started; the receiver reported `cancelled`, `Key stage cancelled`.
- `job_9e52ef4e5b3440fdbb6c213edf0130ab` used the real 0–9 range on Kamino. Its five stage bars completed in Batch, beside Mustafar's cancelled job. Real PC resource bars were visible on the same page. The shortened range limits repeated render work to the changed queue behavior.
- Reopening the final installed UI retained both terminal jobs, their stage results and per-PC grouping. Painted status dots replaced unavailable font glyphs. No additional render was submitted for this last display check.

Local private evidence, including source hashes, artifact identities and job identifiers, is recorded in `~/.cache/ck-network-live-20261023/evidence/batch-queue-20261024.json`.

## Final code review

Opus 5.5 medium reported no remaining normal-user blocker in a read-only whole-client review after the live checks. Its medium queue-history finding was addressed by saving the event cursor together with stage data and draining terminal event pages. The installed submitting app then saved the real terminal cursors 124 and 154 while retaining the cancelled and succeeded stage views; no additional GPU work was submitted. The review did not independently repeat live checks or establish security/release acceptance.

## Limits

No automated tests were created or run, and no full approved-app walkthrough was repeated. Receiver hard-kill/remaining-writer recovery, offline held-attempt shutdown and an adversarial lost-submission-reply reproduction remain unverified live. The final Stopping label and cancellation heartbeat compiled on both platforms; the already-proven GPU cancellation was not repeated after that small polish. This record is targeted client/UI evidence, not a complete release or security acceptance claim.
