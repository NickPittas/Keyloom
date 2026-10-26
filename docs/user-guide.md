# Keyloom user guide

Screenshots show the real native app. Some workflow captures predate the Keyloom rename and retain the CorridorKey title; the illustrated controls are unchanged.

This guide follows one shot from import to delivery, then covers batches, settings and models. For rendering on several computers see [Network rendering](network-rendering.md).

Contents: [Start](#1-start-a-project) · [Import](#2-import-clips) · [Batch board](#3-the-batch-board) · [Media](#4-media) · [Hint](#5-hint) · [Key](#6-key) · [Review](#7-review) · [Deliver](#8-deliver) · [Batches](#9-key-many-shots-at-once) · [Settings](#10-settings) · [Models](#models) · [Keys](#keyboard-shortcuts)

## 1. Start a project

On first launch you see the welcome screen.

![Welcome](screenshots/native/01-welcome.png)

- **Open project…** opens an existing project folder.
- **New project…** creates an empty project.
- **Recent projects** lists projects you opened before. A missing folder is shown as *Missing*.
- **Open a project by path** lets you paste a folder path.

The same commands are in the **File** menu (**New project…**, **Open project…** `Ctrl+O`, **Save project** `Ctrl+S`, **Quit** `Ctrl+Q`). The top bar shows **Saved** or **Unsaved**. Most edits are saved as you make them; **Save project** also stores your workspace (open clip, page, view, frame).

### New project

![New project](screenshots/native/02-new-project.png)

Choose a parent folder, then give the project a name in the **New project** window and press **Create project**. An empty project folder is created inside the folder you picked.

### Project output folder

The bar under the menu shows **Project output folder…**. New renders go under this folder. Existing outputs stay where they are. With nothing set, each clip uses its own default output folder.

## 2. Import clips

Use **Import clips…** (movies, several at once), **Import folder…** (movies plus image sequences found in the folder and its sub-folders), or drag files and folders onto the window. On Linux, drag and drop needs X11 (the app starts through XWayland when it can).

![Import plan](screenshots/native/03-import.png)

Nothing is written until you press **Import N clips** in the plan. The plan lists each clip, what happens to it, and any file that is skipped and why. The **Destination** section shows exactly where the files will go:

- **Add to the open project**
- **Add to another project…**
- **New project in folder…** (the new project folder is `<chosen folder>/<yymmdd_hhmmss>_<name>`)

What import does:

- Movies are **copied into the project** and their frames are extracted to EXR.
- PNG and EXR **image sequences are referenced in place**; your source folder is never changed. JPEG, TIFF and DPX sequences are refused.
- **Cancel import** stops after the current step. A movie that was not finished keeps its copy and shows **Resume** on its card.

## 3. The batch board

Opening a project shows the batch board; import may open the new clip directly. Use **Back to batch** to return.

![Batch board](screenshots/native/04-batch.png)

The captured queue shows results from earlier work; no render was submitted for this screenshot.

- One card per clip, with thumbnail, name, range and a summary of what exists (frames, hint, results).
- **All** and the per-clip checkboxes select clips; **N selected** shows the count.
- **Open clip** opens the one selected clip (it needs frames).
- **Assign selected to** + **Apply to N** choose which computer renders the selected clips (see [Network rendering](network-rendering.md)).
- **Preset** and **Key selected N clips…** start a batch (see [section 9](#9-key-many-shots-at-once)).
- The **Queue** panel on the right tracks work. **Earlier local tasks** holds history.
- **Import clips…**, **Import folder…**, **Open another project…** and **Relink missing footage…** are on the board.
- **Clients** and **Settings** are in the top bar.

## 4. Media

Open a clip. The workspace has five pages above the viewer, **Media · Hint · Key · Review · Deliver** (keys `1`–`5`). A node graph shows the chain Media → Pre-matte → Hint → Key → outputs; click a node to jump to its page. **Properties** holds the settings of the page; **Focus (F)** enlarges the viewer; `G` shows or hides the graph.

![Media](screenshots/native/05-media.png)

The viewer is shared by all pages: mode chips in the top right (**Input, Hint, Guide, FG, Matte, Comp, Processed**), **A/B wipe** at the top, transport and a scrubbable timeline below.

On the Media page:

- **In/Out range:** press `I` and `O` (or use Set In / Set Out) on the frame you want. Everything that follows (hint, Key, delivery) is confined to this range. If you set nothing, the whole clip is used.
- **Input colour** (in Properties): tell Keyloom whether the frames are **Linear** or **sRGB**. Check it; it changes the result.
- **Extract frames from this movie** if extraction has not finished.
- **Relink movie…** or **Relink missing footage…** if the source moved. The size, rate and frame count are checked first; a partial match needs your confirmation.
- **Import alpha or mask image sequence…** brings in a companion matte as **Alpha hint (AlphaHint)** or **Mask (VideoMamaMaskHint)** (PNG/EXR, same names as the frames).

## 5. Hint

The hint is a rough matte that tells Key what is foreground. It has two stages: the **Pre-matte** (a first guess) and the **final Hint** (optionally refined).

![Hint](screenshots/native/06-hint.png)

**Pre-matte** (`Producer`):

| Producer | How it works |
|---|---|
| SAM | Add points, a box or paint on the frame (**Brush** size in Properties). **Preview frame** shows the result without writing; **Track prompts** follows them across the In/Out range. |
| GVM | Automatic. **Generate pre-matte**. |
| BiRefNet | Automatic; choose the variant. **Generate pre-matte**. |
| Chroma key | Pick **Green** or **Blue**, optionally pick the screen colour on the frame, adjust **Strength**, **Clip black/white**, **Shrink / grow**, **Edge blur**. |

**Final hint** (`Method`):

| Method | How it works |
|---|---|
| Generated mask (direct) | The pre-matte is used as the hint as is |
| MatAnyone2 | Refines the pre-matte over the range (seeded from its first frame) |
| VideoMaMa | Refines the pre-matte in chunks (**Chunk size**) |
| Imported alpha | Use a matte you imported; **Choose alpha / mask movie…** then **Import movie** |

Press **Generate hint** to produce it. Other controls:

- **Mark frame / Unmark / Clear marks** place sparse keyframe marks (used by Key's keyframe mode). **On marked frames** (under **More…**) generates only on marks.
- **Overlay**: Off, Pre-matte or Hint on the viewer.
- **Mask cleanup**: **Grow / shrink**, **Fill holes**, **Feather**.
- **Bypass hint** keeps every setting and asset but tells Key not to use the hint.

The previous hint stays in place until the new one is complete. Generating only touches frames inside In/Out.

Companion alpha/mask *movies* are imported here (**Choose alpha / mask movie…**); companion *sequences* are on the Media page.

## 6. Key

Key creates the final foreground, matte, comp and processed images from your frames and hint.

![Key](screenshots/native/07-key.png)

The Key settings are in Properties:

- On **Hint → Hint consumption**: **Hint mode** (none / full / keyframes), **Hint quality**, **Carry hint** between windows, **Carry quality**.
- **Key engine:** **Temporal window** (Auto, 80/40, 48/24, 24/12, 8/4, Off 1/1; each shows a plain explanation), **VRAM profile**, **Despill**.
- **Outputs:** **FG, Matte, Comp, Processed**, each with a **Write** checkbox and format (**exr** or **png**), plus **EXR compression**. DWAB is lossy; use piz, zip or none for exact data.
- **Render client:** which computer runs the job (see [Network rendering](network-rendering.md)).

Start with **Key clip**. Progress and **Cancel** appear in the same place. Choose **Force stop worker** only if Cancel does not respond; frames already written stay.

### New, Resume and Replace

Key never overwrites a result silently. If the clip already has one, open **Output & result actions**:

| Action | When |
|---|---|
| **Key as vNNN / Write as vNNN** | Same settings, next free version folder. Needs `{version}` in the folder template (Deliver). |
| **Resume previous run** | Finish an interrupted local run. Not available when the pipeline regenerates the hint. |
| **Replace result (N files)** | Overwrite only the files that result owns, after you confirm **Yes, replace**. Old files stay until the new ones are verified. Local only. |

If a previous run left a lock (for example after a crash), the same section explains it and offers a confirmed release.

## 7. Review

The Review page is for inspecting what you got. Use the viewer:

![Review](screenshots/native/08-review.png)

- Mode chips: **Input**, **Hint**, **FG**, **Matte**, **Comp**, **Processed**.
- **A/B wipe** compares two views (pick the second view in the box next to it). Drag the divider.
- `Space` play, `←/→` step (hold `Shift` for 10), `Home/End`, `L` loop, wheel or `+` `-` zoom, `0` fit.
- Missing frames show *No file on disk*; frames outside your range show *Outside In/Out*.
- HDR values above the display range are clipped for display and flagged.

## 8. Deliver

Each enabled output (FG, Matte, Comp, Processed) has its own card.

![Deliver](screenshots/native/09-deliver.png)

- **Deliver as:** **Image sequence** or **Video file**.
- For a video: **File type** **MP4** (H.264 or H.265/HEVC, no transparency), **MOV** (ProRes 422 or ProRes 4444, keeps alpha) or **WebM** (VP9, keeps alpha), plus a **Quality** setting per output.
- **Video options:** **Frame rate** (**Use source**), **Range** (**Export the partial result**), **Folder** (**Choose…** or **Project default**).
- **Compression** applies to all EXR sequences.
- **Run full workflow** runs this shot's whole pipeline: pre-matte → hint → Key → every enabled video. It asks before re-running if results exist.
- **Export existing Key result** encodes your videos from the current Key result **without** running Key again.
- Videos have no audio. File names are `<clip>_<output>_<in>-<out>.<mp4|mov|webm>`, and an existing file is **never** replaced.
- **Save output location** / **Reset to default** edit where results go (root, folder template and file template).

**Save current as preset** (in the preset area; type a **Name**) stores your Hint, Key and Deliver choices for batches.

## 9. Key many shots at once

On the batch board:

1. Tick the clips.
2. Choose a **Preset** (or **Current settings (no preset)**).
3. Press **Key selected N clips…**.

![Batch confirmation](screenshots/native/14-batch-confirm.png)

**Review batch** shows, per clip, the frame range, a **new result** with a **queue** checkbox (existing results offer **skip**, **write as vNNN**, **resume** or **replace**), the steps that run first (for example *first: GVM > Key*), the export products and the folder they go to.

- Each shot keeps its **own** In/Out, input colour and hint prompts. A shot that has its own overrides shows **keeps this shot's own: …**; press **Use preset values** to take the preset instead.
- SAM and Chroma pre-mattes must already cover the clip's frames; otherwise the clip is blocked with an instruction. GVM and BiRefNet generate theirs automatically.
- **Render client** (per shot) and **Run on** (whole batch) choose the computer; see [Network rendering](network-rendering.md).
- **Queue N clips** starts the work.

![Batch progress](screenshots/native/15-batch-progress.png)

Example from the earlier real two-PC run.

In the **Queue** panel each clip has its own section with ordered rows: **Pre-matte, Hint, Keying, Export**, each with a progress bar (for example *45% · 11/24*), *Waiting* or *Running*, and a **Cancel** beside live stages. Videos share the Export row. **Details** keeps warnings and recovery options. CPU, RAM, GPU and VRAM bars show how busy each computer is. Cancelling is cooperative: the worker stops at a safe point and frames already written are kept.

## 10. Settings

**Settings** (top bar). Categories on the left; **Apply settings** saves, **Cancel** discards, **Reset defaults** restores.

- **Playback:** decoded-frame RAM budget (0 = Auto), decode workers, default looping.
- **Interface:** UI scale, and [background running](#background-running).
- **AI defaults:** starting values for new clips (VRAM profile, hint mode, temporal awareness, hint and despill strength, linear input). Choices already made on a project or clip are not replaced.
- **Path mapping:** how shared storage is named on this computer, for network rendering ([details](network-rendering.md#4-map-shared-storage-on-each-pc)).

  ![Path mapping](screenshots/native/10-path-mapping.png)
- **AI worker targets:** a local checkout. Normal installs need nothing here; other PCs are used through **Clients** (top bar).
- **Masks and hints:** default SAM2 tracker model.
- **Paths and info:** read-only locations of preferences, runtime and Python.
- **Models & runtime…** button: see below.

### Background running

![Background setting](screenshots/native/11-background.png)

Under **Settings → Interface**:

- **Keep running when the window is closed** – closing the window hides Keyloom so the network client stays available. Starting Keyloom again just shows the running window (only one copy runs). **File → Quit** really quits.
- **Start in the background when I sign in to this computer** – starts hidden when *you* sign in (not at boot, no system service), so your mapped drives (for example `R:` or `Y:`) exist for the same user. Turning it off removes only the entry Keyloom created.

## Models

**Settings → Models & runtime…** (also **Models…** when the status bar says *Models need attention*) shows every model with its status. **Install full profile** downloads everything (large, resumable); **Install required models** the minimum; **Verify all** re-checks files; **Refresh status**; **Cancel operation**. Model changes are blocked while Key work is running; stop workers first.

## Keyboard shortcuts

| Key | Action |
|---|---|
| `B` | Back to the board |
| `1`–`5` | Media, Hint, Key, Review, Deliver |
| `G` | Show/hide the node graph |
| `F` | Focus the viewer |
| `P` | Properties (narrow windows) |
| `Space` | Play/pause |
| `←` `→` | Step one frame (`Shift` = 10) |
| `Home` `End` | First/last frame |
| `I` `O` | Set In / Set Out |
| `L` | Loop In/Out |
| `0` | Fit |
| `Esc` | Leave focus mode or a sheet |
| `Ctrl+O` `Ctrl+S` `Ctrl+Q` | Open, Save, Quit |

Shortcuts are ignored while you are typing in a field.
