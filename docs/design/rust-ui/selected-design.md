# Selected design: Batch Board + Page Tabs + per-clip Node Graph

Status: the design is **selected** and the plan is ready to implement. Neither the Rust UI toolkit nor the playback backend has been selected. Both still wait on the benchmark in `rust-path.md`.
This file replaces the eight-option comparison. Don't build any more alternatives.
**2026-10-22 revision (user-authorized):** In/Out now defines the frames processed by Hint → Key → Export. This supersedes the earlier whole-clip-inference requirement. The project gains a shared output root and tokenized folder/file naming. The exact contract is in `range-output-contracts.md`, and it wins over any older wording in this file. The Hint viewer, transport and viewer-mode placement also changed (§2.1, §2.3).
**2026-10-22 revision 2 (user, supersedes the graph strip):** the graph is a real node canvas (cards, typed sockets, branching/merging connections, drag/pan/zoom/fit, resizable and collapsible lower panel), and buttons/checkboxes get visible borders and states on the gray background. `node-canvas-brief.md` wins over §1 "Graph UNDER EACH PAGE", the "Fixed topology" paragraph, and §2.3 "Graph rules". Backward compatibility with the Qt app's project format is no longer an acceptance requirement; existing user data must still never be destroyed.
Inputs: `node-canvas-brief.md`, `range-output-contracts.md`, `design-brief.md`, `review.md`, `proposals-selfcheck.md`, `rust-path.md`, `research.md`, `preset-contracts.md`, and the code facts below.

---

## 1. The user's choice, kept as stated

| User element | Meaning in this design |
|---|---|
| **Page Tabs + Batch Board + Node Graph** | All three are in one app. None is an optional "expert view". |
| **Batch Board is the main driver** | The Board is the home screen. The app opens on it, and every clip workspace has **← Back to batch**. |
| **Import single/multiple clips** | Board toolbar: **Import clips…** (one or more files) and **Import folder…** (an image sequence or a folder of clips). New cards appear in the *Extracting* or *Raw* state. |
| **Open clip settings goes into Page Tabs, not a popup** | **Open clip settings** (or double-clicking a card) replaces the Board with that clip's workspace: tabs **Media · Hint · Key · Review · Deliver**. No modal or overlay. |
| **Graph UNDER EACH PAGE** | Every page of the clip workspace keeps the same node canvas in a resizable, collapsible lower panel (`node-canvas-brief.md`). |
| **Select fixed Media, Hint, Key and separate export nodes to set properties** | Clicking a node shows its properties in the **right-hand Properties panel**. Edges: Media→Hint, Media→Key, Hint→Key (optional, bypassable), Key→FG / Matte / Comp / Processed. Each export is its own node. |
| **Save workflow/export presets for batch use** | Two kinds of named preset: **Workflow** (graph, Hint, Key **and** exports) and **Export** (exports only). Save, name, and apply to the selected Board clips. |

Current node set is fixed: you can't add, delete, or rewire nodes yet, and the only structural change is **Bypass Hint**. The data model is a real graph (typed ports, edges, fan-out and merge) so later nodes (luminance key → hint keyframes, merge alpha, roto, paint) can be added; none of them, and no graph execution engine, is built now. See `node-canvas-brief.md`.

---

## 2. Screens and flow

