# EZ-CorridorKey2 UI redesign — design brief (pre-Rust)

Status: planning only. No UI or production code changes. Owner of this file: design sub-agent.
Sibling docs (`exploration.md`, `research.md`) may land later; this brief does not depend on them.
Decision owner: the user picks a layout from the gallery. This brief recommends a shortlist, it does not decide.

---

## 1. What the product actually is today (ground truth, not v1 history)

Source: README fork intro, `backend/service/core.py`, `ui/widgets/parameter_panel.py`, `ui/preview/frame_index.py`,
`backend/clip_state.py`, `backend/job_queue.py`, `backend/corridorkey_v2.py`, `backend/input_colour.py`, `backend/hint_keyframes.py`.
The upstream v1 README below the fork intro is historical. Do not design for anything marked "Removed".

**Hard behaviours every mockup must express honestly**

| Fact | UI consequence |
|---|---|
| **Whole-clip inference.** Every run infers the entire clip. | Never imply "key this frame" or "key this range". The Run button reads e.g. *"Key clip — AI processes all 412 frames"*. |
| **In/Out only controls written output.** Range limits which frames are published; separate In/Out runs do not add up to a finished clip; Resume re-publishes only missing frames from a compatible earlier run. | Range control lives under **Output**, labelled *"Write frames 1101–1300 (200 of 412)"*, never under "Process". Clip only marked Complete after a full write or Resume. |
| **Hints are optional.** `hint_mode`: None / Full AlphaHint / Keyframes (sparse, sidecar `.corridorkey_hint_keyframes.json`), plus *Carry hint* + quality. Sources: GVM, BiRefNet, VideoMaMa, MatAnyone2 (VideoMaMa/MatAnyone2 need paint strokes: 1 = subject, 2 = background); current code also has Chroma Key generator and Import Alpha. | Hint is an optional step with "None (recommended to try first)" as a real choice. Hint generation is its own queued job, separate from keying. |
| **Sequential jobs.** One GPU job at a time; queue of typed jobs (Inference, GVM/BiRefNet/VideoMaMa/MatAnyone2/Chroma hint, extract, stitch). No parallel clips. | Queue shows one *Running*, rest *Queued*. No "run 3 in parallel" affordance anywhere. |
| **Phased progress.** Engine reports phases `rvm → vfm → stage1 → stage2 → publish`. | Progress = phase stepper + per-phase bar + overall ETA. Use plain labels: *Motion prep · Feature pass · Coarse matte · Refine · Writing files*. |
| **Cooperative cancel.** Stops after the current GPU step; quitting mid-job asks first. | Cancel → state "Stopping after current step…" (not instant). Quit dialog when a job runs. |
| **Results/playback ≠ AI inference.** Viewer plays files already on disk (Input, Mask, Alpha hint, FG, Matte, Comp, Processed). Scrubbing never triggers the AI. No live single-frame re-key. | Viewer must visually separate *"Viewing results on disk"* from *"AI job"*. Changing settings marks results **stale** ("Settings changed since last run — re-key to update"); it does not live-update. |
| **Settings that exist.** Input colour sRGB/Linear (+ heuristic notice from sampling first/middle/last frame; never auto-switches), VRAM profile Auto/Low, Temporal window Auto / 80/40 / 48/24 / 24/12 / 8/4 / 1/1 with per-option peak GiB estimate, Despill 0–1, Cutout linear (shown disabled), outputs FG/Matte/Comp/Processed each EXR/PNG, manifest `.corridorkey_manifest.json`. | Default view shows only: Input colour, Hint, Output range, Outputs. Temporal/VRAM/Despill in an "Advanced" disclosure with plain-English help and the GiB estimate. |
| **Clip states.** Extracting, Raw, Masked, Ready, Complete, Error. | Use these as the single status vocabulary (plus job states Queued/Running/Stopping/Cancelled/Failed). Optional friendlier labels: *Importing · New · Hint ready · Ready · Done · Error*. |
| **Not available.** Live re-key, parallel clips, model resolution/backend, MLX, macOS/Apple Silicon/Apple Vision, auto-update. NVIDIA CUDA only. | Must not appear in any mockup, even greyed out. GPU readout shows NVIDIA VRAM. |

