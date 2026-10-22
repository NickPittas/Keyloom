# Native first-slice parent verification — 2026-10-22

Status: first runnable read-only native preview delivered. Independent second review accepts the code for S1, and the final narrow-layout follow-up is visually verified. Native OS shortcuts and production-media GUI performance remain unverified. This is not full Rust migration acceptance.

## Post-fix verification

- Parent rerun: 41 release tests passed (23 unit, 3 synthetic decode, 13 loader, 2 viewer), plus both opt-in real-media tests passed with `CK_REAL_PROJECT` and `CK_REAL_REFERENCE`. The numeric reference comparison read all four Python reference files.
- Viewer tests now use separate temporary directories for forward and reverse tests, fixing a parallel-test race. Implementor reran them three times after that change.
- Parent formatting and Clippy checks pass after the test-isolation change. Following the server restart, the old rustup toolchain path was absent; system `/usr/bin/cargo`, `/usr/bin/rustc`, `/usr/bin/rustdoc`, `/usr/bin/cargo-fmt` and `/usr/bin/cargo-clippy` worked with explicit `CARGO`, `RUSTC`, `RUSTDOC`, `RUSTFMT` and `/usr/bin:/bin` PATH. No system configuration was changed.
- Read-only verification still reports all 5,417 files unchanged, including baseline JSON hashes.
- Normal app launch (without `--keys` or `--screenshot`) displayed the real matte. OS-input verification was abandoned when desktop focus changed away from the app. Neither physical shortcuts nor sustained cached playback in the live window is claimed verified.
- Implementor reports exact-size 1440×900 and 728×900 screenshots with independent transport/A-B centering. These measurements have not been independently repeated by the parent.

## Final parent live QA

- Helper-free native playback: launched a temporary synthetic 20-frame PNG project at 2 fps with `--play`, without `--keys` or `--screenshot`. After resizing only that app window, two external compositor captures two seconds apart showed frame 4 and frame 8, zero reported drops. This verifies progression in the actual normal window, including intervals when the tiny prefetched frames were already decoded. It is not a production-media GPU performance benchmark.
- The 1440×900 captures show independently centred icons and A/B, with range controls separately to the right.
- Real-project Hint/Input-versus-Matte at exactly 728×900: A/B and transport groups are visually centred, range controls have their own row, and the image has ample height. However, the horizontally scrolling graph hides Comp/Processed and part of Matte, and the primary action is hidden when Properties is closed. These are assigned as a final narrow-layout fix; see implementation-brief acceptance item 7.
- Native OS shortcut testing remains unverified. No further global keyboard input was used.

## Narrow-layout follow-up closure

Parent inspected the implementor's final exact-size screenshots `/tmp/ck-shots/n728.png`, `n728p.png` and `n1440.png`. All seven nodes and the disabled Key action are visible at 728×900 with Properties both closed and open. Transport icons and the active A/B group are centred within the viewer, and no header controls overlap. At 1440×900 the large Hint viewer remains intact. The implementor reran formatting, Clippy and all 41 regular tests after this layout-only change; parent reran the read-only inventory, again with zero changes to 5,417 files.

Known minimum-size limit: at 700×660 the viewer shrinks below 280 px; use a taller window or Focus mode. The ordinary 728×900 and 1440×900 layouts meet the large-viewer requirement. Real OS key/mouse input remains unchecked; synthetic egui input coverage is not represented as physical-key verification.

Next implementation milestone: shared Python/Rust processing and persistence contracts, including processing only In/Out, output provenance/compression, worker messages and queue ownership. The current native preview must continue to leave existing projects untouched until saving and inference integration have their own validation.

## Independently verified

- `cargo test --locked`: 28 passed, 1 opt-in real-project test ignored in this parent run.
- `cargo fmt --check` and `cargo clippy --locked --all-targets -- -D warnings`: pass.
- Used `/home/npittas/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/cargo` because the harness invocation of the rustup cargo proxy reported `unknown proxy name: 't3_code_alpha'`. No system configuration changed.
- `tools/verify_readonly.py` against the before-run inventories: 5,417 project files before and after; zero added, removed or changed; project JSON hashes unchanged.
- `ck-bench inspect` opens the 1080p project and reports its 161-frame input, hint, matte, comp and processed sequences, 24 fps and session colour override.
- Native Hint/Matte screenshot displays real project footage. Requested 1440×900 became 1376×1852 under desktop tiling, so this screenshot does **not** validate the requested window size.

## Parent benchmark rerun

Release `ck-bench`, Input view, 40 frames, 1,024 MiB cache, four workers; cold application cache, warmed filesystem cache. Ryzen 9 9950X / RTX 4090 machine. These measure the core decode/cache/playback scheduler, excluding GUI texture upload and redraw scheduling. They are upper bounds for GUI playback, not measured GUI frame rates or long-session guarantees.

| Source | Playback fps | Dropped | Median cold seek | Peak RSS |
|---|---:|---:|---:|---:|
| 1920×1080 RGB half DWAB | 23.99 | 0 | 32.2 ms | 840,620 KiB |
| 3200×1800 RGB half DWAB | 18.26 | 9 | 76.4 ms | 2,191,576 KiB |

The larger source does not meet its 24 fps frame rate. Do not describe all formats/resolutions as real-time capable. The implementor's other view measurements are in the delegated implementation report, not independently repeated here.

## Pending acceptance checks

- Opus review completed in `native-review.md`: fixes required for cached playback redraw scheduling, renamed clip paths/colour lookup, strict keyframe sidecar validation, missing-source states, legacy alpha handling, frame-template expansion and v1 global output paths. Smaller decode/graph/test findings are also assigned to the implementor. Independent regression review remains pending.
- Cargo default binary and independent transport/A-B centering are implemented; second review remains pending.
- Scoped native keyboard checks and actual 728-pixel Properties-sheet/layout verification.

AI, queue execution, saving, import, presets, movie decoding and mask annotation remain later milestones. The current app reads project data and edits display range/colour only in memory.