### 2.1 Window
- Desktop app with bounded internal panels. Panels scroll; the page never does. The primary action and the queue strip are always visible. The old fixed **660 px** height is now the *minimum* supported height, not the target. At a regular desktop size (≥1440×900) the window fills the available height.
- **Viewer size (revised):** at 1551×903 the Hint scene measured only ~121 px high, which is too small for SAM masks, points and boxes. Target: **≥280–300 px of drawing height** at a regular desktop size. To get it, the graph strip may collapse to a compact one-row strip (~40 px, still showing all 7 nodes and badges) on Hint/Review, and the Properties panel may narrow. **Focus mode** (`F`, or a button on the viewer; Esc exits) hides the graph, Properties and queue strip so the viewer fills the workspace. The primary action stays reachable in the viewer toolbar.
- **Every viewer** (Media, Hint, Review, and any Key/Deliver preview) has the same layout: view-mode chips **Input · Hint · FG · Matte · Comp · Processed** top-right, **A/B wipe** top-centre, and standard transport icons (⏮ ◀︎| ◀ ▶ |▶ ⏭) centred below the image. Next to the transport sit **Set In (I)**, **Set Out (O)** and **Clear In/Out**, labelled identically everywhere, showing only the shortcut, with no "= playhead" text. Unavailable modes are disabled with a reason ("No hint yet").
- **Shared playback timeline:** every viewer, including Media, has a compact scrubbable timeline with a playhead, source frame numbers and a visible In/Out selection. Pointer seeking and keyboard scrubbing update the same clip playhead; Set In/Out updates the same processing range on every page. Playback advances the timeline. Keep it compact enough to preserve Hint drawing space.
- Text is at least 12 px (13 px base) and numerals are tabular. Hit targets are at least 28 px, or at least 40 px below 640 px wide. The visual tokens from `design-brief.md` §2 are unchanged.
- At 728 px wide it stays responsive with no horizontal page scroll (see §2.5).

### 2.2 Board (home)
```
┌ Harbour Promo ─────────── [Import clips…][Import folder…] ─ GPU 17.8/24.0 GiB ┐
│ Toolbar: ☐ All · 2 selected · Workflow preset [▾ Apply] · Export preset [▾ Apply]│
│          [Open clip settings]                       [Key 2 clips — one at a time]│
├──────────────────────────────────────────────────────────┬─────────────────────┤
│ Cards/list (scrolls): ☐ thumb · name · state · readiness │ Queue (scrolls)     │
│  hint route · Workflow: "Hair closeups" (modified) ·     │ Running · Refine…   │
│  Export: "Comp review PNG" · Writes 1101–1300 of 412     │ Queued ×2 · History │
│  · outputs FG EXR, Matte EXR, Comp PNG                    │ [Cancel]            │
└──────────────────────────────────────────────────────────┴─────────────────────┘
```
- The card checkbox is the multi-select control. Space must toggle it (review B2).
- **Readiness** is computed per clip and shown as text plus an icon: *Ready to key*, *Needs hint* ("Hint route GVM: generate hint first"), *Needs keyframe marks*, *Needs colour check* (sampled-notice clips), *No outputs enabled*, *Error: hint has 94 frames, clip has 96*. **Key N clips** skips blocked clips and names them with their reasons. It never fabricates assets.
- Bulk **Key** on a *Complete* clip shows "Overwrites existing results for SH060" inline before it queues.
- Applying a preset (§4) opens an **inline apply bar** under the toolbar, not a popup: "Apply 'Hair closeups' to 2 clips: changes Hint route, Despill 0.5→0.65, Temporal Auto→48/24. Keeps each clip's source, colour, range, marks, paint and output folder. [Apply] [Cancel]".

