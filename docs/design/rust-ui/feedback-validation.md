# Feedback fixes and Settings validation — 2026-10-22

Feedback fixes and final review complete. The verified results below apply to the native viewer/importer slice, not a completed AI rewrite.

## Test boundary

The user explicitly prohibits testing on the live desktop. All automated tests use `rust-ui/tools/isolated-test.sh`; rendered tests use `virtual-display-test.sh`. Both run in a bubblewrap namespace with fresh temporary/runtime directories, isolated networking, cleared environment and no host input/GPU devices. The second starts a private Xvfb with software Vulkan. Never use the old `/tmp/shot.sh` helper.

Parent verified an existing release build rendered `rust-ui/target/isolated-landing.png` and exited inside the virtual display. The original 39 library tests passed in the isolated environment. These establish the test environment, not acceptance of subsequent changes. Opus independently reviewed both isolation wrappers and found no route to the live display or session bus.

## Settings review

Opus found nested unknown JSON fields were discarded on Apply and repair of an unusable file required an unrelated edit to enable Apply. Sonnet fixed both, preserved unknown fields on Reset, added an exact-button-position click test for repair, and reported all 12 settings tests passing in the real crate under isolation. The save path also attempts to restore the original file if replacement fails after moving it aside; that failure branch was not exercised.

## Combined checks independently verified by parent

- 113 regular Rust tests passed through `isolated-test.sh`: 60 library, 3 decode, 6 import, 13 loader, 7 navigation, 8 settings integration, 16 viewer. Three opt-in real-media tests were ignored in that regular run. A prior full rerun exposed three navigation fixtures deleting the same temporary directory; each now uses a unique tag. Sonnet reported three consecutive clean full runs after that correction; parent independently repeated the full suite successfully.
- The separate release `real_preview` test read 129 existing 1920×1080 frames: RAM fill 1.728 s after the final production fixes (1.464 s in the earlier run), exactly 129 decodes, two warm loops without further decoding, zero headless drops. This measures the loader/scheduler, not GPU upload or live-window performance. The test was explicitly enabled with `CK_PREVIEW_PROJECT=../Projects/261022_122316_V1-0047_A001C055_261012_R4LF_1080`.
- Settings integration tests cover both entry points, persisted Apply/reload, draft Cancel, immediate UI scale, CLI cache precedence, workers/loop consumption on next clip open, workspace preservation, shortcut suppression and deferred import handoff.
- Mock picker and navigation tests cover asynchronous dialogs, one outstanding picker, deferred responses, actual Open clip settings buttons and board selection. Drop tests cover workspace and picker refusal notices.
- Viewer tests cover progress, budget fallback, A/B and interpretation invalidation, forward/reverse In/Out looping, range edits, drop-counter reset, loop-off clock rebasing, oversized unknown-size frames, cursor-anchored zoom, pan and A/B wipe.
- Parent inspected private-display renders at 728×900 and 1440×900 with the real clip playing: `rust-ui/target/isolated-playback-final-narrow.png` and `isolated-playback-final-wide.png`. Both have visible transport, timeline, graph and Settings controls. Software Vulkan emitted the expected DRI3 warning; both runs exited successfully.
- Parent opened Settings from the observed workspace button at 728×900 and inspected `isolated-settings-final-narrow.png`. The page and its Apply/Cancel controls render correctly. The final production build also rendered after synthetic `L,L` input, toggling Loop off then on and continuing playback (`isolated-loop-toggle-final.png`).
- System `cargo-fmt` check and Clippy with `-D warnings` passed inside isolation. On this machine invoke `/usr/bin/cargo-fmt` and `/usr/bin/cargo-clippy` directly with `CARGO=/usr/bin/cargo` to avoid rustup shim discovery; README has the commands.

## Final review

Opus confirmed duplicate-decode prevention, oversized-frame rejection, range clamp, per-run drop reset, loop-off rebase and Settings integration. Its follow-up findings are implemented and covered by the passing suite:

- Failed end frames count as terminal states, so non-looping playback stops in both directions, including when every frame fails.
- Loop is part of the buffer signature. Enabling it mid-play fills the missing frames or reports over-budget streaming. The regression observes two actual wraps in each direction before checking that no further decodes occur.
- The busy-import test uses a temporary fake importer that remains active until stdin closes; it cannot silently skip or finish before the drop assertion.
- A drop on Settings produces a refusal notice and preserves the open clip, playhead and range after Back.

The final independent Opus review found no blockers or regressions in the reviewed changes and independently passed the full isolated suite. Its report mentioned missing Settings-drop preservation coverage; that test had subsequently landed as `tests/settings.rs::drop_on_settings_is_refused_and_keeps_the_open_clip` and passed the parent's final full run. The final release build, formatting check and Clippy with `-D warnings` also passed independently. Release executable: `rust-ui/target/release/corridorkey-ui`.

Private software rendering does not prove live Hyprland/XWayland drag-and-drop or hardware playback smoothness. Those remain explicitly unverified unless the user later supplies results or authorizes a suitable isolated hardware environment.

## Subsequent import destination change

Interactive imports now have an explicit Destination section. Landing imports require choosing a parent folder for a new project or an existing project; batch imports show the current project and allow a different destination before confirmation. New projects go directly beneath the chosen parent. CLI QA keeps its documented `data-dir/Projects` default.

Parent independently passed all 120 regular Rust tests and the release build. The 728×900 private-display render `rust-ui/target/import-destination-narrow.png` shows the destination controls, name, path preview and Import/Cancel without overlap. This render uses CLI QA sources and therefore a preselected destination; mocked interactive-drop tests cover the initially unselected destination and disabled Import. These checks did not catch the review findings below; the change is not accepted yet.

Opus found two blockers: an empty `--parent-dir` still falls back to the repository's default Projects directory in the helper, and changing the open project leaves a waiting plan attached to the old destination. It also requested proper existing-project metadata validation and disabling confirmation while a destination dialog is open. Fixes and direct helper regressions are assigned; new-project name exhaustion must also fail rather than reuse an existing folder.

Follow-up review confirmed those fixes and passed 11 import tests plus the Rust suite. It identified a remaining race between the folder-name precheck and backend creation: concurrent app instances can select the same folder. Exclusive directory claiming, validation of the existing project's `clips` list, and disabling all destination controls during a pending picker are assigned before final acceptance.

Isolation incident, disclosed to the user: the implementer reported running a helper outside the sandbox, creating `Projects/261022_173955_one`, then deleting it. Parent confirmed that folder is absent and the three current project folders are present. Without a pre-incident inventory, parent cannot independently verify the claim that all other project contents were untouched. Subsequent parent tests and the render used the required wrappers, and active implementers were reminded not to execute helpers outside them.
