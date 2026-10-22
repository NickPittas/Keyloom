# AI implementation brief: Key one clip, local and remote workers

Status: implementation contract, 2026-10-22. Planner: Opus (design only, no production edits). Approved by the user:

- The **existing Python/PyTorch models** run in a **local worker process**. A Python-free runtime is out of scope.
- **Remote machines** (added later in the same request): run models on other machines, assign jobs per machine or automatically, manage batch outputs, and study splitting work across machines.
- `range-output-contracts.md` still applies in full. It wins over older wording in `rust-path.md` and `selected-design.md`. Where this brief narrows that contract for the first slice, the narrowing is listed in §10 with its later phase. Nothing in it is dropped.

Ownership while this lands. The import agent (Sonnet) owns `rust-ui/src/ui/**`, `rust-ui/src/import.rs` and `rust-ui/tools/ck_import.py` until its drag/drop fix merges. The backend agent starts now on the Python files only (§9). Rust work that touches `ui/` waits for that merge.

### Parent implementation decisions (supersede conflicting proposals below)

- Honor selected EXR compression, re-encoding when necessary; record the actual compression. B1 is resolved.
- Persist a random stable `clip_id` on first Key with an atomic, unknown-field-preserving update. B2 is resolved.
- Never use check-then-`os.replace` as a no-clobber fallback. If atomic no-clobber publication is unavailable, fail clearly and preserve existing files.
- A stale heartbeat alone does not authorize taking a lock or redispatching a writer. Require proof of termination or explicit recovery with ownership fencing; v1 may conservatively refuse.
- Validate resolved output containment against symlinks as well as traversal, and reject source/output overlap before writing.
- Range confinement includes dimension probes and hint preparation, not just the engine directory. Tests must detect out-of-range image reads in those helpers.
- Whole-clip batch submission belongs with remote scheduling in this requested implementation, not an optional later parity task. Transfer/shared-storage support depends on the user's setup; deployment to real machines needs their connection details.

---

## 0. Decisions in one screen

| Topic | Decision |
|---|---|
| Scheduler | **One scheduler, in the Rust app**, from the first slice. A worker runs **exactly one job at a time** and has no queue of its own. The Python `GPUJobQueue` is not used by the native app. The staged “Python queue first, Rust later” plan in `rust-path.md`/`selected-design.md` §3.3 is superseded: remote targets need one dispatcher that can see every machine. |
| Worker | `python -m backend.worker` (new). The same JSON-lines protocol over stdin/stdout for every target. It stays alive across jobs so the model stays loaded. |
| Target | A target is `(host, gpu index)`. Local = spawn the worker. Remote = spawn the **same command through the system `ssh` client** (§7). No HTTP server, listening port or new Rust network crate in v1. |
| Range | Only `in..out` is read, staged, inferred and published. This is enforced in a **new isolated entry point** (`key_job.py`). The Qt path (`run_inference`) is unchanged (§2). |
| Output | The Rust app resolves every output path before submit. The worker never expands templates. It writes **no-clobber** and never replaces a file it did not write in this job (§4). |
| Splitting | v1 distributes **whole clips** only. Intra-clip temporal chunks are a quality study first (§8); no naive splitting. |
| GPU tests | Not run yet. They need a separate GPU-isolated wrapper (§9.3). |

---

## 1. First slice: Key one clip end to end (local target)

User path: open a clip → Key tab/Key node Properties → set params and outputs → **Key clip** → phase progress with Cancel → Review shows new frames as they appear → Complete for the recorded range.

### 1.1 Controls (Key node, Hint node, export nodes)

Only fields the engine already has. Same names, defaults and validation as `backend/service/core.py`. Mirror them in Rust; never invent fields.

| Node | Control | Field | Values |
|---|---|---|---|
| Media | Input interpretation | `input_is_linear` | bool (per clip, existing auto-detect/override) |
| Hint | Hint mode (Bypass = none) | `hint_mode` | `none` / `full` / `keyframes` |
| Hint | Hint quality | `hint_quality` | 0–1 |
| Hint | Carry hint, carry quality | `carry_hint`, `carry_hint_quality` | bool, 0–1 |
| Key | Temporal window | `temporal_window` | `auto`, `80/40`, `48/24`, `24/12`, `8/4`, `1/1` |
| Key | VRAM profile | `vram_profile` | `auto` / `low` |
| Key | Despill | `despill_strength` | 0–1, step 0.1 |
| FG/Matte/Comp/Processed | Enabled, format | `<o>_enabled`, `<o>_format` | bool, `exr`/`png` |
| any EXR export | EXR compression, “Shared by all EXR exports” | `exr_compression` | `dwab`/`piz`/`zip`/`none` |

