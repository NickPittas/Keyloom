# Range and output contracts (TARGET)

Status: proposed implementation contract for the user's range and output requirements of 2026-10-22. The selected-range requirement **supersedes** the earlier rule "inference is always whole-clip; In/Out limits only written frames". The native/backend behavior is not implemented yet. "Today" rows describe the current Python code; "Target" rows describe the intended Rust app and R0 Python changes. Detailed versioning and recovery policies below are design proposals, not separate user decisions.

## 1. In/Out = the processed range

| | Today (Python) | Target |
|---|---|---|
| Storage | `clip.json` `in_out_range {in_point, out_point}`, zero-based, inclusive, `None` = whole clip (`backend/project.py`) | Same fields, plus `in_stem` and `out_stem` (the source frame stems at those indices). Identity = `(source fingerprint, in_point, out_point, in_stem, out_stem)`. If a stem no longer matches its index, the range is **invalid** ("Source changed: re-set In/Out"). It is never shifted silently. |
| Hint generation (GVM, BiRefNet, VideoMaMa, MatAnyone2, SAM2, mask export) | Already range-limited: `_selected_sequence_files`, `start_index = in_point`, SAM2 prompts limited to `allowed_indices` | Unchanged. This becomes the rule for every stage. |
| Key inference | **Whole clip** is inferred for temporal context. `frame_range` only limits publication (`InferenceMixin` docstring, `_v2_publish`) | Only frames `in..out` are read and inferred. The temporal window is clamped at the range edges: no frame outside the range is ever read, padded from, or written. Edge frames may differ from a whole-clip run. That is expected, and it is why a range change invalidates results (§3). |
| Export / video export | Image outputs are published for the range. Video export reads what is on disk. | Image and video exports cover exactly `in..out`. |
| Review | Plays the whole timeline | Plays the whole source. Output views outside the range show "Outside In/Out" instead of "No file on disk". |

UI labels are identical everywhere: **Set In (I)**, **Set Out (O)**, **Clear In/Out**. The range readout is "Range 1101–1300 · 200 of 412 frames" (displayed numbers = source frame numbers, see §4). Don't add "= playhead" text.

## 2. Hints and marks inside the range

- The dense hint (AlphaHint) must cover every frame in the range, matched by source stem, not by count. Today `clip_state` compares counts only (`alpha_asset.frame_count < in_out_range.frame_count`). The target matches by stem, and a mismatch shows the existing *Error: hint has N frames…* readiness, naming the missing stems.
- Keyframe marks and paint/SAM prompts outside the range are **kept** in the sidecar/`annotations.json`. The job ignores them and the timeline greys them out. Nothing is deleted when the range shrinks.
- Keyframes mode needs at least one mark **inside** the range, otherwise the clip shows *Needs keyframe marks (none inside In/Out)*.
- A hint generated for an older range still counts if its stems cover the new range. If they don't, the clip shows *Needs hint (range changed)*.

## 3. Completion, stale and Resume

- The manifest (version bump in R0) records `range {in_point, out_point, in_stem, out_stem}` (or `null` = whole clip), `exr_compression`, the resolved output paths/templates, `clip_id`, and `job_id`.
- *Complete* means the manifest is `complete: true` **for its recorded range**, and every frame of that range is present for every enabled output. Today only a full-clip manifest (`frame_range is None`) can be Complete. That restriction goes away.
- Changing In/Out after a run makes the clip **Stale (range changed)**. Resume is offered only when the range identity, all settings (the existing dictionary-equality rule plus compression), and the resolved output paths are all identical. A range change is never resumable, because with confined temporal context the edge frames differ.
- Re-keying a stale clip into the **same** resolved folder is blocked. The two options are:
  - **Write as v002** (default; bumps `{version}` explicitly and shows the new path).
  - **Replace v001**, an explicit overwrite of the app-owned prior result. Its native implementation needs staged writes and recoverable publication; do not delete a good result before its replacement succeeds. Files outside the prior manifest are never removed. The prototype must not imply it has inspected or deleted real files.

## 4. Output root and naming