### 2.3 Clip workspace (Open clip settings)
```
┌ ← Back to batch │ SH040_talent_walk · Ready │ Media  Hint  Key  Review  Deliver ┐
├────────────────────────────────────────────────────────┬────────────────────────┤
│ Page content (viewer + page tools), scrolls internally  │ Properties: <node>     │
│                                                          │ (scrolls)              │
│                                                          │ ── pinned footer ──    │
│                                                          │ [Key clip — AI         │
│                                                          │  processes 1101–1300]  │
├────────────────────────────────────────────────────────┴────────────────────────┤
│ Graph (fixed, ~130 px):  [Media]→[Hint ◐ bypass]→[Key]→[FG][Matte][Comp][Proc]  │
├──────────────────────────────────────────────────────────────────────────────────┤
│ Queue strip: Running Key SH040 · Refine 212/412 · 68% · ETA 4m10s [Cancel]       │
└──────────────────────────────────────────────────────────────────────────────────┘
```
| Tab | Page content | Linked node (auto-selected when the tab opens) |
|---|---|---|
| Media | Source viewer with scrubbable timeline, source facts (resolution, fps, frames, path), **input colour** sRGB/Linear with the sampled notice ("Keep sRGB" / "Switch to Linear", never automatic) | Media |
| Hint | Large drawing viewer (§2.1). Hint route: None / GVM / BiRefNet / VideoMaMa / MatAnyone2 / Chroma Key / Import Alpha. Paint strokes 1 = subject, 2 = background, **SAM prompts: positive/negative points and box** (today the backend derives points and a box from strokes in `annotation_prompts.py`; explicit point/box tools need an `annotations.json` schema addition in R0/R5), holdout, SAM2 tracking (inside In/Out only), keyframe marks (M), Reset Hint, **Generate hint** (a separate queued job) | Hint |
| Key | Plain-English summary ("AI will process frames 1101–1300 (200 of 412). Temporal context stays inside this range. Writes Matte (EXR) to /mnt/NAS/…/clip_name_2K_18-10-2027_v001/"), phase stepper + Cancel, stale banner ("Range changed: write as v002 or replace v001") | Key |
| Review | "Playback of files on disk": the shared viewer-mode chips (top-right) and A/B wipe (top-centre), background. Missing frames show "No file on disk"; frames outside the range show "Outside In/Out". Never triggers AI. | none (graph shows written coverage badges on export nodes) |
| Deliver | Range readout, then the **Output location** fields from `range-output-contracts.md` §6: root, folder template, file template, resolution label ("label only, no resize"), date, version + **Next free version**, live resolved example path, and validation/collision message. Also Resume, open output folder, and video export (default path proposed from the location, still confirmed at export time) | the FG export node; clicking another export node switches Properties |

Graph rules:
- Click a node to select it and show its Properties. The page doesn't change. Double-click or Enter on a node opens that node's tab (export nodes open Deliver). Arrow keys move between nodes. Nodes are real buttons with an `aria-pressed` selected state.
- **Bypass Hint** (a toggle on the Hint node and in its Properties) sets `hint_mode=none`. It doesn't delete hint assets, marks or paint, so un-bypassing restores the previous route and mode. A bypassed node is drawn dashed, and its edge routes Media→Key.
- Every node shows a status text badge: Media *Linear ▲ check*, Hint *Needs hint* / *GVM · Full ✓* / *Bypassed*, Key *Stale*, exports *Off* / *EXR* / *PNG* + "200/200 on disk".
- ~~The graph strip has a fixed height.~~ Superseded: node canvas with drag/pan/zoom/Fit in a resizable, collapsible lower panel, identical on all five tabs, Hint viewer kept ≥ 300 px (`node-canvas-brief.md` §3). Bypass dashes the Hint card and its Hint→Key edge; no edge is rerouted.

### 2.4 Node properties: what each node can actually set (honest limits)

| Node | Properties | Backend field / owner | Limits |
|---|---|---|---|
| Media | Input colour sRGB/Linear | `input_is_linear`, per-clip override | **Not in any preset**: mixed sources need different interpretations. |
| | Source path, frames, fps (read-only) | clip metadata | Never edited by presets. |
| Hint | Bypass; route; mode Full/Keyframes; hint quality 0–1; Carry hint + carry quality 0–1 | `hint_mode`, `hint_quality`, `carry_hint`, `carry_hint_quality`; route = new per-clip field (today: transient `BatchClipConfig`) | Generating or importing creates **assets** per clip. Marks, paint, chroma samples and params, and AlphaHint files are source-bound and never copied. |
| Key | Temporal window (with the peak GiB estimate), VRAM profile, Despill 0–1 | `temporal_window`, `vram_profile`, `despill_strength` | **Target:** inference covers only the clip's In/Out range, with the temporal window clamped at the range edges (today: whole clip, range limits publication only). The range is per clip, never per node. The ineffective Cutout linear control is omitted; retain its engine default. |
| FG / Matte / Comp / Processed | Enabled; format EXR/PNG; **EXR compression** (shown on each EXR node) | `<p>_enabled`, `<p>_format`, `exr_compression` | **Compression is one value shared by all EXR exports of the clip's run** (`OutputConfig.exr_compression`). Editing it on one node changes all of them, labelled "Shared by all EXR exports". Bit depth (EXR half / PNG 8-bit) and range are **not** per node. Location and naming are per clip/project (Deliver, `range-output-contracts.md` §4). Exports are separated by the `{output}` token, and a template without `{output}` is allowed only when exactly one export is enabled. At least one export must be enabled (`validate_selection`). |