`cutout_linear` and `use_alpha_hint` are not shown. They are sent with their `to_dict()` values so the dictionaries stay equal.

**Persistence (new, versioned, unknown keys preserved, atomic replace):**
- `clip.json` → `"workflow": {"version": 1, "params": {InferenceParams.to_dict()}, "output_config": {OutputConfig.to_dict()}}`.
- `clip.json` → `"output_values": {"resolution": "2K", "date": "18-10-2027", "version": 1}` (written once, when output is first configured).
- `project.json` → `"output_location": {"root": "/abs", "folder_template": "...", "file_template": "...", "date_format": "DD-MM-YYYY"}`.
- If `output_location` is absent, the clip uses the **Legacy layout**: `<ClipEntry.output_dir rule>/FG|Matte|Comp|Processed/<stem>.<ext>`. The Rust loader already computes that directory (`project.rs`). Legacy paths still go through the same “resolved path list” interface (§3), so the worker has one code path.

### 1.2 Readiness gate before Submit (Rust, pure functions, unit-tested)

The gate blocks Submit with a reason. It never fabricates assets.

1. `RangeIdentity::validate(stems)` passes (`contracts.rs`). Otherwise: *Source changed: re-set In/Out*.
2. At least one output is enabled.
3. `hint_mode=full`: the AlphaHint covers every **range stem** by stem match (the legacy ordinal rule stays as a fallback). Otherwise: *Needs hint (range changed)* naming the missing stems.
4. `hint_mode=keyframes`: at least one mark inside the range. Otherwise: *Needs keyframe marks (none inside In/Out)*.
5. Paths resolve. There is no collision between outputs, with any queued/running job, or with an existing folder whose manifest has a different `clip_id` or that holds non-CorridorKey files. The outcomes follow `range-output-contracts.md` §4: **Next free version** / **[Add {output} subfolder]** / Resume / **Write as v002**. Replace v001 is not in v1 (§10).
6. Resume is offered only if the existing manifest is compatible (§5).

---

## 2. Range-confined inference (the authorized engine change)

**Today:** `prepare_v2_inputs` stages the whole clip, so the engine reads every frame. `frame_range` only limits `_v2_publish`.

**Fix: a new isolated entry point.** `run_inference` keeps its current Qt behavior. Switching Qt to confined ranges is a separate decision (§10).

`backend/service/v2_inputs.py`: add one keyword. The default keeps today's behavior.

```python
def prepare_v2_inputs(clip, *, use_alpha_hint, staging_dir, check_cancel=None,
                      hint_mode=None, frame_range: tuple[int, int] | None = None) -> V2PreparedInput
```

With `frame_range=(a, b)`:
- **Input:** `stems = all_stems[a:b+1]`. Always stage, even if the listing already matches: `_stage_files(files[a:b+1], staging/input)` writes symlinks named `000000…`. The engine's `frame_paths()` glob then sees only the range. `_probe_clip` and the Hann scheduler also read only that directory, so the temporal windows are clamped to the range edges automatically.
- **Video input asset** (legacy Qt projects only; native imports are extracted sequences): v1 raises `V2InputError("Range keying needs an image sequence; re-import or extract this clip")`. Range staging of video sources follows in a later phase (§10).
- **Dense hint:** match hint files to the **range stems** with `_hint_by_stem`/`_pick`. Files outside the range are ignored. This is the new rule “an older hint still counts if its stems cover the range”. `_match_hints`' `len(files) != total` check does not apply in range mode. Keep the ambiguity error. Always stage as `{i:06d}` (relative index). A hint *video* is decoded for frames `a..b` into PNGs with the existing `_write_matte`. The size/aspect checks stay as they are.
- **Keyframes:** filter marks to `a <= k <= b` and rebase them to `k - a` for the engine. If none remain, raise the readiness error. Marks outside the range stay in the sidecar untouched.
- `V2PreparedInput.source_stems`/`frame_count` are the range values. Add `range_offset: int = a` so that callers map back to absolute indices: `abs = rel + range_offset`.

`_v2_check_disk` must use the range frame count (a new `count` argument). Otherwise a short range on a long clip fails falsely.