**Ordinary-user golden path (all 8 layouts must support it):**
Import clip → (optional) choose/generate hint → confirm input colour → set output range & formats → Key clip (queued) → watch phases / cancel → review results (A/B, wipe, matte) → open output folder.

---

## 2. Visual language (shared by all 8)

Nuke-style muted neutral grey. Low chroma, one restrained accent, status colours desaturated. No brand yellow/green fills (logo only).

| Token | Hex | Use |
|---|---|---|
| `--bg-0` | `#1c1c1c` | app background, viewer surround |
| `--bg-1` | `#262626` | panels |
| `--bg-2` | `#303030` | raised controls, headers |
| `--bg-3` | `#3a3a3a` | hover |
| `--line` | `#141414` / `--line-soft` `#3f3f3f` | dividers |
| `--text` | `#d0d0d0` | body (≈10:1 on bg-1) |
| `--text-dim` | `#9a9a9a` | labels (≥4.5:1 on bg-1 — verify) |
| `--accent` | `#c8873a` | selection, primary action, focus ring (Nuke-ish amber, muted) |
| `--run` | `#6f93c0` | running job |
| `--ok` | `#7c9f6a` | complete |
| `--warn` | `#c4a24f` | stale / notices |
| `--err` | `#c0574b` | failed |
| `--viewer` | `#121212` | image area |

Type: system UI stack (`Inter, "Segoe UI", system-ui`), 13 px base / 12 px min for dense labels, tabular numerals for frame numbers & timecode (`font-variant-numeric: tabular-nums`). Hit targets ≥ 28 px desktop, ≥ 40 px on narrow viewport. Visible 2 px accent focus ring. Status always text + icon, never colour alone. Corner radius 2–3 px, no gradients, no glass.

---

## 3. Consistent sample scene (use in every mockup)

- Project **"Harbour Promo"**, clips:
  1. `SH040_talent_walk` — 3840×2160 EXR (Linear), 24 fps, frames **1001–1412 (412 f)**, state **Ready**, hint **GVM · Full**, In/Out **1101–1300**, outputs FG EXR + Matte EXR + Comp PNG. Selected clip.
  2. `SH050_closeup_hair` — 1920×1080 PNG (sRGB), 168 f, state **Raw**, hint **None**, Keyframes 3 marked (1012, 1060, 1144).
  3. `SH060_wide_crane` — 3840×2160 MOV → extracted, 640 f, state **Complete**.
  4. `SH070_insert_hands` — 2048×1080, 96 f, state **Error** ("Hint has 94 frames, clip has 96 — regenerate hint").
- Queue: Running *Key SH040* (phase **Refine 212/412**, overall 68 %, ETA 4 m 10 s); Queued *GVM hint SH050*; Queued *Key SH050*; Completed *Key SH060*; Cancelled *BiRefNet hint SH070*.
- GPU: RTX 4090, VRAM 17.8 / 24.0 GiB. Temporal Auto → 48/24 (~18.2 GiB est.).
- Colour notice on SH050: *"Sampled peaks ≤ 1.0 in frames 1001/1084/1168 — could be sRGB or linear. Check before keying."*
- Stale example: SH060 results older than current Despill (0.5 → 0.65): *"Results made with Despill 0.50."*
- Viewer image: CSS/SVG-drawn placeholder (grey studio wall, silhouetted figure, checkerboard for alpha). No external images.

---

## 4. The eight proposals (layouts/workflows, not themes)

Each entry: idea · layout · ordinary-user flow · how it shows the critical facts · tradeoffs.