### 2.5 Responsive (728 px)
- Board: the queue panel collapses to a one-line strip with a **Queue (3)** toggle that opens it inside the window. Cards switch to a single-column list.
- Clip workspace: Properties becomes a toggleable side sheet inside the window (a **Properties** button in the header, Esc closes it, focus returns to the node). The Key action stays in the queue/action strip. The graph scrolls horizontally. Tabs may wrap to two rows but stay inside the header.

---

## 3. Ownership and state model

### 3.1 Entities

| Entity | Contents | Persisted where | Written by | Notes |
|---|---|---|---|---|
| **Project** | clips, project output folder → **output location** (root, folder/file templates, date format) | `project.json` | UI | The folder exists today; the templates are new (legacy projects keep the old layout). |
| **Clip (source-bound)** | source path/type/stems, `in_out_range` (zero-based, inclusive, `None` = whole; target adds `in_stem`/`out_stem`), custom output folder → root override + `{resolution}`/`{date}`/`{version}` values, input colour override, hint keyframe sidecar, `annotations.json`, `chroma_params`, AlphaHint/mask assets | existing `clip.json`, sidecars, files; colour override is currently in the session map → move it to `clip.json` (versioned, read the old session map first) | UI (hint assets: worker) | **No preset ever writes these.** |
| **Clip workflow (new)** | per-clip node settings: Hint route + bypass + InferenceParams reusable fields + OutputConfig (incl. `exr_compression`) + `{workflowPreset:{id,name,rev}, exportPreset:{…}, modified:bool}` | new versioned `workflow` key in `clip.json` | UI | Today one global panel feeds every job. This makes settings per clip. A new clip gets the last-used/default workflow. Compression default comes from the existing global preference. |
| **Preset (new)** | `{id, name, kind: workflow\|export, version, rev, fields}` | user config dir `presets/*.json` (prototype: `localStorage`) | UI | **New feature; nothing like it exists today.** It isn't project-bound, so it works across projects. |
| **Job** | immutable snapshot: deep-copied clip + InferenceParams + OutputConfig + range identity + marks + **resolved absolute output paths** + resume flag | queue (Python now, Rust after the queue-parity milestone) | queue owner | Already done by `create_job_snapshot` and `_batch_frozen_settings`. **Frozen at submit.** |
| **Manifest / outputs** | published frames, params, outputs, range identity, keyframes, `exr_compression`, resolved paths, `clip_id`, `complete` flag (Complete = complete for its recorded range) | `.corridorkey_manifest.json` + files | worker only | The single source of truth for *Complete*, partial and stale. |
| **UI state** | Board selection, open clip, active tab, selected node, viewer mode/frame | memory (+ session file for restore) | UI | Never sent to the worker. |