**Golden tests (CPU, FakeEngine, required before any GPU run):**
- The FakeEngine records `sorted(os.listdir(input_path))` and the resolved symlink targets. The assertion: the targets are exactly `files[a..b]`, and no out-of-range path is ever opened. Also wrap `builtins.open`/`imread_unicode` in a spy that fails on any out-of-range source path during prepare and publish.
- Dense hint covering the whole clip + a range → only the range is staged. A hint missing one range stem → an error naming that stem.
- Keyframes `[3, 50]` with range `10..60` → the engine gets `[40]`. Range `0..9` → *none inside In/Out*.
- `frame_range=None` → byte-identical `V2PreparedInput` compared with today (regression for Qt).

Edge frames may differ from a whole-clip run. That is expected and documented (contract §1).

---

## 3. Worker protocol v1 (JSON lines; one object per line)

Pattern copied from `ck_import.py`: stdout is protocol only (`sys.stdout = sys.stderr` after capture), logs go to stderr, and stdin EOF means cancel and exit. Every message has `"v": 1`.

### 3.1 Worker → app

```jsonc
{"v":1,"event":"hello","protocol":1,"worker_rev":"<git sha or 'unknown'>","engine":"corridorkey-v2",
 "host":"<hostname>","gpu":{"index":0,"name":"NVIDIA GeForce RTX 4090","vram_gb":24.0} | null,
 "models":{"corridorkey_v2":{"path":"…","bytes":123,"sha256":"…"|null}},
 "capabilities":{"key":true,"generate_hint":false,"publish_modes":["final"]}}
{"v":1,"event":"accepted","job_id":"…"}
{"v":1,"event":"rejected","job_id":"…","code":"busy|protocol|invalid|paths|exists|range|hint","message":"…"}
{"v":1,"event":"status","job_id":"…","message":"Loading model..."}
{"v":1,"event":"progress","job_id":"…","phase":"stage1|refine|…|publish","done":12,"total":200}
{"v":1,"event":"warning","job_id":"…","message":"…"}
{"v":1,"event":"published","job_id":"…","stems":["1101","1102"]}          // batched, ≤ 4 Hz
{"v":1,"event":"profile","job_id":"…","hann_chunk":80,"hann_stride":40,"low_vram":false}
{"v":1,"event":"done","job_id":"…","outcome":"complete|cancelled|failed",
 "message":"…"|null,"error_kind":"oom|input|disk|engine|internal"|null,
 "manifest":"/abs/.corridorkey_manifest.json","published":198,"skipped":2,"expected":200,
 "manifest_complete":true}
```

`done` is always sent, including after cancel or failure. If the worker dies without it, the app rescans (§5.3).

### 3.2 App → worker

```jsonc
{"v":1,"cmd":"key","job":{ …KeyJob… }}
{"v":1,"cmd":"cancel","job_id":"…"}
{"v":1,"cmd":"shutdown"}
```

### 3.3 `KeyJob`, frozen at submit (Rust `serde` struct = Python dataclass)

```jsonc
{
  "job_id": "uuid4",
  "clip_id": "<stable id: clip folder path relative to project + project uuid>",
  "clip_root": "/abs/clip",                    // worker builds ClipEntry from this (existing loaders)
  "range": {"in_point":1100,"out_point":1299,"in_stem":"1101","out_stem":"1300"},
  "params": { InferenceParams.to_dict() },
  "output_config": { OutputConfig.to_dict() },
  "hint_keyframes_abs": [1150] | null,          // the marks used, absolute; worker re-checks the sidecar
  "outputs": {                                  // only enabled outputs; one path per range frame, in order
    "matte": ["/mnt/NAS/…/clip_name_1101.exr", "…"],
    "fg": ["…"]
  },
  "manifest_dir": "/abs/deepest/common/folder",
  "mode": "new" | "resume",                     // resume: skip compatible published frames (§5)
  "publish": "final",                           // v1; "staging" reserved for transfer mode (§7.3)
  "path_map": [] ,                              // remote only: [["/local/prefix","/remote/prefix"]]
  "require": {"protocol":1,"models":{"corridorkey_v2":"<sha256|null>"}}
}
```

The worker revalidates everything before writing. It recomputes the range identity from disk stems, checks each `outputs[o]` length against the range length, and checks that every path is absolute, under no `..`, and ends in the format's extension. It checks the manifest ownership (§4) and that the job's hint mode is still satisfiable. On failure it sends `rejected`, writes nothing, and the scheduler marks the job **Failed (not started)**.

**Cross-language golden fixtures:** `tests/fixtures/worker_protocol/*.json` (key_new, key_resume, legacy_layout, rejected_paths). The Python tests load them into the dataclass and assert `InferenceParams.from_dict(job.params).to_dict() == job.params`. The Rust tests deserialize, re-serialize and compare as `serde_json::Value`, so key order and whitespace may differ but the values may not.

