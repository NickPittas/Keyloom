# Review round 1 — `proposals.html` (8 UI directions) and the staged Rust plan

Reviewer: Opus 5.5 (planner/reviewer role). Scope: design prototype fitness for **user selection**, not production readiness.
No proposal or production file was edited. This review does not choose a design or approve a rewrite.

Inputs read: `design-brief.md`, `proposals.html` (all 796 lines, HTML/CSS/JS), `proposals-selfcheck.md`, `exploration.md`,
`research.md` (rev 2), `rust-path.md`, plus `backend/service/core.py`, `backend/service/inference.py`, `ui/widgets/parameter_panel.py` to check the claims.

Measured myself: headless Chromium on a `/tmp` copy with a measuring script added (the original was not modified). At 1440×900 and 728×900:
self-check 88/88, no horizontal page overflow (`scrollWidth` 1425/713), plus where the primary action lands on the page (see B1).
Not done (parent's QA covers these): screenshots, real keyboard/screen-reader passes, T3 `html_render`.

---

## Verdict

**Not ready to show for selection yet. Two blockers to fix first, then one more short round.** The eight layouts really are different
(P1 3-column station, P2 step rail, P3 card grid + queue, P4 page tabs, P5 viewer-first + drawer, P6 ticket form, P7 multi-clip lanes,
P8 fixed DAG). The core wording is honest everywhere: "Key clip — AI processes all 412 frames", output range lives under Output,
stopping is cooperative, playback is labelled as on-disk. Hints are optional, the queue runs one job at a time, partial runs
stay "Ready · partial", Resume finishes the clip, stale results show a banner, and the colour choice is never switched automatically. The
problems are (a) how the app shell sits on a real screen, which hurts the very comparison the user needs to make, and (b) a few simulated
behaviours that contradict the backend in ways that would teach the user the wrong mental model.

---

## Blockers (fix before showing the user)

### B1 — The "app window" has no height limit. In 4 of 8 designs the primary action is below the fold at 1440×900
Evidence (measured, 1440×900, `getBoundingClientRect().top` of the primary button, page height):

| Design | Primary action y | Page height |
|---|---|---|
| P1 Shot Station | **Key clip @ 1166** | 1373 |
| P6 Job Ticket | **Key clip @ 1201** | 1357 |
| P7 Timeline Lanes | **Key clip @ 1316** | 1567 |
| P8 Node Graph | Key clip @ 569 | 1396 |
| P5 Review Room | Re-key… @ 208 | 1372 |
| P2 / P3 / P4 | 587 / 208 / (page-dependent) | 813 |

At 728 wide, P1's Key button is at 1983 px and P7's at 2121 px. Cause: `.win` (`proposals.html:36`) and the layout grids `.l1`–`.l8`
(`:126–136`) use auto row heights, so the page grows to fit the inspector. The viewer, timeline and job bar then scroll away with
everything else. That is document behaviour, not desktop-app behaviour. P1, the planner's top pick, is the worst case: it looks
"dense" because it is an unbounded scroll, not because of its layout. That biases the comparison against P1/P6/P7 and in favour of P3/P4.

Fix (fits the no-`vh` rule the self-check enforces):
- Give `.win` an explicit app height. Either add the **viewport switch the brief required** (brief §5: Desktop 1920×1080 / Laptop 1440×900 /
  Small 1280×720 / Fit, scaled with `transform`; it is missing, and `grep -i viewport` finds only the meta tag) or set
  `win.style.height = innerHeight - win.getBoundingClientRect().top` from JS on resize.
- Inside it: `grid-template-rows: auto minmax(0,1fr) auto` per layout. Give the side panels (`.ga-insp`, `.ga-side`, `.ga-props`, `.ga-ticket`, `.ga-q`)
  `overflow:auto`. Make the `runBtns()` block (`:351`) a `position:sticky; bottom:0` footer of its panel, so **Key clip / Cancel is always visible**.
- Add one self-check per design: the primary action's rect lies within `.win` at the Laptop preset.

### B2 — Keyboard users cannot toggle checkboxes: Space is captured as Play
`proposals.html:725–734`: `typing` deliberately leaves out `type==='checkbox'`, and `interactive` (`:727`) does not list checkboxes.
So Space on a focused checkbox reaches `if(k===' '){e.preventDefault();ACT.play()…}`. `preventDefault` on keydown stops the native toggle.
Affected controls: the FG/Matte/Comp/Processed output checkboxes (`secOutput`, `:429`) in P1/P2/P3/P4/P5/P6/P7/P8, and the P3 card
selection (`:491`), which is the main multi-select control in Batch Board. `K`/`I`/`O` on a focused checkbox also fire shortcuts.
Fix: add `|| tg.type==='checkbox'` to `interactive` (one token on `:727`). Better: return early for any focused form control or
`[role=region]` and only handle shortcuts when focus is on `body` or the viewer. Parent: please confirm with a real Tab → Space during QA.

---

## Material honesty issues (fix in the same round; each one is small)

### H1 — "Cancelled run publishes nothing" is false if you cancel during *Writing files*
Prototype: `finish(j,'cancelled')` (`:316`) never touches `c.written`, and the self-check asserts it (`:766`), but only cancels in the first phase.
Backend: `_v2_publish` writes the manifest with `complete=False` and then publishes frame by frame with `check_cancel`
(`backend/service/inference.py` ≈ `:204–216`). A cancel during publish therefore leaves **some new frames on disk and an incomplete manifest**.
The brief explicitly asks for "partial output versus complete".
Fix: in `finish` for a cancelled key/resume job that has reached the `publish` phase, merge the first `frac*j.w` frames of `j.range`
into `c.written`, keep the state at Ready, and say: "Stopped while writing: 87 of 200 frames written; the clip is not Complete." Add a self-check that cancels during publish.

### H2 — Resume is shown as cheaper than it is
`DUR.resume=8000` vs `key=15000` (`:271`), and the button reads "Resume — write 212 missing frames only" (`:352`).
Backend: `engine.run_clip(...)` runs unconditionally (`inference.py` ≈ `:90–104`). Resume only skips stems at **publish** (`:190–200`),
so the AI still processes the whole clip. Fix: use the same duration as `key`; change the label to
"Resume — AI processes all 412 again, writes only the 212 missing frames"; mirror the same wording in `summary()` for resume.

### H3 — Resume is offered in cases the backend would refuse
`resumeRange()` (`:350`) only checks `stale()`, which compares Despill, colour and hint mode (`:294`). The backend's
`_v2_resume_compatible` (`inference.py:159–181`) requires **exact equality** for the params dict (temporal window, VRAM profile, hint/carry
quality, …), keyframe marks, enabled outputs and formats, and source stems. Otherwise it warns "re-publishing all selected frames".
In the prototype you can change the temporal window or an output format and still be offered "write 212 missing frames only".
Fix: store the whole settings snapshot in `j.snap` and `c.made` (temporal, vram, outs, kf, gen). Compare it in `diffs()`.
That gives the stale banner and the Resume gate one rule.

### H4 — "Keyframes" mode with zero marks can be keyed
`primary()` (`:343–349`) checks `hasHint` but not `c.kf.length`. Backend raises "has no keyframe marks; mark at least one frame"
(`inference.py` ≈ `:62–68`). Fix: one extra branch → "Mark at least one keyframe (M)" as the primary action.

### H5 — P2 rail ticks steps as "done" just because the user passed them
`done=i=>i<s` (`:472`). Jumping straight to step 5 shows ✓ on "Check colour" for SH050 even though `checked` is false. That defeats the one
thing P2 exists to guarantee (brief: "impossible to miss colour check"). Fix: derive the ticks from state
(step 3 ✓ only if `c.checked`; step 2 ✓ if `mode==='none'` or `hasHint`; step 4 ✓ if at least one output is enabled). Show "▲" on step 3 when unchecked.

---

## Missing current capability (risk of implying removal)

The parity list in `rust-path.md` already says that simplifying presentation does not authorize removal. The prototype, however,
looks like a *complete* settings surface, so the user may read anything missing as gone. Either show these items (collapsed) or add a visible
"Not shown in this prototype, still in the app:" line to the info row (`renderChrome`, `:592`).

| Capability (current code) | In prototype? | Recommendation |
|---|---|---|
| **Hint quality** 0–1, **Carry hint** + **carry quality** (`core.py:40–42`, `parameter_panel.py:551–572`) | No. The brief §1 lists them | Add to `secHint` when mode ≠ None (they change output and resume compatibility). **Material.** |
| **BG Color Auto/Green/Blue** (`parameter_panel.py:509–520`) | No | Not in `InferenceParams`. It drives the screen-colour detection used by Chroma Key and the UI accent. **Ask the user** whether it stays (open question). Don't drop it silently. |
| Chroma Key controls (eyedropper, strength, clip black/white, shrink/grow, blur, live preview) | Only the name in the generator list | Note it as a sub-panel. Ties to brief open question 3. |
| SAM2 tracking, holdout strokes, Reset Hint (keeps outputs) | No | Note it. Painting exists only as the 1/2 strokes. |
| Import / extraction / Welcome + recent projects; *Extracting* state; extract and stitch/video-export job types | No (P2 step 1 says importing happens "in the real app") | Acceptable for this round; `rust-path.md` sequence step 1 already schedules empty/importing states. |
| EXR compression preference (DWAB/PIZ/ZIP/none), custom output folder, source copy/reference | No | One line in Output/"Preferences". EXR compression matters for the Rust EXR decoder choice. |
| Quit-while-running confirmation (brief §1) | No | Optional for selection; mention as not shown. |
| Brief §3 busy queue seed (Running Key SH040 @ Refine 212/412, two Queued) | No. Initial queue is idle (`:282`) | Seed it. Otherwise every design first shows "No job running", and the user is comparing empty queues. |
| *Failed* job example | `JST.failed` exists, none in data | Seed one failed history item so failure styling can be compared. |

---

## Usability / accessibility (non-blocking)

- **Contrast** (computed WCAG): `.btn.dng` text `#d37466` on `--bg2` = **4.06:1** (fails AA for 12 px Cancel buttons, `:61`); fix by
  using `--bg1` for the button background or lightening `--err` to about `#e0857a`. `c-dim` chip on the selected clip `--selbg` = **4.33:1**,
  `c-ok` chip on selected = **4.43:1** (`:74–75,122`); fix by giving chips inside `.clipbtn[aria-current=true]` `--selfg`. All other checked pairs pass
  (`--dim` on bg1 6.0, on bg2 5.2; ink on accent 6.2; warn note 6.8).
- P3 overlay is `aria-modal="true"` (`:497`) but the background is not `inert`. Tab leaves the dialog. Fix: set `inert` on `.ga-grid,.ga-tool,.ga-q` while the overlay is open.
- The full `innerHTML` re-render whenever a job changes state (`:739` → `render()`) discards an in-progress number entry in In/Out
  and closes an open `<select>`. It's a prototype annoyance, but the parent may hit it during QA while a job runs. Fix: skip `render()` while
  `document.activeElement` is an input/select inside `#mount`, and only `liveUpdate()`.
- P5 says "Re-key…" (`:519`) for a never-keyed clip. Use "Key clip…" when `!c.written.length`.
- Stale banner in P1 sits in the inspector, not on the viewer the user is judging. Brief §1 asks for viewer-adjacent. Add a one-line
  warn strip to `.vhead` when `stale(c)` (all designs benefit).
- The brief's draggable In/Out band was not built (only numeric fields + "= playhead"). This is acceptable for selection.
- The `distinct layouts` self-check (`:751,761`) compares the first 4000 characters of markup. That is weak evidence. Visual distinctness is genuine (see below); don't cite the check as proof.

## Per-design notes for the user's selection

| Design | Distinct? | Action hierarchy | Honesty | Notes |
|---|---|---|---|---|
| P1 Shot Station | Yes | Weak until B1 (Key at y 1166) | Good | Will look much better once panels scroll and the Key footer is sticky. |
| P2 Guided Stepper | Yes | Clear (Next →, sticky step foot) | H5 | Best first-run teaching. Range diagram (`rangeDiagram`) is the clearest whole-clip explanation in the set. |
| P3 Batch Board | Yes | Clear ("Key N clips — one at a time") | Good. Error clip skipped with reason | Re-keying a Complete clip (SH060) in bulk has no "overwrites results" note. Add it. |
| P4 Page Tabs | Yes | Good, but Key exists on both *Key* and *Deliver* | Strongest results-vs-AI split | The output range lives only on Deliver, while the Key page shows the summary sentence. That's fine, but say "Range set on Deliver" on the Key page. |
| P5 Review Room | Yes | Primary hidden in drawer by design | Good | Consciously weak for first run. Fine as a review mode, not as the default. |
| P6 Job Ticket | **Weakest**: the same `secHint/secInput/secOutput/secAdv` as P1 in monospace with numbering | Weak until B1 | Good | Its value is the plain-English sentence, which every design already shows (`summaryBlock`). Better treated as a feature than as a layout. |
| P7 Timeline Lanes | Yes | Weak until B1 (Key at y 1316) | Good; "layout only — no editing" caption is honest | Coverage/gap view is the best Resume explanation. Heavy at 728 wide. |
| P8 Node Graph | Yes | OK | Good; "wiring is not editable" | Expert-only, as the brief says. |

---

## Ranked shortlist (recommendation only; the user decides)

1. **P1 Shot Station, after B1**: lowest migration risk from today's Qt layout, everything for one shot on one screen. Fold in
   P2's range diagram into its Output section and a first-run colour nudge.
2. **P4 Page Tabs**: the cleanest structural "AI processing vs files on disk" separation, and it scales to more hint tooling (paint, SAM2,
   chroma sub-panels) without crowding. Costs more navigation and more screens to build.
3. **P3 Batch Board**: choose this over P4 if the user's answer to open question 1 is "many shots per session". It can coexist as P1's bin view.

Keep **P2** as an optional first-run/"guided" mode and **P8** as an optional expert view rather than default shells. **P6** and **P7**
should be mined for parts (summary sentence; coverage lanes for Resume) rather than chosen as whole layouts. **P5** is a good review mode
inside P1/P4, weak as the default.

---

## Rust plan (`rust-path.md`, brief §6, `research.md` rev 2): reasonable as a staged recommendation, not a selected architecture

What holds up:
- "Rust app + supervised Python CUDA worker first; Rust-native inference is a separate, evidence-gated feasibility milestone" matches
  the code. `run_clip` and the hint models are deeply Python/CUDA, and the UI should not own model lifetime.
- The read-only viewer slice first and the benchmark-before-choosing playback table are the right gates. Research rev 2 correctly records that
  FilmCraft's EXR decoder refuses DWAB/PIZ (our default matte output). EffectCraft's `exr`-based arbitrary-channel route is plausible but **untested
  end to end**. Pinned git deps are valid. **Nothing is benchmarked.** The plan does not overclaim any of this.
- Single-scheduler warning ("only one layer should schedule GPU jobs") is important and present.

Gaps to add before anyone implements:
1. **Queue ownership is inconsistent between docs.** Brief §6 puts the job queue in Rust (stage 3, port of `job_queue.py`). `rust-path.md`
   keeps current queue semantics behind a worker adapter during transition. Pick one owner per stage explicitly.
2. **Resume compatibility is byte-equality on `params.to_dict()`** (`inference.py:173`). Any Rust-side field rename, default change, float
   round-trip or added key silently turns every Resume into a full re-publish. Freeze the params schema and add a golden test that
   Rust-serialized params equal Python's (brief stage 0 should name this).
3. **Protocol terminal events need partial-output detail.** `cancelled`/`failed` should report frames published and whether the manifest
   is complete (see H1). Otherwise the Rust UI cannot tell "partial" from "nothing written".
4. **Worker crash / restart mid-publish**: the manifest stays `complete=False` with some frames on disk. Recovery rules belong in the protocol
   doc, not just "worker-exit recovery".
5. Brief §6's "P7/P8 favour immediate-mode egui" is premature. Toolkit choice should follow the layout pick **and** the playback benchmark;
   iced/Slint/Tauri rows in research are unverified general knowledge.
6. EXR compression preference is user-selectable today. The playback acceptance matrix already says "every compression setting emitted by the
   current writer". Keep that as a hard gate, since it is what excludes FilmCraft-as-is.

---

## Round-2 checklist for the implementer

- [ ] B1 bounded app height + scrolling panels + sticky primary action (+ viewport presets from brief §5) + self-check "primary visible".
- [ ] B2 Space/shortcut capture excludes checkboxes and form controls.
- [ ] H1 cancel during publish leaves partial frames, not Complete; self-check for it.
- [ ] H2 Resume duration and label say the AI reprocesses the whole clip.
- [ ] H3 full settings snapshot drives both stale and Resume.
- [ ] H4 Keyframes with no marks blocks Key with a fix action.
- [ ] H5 P2 ticks from real state.
- [ ] Hint quality / carry hint / carry quality added; "not shown in prototype" line for the other parity items; BG Color raised as a user question.
- [ ] Seed the brief §3 busy queue and one Failed job.
- [ ] Contrast fixes for `.btn.dng` and chips on the selected clip.

Not checked: screenshots, actual screen-reader output, T3 `html_render` behaviour, widths below 728 px, the brief's 1920/1280 presets.
