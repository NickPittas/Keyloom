# Workflow revision review: `selected-workflow.html`

Reviewed 2026-10-22 against the user's latest brief, `selected-design.md` and `range-output-contracts.md` §1–6.
Scope: is the design prototype correct. Production parity is out of scope. Already deferred, so not raised: real FS/NAS checks, real SAM, Output location preset manager, Replace cleanup, pan/zoom.

How this was checked: I read the source. I reproduced every finding below in headless Chromium, using a throwaway copy in `/tmp` that exposes the closure (`ACT`, `clip`, `primary`, `locOf`…). The repo file was not edited. The built-in self-check currently reports **249/249 at 1440 and 252/252 at 728**. `selected-selfcheck.md` says 248/251, so the doc is off by one.

## P1: output version and location identity (one root cause)

`c.written` and `c.made` describe the files at **`c.made.loc`**, but the code treats them as if they describe the **current** path. Nothing outside `nextVer` keeps the two in step, and the earlier versions in `c.prev` are never checked for collisions.

**P1-a: Range-change block ignores where the job will write.** `rangeStale` (line 375) only compares `made.rng` with the current range, so it ignores version, root and template. Repro (SH060, Complete 1001–1640 v001):
- Set In 1101. Primary shows "Write as v002". Correct.
- Type 2 in the Version field. Primary now shows **"Write as v003"**, and Key into the already-new v002 folder stays blocked.
- Switch to the project root. Still "Write as v003".

The contract (§3) blocks re-keying a stale clip into the **same resolved folder** only. Fix: block only when `locOf(c).key===c.made.loc`.

**P1-b: Going back to an earlier version overwrites it silently.** `nextVer` (lines 951–956) moves the old result into `c.prev` and clears `written`. After that, nothing refers to `prev`. Repro (SH060):
- Set In 1101, then Write as v002.
- Type 1 in the Version field. Primary shows **"Key range 1101–1640 (540 frames)"** with no block. The path is `D:/delivery/crane/SH060_wide_crane_v001/…`, which is where v001 (1001–1640) still lives.

The UI says "Earlier versions stay on disk". The contract (§4 Collisions) says a same-clip folder whose manifest doesn't match must be blocked, or go through §3. "Next free version" also skips the `prev` versions only by luck: its loop (line 952) never looks at `prev`. Fix: put `{ver, loc}` in each `prev` entry, and make `outCheck` / `nextVer` treat those paths as taken.

**P1-c: Clearing the custom root silently switches the clip to the project root.** `INP.croot` (line 998) stores `''`. `locOf` then falls back with `c.outRoot||p.root`, and `custom=!!c.outRoot` (line 717) flips the selector to "Project root" in the middle of typing. Repro: SH060 Deliver, select the Custom root text and delete it. The paths become `/mnt/NAS/Project/Masks/SH060_wide_crane_v001/…` and the field disappears. This breaks "custom folder kept unless explicit Use project root N selected…". Fix: keep `''` as a custom value, show the error "root is empty" (`checkRoot` already does this), and leave the root mode alone.

## P2

**1. Viewer shows results for outputs that are off.** `sceneSVG` (line 570) uses `have=covers(c.written,frame)` for every output channel. Repro: SH060 (Processed off), Processed channel, frame 1100. It draws a result image instead of "No file on disk". The same happens for any output turned on after the run. Fix: also require `c.outs[mode]` (or the outputs recorded in `made`).

**2. A `#` in a clip name breaks the frame pattern.** `resolveOut` (line 356) replaces the first `#+` in the **substituted** file name, and `safe()` doesn't escape `#`. Repro: name `SH#40` gives `…/SH#40_v001/FG/SH110140_####.exr`. The frame number goes into the name, the literal `####` stays, and collision checks then compare the wrong patterns. Fix: replace `####` before substituting tokens, or turn `#` in token values into `_`.

