# Stability validation: UI freeze (Linux 2.4.0-e73e040202)

## Baseline measured (installed Linux build, read-only)

- Idle network service (`backend.network_service`): about 0.2 CPU jiffies/s, 34 threads, no growth over 10 s.
- One `discover` op through the real bridge: 7.2 s wall, up to 64 threads in the bridge, exit clean, no stderr.
- App start with 4 saved networks: 4 persistent bridges. Clients opening starts exactly one extra bridge (the scan) that
  ends after about 8 s; ReScan/Cancel/ReScan never ran two scans at once. There is **no continuous scanning**.
- Claim polling: 4 networks claim about every 3 s each (about 40 log lines per 30 s), constant, bounded.
- Typing in Path mapping (mount path, then another field) responded normally. Idle CPU about 2 %.

## Reproduced freeze and root cause

After a ReScan/Cancel/ReScan + Settings sequence the window stopped repainting and stopped taking input. `/proc` showed
the UI main thread and the `ck-driver` thread both in `futex_wait`, and the `[net ...]` claim log stopped at the same
second and never advanced (90+ s): a deadlock, not scanning, GPU or blocking I/O.

Cause (`rust-ui/src/ui/app.rs`): the driver thread locks `ai -> remote -> metrics` every 100 ms. The UI's `frame()`
takes the same three in that order and releases them. `App::logic()`, which runs between frames, called
`net_tick`, which read `self.remote` first. `Held` locks lazily on first use and `logic()` never released, so the UI kept
`remote` until the next frame's `ai.hold()`. If the driver took `ai` in between, the UI waited for `ai` and the driver for
`remote`. Frequent redraws (Settings, Clients, typing, scans) made that window likely, which matches "freezes constantly".

## Fix

`net_logic` takes `ai`, `remote`, `metrics` in the driver's order and always releases them. No scan, claim or retry
behaviour changed. Test: `ui::app::lock_order_tests::logic_never_keeps_a_shared_lock_between_frames` (runs a real
driver thread; fails with the release removed, passes with it).

## Exit evidence

- `bash rust-ui/tools/isolated-test.sh /usr/bin/cargo test --offline --lib`: 181 passed.
- `--test netlink_process --test settings --test path_mapping`: 13, 4, 9 passed.
- Installed e73e040202 was only observed, not patched.

## Installed-candidate verification (S3/S4)

Candidate carries the lock fix: Linux `2.4.0-defb03b29b`, Windows `2.4.0-6ddc7fc93c` (canonical build results trusted;
transfer copies verified by sha256, installer exe `3719a13a...30bb`, slices match).

**Endor (Linux, existing prefix `~/.local/opt/CorridorKey`, upgraded from e73e040202):**
- Installer `--models skip --firewall skip`: files installed, active `2.4.0-defb03b29b`, settings/certs hashes identical to
  the pre-install private backup. The installer exit code was 3 (models not provisioned because they were skipped; the
  version was installed and activated). It also reported "added 4 UFW rule(s)" even with `--firewall skip`; not investigated.
- App ran 247 s+: startup 60 s, then Clients, ReScan/Cancel x3 (the original reproducer), Settings, typing in a mount path
  field and then a second field, rapid Clients/Settings switching, the 57-clip test project `/mnt/Kessel/GreenScreens` opened (Kessel test writes are allowed; this is not the
  production Projects share). UI stayed responsive;
  claim log kept advancing (about 1.4/s throughout); 4 bridges, 28 threads, about 1.4 % CPU, no `futex_wait` mutual stall.
- Only settings change: scan address-order refresh (designed). Typed text was cancelled, not saved.

**Kamino1 (Windows, `%LOCALAPPDATA%\Programs\CorridorKey`, upgraded from 6a4802b578):**
- Silent install `/TASKS="!models"`: exit 0, `current.txt` = `2.4.0-6ddc7fc93c`; settings hash unchanged by install.
- Driven in the real console session (scheduled interactive task + screenshots): 70 s startup sampled every second
  (Responding true throughout, 0 false samples), Clients, ReScan/Cancel x3, Settings, typing in two path-mapping fields,
  Back/Models/Settings switching. Input landed (screenshots), app ran 415 s+, zero new AppHang/Application Error events
  after the install. Only settings change: scan address-order refresh.
- Open observation: on Clients a message "Could not read the service status (internal: service-install.json is
  unreadable; it was left untouched)" was shown. Not a hang; not investigated or changed.
- Windows Firewall showed a "Python" allow prompt for the new prefix's python; left unanswered (no firewall changes).

**Private evidence:** Endor backup `~/.local/share/keyloom-stab-backup-164700`; Kamino1 backup
`C:\Users\Nick\kb-backup-164700` (settings copy, receipts, `gui.log`, screenshots); local logs `/tmp/ck-stab`.
All test processes stopped, scheduled task removed. No renders, downloads, project edits or firewall changes.
No project or clip edits were observed in `/mnt/Kessel/GreenScreens`, but that is not proof of no writes:
`.ck_queue.json` (the app's queue history file) has mtime 16:59:44, inside the open/shutdown window. `project.json`
(15:29:14), `Exports` (15:29:34) and `clips` (10-23) predate the test and nothing under `clips` (depth 2) is newer than
16:50. There was no pre-test snapshot, so whether `.ck_queue.json` content changed, and whether open or shutdown wrote it,
is unproven.

## Remaining limits

- Windows root cause stays plausible, not proven (no stack from the original AppHang); acceptance is responsiveness
  with the same interaction, which passed for about 7 minutes.
- `Link::drop` (kill, wait, thread join) still runs on the UI thread; no evidence it blocks.
- Not tested: long soak, large projects on Windows, other PCs.