### 3.2 Preset semantics
- **Workflow preset** = Hint route + bypass + `hint_mode` + `hint_quality` + `carry_hint` + `carry_hint_quality` + `temporal_window` + `vram_profile` + `despill_strength` **+ all export fields** (enables, formats, shared EXR compression).
- **Export preset** = the export fields only. Applying it never touches Hint/Key.
- **Never in either preset:** source, input colour, In/Out range, keyframe marks, paint/annotations, chroma samples and params, hint assets, output root/folder/file templates/date/version, video export path.
- **Output location:** shared project root + folder template + file template, with explicit batch application showing resolved paths and collisions. Generic Workflow/Export presets never copy a location. A separate location-preset manager is optional later work, not required for this revision.
- **Save:** "Save as preset…" in Properties (any Hint/Key/export node) and on the Board shows an inline name field. The default name is the clip's preset name + " copy". A duplicate name asks "Replace 'X'?" inline. Deletion asks for confirmation; rename is deferred in the prototype. Never overwrite an unreadable or unsupported-version preset store: preserve the raw data before allowing a new store to be saved, and report backup/save failures.
- **Apply = field merge, copy-on-apply.** The values are copied into each selected clip's workflow, and the clip records `{id, name, rev}`. Editing a preset later changes nothing until you re-apply it. Editing a clip after apply shows "Hair closeups (modified)".
- **Per-clip resolution after apply:** if the route needs assets the clip lacks, the readiness is *Needs hint*. Keyframes mode with no marks gives *Needs keyframe marks*. GVM/BiRefNet route without a hint shows **Generate hint** for that clip and on the Board ("Generate hints for 2 clips — one at a time"). Nothing is fabricated or copied from another clip.
- **Effect on results:** applying settings, a range, or a location that differ from the last run marks the clip **stale** (a range change is never resumable; see `range-output-contracts.md` §3). The same full-settings rule gates Resume (review H3). Including EXR compression is a proposed stricter rule: today's manifest does not record it. R0 must version the manifest and record `exr_compression`; legacy manifests with unknown compression require a labelled full rewrite before this new compatibility guarantee can be made. Preserve existing inference/output dictionary equality checks.
- **Effect on the queue:** apply never changes a job that is running or queued. A queued job of an affected clip gets the badge "Queued with earlier settings" + **Requeue with current settings** (removes that queued job and submits a new snapshot). Running jobs just finish with their snapshot.
- **Dependencies:** “Generate hint, then key” records an explicit prerequisite. A failed or cancelled hint job prevents its dependent Key job from starting. Revalidate required assets against the frozen job settings at dispatch. Requeue must preserve Resume when compatible, or explicitly offer a full Key; never silently convert it to an overwrite.

### 3.3 Staged owner per layer (resolves review gap 1)

| Stage | Queue/scheduler | Inference + hints | Clip/workflow/preset files | Playback |
|---|---|---|---|---|
| Today | Python (Qt app) | Python | Python | Python/Qt |
| R2–R3 | Python (Qt app) | Python | Rust reads; R3 Rust writes workflow/presets (Qt keeps reading `clip.json`, ignoring unknown keys) | Rust |
| R4–R6 | **Python worker** owns the queue; Rust submits snapshots and displays them | Python worker | Rust | Rust |
| R7 | **Rust**; the worker accepts exactly one active job, with no pending queue | Python worker | Rust | Rust |

There is only ever one scheduler at a time.

---

## 4. Prototype brief for Sonnet (`docs/design/rust-ui/selected-workflow.html`)

**Deliver** one self-contained file that opens from `file://` with no network, CDN, fonts or images (inline CSS/JS/SVG). It's a compact prototype. Don't rebuild every existing dialog. Reuse from `proposals.html`: the tokens, the sample scene (§3 of the brief), the job simulator (phases, cooperative cancel, partial publish on cancel during Writing files, Resume = full AI pass writing only missing frames, one running job), the stale/Resume full-settings rule, the colour-notice behaviour and the self-check harness. Don't touch any other file.

