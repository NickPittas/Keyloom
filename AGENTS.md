# Agent instructions

## Building installers (offline + online)
Build installers ONLY with the canonical command, on the target OS:

    python packaging/build_release.py        # Linux: .venv/bin/python

- It builds the UI, one stage and both installers from it, runs the required checks once, verifies outputs, and exits nonzero on any failure. Run it once, wait for its final exit, and report the exit code, the `build.log` and `build-result.json` paths, the payload `version_id` and the artifact sha256s from the result file.
- Do NOT hand-compose `packaging/bundle.py`, cargo, Inno or `build-ui-old-glibc.sh` steps, and do not rerun or re-inspect the checks it already did. If it fails, read the printed phase and `build.log`; fix the cause and rerun the same command.
- Do not change source files while a build runs (the build fails if they change). Never publish, install or launch the result as part of a build.
- Details, config and limits: `packaging/README.md`.

### Windows build workflow (Kamino3 only)
- Edit on Endor, commit and push to the private `origin`, then `git pull` in the ONE persistent Kamino3 checkout. That is the only way source reaches Windows: no source archives, snapshots, scp/rsync/SSH file pipes, fallback copies or duplicate checkout folders, and no copy of any runtime/model/artifact over 100 MB.
- Build dependencies (pinned Python, wheels, FFmpeg, toolchains) are downloaded once into ONE persistent git-ignored build folder outside Git and only re-downloaded when their pinned version changes. They are never committed and never copied from an installed app.
- Windows builds and tests never run on Kamino1; a build never installs, launches or modifies an installed app.
- The staged self-check at build time uses `bundle.BUILD_SELFCHECK_TIMEOUT` (finite, 300 s); installer phase limits (`lifecycle.PHASE_TIMEOUT`, 10 s / 30 s total) are unchanged.

## Tests
Follow `rust-ui/AGENTS.md`: run tests only through `rust-ui/tools/isolated-test.sh` (use absolute paths for the interpreter and test files), never on the live desktop.

## Command monitoring and timing gates (persistent user requirements)
- Run every ordinary command in the foreground through `.venv/bin/python packaging/run_monitored.py --deadline <=10 --label <safe name> -- <argv>` (use `/usr/bin/python3` if no venv). It streams flushed output, prints a one-second heartbeat (supervisor liveness, NOT progress), reports exit status and elapsed time, exits 124 on deadline and kills the whole process tree. Set the tool's own timeout <=10000 ms as well; no pipes, `&`, detached processes or silent waits, and no unchanged retry or longer deadline after a failure: report FAILED and stop.
- Canonical builds are the user-approved exception to the 10 s gate: each phase has an explicit finite deadline (config `timeouts`, `deadline_seconds`; defaults in `packaging/build_release.py`) with live output and a one-second heartbeat in the console and `build.log`. Their only checks are the staged app self-check, the firewall-helper import check, `online.py --help` and artifact sha256 verification; no tests, test discovery or test-directory access (the source fingerprint prunes test directories before traversal).
- Installs: 30 s absolute (target 20 s), each local phase 10 s; downloads/models may exceed it only with visible bytes, speed, elapsed and stall handling.
- Never print secrets or credential argv (use `--label`). Mock/static tests never prove performance or packaged/Windows gates.
- No build until the user/parent authorizes it for a frozen snapshot. Existing user prefs, models, certs, jobs and custom roots must stay untouched.

## Absolute prohibition on tests — explicit user instruction

No agent has permission to write, create, run, execute, open, read, edit, inspect, search for, glob, discover, collect, list, or otherwise access any test, test file, test directory, test suite, fixture, smoke test, self-check, launch validation or benchmark. This applies to every agent, all features and builds, and all future work. Do not invoke tools or build steps that implicitly access or execute tests. Do not delegate any prohibited test activity. Do not infer exceptions from implementation completion, review, build instructions or previous permissions. Earlier testing instructions describe no currently permitted activity and are superseded by this prohibition.

## Implementation-only scope and syntax checks

Only pure implementation and LSP code checks strictly limited to syntax are approved. No other validation, reviews, command-line syntax checks, compilation, builds, tests or operational activity is authorized. Running the implemented feature and timing that run may be separately approved by the user; this statement does not itself approve a run. Do not infer execution permission from a request to implement. This applies to the parent and every delegated agent and supersedes earlier broader permissions.