**3. "Next free version" while a Key job is running mislabels the files.** Repro: SH040 Key done, Key again, `nextVer` while it runs, then the job finishes. The clip is v002 with `written=[[1101,1300]]`, `prev=[v001]`, and stale=true. The frames shown as written are actually in the frozen v001 folder. Fix: disable `nextVer` / `cver` / location edits while a job for this clip is active (as `primary` already does), or record the result against `j.loc` and not the clip.

**4. Stroke prerequisite only counts strokes, not both kinds.** `strokesIn(c)<2` (lines 378, 467). Two **subject** strokes satisfy VideoMaMa or MatAnyone2 (repro: readiness moves on to "Needs hint — generate VideoMaMa hint first"). The text promises "subject and background strokes". Fix: require at least one stroke with `t===1` and one with `t===2` inside the range.

**5. Marks are invisible on the Hint channel when there is no hint yet, but clicks still add them.** `sceneSVG` returns "No hint generated yet" before `annSVG` runs (line 571). Repro: SH050, Hint channel, Point+, click. A point is added but not drawn (checked: the SVG has no circle). Fix: draw `annSVG` on the placeholder too, or switch the annotation tools to Input.

**6. Single-key shortcuts don't fire while a button has focus.** `if(interactive)return;` (line 1051) comes before I/O/M/K/←/→. Repro: click Next frame (the button keeps focus), then press `I`. Nothing happens. The brief makes I/O the main range action, and the parent is doing native keyboard QA, so please confirm there. Fix: only let Space/Enter through to focused buttons.

## P3

- **Requeue promises a full Key that is always refused.** For an outdated Resume after a range change, the button says "Requeue as full Key — rewrites every selected frame" (line 635). The new Key is then cancelled at `gate`, because `rangeStale` gives "write as vNNN". Offer "Write as vNNN" in the row instead.
- **Requeuing an outdated hint job orphans its dependent key.** In `requeue` (line 941) the old hint becomes `cancelled`. The `genThenKey` key job then fails with "its hint job did not finish (cancelled)" and is not re-linked to the new hint.
- **Clicks on letterbox bars become edge marks.** `svgPt` (line 1008) clamps points outside the 640×360 image to the edge, so they become marks on the border. Dropping them would be more honest. Otherwise the letterbox mapping maths is correct.
- **Hint quality and strokes are not part of hint identity.** Changing hint quality or strokes after generating a hint doesn't mark the hint stale (`hintOk`, line 407). This is acceptable if intended, but it should be written down.

## Verified correct (no action)

- **Shared scrubber and frame:**
  - Media, Hint and Review use the same `viewer()`: channel selector top-right, A/B centred, transport icons centred.
  - Key and Deliver show the same `scrub()` without an image.
  - `S.frame` is restored per clip and clamped.
- **Range identity everywhere:** summary, progress `/ j.w`, queue rows, the Resume label, graph badges, keyframes inside In/Out, and "Outside In/Out" on output channels. Labels are exactly `Set In (I)`, `Set Out (O)`, `Clear In/Out`.
- **Hint prerequisites:** route plus coverage, and "(range changed)". The hint→key dependency `wait` / cancel works. `gate` re-checks against the frozen config.
- **Completion and Resume:**
  - A run is Complete only for the recorded range.
  - Resume is blocked by any snapshot difference (range, compression, location).
  - Resume keeps the frozen date and version.
  - Cancelling mid-publish gives partial or mixed output, never Complete.
- **Queue:** the config is frozen, including resolved paths (`frozenLoc`). Edits show "Queued with earlier settings". Requeue takes a new snapshot. At dispatch, collisions are re-checked against other clips and active jobs.
- **Naming:**
  - Token set is closed.
  - `..`, `.`, empty segments and backslashes are rejected.
  - `/` in the file template is rejected, and exactly one `####` run is required.
  - Windows reserved names are rejected.
  - A clear error is shown when several outputs are enabled without `{output}`.
  - The Matte-only example resolves as specified.
- **Batch root apply:** a preview bar lists only the selected clips, and presets never carry the location.
- **Out of scope:** Batch Board → Page Tabs → fixed graph is kept. Bigger Hint preview size and narrow layout are a rendering question for the parent's T3 pass and were not measured here.