---

## 4. Publication, no overwrite, manifest v3

New module `backend/service/key_job.py`:

```python
@dataclass(frozen=True)
class KeyJob: ...                  # §3.3, from_dict() rejects unknown/missing required keys

def run_key_job(service, job: KeyJob, *, emit: Callable[[dict], None],
                cancel: threading.Event) -> dict:   # returns the `done` payload
```

It reuses `service._get_v2_engine`, `prepare_v2_inputs(frame_range=…)`, `engine.run_clip`, `FrameOpsMixin._v2_runtime_file/_v2_read/_v2_atomic_write` and `_linear_to_srgb`. It must **not** call `ensure_output_dirs`/`clip.output_dir`: those encode the legacy layout.

Rules:
1. **Ownership check before anything is created.** If `manifest_dir/.corridorkey_manifest.json` exists, it must have the same `clip_id`. `mode=resume` additionally requires compatibility (§5). Otherwise `rejected: exists`. If any target file exists and is not covered by a compatible resume, `rejected: exists`. The Rust gate already checked this; the worker repeats the check because the disk may have changed.
2. **Lease file:** `manifest_dir/.corridorkey_job.lock` holds `{job_id, host, pid, started, heartbeat}`, created with `O_CREAT|O_EXCL`. The heartbeat mtime is touched every 10 s. A fresh lock held by another job means `rejected: busy`. The lock is removed on `done`. Section §7.4 uses it as the cross-machine fence.
3. Write manifest v3 `complete:false` **before** the first output file.
4. **No-clobber per file:** write to `.<name>.tmp.<job8>` in the target folder, then `os.link(tmp, final)` (fails if `final` exists) and unlink tmp. If `os.link` raises `EPERM/ENOTSUP/EXDEV` (some SMB/NAS mounts), fall back to `if exists: fail; os.replace` and log it. `ponytail:` this fallback has a race window between the check and the replace; the lease file covers app-managed writers; upgrade path is a per-folder exclusive create sentinel if NAS users see races. Runtime EXRs are moved this way too, never re-encoded (same as `_v2_move`). **EXR compression:** the runtime writes ZIP (`video_io.py:844`), and today's EXR publish moves those files without re-encoding. So `exr_compression` is currently **not applied to EXR outputs that come from the runtime**. v1 must either re-encode with `write_exr(compression=cfg.exr_compression)` or record the true `"zip"`. **Blocker B1** (§11); default: re-encode when `exr_compression != "zip"`.
5. Cancel is checked between frames. Already published files stay. `done.outcome=cancelled`, and the manifest stays `complete:false`.
6. When every frame is present: the manifest `complete:true`, and the runtime settings txt is copied next to the manifest.

**Manifest v3** (`version: 3`). It is a superset of v2, so old readers that ignore unknown keys still work:

```jsonc
{ "version":3, "engine":"corridorkey-v2", "clip_id":"…", "job_id":"…",
  "enabled_outputs":[…], "formats":{…}, "exr_compression":"dwab",
  "params":{…}, "hint_keyframes":[abs…] (keyframes mode only),
  "range":{"in_point":…,"out_point":…,"in_stem":…,"out_stem":…},
  "frame_range":[in,out],                  // kept for v2 readers
  "source_stems":[range stems],
  "files":{"matte":["clip_name_1101.exr", …]}, // relative to manifest_dir, same order as source_stems
  "effective_profile":{"hann_chunk":80,"hann_stride":40,"low_vram":false},
  "worker":{"host":"…","worker_rev":"…","gpu":"…"},
  "complete":false, "alpha_convention":{…} }
```

`files` is required. With templated names the file stem is not the source stem, so stem-based directory listing (`Clip::completed_stems`, `FrameIndex::build`) cannot find templated outputs.

---

## 5. Completion, Resume, stale and crash recovery

### 5.1 Complete
Complete means a v3 manifest with `complete:true` **and** every `files[o][i]` present for every enabled output. Never decide it from a file count. A v2 manifest keeps today's rule (whole clip, legacy folders).

### 5.2 Resume compatibility (`key_job.resume_compatible(manifest, job)`)

