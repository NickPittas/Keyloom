#!/bin/bash
# Live suite runner: the real App (headless), REAL worker/GPU/models (read-only) on a temporary copy of a real clip.
#   bash rust-ui/tools/live-test.sh list
#   bash rust-ui/tools/live-test.sh build            # builds the test binary once (deadline 300 s, live cargo output)
#   bash rust-ui/tools/live-test.sh <test_name>      # exactly ONE test (deadline 60 s, live output)
# Output streams to the console as it happens (run_monitored.py heartbeat + test output); `tee` only keeps a copy in rust-ui/target/live/logs.
# Ends with `PASS <name>` or `FAIL <name>: <reason>`, also appended to rust-ui/target/live/report.md. See rust-ui/tests/LIVE.md.
set -u
here=$(cd -- "$(dirname -- "$0")" && pwd)
repo=$(cd -- "$here/../.." && pwd)
out="$repo/rust-ui/target/live"
report=${KEYLOOM_LIVE_REPORT:-$out/report.md}
mon_py="$repo/.venv/bin/python"; [ -x "$mon_py" ] || mon_py=/usr/bin/python3
mon() { # mon <deadline> <label> -- argv...
  local d=$1 l=$2; shift 3
  "$mon_py" "$repo/packaging/run_monitored.py" --deadline "$d" --label "$l" -- "$@"
}

# name|what it checks
TESTS='live_worker_idle_at_open|one idle worker per GPU at app open, no model lock
live_worker_first_action_completes|first action (Chroma pre-matte) completes on the idle worker
live_worker_two_clips_same_pid|two clips queued from the Board run in order on one worker pid
live_worker_cancel_key_leaves_worker_idle|cancelling a Key part-way leaves the worker idle and alive
live_worker_killed_is_reported_and_restarted|kill -9 the worker: reported, next job restarts it
live_producers_sam_preview_and_track|SAM preview + track with one prompt
live_producers_gvm|GVM pre-matte
live_producers_birefnet|BiRefNet pre-matte (one variant)
live_producers_chroma|Chroma pre-matte
live_producers_matanyone2|MatAnyone2 hint
live_producers_videomama|VideoMaMa hint
live_hint_direct_without_prematte_then_board_queues_no_hint_steps|Direct Generate hint with no pre-matte, Board queues zero hint steps
live_hint_changed_setting_reruns_producer|a changed setting reruns the producer
live_hint_chroma_pick_colour_with_sam_overlay_off|Chroma Pick colour stays selected with the SAM overlay off
live_board_key_selected_reuses_matching_hint_and_asks_on_change|Key selected reuses a matching hint, asks when a pre-matte frame changed
live_board_after_key_open_clip_shows_outputs_without_update_required|after Key the open clip shows FG/Matte/Comp/Processed, no Update required
live_export_prores4444_has_alpha_and_six_frames|Key 6 frames, ProRes 4444: alpha pix_fmt, 6 frames, alpha vs EXR within 1/255
live_endpoint_listener_in_app_pid_no_child|listener in the app PID, no netlink/service child
live_endpoint_authenticated_push_lands_in_inbox|authenticated push lands in the inbox
live_endpoint_quit_closes_the_socket|Quit closes the socket
live_endpoint_passive_while_old_service_holds_state|app stays passive while the old service holds the state
live_endpoint_damaged_runtime_is_a_notice_not_an_abort|damaged runtime gives a failure status, not an abort
live_import_movie_in_place_makes_no_source_copy_and_extracts|movie imported in place: no Source/ copy, extraction works
live_import_hint_tab_extract_works|Hint-tab Extract re-extracts from the movie in place
live_import_missing_original_is_named_and_recovers|missing original is named (reconnect or relink it); renaming back recovers'