### P1 — Shot Station (single-clip workspace)
Closest evolution of today. Left: clip list. Centre: big viewer + timeline (In/Out output range band, keyframe ticks, cached/written frame strip). Right: inspector with sections *Hint · Input · Output · Advanced*. Bottom: one-line job bar.
- Flow: click clip → inspector top-to-bottom → **Key clip** at inspector foot.
- Facts: timeline has two lanes — "AI processes" (full bar, always whole clip) and "Writes" (In/Out band).
- + Familiar to current users, cheapest migration. − Dense; queue is secondary; batch users scroll.

### P2 — Guided Stepper
Left vertical step rail: **1 Import · 2 Hint (optional) · 3 Check colour · 4 Output · 5 Key · 6 Review**. One step's content + viewer at a time; Next/Back; skippable step 2 with "Skip — key without hint".
- Facts: step 4 explains range-vs-whole-clip with a diagram; step 5 is phase stepper + Cancel; step 6 is pure playback.
- + Best for first-time / occasional users; impossible to miss colour check. − Slow for repeat work; needs "expert: show all" toggle; multi-clip awkward.

### P3 — Batch Board (bin-first)
Grid/list of clip cards with thumbnail, state chip, hint chip, range, output summary. Multi-select → bulk "Apply settings" / "Add to queue". Right drawer: queue. Viewer opens as overlay on double-click.
- Facts: queue is the hero, strictly sequential order with drag-reorder; "Key 3 clips (one at a time)".
- + Great for many shots; clear status overview. − Viewer is second-class; detail tuning needs extra click.

### P4 — Page Tabs (Resolve-style modes)
Top page switcher: **Media · Hint · Key · Review · Deliver**. Each page is a purpose-built layout sharing the clip strip at the bottom. Hint page = paint strokes + generator choice + keyframes; Key page = settings + queue + progress; Review page = big A/B viewer; Deliver = formats, range, output folder, manifest.
- Facts: Review page labelled "Playback of files on disk"; Key page labelled "AI processing". The separation is structural.
- + Clean mental model, scales to experts, each page uncluttered. − More navigation; risk of "where is that setting?"; more screens to build.

### P5 — Review Room (results-first)
Viewer dominates (≈80 %). Bottom: output-mode chips (Input, Hint, FG, Matte, Comp, Processed), A/B wipe, checker/black/white BG, loupe. AI controls live in a slide-in **"Re-key…"** drawer that becomes a job sheet. Stale banner on top of viewer when settings differ.
- + Matches how users spend most time (judging mattes); strong results-vs-inference split. − First run feels hidden; weak for batch; hint painting cramped.

### P6 — Job Ticket (form/recipe)
Each clip opens as a printable-looking "ticket": Source facts (res, fps, frames, colour sample) → Hint → Output → Advanced, with a live plain-English summary: *"AI will process 412 frames of SH040 using the GVM hint, then write FG (EXR), Matte (EXR), Comp (PNG) for frames 1101–1300."* Submit → queue. Small preview thumbnail strip, full viewer separate.
- + Most explicit about what will happen; great for ordinary users and for errors/validation. − Least visual; feels like admin software; reviewing needs another view.

### P7 — Timeline Lanes (NLE-style)
Wide timeline with lanes per clip: *Source · Hint (full/keyframes) · Written output · Stale*. Selecting a lane shows its inspector. Viewer top-centre, queue top-right. Shots laid end-to-end like an edit.
- Facts: hint keyframe diamonds, In/Out output band, written-frame coverage (gaps visible → Resume).
- + Excellent for coverage/resume clarity and keyframes. − Implies editing/sequence semantics the app doesn't have; heavier to build; crowded at many clips.

### P8 — Node Graph (expert option, not default)
Nuke-style DAG: `Read SH040 → [Hint: GVM] → CorridorKey 2.0 → Write FG / Write Matte / Write Comp`. Properties bin on right, viewer top. Nodes are a *view of fixed pipeline*, not freeform compositing (no arbitrary wiring; only hint node swappable/bypassable).
- + Familiar to compositors, shows data flow and what's optional. − Wrong default for normal users; implies flexibility that doesn't exist; offer only as "Expert view" toggle in another layout.