**Must show**
1. Persistent banner: "Prototype — simulated data. No AI runs here. Presets are saved only in this browser (localStorage)."
2. The Board as home, as in §2.2. Import clips… / Import folder… add simulated clips (e.g. `SH080_rooftop` and `SH090_rooftop_alt`, 120 f, Raw, *Extracting* for ~1 s first) and say "simulated import".
3. Multi-select, readiness text, **Open clip settings** → clip workspace (no popup), **← Back to batch** keeps the selection.
4. Five tabs with the bottom graph on every tab, the Properties panel on the right, and a pinned primary action. Node click/keyboard behaviour, Bypass Hint, status badges and the shared-compression label are as in §2.3–2.4.
5. Presets: save/name/rename/delete, Workflow vs Export, the inline apply bar with a diff summary, "(modified)" marker, per-clip *Needs hint* / *Needs keyframe marks* after apply, "Queued with earlier settings" + Requeue. Seed two presets: Workflow "Hair closeups" (GVM Full, Despill 0.65, 48/24, FG EXR + Matte EXR, PIZ) and Export "Comp review PNG" (Comp PNG only).
6. Queue in a Board side panel / workspace strip; Key N clips — one at a time; blocked clips listed with reasons; overwrite note for Complete clips.
7. Footer "Not drawn in this prototype, still in the app:" Welcome/recent projects, Preferences, chroma-key sub-panel detail, SAM2/holdout detail, video export dialog, diagnostics, quit-while-running dialog.

**Also show (revision):** the large Hint viewer and Focus mode, shared viewer chrome on every viewer, consistent Set In (I)/Set Out (O), range-confined Key wording, and the Deliver location fields with a live resolved path. Collision examples: Matte-only with the user's template passes, then enabling FG blocks with **[Add {output} subfolder]**. Two clips named alike are blocked. A range change produces **Write as v002**.

**Must not show:** live re-key, parallel jobs, per-frame inference, any implied resize from `{resolution}`, silent version/suffix changes, model resolution/backend, MLX/macOS, auto-update, arbitrary node add/wire, per-export compression or range.

**Keep (regression of fixes already made, no re-review needed):** 660 px minimum window, internal scrolling and a visible primary action (B1). Space/shortcuts are ignored on form controls and checkboxes (B2). Partial publish on cancel (H1). Resume wording/duration (H2). Full-settings stale/Resume (H3). Keyframes with no marks are blocked (H4). Readiness comes from real state, not navigation (H5). The AA contrast fixes, and no `vh`/`dvh`/`svh` units or `height:100%` on html/body.

**Keyboard:** `B` Back to batch, `1`–`5` tabs (in the workspace), `G` focus graph, ←/→ in the graph = nodes, Enter = open node page, Space play/pause (viewer only), ←/→ frame step outside the graph, I/O/M, K key, F focus viewer, Esc cancel/close sheet/exit focus, `?` sheet. Ignore keys while typing.

**Self-check additions** (footer N/N, `__selfcheck()`): the Board is the initial screen; Open clip settings creates no dialog/overlay element; the graph exists with the same 7 nodes on all 5 tabs; selecting each node changes the Properties title; Bypass sets mode none and restores it; changing compression on FG changes Matte/Processed; disabling all exports blocks Key with a reason; applying a Workflow preset preserves the colour, range, marks and output folder of each target; an Export preset doesn't change Despill; apply while a job is running leaves the job snapshot unchanged; a preset survives reload (localStorage); the primary action and graph lie inside the window at 1440 and 728; no horizontal page overflow at 728; the Hint viewer is ≥280 px high at 1440×900 and larger in Focus mode; every viewer has the 6 mode chips top-right and the wipe top-centre; the Set In/Set Out labels are identical on every page; the resolved path for the user example equals `/mnt/NAS/Project/Masks/clip_name_2K_18-10-2027_v001/clip_name_1101.exr`; multiple outputs without `{output}` are blocked; `..`/absolute templates are rejected; a Workflow/Export preset leaves the location unchanged.

**Acceptance:** self-check all pass at 1440×900 and 728×900. Manual: keyboard-only path import → select 2 → apply preset → open settings → Hint generate → Key → Cancel → Back to batch → Key 2 clips.

---

## 5. Rust implementation milestones (after the prototype is accepted)

Each milestone has an exit check. Don't retire Qt before R7.

