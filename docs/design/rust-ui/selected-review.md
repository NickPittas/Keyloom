# Review: selected-workflow.html (Batch Board + Page Tabs + per-clip graph)

Scope: whether the prototype is fit as a design prototype for the user's choice. Checked against `selected-design.md` (read in full), `preset-contracts.md` and `selected-selfcheck.md`. I did not render it; rendering and browser QA belong to the parent. Line numbers refer to `selected-workflow.html`.

## Verdict

**The desktop layout passes, with fixes. The narrow layout does not pass yet.** At 1440 px every part of the user's choice is present:

- The Board is home, with multi-file import.
- **Open clip settings** replaces the Board with the five page tabs. There is no popup.
- One fixed 7-node graph spans the full width under the page and Properties on every tab.
- Media, Hint, Key and four separate export nodes drive the Properties panel.
- Workflow and Export-only presets can be saved and applied to the Board selection through an inline apply bar.

The preset boundary, input escaping, storage validation and frozen job snapshots hold up (see "Verified OK"). There are three blocking issues: the 728 px layout, a chained job that can fake a successful run, and EXR compression.

---

## Required fixes (P0 — before acceptance)

### P0-1. The narrow layout breaks "graph under each page", the bounded window and the pinned action
CSS lines 181–200. Below 1030 px the `.win` has no fixed height. `.wsmain` collapses to one column, giving this order: page → whole Properties panel, including its footer → graph → queue strip. At 728 px:
- The graph sits under a long Properties panel, not under the page. To reach it you scroll the whole document, which the B1 fix and design §2.1 ("panels scroll; the page never does") forbid.
- The "pinned" primary action is no longer pinned. It ends up somewhere in the middle of the scrolled document.
- `.graph` is a fixed 950 px wide (line 170), and the export nodes sit at x = 730–940 (line 558). On a window about 708 px wide, all four export nodes start off-screen behind a horizontal scroll with no cue.
- The Board's queue drops below every card instead of becoming the "Queue (N)" strip from §2.5.
- The self-check skips every bounds check at this width (`if(wide)`, lines 835/851/857/963). The design's acceptance criterion ("primary action and graph inside the window at 1440 **and** 728") is therefore untested and in fact fails.

**Fix:** keep the 660 px window at every width. At ≤1000 px, use this vertical order: header → page (scrolls) → graph → action/queue strip. Put the Key/primary action in the strip. Make Properties a toggleable sheet inside the window: a **Properties** button in the header, Esc closes it, and focus returns to the node, as §2.5 specifies. Lay the graph out more compactly at narrow widths (for example, Media/Hint/Key at about 140 px wide, with the exports in a 2×2 block) so all 7 nodes fit in about 700 px without scrolling. Extend the self-check so the bounds checks run at 728 too.

### P0-2. A chained "Generate hint, then key" runs the key even after the hint is cancelled or fails, and fakes a result
`genThenKey` (line 721) queues the hint job and the key job as independent jobs. `startNext` (line 358) starts the next queued job with no prerequisite check. If the user cancels the queued or running hint job, the key job still starts. `finish` (line 363) then writes frames and can mark the clip **Complete**, keyed "with the GVM hint" that never existed. This breaks "never fabricates assets" and gives the user a false promise. The **K** shortcut (line 808) reaches the same path.

**Fix:** record the dependency (`j.after = hintJob.id`). When the hint job is cancelled or fails, either cancel the dependent key ("Key SH050 cancelled — its hint job did not finish") or leave it queued and blocked, with a reason. `startNext` must re-check `readiness()` before starting any key or resume job, and skip and flag a job that is no longer ready. Add a self-check: genThenKey → cancel hint → no key frames are written.

### P0-3. EXR compression: wrong values, wrong default, and inconsistent stale rules (a known issue; this is the full list of places to change)
- `COMPS=['PIZ','ZIP','DWAA']` (line 241) must be the real set `dwab / piz / zip / none`. Show them as "DWAB / PIZ / ZIP / None". DWAA is not a valid value.
- `base()` defaults `comp:'ZIP'` (line 254). The real default is `dwab`, taken from the global preference (design §6.5).
- Update the selfcheck literals `'DWAA'` (lines 876–880) and the seeded/`good` presets (`comp:'ZIP'`, lines 264 and 939). The `PRE_KEY` store is validated against `COMPS`, so a stored preset with an old value is dropped. That is acceptable for a prototype.
- **Inconsistency:** `cfgKey` (line 350) includes `comp`, so changing compression flags a queued job "Queued with earlier settings". `snapOf` (line 283) excludes it, so the same change does not mark results stale and does not block Resume. Resume could then write a sequence that mixes PIZ and DWAB frames. Add `comp` to `snapOf`/`SNAPLBL` ("EXR compression").
- **Design doc:** `preset-contracts.md` says the manifest does not record compression. §3.2 ("full-settings rule gates Resume") therefore can't be met as written. Add "record `exr_compression` in the run manifest" to the R0 contract and its exit tests in `selected-design.md` §5.

---

## Should fix (P1)