**Location (not a processing setting):** `output_root` (an absolute path) + `folder_template` + `file_template`.
- Owner: project (`project.json`), with a per-clip override (the existing `custom_output_dir` becomes a per-clip root override).
- Workflow and Export presets **never** carry these.
- A separate **Output location preset** (root + both templates + date format) is an optional later extension. The current revision needs shared project output settings and explicit batch application with one resolved example path per target clip; it does not require a third preset manager.
- Old projects without templates keep today's layout (`<clip output dir>/FG|Matte|Comp|Processed/<source stem>.<ext>`). On the Deliver page it is labelled "Legacy layout".

**Tokens** (a closed set; any other `{…}` is an error):

| Token | Value | Notes |
|---|---|---|
| `{project}` | project name | |
| `{name}` | clip name | |
| `{output}` | `FG`, `Matte`, `Comp`, `Processed` | Required when more than one output is enabled (see Collisions). |
| `{resolution}` | per-clip text. The default label comes from **source** width: `<1920` → `{height}p`, `1920–3199` → `2K`, `3200–4999` → `4K`, `≥5000` → `{round(w/1000)}K` | **Label only. The app does not resize.** The field shows "Source 1920×1080 · label only, no resize". The value is editable. |
| `{date}` | per-clip stored string, default = today in the project date format (default `DD-MM-YYYY`) | Set once when the clip's output is first configured, editable, and **never re-evaluated** on requeue/Resume. |
| `{version}` | per-clip integer rendered `v%03d`, default 1 | Changes only through explicit actions (edit, **Next free version**, **Write as v002**). Never auto-incremented silently. |
| `####` (file template only, exactly one run) | source frame number, zero-padded to the number of `#` | For numbered sequences this is the stem's trailing number. For video-extracted frames it is the extracted frame number. It is **not** renumbered from 1, so files line up with the timeline. A number wider than the padding grows; it is not truncated. |

The extension is added from the output's format (`.exr`/`.png`). The file template contains no extension.

**User example (Matte only):** root `/mnt/NAS/Project/Masks`, folder `{name}_{resolution}_{date}_{version}`, file `{name}_####`. The result is
`/mnt/NAS/Project/Masks/clip_name_2K_18-10-2027_v001/clip_name_1101.exr` … `clip_name_1300.exr`.

**New-project defaults:** folder `{name}_{version}/{output}`, file `{name}_####`.

**Validation (at edit time and again at submit):**
- The root is absolute, exists or can be created, and is writable (tested at submit).
- Templates: no absolute path, no `..` or `.` segment, no empty segment, no unknown token. The file template has exactly one `####` run and no `/`.
- Token *values* are sanitized: `/ \ : * ? " < > |` and control characters become `_`, and leading/trailing dots and spaces are trimmed. A value that becomes empty, `.` or `..` is an error naming the clip. Windows reserved names (`CON`, `NUL`, `COM1`…) are an error. Each segment is at most 255 bytes.
- The resolved path must stay under the root after normalization. This is rechecked on the final string.

**Collisions (blocking, never silent suffixes):** before Key/queue, resolve every `(clip, output, frame)` path for the submission **plus** all queued and running jobs.
- Two enabled outputs on one path: "FG and Matte would write the same files. Add `{output}` to the folder or file name" with the explicit fix **[Add {output} subfolder]**. A template without `{output}` is valid only when exactly one output is enabled (the user's Matte-only example).
- Two clips on one path (e.g. the same `{name}` in two projects with a shared root): blocked, and both clips are named.
- An existing folder whose manifest belongs to another `clip_id`, or that holds non-CorridorKey files: blocked, with the offer **Next free version**.
- The same clip, same version, and compatible manifest is Resume. The same clip and version with an incompatible manifest follows §3.
- The manifest lives in the deepest folder shared by all of the job's enabled outputs.

## 5. Queue snapshot

Freeze at submit: the range identity, the ignored-mark set, and the **fully resolved** absolute paths per output (the strings, not the templates), along with the templates, the `{date}`/`{version}` values, and the existing params/output snapshot. Editing the range, templates, date or version later never changes a queued or running job. Instead the job is badged "Queued with earlier settings" and offers **Requeue with current settings** (existing rule). At dispatch, revalidate the frozen paths for collisions and writability, and the range stems against the source.

## 6. Minimum UI fields (Deliver page / Output node properties)

Output root [Browse] · Folder template · File template · Resolution label · Date · Version [Next free version] · a live **resolved example path** for the first enabled output and first range frame · a collision/validation message under the fields. Nothing else is required for v1.