### Comparison matrix (gallery must render this)

| | Ordinary user | Batch | Review | Range/whole-clip clarity | Build cost | Fit to current code |
|---|---|---|---|---|---|---|
| P1 Shot Station | ●●● | ●● | ●●● | ●●● | Low | High |
| P2 Stepper | ●●●● | ● | ●● | ●●●● | Med | Med |
| P3 Batch Board | ●●● | ●●●● | ● | ●● | Med | Med |
| P4 Page Tabs | ●●● | ●●● | ●●●● | ●●● | High | Med |
| P5 Review Room | ●● | ● | ●●●● | ●● | Med | Med |
| P6 Job Ticket | ●●●● | ●● | ● | ●●●● | Low | High |
| P7 Timeline Lanes | ●● | ●●● | ●●● | ●●●● | High | Low |
| P8 Node Graph | ● | ●● | ●● | ●● | High | Low |

**Recommended shortlist to compare (user decides):** **P1 Shot Station** (safe evolution), **P4 Page Tabs** (cleanest results-vs-AI split, scales), **P3 Batch Board** if most work is multi-shot. Worth stealing regardless of choice: P6's plain-English run summary and P2's colour-check step as a first-run nudge. P8 only as an optional expert view.

---

## 5. Implementation brief for Sonnet — interactive HTML gallery

**Output:** one self-contained file `docs/design/rust-ui/gallery.html`. No network, no CDN, no external fonts/images; inline CSS + vanilla JS + inline SVG. Must open from `file://`. Do not touch any other file.

**Structure**
1. Persistent top bar: title, proposal selector (8 tabs, labelled `P1 Shot Station` …), **Compare** tab, viewport switch, help (`?`).
2. Persistent banner: *"Prototype — simulated data and interactions. No AI runs here."* Every simulated control result shows a small "simulated" tag where ambiguous.
3. Each proposal = a full-app mockup at the chosen viewport inside a framed "window", plus a side card: idea (1 line), golden-path steps, pros/cons, how it shows whole-clip / range / hints / queue / cancel / results-vs-AI.
4. Compare view: matrix from §4 (as an accessible `<table>`, dots plus text values for screen readers), shortlist note, small thumbnail of each layout (scaled live DOM or simplified SVG).

**Viewport switch:** Desktop 1920×1080, Laptop 1440×900, Small 1280×720 (scale the window with CSS `transform` to fit the page, keep aspect), plus "Fit" (responsive real width). Gallery chrome itself responsive down to ~1024 px.

**Shared simulated interactions (all proposals, same JS state):**
- Select clip (4 sample clips, §3). Clip state chips update everywhere.
- Hint mode None/Full/Keyframes; generator select; "Generate hint" → queues a hint job.
- In/Out range edit (numeric inputs + draggable timeline band) → live text "Writes 200 of 412 frames · AI processes all 412".
- Key clip → job enters queue; fake progress walks phases rvm→vfm→stage1→stage2→publish (~15 s total, speed toggle), only one job running at a time.
- Cancel → "Stopping after current step…" for ~1.5 s → Cancelled.
- Change Despill after completion → stale banner.
- Viewer mode chips (Input/Hint/FG/Matte/Comp/Processed), A/B wipe slider, play/pause scrub over fake frames (just changes frame counter + subtle image shift) labelled "Playback of files on disk".
- Colour notice on SH050 with "Keep sRGB" / "Switch to Linear" (user choice, never automatic).
- Error clip SH070 shows actionable error text.
- P8: nodes selectable (shows properties), hint node bypass toggle; no free wiring.