1. **The Bypass edge is invisible** (line 556). The solid Media→Key edge is a straight line at the same y as the Hint node, drawn in the SVG *under* the node buttons. The parts you can see (190–240 and 420–470) look solid, so a bypassed graph reads as an ordinary connected chain whose Hint node merely has a dashed border. Route the bypass edge around the node (an arc under it, for example `C … cy+48`) and fade or dash the Media→Hint→Key segments.
2. **The acceptance path can't run end to end from an import.** Imported files are honest "metadata not inspected" cards that can never be keyed (lines 663–668). The manual path "import → select 2 → apply → … → Key 2 clips" therefore has to switch to sample clips partway through. Keep the real file chooser and add the brief's **simulated import** (`SH080_rooftop` / `SH090_rooftop_alt`, *Extracting* for about 1 s, then Raw, 120 f) next to it, labelled "simulated". Also, `addLocal` silently adds the new cards to the existing selection (line 667). Select only the new cards, or none, and say so.
3. **Preset Delete takes one click with no confirmation and sits next to Apply** (lines 604–605, 737). It is destructive and persisted immediately. Add an inline "Delete 'X'? [Delete] [Keep]", the same pattern as Replace. `delPreset` also ignores `persist()` failure: report it the way `savePreset` does.
4. **Storage overwrite after an unreadable or unknown-version store.** `loadPresets` returns `[]` for corrupt or `v≠1` data (lines 302–303). The next `persist()` then overwrites that blob, which loses a newer-version or hand-edited store. In the prototype, keep the raw value under a backup key, or block saving until the user chooses "Discard stored presets". Write the same rule into the R0 preset contract: never overwrite a store you couldn't parse, and back it up first.
5. **Requeue turns a Resume into a full Key.** `requeue` always calls `enqueue('key')` for non-hint jobs (line 731). An outdated Resume becomes a full overwrite without saying so. Either keep the type (and re-check `resumeRange`, which will normally be null because the settings changed) or label the button "Requeue as full Key".
6. **An Export preset applied after a Workflow preset marks the Workflow "(modified)"** (lines 326–327). The self-check enshrines this at line 919. The user changed nothing by hand. When `applied.ex` is set, compare only the non-export fields for the `wf` slot.
7. **The applied-preset record has no id** (line 324). It stores `{name, rev, fields}`, but design §3.2 says `{id, name, rev}`. Without the id, "re-apply" or "preset updated since" can't be resolved after a Replace or a delete-and-recreate under the same name. Add `id`. If you want, show "rev 1 · preset now rev 2" on the card.

## Doc / prototype consistency (P2, make the design doc and prototype agree; quick)

- **`cutout_linear`:** design §2.4 says "Advanced disclosure, shown disabled" and §3.2 includes it in Workflow presets. The prototype omits it, and the self-check asserts that it is absent (line 853). `preset-contracts.md` says to copy it only if it is effective. Pick one: preferably drop it from §2.4/§3.2 and carry the engine default.
- **Save as preset:** §3.2 says "Properties (any node)", but the prototype hides it on Media (line 563). That's reasonable, since Media holds no preset fields. Update the doc to say "any Hint/Key/export node".
- **Keyboard:** §4 says Space works "viewer only" and that `?` opens the shortcut sheet. The prototype toggles play from any non-interactive spot, including the Key and Deliver pages where no viewer exists (line 804), and has no `?` key. Either gate Space on `.viewer` being present and add `?`, or change the doc.
- **Local cards claim a colour choice:** imported cards are created with `checked:true` and `colour:'srgb'` (line 666), so Media Properties says "✓ sRGB — your choice" (line 493) about a file that was never inspected. Use `checked:false` and "Not inspected" for local cards.
- The Board has no toolbar **Open clip settings** button and no **Import folder…** (§2.2). Per-card Open and multi-file import cover what the user explicitly asked for, so this doesn't block. List both in the "Not drawn" footer (folder import is already listed there).

## Not required (out of user scope; do not block on these)
Preset rename, the folder importer, per-export folders and compression, and video-export presets. The doc already lists the first two as optional or deferred.

---

## Verified OK
- **Preset boundary:** `fieldsOf`, `cleanFields` and `mergePreset` (lines 292–325) copy only route, mode, qualities, carry, VRAM, despill, temporal, exports and compression. Colour, `checked`, range, keyframes, strokes, hint assets (`hasHint`/`hintGen`), output folder, `written`/`made` and the source are untouched, and the self-check covers both clips. Every fields object is a fresh copy, with no shared references between presets and clips. Readiness after apply comes from real state (*Needs hint* / *Needs keyframe marks*), and nothing is copied between clips.
- **Escaping:** `esc()` is applied to clip, file and preset names, the apply-bar diff, skip reasons and output folders. `say()` uses `textContent`. Focus restore uses `CSS.escape`. Imported names are capped at 80 characters.
- **Storage validation:** version gate, 50-item cap, id regex and dedupe, name rules, enum/range checks, unknown fields dropped, `__proto__` keys can't leak (fields are rebuilt explicitly). P1-4 is the only gap.
- **Queue immutability:** `CFG` is frozen at submit, and `finish` records `c.made` from the job snapshot, not current settings. Apply never touches queued or running jobs. Outdated queued jobs are flagged and Requeue takes a new snapshot.
- **Graph navigation:** a click selects without changing the page. Enter opens the node's page, and export nodes open Deliver with that export kept selected. Arrow keys cycle nodes, `aria-pressed` marks the selection, and G focuses the selected node. Typing and form controls are ignored for shortcuts, and Space on a checkbox is not hijacked (B2). Double-click re-renders between its two clicks. That should work in Chromium, but please confirm it in your browser QA.
- **Graph "under each page" (desktop):** it is identical on all five tabs, sits outside the scrolling panels and stays inside the 660 px window. It takes about 150 of the 660 px. That is readable, though the Hint page viewer can squeeze to its 110 px minimum: check this in rendering.
- **Honest limits:** cancel publishes partial output, a range run stays *Ready*, Resume writes only missing frames, keyframe mode with zero marks is blocked, and local files are never queued. P0-2 is the exception.