All must hold:
- Same `clip_id`, same `range` (all four fields), `params == job.params` (existing dict equality, including the legacy defaults `setdefault` in `_v2_resume_compatible`), the same `enabled_outputs`/`formats`/`hint_keyframes`, `exr_compression` equal (v2 manifest → unknown → not compatible, “labelled full rewrite”), and `files` equal to the job's relative paths.
- **Effective profile:** with `temporal_window=auto`/`vram_profile=auto`, a different GPU can pick a different Hann window, which changes the output. After the engine run, compare the `profile` event with `manifest.effective_profile`. If they differ, **do not publish into this folder**: `done.outcome=failed`, `error_kind=input`, message *“This machine chose window 48/24; existing frames used 80/40. Write as v002, or pin the temporal window.”* Resume still runs the full AI pass over the range (existing behavior) and only skips publishing existing files.

A range change is never resumable (contract §3).

### 5.3 Crash or lost worker
If no `done` arrives, the app re-reads the manifest and the lock. Files that exist stay reviewable. `complete:false` stays incomplete even when all files exist. The clip shows **Ready · partial (N of M)** and offers Resume if the manifest is compatible, otherwise **Write as v002**. A stale `.tmp.*` file in the folder is ignored by the viewer and removed only by the next job that owns the lease. Files never listed in a manifest are never deleted.

---

## 6. Rust side (after the import UI merge)

New files. No changes in `ui/` until the import agent hands off.

- `src/jobs.rs`: `KeyJob` (serde, mirrors §3.3), `build_key_job(clip, project, settings) -> Result<KeyJob, Vec<Blocker>>` (the gate from §1.2, using `contracts::resolve_path` per range frame; legacy layout produces the same list), and `JobState {Queued, Dispatching, Running{phase,done,total}, Done(outcome), Lost}`.
- `src/worker.rs`: `WorkerConn::spawn(target: &Target) -> io::Result<Self>` (local: `.venv/bin/python -m backend.worker --gpu N`, same discovery as `import::python`; remote: §7). Plus `send(&Cmd)`, `try_recv() -> Option<Event>` (reader thread + channel like `import::Job`), `cancel(job_id)`, and a `Drop` that closes stdin.
- `src/scheduler.rs`: `Scheduler { jobs: Vec<Job>, targets: Vec<TargetState> }`. `submit(job, Placement::{Auto, Target(id)})` and `tick()`: give the next queued job to the first idle target that can take it (§7.5). One job per target. A job with a failed prerequisite never starts. Snapshots stay frozen; a changed clip shows *Queued with earlier settings* (existing rule).
- `src/project.rs`: parse manifest v3 (`clip_id`, `range`, `files`, `exr_compression`, `effective_profile`). `completed_stems` and the Complete state use `files` when present. `resolve_state` uses “complete for its recorded range”.
- `src/frames.rs`: `FrameIndex::build` maps output views through `manifest.files` when present (stem → path), and through the directory listing for legacy manifests.

**Viewer refresh and cache invalidation.** On `published{stems}`, insert those `(view, stem) → path` entries into the open clip's `FrameIndex` and request a repaint. A rebuild is not needed per event. On `done`, re-read the manifest and rebuild the index. Cache keys are `(path, interp)`, and v1 never overwrites a path, so stale pixels are not possible. Still, call a new `Cache::remove_path(&Path)` for every published path, because a resumed job may publish a path the viewer saw as missing. Rebuild the playback buffer signature for that view. Re-reading must not reset playhead, range or A/B state (`tests/viewer.rs` already covers those invariants).

**Key tab UI:** the summary sentence from `selected-design.md` §2.3 (“AI will process frames 1101–1300 (200 of 412). Temporal context stays inside this range. Writes Matte (EXR) to …”), the target picker (§7.5), the phase stepper, Cancel, blockers with their fix actions, and the stale banner. Remove the `NOT_INTEGRATED` disabled state only for Key; Generate hint stays disabled with its reason (§10).

---

## 7. Remote workers (foundation designed now, built in phase A4)

### 7.1 Target model

```jsonc
// settings.json (Rust Settings, existing atomic store), list "ai_targets":
{"id":"local-0","kind":"local","gpu":0,"enabled":true}
{"id":"render2-gpu0","kind":"ssh","host":"render2","user":null,"port":null,
 "python":"/opt/EZ-CorridorKey2/.venv/bin/python","repo":"/opt/EZ-CorridorKey2","gpu":0,
 "storage":"shared","path_map":[["/mnt/NAS","/mnt/NAS"]],"enabled":true}
```