**Accessibility & keyboard**
- Semantic landmarks, real `<button>`/`<input>`/`<select>`, `role="tablist"` for selectors with arrow-key nav.
- Keys: `1`–`8` proposal, `C` compare, `V` cycle viewport, `Space` play/pause viewer, `←/→` step frame, `I`/`O` set output in/out at current frame, `K` key clip (simulated), `Esc` cancel job / close drawer, `?` shortcut sheet. Ignore shortcuts while typing in inputs.
- Focus ring visible; contrast AA on all text (check `--text-dim`); `prefers-reduced-motion` disables progress animation easing; `aria-live="polite"` for job status changes.
- Min font 12 px; buttons ≥ 28 px tall.

**Copy rules:** Use the phrases from §1 verbatim where given. Never show removed features. Use realistic numbers from §3, tabular numerals, frame ranges as `1101–1300`.

**Acceptance check:** open via `file://` with network disabled; all 8 + Compare reachable by mouse and keyboard only; no console errors; each mockup visibly answers "does the AI process the whole clip?", "what will be written?", "what's running / queued?", "how do I cancel?", "am I looking at results or running AI?".

---

## 6. Later Rust boundaries (staged, not a promise of Rust inference)

Proposed final split: Rust owns the app, Python owns CUDA during a staged migration. This is not yet an approved architecture. The process boundary is the contract; files on disk (outputs + manifest + sidecars) are the shared truth. During the first integration step, the Python worker retains the existing queue; Rust displays and submits to it. Queue ownership transfers only at the queue-parity milestone, when the worker has no separate pending queue. See `rust-path.md` for recovery and compatibility requirements.

| Layer | Owner | Notes |
|---|---|---|
| UI shell, layout, input, shortcuts | Rust (egui/iced/Slint — decide after layout pick) | Chosen proposal drives toolkit choice (P8 graph or P7 lanes favour immediate-mode egui). |
| Project/clip model, clip state, In/Out, hint keyframe sidecar, manifest read | Rust | Port of `backend/clip_state.py`, `hint_keyframes.py`, `project.py`; keep JSON schemas byte-compatible. |
| Playback / frame cache / display transform / EXR+PNG decode | Rust | Results-only path; never talks to the GPU worker. Mirrors `ui/preview/*`. |
| Job queue (sequential), cancel requests, quit-while-running guard | Rust | Port of `backend/job_queue.py` semantics. |
| Inference (CorridorKey 2.0), hint generators (GVM, BiRefNet, VideoMaMa, MatAnyone2), VRAM/temporal estimates | **Python CUDA worker** | Long-lived subprocess; one job at a time. |

**Worker protocol (proposal):** JSON-lines over stdio. Requests: `hello`, `estimate{clip,params}`, `run{job_id,type,clip,params,range}`, `cancel{job_id}`, `shutdown`. Events: `progress{job_id,phase,cur,total}`, `notice`, `done{job_id,manifest_path}`, `failed{job_id,error}`, `cancelled{job_id}`. `params` = `InferenceParams.to_dict()` (already validated in `__post_init__`). Cancel stays cooperative (checked between GPU steps, as today). Frames are never streamed over the pipe — worker writes files, Rust reads them.

**Stages**
0. Freeze contracts: InferenceParams schema, manifest, keyframe sidecar, progress phases, error codes. Add golden tests on the Python side.
1. Headless Python worker entry point (building on `clip_manager.py` / `backend/service`), driven by the protocol; current Qt UI keeps working.
2. Rust viewer/review app reading existing outputs (no GPU) — validates playback and the chosen layout cheaply.
3. Rust queue + project + worker supervision → feature parity; retire Qt UI.
4. Optional, evidence-gated: evaluate moving individual non-core steps (e.g. chroma key, file publishing) to Rust. Rust-native model inference (ort/candle/TensorRT) is out of scope unless separately proven for quality and VRAM parity.

---

## 7. Open questions for the user (answer after viewing the gallery)
1. Mostly one hero shot at a time, or many shots per session? (decides P1/P4 vs P3)
2. Should hint painting (VideoMaMa/MatAnyone2 strokes) stay in-app, or is generator-only/import acceptable?
3. Keep Chroma Key generator and Import Alpha as hint sources in the new UI?
4. Expert node view wanted at all?
