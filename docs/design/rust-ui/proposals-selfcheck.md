# proposals.html self-check

Open `proposals.html` standalone (file://) or via html_render. No setup, no network.

**Built-in:** footer shows `Prototype self-check: N / N passed` (120/120 at 1440 wide; 104/104 below 1030 wide, where layout-bounds checks are skipped). Re-run with `__selfcheck()` in the console; failures are listed in the footer and `console.error`.
It covers: 8 designs + Compare tabs, no vh-family units, no `height:100%` on html/body, per design (app header, controls, working primary action, "whole clip" wording, queue, no disabled primary, named buttons, ≥12px text), distinct layouts, and the simulation: Key → Cancel ("Stopping after current step…", not instant, publishes nothing), one running job at a time, range write stays Ready (not Complete), Resume writes only missing 212 frames then Complete, Despill change → stale, colour never auto-switched, Error clip actionable, shortlist is local-only.

Round-2 additions: at ≥1030 px wide each design's primary action and playback controls must lie inside the fixed-height app window (660 px; panels scroll inside it, Key/Cancel live in pinned footers); no "Cutout linear"/manifest file text in default UI; cancel during *Writing files* leaves partial frames (never Complete); Resume/stale share one full-settings rule (temporal, VRAM, outputs, hint quality/carry, keyframes); Keyframes with no marks asks for a mark; P2 ▲ on unconfirmed colour; Space on a checkbox does not play; failed job in sample history.

**Manual (parent):**
1. 1440×900: each tab P1–P8 shows a full app in ≤ ~850 px page height; "Show busy queue example" loads a running + 2 waiting queue on demand; 728 wide: panels reflow, no horizontal page scroll; ≤640 wide larger hit targets.
2. Keyboard: Tab to tabs, ←/→/Home/End move between them; `1`–`8`, `C`, Space, ←/→ frames, I/O/M, K, Esc.
3. P1: press Key clip, watch phases, Cancel, confirm "Stopping after current step…" then Cancelled. Toggle "Simulation speed" for faster runs.
4. P1: Matte view with no written frames shows "No file on disk"; after a run, written frames show matte; changing Despill shows the stale banner.
5. Shortlist button: marks locally (also saved in localStorage) and says nothing was sent.

**Known limits:** window height is a fixed 660 px (no viewport presets); P7 viewer is small by design (lanes + inspector share the window); BG Color, Chroma Key panel, SAM2/holdout, import, EXR compression etc. are listed under “Not drawn in this prototype” in the footer, not drawn; resume-compatibility is a simplified model of the backend rule; P3 bulk re-key of a Complete clip has no overwrite note; scene is a drawn sample, legacy screenshot is not used; timings and the 18.2 GiB figure are sample values; ratings in Compare are the planner's opinions; not tested in a real T3 html_render.