| # | Milestone | Scope | Exit evidence |
|---|---|---|---|
| R0 | **Contracts (Python side)** | Freeze the `InferenceParams`/`OutputConfig`/keyframe-sidecar schemas. Version the run manifest to record `exr_compression`, with the legacy compatibility policy in §3.2. Define the versioned `clip.json` `workflow` key, preset JSON (`kind`, `version`, `rev`, `fields`), safe storage recovery and field-merge rules. Move the colour override into `clip.json`. **Range-confined inference** (engine reads only `in..out`; temporal window clamped), range identity with stems, stem-based hint coverage, the manifest range/paths/`clip_id`, and the output-location templates/validation/collision resolver from `range-output-contracts.md`. Golden fixtures. | Python tests: round-trip, unknown-key tolerance, merge never touches source-bound fields, existing Resume dict equality preserved; changed or unknown compression requires a full rewrite under the new rule; unreadable preset stores survive a failed load/save; no frame outside In/Out is read or written; a range change blocks Resume; the template validator rejects traversal and multi-output collisions; the user's example path resolves exactly. |
| R1 | **Benchmark gate** | Playback matrix from `rust-path.md` (incl. every EXR compression, DWAB/PIZ). Toolkit spike per candidate: 660 px Board + workspace + graph strip + Properties, keyboard/focus, 12 px text. | A written result per candidate; the toolkit and playback backend chosen **here**, not earlier. |
| R2 | **Read-only Rust shell** | Open an existing project: Board (states, readiness, thumbnails), workspace tabs, graph showing the current settings, Review playback by frame identity. No writes. | It opens real projects unchanged; matte/alpha/colour are correct against reference frames. |
| R3 | **Settings + presets** | Properties editing per node, Bypass, the shared-compression field, preset store (user dir), save/rename/delete/apply bar, readiness. Writes `clip.json` with backup. | The R0 golden tests pass against Rust-written files; Qt still opens the project. |
| R4 | **Worker + submit** | Headless Python worker over JSON-lines stdio (brief §6), Python-owned queue. Rust submits immutable snapshots, shows phases, cooperative cancel, partial-publish/terminal events, crash rescan. | End-to-end import → key → review → deliver on one clip; cancel during publish leaves Ready·partial; the Rust-serialized params equal Python's. |
| R5 | **Hint page parity** | All routes (GVM, BiRefNet, VideoMaMa, MatAnyone2, Chroma Key + sub-panel, Import Alpha), paint 1/2, holdout, SAM2, keyframes, Reset Hint, Needs-hint resolution. | Parity inventory ticked; hint jobs are queued sequentially. |
| R6 | **Media/Deliver/batch parity** | Import single/multiple/folder, extraction, Welcome/recent, video export, Preferences, quit guard, Key N clips with blocked reasons, Requeue with current settings. | Representative multi-clip project batch-keyed. |
| R7 | **Queue to Rust + retire Qt** | Transfer queue ownership (§3.3), recovery on worker exit, Windows build check. | Representative projects verified; Qt removed only after sign-off. |

Rust-native inference remains out of scope (`rust-path.md` step 6).

---

## 6. Decisions made here (from the `preset-contracts.md` gaps)
1. The Hint node = **route** (how assets are made or imported, per clip) **plus** consumption settings (`hint_mode`, qualities). Generation is its own queued job.
2. Presets are stored per user with a version and copied into clips on apply (field merge), never live-linked.
3. Input colour and output locations are never Workflow/Export preset fields. Shared project output settings and explicit batch application provide location reuse; a separate location preset is optional.
7. In/Out is the processed range for every stage (supersedes whole-clip inference). Range changes invalidate Complete/Resume. Versions and dates change only explicitly.
4. The Export preset covers only the image-sequence `OutputConfig`. Video export stays path-prompted on Deliver.
5. EXR compression stays **one value per clip run**. It moves from the global preference to the clip workflow, with the global preference as the default.
6. Prerequisite failures show as per-clip readiness on cards, nodes and the Key action.

## 7. Open, not blocking the prototype
- Opt-in preset field for video codec (only if the user asks).
- Whether `####` should optionally renumber from 1 (not planned: it would break timeline identity).
- Per-export compression would need an engine/manifest change. Not planned.
- Toolkit/playback choice: decided in R1.