usage() {
  echo "usage: bash rust-ui/tools/live-test.sh list | build | <test_name>      (one test per command, 60 s)"
  echo "$TESTS" | awk -F'|' '{printf "  %-70s %s\n", $1, $2}'
}
[ $# -eq 1 ] || { usage >&2; exit 2; }
mkdir -p "$out/logs" "$out/tmp" "$(dirname -- "$report")"

if [ "$1" = list ] || [ "$1" = -h ] || [ "$1" = --help ]; then usage; exit 0; fi

if [ "$1" = build ]; then
  echo "== building the live test binary (deadline 300 s)"
  mon 300 live-build -- /usr/bin/nice -n 10 bash "$here/gpu-isolated-test.sh" /usr/bin/cargo test --offline --test live --no-run
  rc=$?
  [ $rc -eq 0 ] && echo "BUILD OK" || echo "BUILD FAILED (exit $rc)"
  exit $rc
fi

t=$1
echo "$TESTS" | cut -d'|' -f1 | grep -qx -- "$t" || { echo "unknown test: $t" >&2; usage >&2; exit 2; }

# ---- environment discovered from this machine (every part overridable)
xdg_data=${XDG_DATA_HOME:-$HOME/.local/share}
install_root=${KEYLOOM_LIVE_INSTALL:-$HOME/.local/opt/Keyloom}
py=${KEYLOOM_LIVE_PYTHON:-}
if [ -z "$py" ]; then
  for c in "$install_root"/current/runtime/python/bin/python3.11 "$install_root"/current/runtime/python/bin/python3 "$install_root"/current/runtime/python/python.exe; do
    [ -e "$c" ] && { py=$(readlink -f "$c"); break; }
  done
fi
[ -n "$py" ] && [ -e "$py" ] || { echo "no installed Keyloom runtime Python found (looked in $install_root/current/runtime); set KEYLOOM_LIVE_PYTHON" >&2; exit 2; }
# Same model roots as the installed app: its descriptor has null roots = platform defaults from XDG, so pass the real XDG dirs + HF cache
# (gpu-isolated-test.sh clears the env and sets HOME=/tmp). Nothing is verified, repaired or written there (the sandbox root is read-only).
hf=${KEYLOOM_LIVE_HF:-${HF_HUB_CACHE:-$HOME/.cache/huggingface/hub}}
xdg_cache=${XDG_CACHE_HOME:-$HOME/.cache}
clip=${KEYLOOM_LIVE_CLIP:-/mnt/Projects/26109_Newtons_Piraeus Book Presentation_2026-11-02/Projects/keyloom/New project/clips/SHOT_03}
[ -d "$clip/Frames" ] || { echo "clip not found: $clip (set KEYLOOM_LIVE_CLIP to a clip folder with Frames/ and .video_metadata.json)" >&2; exit 2; }

# env handed to the process INSIDE the sandbox (gpu-isolated-test.sh clears the rest)
inner_env=(
  KEYLOOM_LIVE=1 "CK_INSTALL=$install_root/current/keyloom-install.json"
  "KEYLOOM_LIVE_PYTHON=$py" "KEYLOOM_LIVE_HELPER=$repo" "KEYLOOM_LIVE_CLIP=$clip"
  "XDG_DATA_HOME=$xdg_data" "XDG_CACHE_HOME=$xdg_cache" "HF_HUB_CACHE=$hf" OPENCV_IO_ENABLE_OPENEXR=1
)
for v in KEYLOOM_LIVE_BIREFNET; do [ -n "${!v:-}" ] && inner_env+=("$v=${!v}"); done

tmp="$out/tmp/$t"; log="$out/logs/$t.log"
rm -rf "$tmp"; mkdir -p "$tmp"
start=$(date +%s)
mon 60 "$t" -- /usr/bin/nice -n 10 bash "$here/gpu-isolated-test.sh" /usr/bin/env "${inner_env[@]}" "KEYLOOM_LIVE_TMP=$tmp" \
  /usr/bin/cargo test --offline --test live -- --ignored --exact "$t" --nocapture --test-threads=1 2>&1 | tee "$log"
rc=${PIPESTATUS[0]}
secs=$(( $(date +%s) - start ))
rm -rf "$tmp"

if [ $rc -eq 0 ] && grep -q 'test result: ok. 1 passed' "$log"; then
  line="PASS $t (${secs}s)"
else
  if [ $rc -eq 124 ]; then reason="hit the 60 s deadline (see $log)"
  else reason=$(grep -h -A1 'panicked at' "$log" | grep -v 'panicked at' | head -1 | sed 's/^\[[0-9: +.s]*\] | //')
       [ -n "$reason" ] || reason="exit $rc, no panic message (see $log)"
  fi
  line="FAIL $t: $reason (${secs}s)"
fi
echo "$line"
echo "$(date '+%F %T') $line" >>"$report"
case "$line" in PASS*) exit 0;; *) exit 1;; esac