### 7.2 Transport and auth
- Remote launch is `ssh -T -o BatchMode=yes -o ServerAliveInterval=10 -o ServerAliveCountMax=3 [user@]host -- <python> -m backend.worker --gpu N`. The protocol is identical to local stdio.
- **Auth = the user's SSH keys/agent.** The app stores no secrets, opens no port and adds no network crate. Host key checking is left to the user's `known_hosts`; the app never passes `StrictHostKeyChecking=no`.
- The remote worker exits on stdin EOF (ssh closes → cancel → exit), exactly like the local one.
- `ponytail:` SSH only; upgrade to a stdlib HTTP/TLS worker service (GLM's suggestion, not approved) only if a user needs targets without SSH, e.g. Windows without OpenSSH or a managed farm.

### 7.3 Storage and output ownership
- **`shared`** (v1 remote): both machines see the same files. The scheduler rewrites every absolute path in `KeyJob` through `path_map` (longest prefix wins). If a path has no mapping, the target is ineligible for that job (reason shown). The worker publishes final files directly (§4). Pre-flight via the hello round-trip: the worker `stat`s `clip_root` and `manifest_dir`'s parent and reports `rejected: paths` if they are missing or not writable.
- **`transfer`** (phase A5, protocol reserved now): the app copies the **range frames + required hint frames** to a remote job scratch with `rsync -a --files-from` over the same SSH. The worker runs with `publish:"staging"`, writes outputs into the remote scratch with the same relative `files` names, and reports `done`. The app pulls the results and **publishes locally** with the same no-clobber + manifest logic (the Rust port of rules 3–6, or a local `python -m backend.worker publish-staged`). The app owns the final paths and the manifest; the remote side never writes into the user's output tree.
- **Output ownership rule (both modes):** one manifest folder has exactly one writer at a time, the job holding the lease. Batch outputs stay per clip, per resolved folder. The scheduler's collision check covers jobs on every target.

### 7.4 Disconnect, retry, idempotency
- `job_id` is the idempotency key. A worker that receives a `key` for a `job_id` it already ran or runs sends `rejected: busy`.
- If the connection is lost mid-job, the job becomes **Lost (render2 unreachable)**. It is **not** automatically re-dispatched while the lease heartbeat in `manifest_dir` is fresher than 60 s, because the remote process may still be writing (shared storage). After the lease goes stale, or a reconnect shows the worker gone, the user (or Auto, if “Retry lost jobs” is on, default off in v1) can retry. A retry is a **new job_id** in `mode=resume` against the same folder; the §5.2 rules decide whether frames are skipped. In transfer mode a lost job has never touched the final folder, so retry is always safe.
- Reattaching to a still-running remote job after reconnect is out of v1. The worker dies on EOF, so there is nothing to reattach to.

### 7.5 Placement, Auto, version compatibility
- Per job: **Target** = `Auto` | a specific target id, chosen in the Key tab and in Key N clips. It is frozen in the job. Auto resolves at dispatch.
- **Auto (v1):** whole clips only. Pick the first idle, enabled, connected target that is eligible: hello `protocol == 1`; model sha256 equal to the local reference (or, if hashes are unknown, the same `bytes` + filename, flagged in the UI); paths mappable/reachable; `gpu.vram_gb ≥` the existing peak estimate for the job's window. Order is local first, then the settings list order. No cost model.
- **Reproducibility across machines:** with `temporal_window=auto`, different GPUs may pick different windows (§5.2). The Key tab warns when a batch spans targets with different VRAM, offers **Pin window for this batch** (sets the resolved window explicitly), and records `effective_profile` and `worker` in each manifest.
- **Version gate:** a protocol mismatch refuses the target. A different `worker_rev` with the same protocol and models is allowed and shown as a badge. A model hash mismatch refuses the target.

---

## 8. Splitting one clip across machines: study, not v1

The engine runs overlapping Hann windows (default 80/40, auto-chosen per VRAM) and, with `carry_hint=True`, feeds predicted hints from earlier windows forward through `carry_cache` (`runtime.py` ~519–607). GVM blends latents across overlapping windows. Disjoint chunks therefore produce a different result from one pass: the window phase shifts, carried state is lost, and each chunk gets new range-edge effects. Range confinement makes each chunk behave like its own In/Out.

Study protocol (phase A6, GPU-isolated, real model, representative hair/motion-blur clips):
1. Baseline: one pass over `a..b`.
2. Candidates: (i) chunks aligned to the Hann stride, with one full window of overlap read and discarded at each inner boundary; (ii) the same plus seeding `carry_cache` at the boundary from the previous chunk's predicted hint (needs an engine hook, so it is an engine change); (iii) keyframes mode, where each chunk contains its own marks.
3. Metrics per boundary ±1 window: max/mean abs alpha diff vs baseline, temporal flicker (frame-to-frame diff delta), visual review of hair edges.
4. Decision gate: ship a candidate only if the user accepts the measured difference. Until then, a single clip runs on one target.

---

## 9. Phases, ownership and tests

| Phase | Owner | Files | Done when |
|---|---|---|---|
| **A0 Contracts** | backend Sonnet | `tests/fixtures/worker_protocol/*.json`, this protocol | Fixtures reviewed by Opus. |
| **A1 Python worker (CPU-tested)** | backend Sonnet, now | new `backend/service/key_job.py`, new `backend/worker/__init__.py` + `__main__.py`; additive edit to `backend/service/v2_inputs.py` (`frame_range`, `range_offset`); `_v2_check_disk` count arg; new `tests/test_key_job.py`, `tests/test_worker_protocol.py`, `tests/test_v2_inputs_range.py` | All §2/§4/§5 tests pass under isolation with FakeEngine; existing `test_v2_inputs.py`, `test_v2_service_inference.py`, `test_v2_outputs.py`, `test_v2_hint_resume.py` still pass unchanged. |
| **A2 Rust local Key** | Rust Sonnet, **after the import merge** | new `src/jobs.rs`, `src/worker.rs`, `src/scheduler.rs`; edits to `src/project.rs`, `src/frames.rs`, `src/settings.rs`, then `src/ui/app.rs`/`viewer.rs` | Rust tests with a **fake worker script** (Python, stdlib only, `rust-ui/tests/fixtures/fake_worker.py`) that speaks the protocol and writes tiny PNGs: progress → published → done; cancel → partial; crash → Lost/partial; rejected; viewer refresh without a playhead reset. |
| **A3 GPU validation** | parent, user-authorized | new `rust-ui/tools/gpu-isolated-test.sh` (§9.3) | A real model keys a 20-frame fixture range. Out-of-range reads: none (fs spy). Interior frames vs a whole-clip run reported. VRAM/time recorded. |
| **A4 Remote, shared storage** | Rust Sonnet + backend Sonnet | `worker.rs` ssh spawn, `scheduler.rs` Auto/eligibility/lease, settings `ai_targets`, Key target picker; worker `stat` pre-flight + hello fields | Tests use a fake “ssh” (a shell script that drops the host args and execs the local fake worker) inside isolation: path_map rewrite, unmappable path makes the target ineligible, disconnect → Lost, the lease blocks retry until stale, version gate. Real ssh only on user-designated machines. |
| **A5 Transfer storage** | later | rsync staging + local publish | Same tests with `storage:"transfer"`. Enter only if the user has no shared storage (U2). |
| **A6 Split study** | later, GPU | §8 | Metrics delivered to the user. |
| **A7 Hint jobs, batch, presets** | later | §10 | Parity list ticked. |

**Parallelism now:** A0+A1 (backend) run in parallel with the import UI fix; there is no file overlap. A2 starts after both merge.

### 9.1 Python tests through the wrapper
The wrapper binds only `rust-ui/` writable and the repo read-only, which fits a CPU test run that writes only to tmpfs:

```sh
cd rust-ui && bash tools/isolated-test.sh /home/npittas/EZ-CorridorKey2/.venv/bin/python -B -m pytest \
  -p no:cacheprovider --rootdir=.. ../tests/test_key_job.py ../tests/test_worker_protocol.py \
  ../tests/test_v2_inputs_range.py ../tests/test_v2_inputs.py ../tests/test_v2_service_inference.py \
  ../tests/test_v2_outputs.py ../tests/test_v2_hint_resume.py
```

`-B` and `-p no:cacheprovider` prevent `__pycache__`/`.pytest_cache` writes into the read-only repo. Fixtures must use `tmp_path` (under `/tmp` tmpfs). `CORRIDORKEY_V2_SCRATCH_DIR` must be set to a tmp path, because the default is next to the model dir. If the venv's symlinked interpreter under `~/.local/share/uv` fails inside the namespace, report it. Do not bypass the wrapper.

### 9.2 Rust tests
These are unchanged: `bash tools/isolated-test.sh /usr/bin/cargo test --offline`. The fake worker runs as `/usr/bin/python3 -I` (stdlib only) so it does not depend on the venv.

### 9.3 GPU wrapper, recorded correction and plan
**Correction (parent-verified 2026-10-22):** `isolated-test.sh` uses `bwrap --dev /dev`, which creates a fresh minimal devtmpfs. Inside it, `/dev` holds only null/zero/full/random/urandom/tty/pts/shm; `/dev/nvidia*` is absent. The earlier claim that the GPU was visible in the wrapper was wrong. **The existing wrapper stays exactly as is.**

Proposed `tools/gpu-isolated-test.sh`. **Not created and not run until the user authorizes A3.**
- The same flags as `isolated-test.sh` (unshare-all incl. net, clearenv, tmpfs `/tmp` `/run`, no display/D-Bus/input sockets, no `/dev/input`, `/dev/dri` not bound).
- Plus `--dev-bind` of only `/dev/nvidiactl`, `/dev/nvidia-uvm`, `/dev/nvidia-uvm-tools` and the selected `/dev/nvidiaN`. Optionally `--ro-bind /sys` paths the driver needs; to be found empirically, starting from nothing extra.
- Model checkpoints read-only through the existing `--ro-bind / /`. All caches point to tmp: `HOME=/tmp`, `XDG_CACHE_HOME=/tmp/cache`, `HF_HOME=/tmp/hf`, `TORCH_HOME=/tmp/torch`, `HF_HUB_OFFLINE=1`, `TRANSFORMERS_OFFLINE=1`. The writable host weight cache is not needed (the engine already runs under `_offline_env`).
- Output and scratch to tmpfs or a dedicated bind of `rust-ui/target/gpu-test/`. Never write into real projects.
- Acceptance: `nvidia-smi -L` works inside; `ls /dev` shows no input/dri nodes; `DISPLAY`/`WAYLAND_DISPLAY`/`DBUS_SESSION_BUS_ADDRESS` are unset.

---

## 10. Explicitly deferred (named, not dropped)

| Item | Contract source | Phase |
|---|---|---|
| **Replace v001** (staged write, recoverable publication, delete-after-success) | range-output §3 | after A2; v1 offers Write as v002 / Next free version only |
| Range-confined Key for **video** input assets | range-output §1 | after A2 (ffmpeg trim to staged frames) |
| Qt `run_inference` switching to confined range | rust-path §compat | user decision; Qt keeps whole-clip context until then |
| **Generate hint** jobs (GVM, BiRefNet, VideoMaMa, MatAnyone2, SAM2, chroma, import alpha) as worker `generate_hint` jobs + “generate then key” prerequisite chain | selected-design R5, §3.2 | A7; they are already range-limited in Python |
| Key N clips batch, blocked-clip reasons, Requeue with current settings, overwrite note | selected-design §2.2, R6 | A7, on top of the A2 scheduler |
| Workflow/Export presets | preset-contracts, selected-design §3.2 | A7 |
| Video export of `in..out` | range-output §1 | A7 |
| Stem-based hint coverage in **Python `clip_state`** readiness (Qt) | range-output §2 | with the Qt range switch |
| Output location preset | range-output §4 | optional, later |
| Transfer storage, reattach to remote, intra-clip split | §7.3, §7.4, §8 | A5, later, A6 |
| SageAttention / accelerator evaluation | rust-path “Later AI performance” | after A3, same GPU wrapper, quality-gated |

---

## 11. Blockers and questions

**Blockers for A1 (backend can start; decide before merge):**
- **B1 EXR compression is not applied today.** The runtime writes ZIP EXRs, and publish moves them without re-encoding, so `exr_compression` (default `dwab`) is silently ignored for runtime EXR outputs. Proposed: re-encode with `write_exr(compression=…)` when it differs from `zip`, and record the real value in manifest v3. This costs CPU time per frame. Alternative: record `"zip"` and show the compression control as fixed. Parent/user to confirm. The default in this brief is re-encode.
- **B2 `clip_id`.** No stable clip id exists today. Proposed: `clip.json["clip_id"] = uuid4`, written by the Rust app on first Key and preserved forever. Folder names can be renamed or deduplicated.

**User-specific unknowns (do not block A0–A2):**
- U1: The remote machines' OS (Linux only, or Windows too?), and whether OpenSSH key login to them is acceptable. That determines whether SSH stays the only transport.
- U2: Do local and remote machines mount the **same storage**, and at which paths (e.g. `/mnt/NAS` on both)? Yes → A4 shared storage is enough. No → A5 transfer is required.
- U3: Are the repo, venv and model checkpoints installed on each remote, and who updates them? v1 expects them installed and only verifies them (hello/version gate). Deploying them is out of scope.
- U4: How many GPUs per machine? One target per GPU is supported.
- U5: Is the network trusted (LAN only)? This only matters if a non-SSH transport is ever requested.
- U6: Is intra-clip splitting a priority worth GPU study time (A6), or are whole clips across machines enough?
