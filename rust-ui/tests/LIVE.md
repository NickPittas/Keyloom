# Live tests (real worker, GPU, models; read-only) — one test per command, 60 s

Commands (from the repo root):

    bash rust-ui/tools/live-test.sh build            # once; deadline 300 s, live cargo output
    bash rust-ui/tools/live-test.sh <test_name>      # ONE test, deadline 60 s, output streams live; `list` shows the names

Each ends with `PASS <name>` / `FAIL <name>: <reason>` (also appended to `rust-ui/target/live/report.md`). At most 6 frames per clip. Models are never verified.

Name — what it checks — typical time (filled in from runs; `?` = not yet timed):

- live_worker_idle_at_open — one idle worker per GPU at app open, no model lock — ?
- live_worker_first_action_completes — Chroma pre-matte completes on the idle worker — ?
- live_worker_two_clips_same_pid — two Board clips run in order on one worker pid — ?
- live_worker_cancel_key_leaves_worker_idle — cancelling a Key leaves the worker idle and alive — ?
- live_worker_killed_is_reported_and_restarted — kill -9 is reported, next job restarts the worker — ?
- live_producers_sam_preview_and_track — SAM preview + track, one prompt — ?
- live_producers_gvm — GVM pre-matte — ?
- live_producers_birefnet — BiRefNet pre-matte, one variant — ?
- live_producers_chroma — Chroma pre-matte — ?
- live_producers_matanyone2 — MatAnyone2 hint — ?
- live_producers_videomama — VideoMaMa hint — ?
- live_hint_direct_without_prematte_then_board_queues_no_hint_steps — Direct hint with no pre-matte; Board queues zero hint steps — ?
- live_hint_changed_setting_reruns_producer — a changed setting reruns the producer — ?
- live_hint_chroma_pick_colour_with_sam_overlay_off — Pick colour stays selected with SAM overlay off — ?
- live_board_key_selected_reuses_matching_hint_and_asks_on_change — Key selected reuses a matching hint, asks on a changed frame — ?
- live_board_after_key_open_clip_shows_outputs_without_update_required — open clip shows outputs after Key, no "Update required" — ?
- live_export_prores4444_has_alpha_and_six_frames — Key 6 frames, ProRes 4444 has alpha, 6 frames, alpha within 1/255 of EXR — ?
- live_endpoint_listener_in_app_pid_no_child — listener in the app PID, no child — ?
- live_endpoint_authenticated_push_lands_in_inbox — authenticated push lands in the inbox — ?
- live_endpoint_quit_closes_the_socket — Quit closes the socket — ?
- live_endpoint_passive_while_old_service_holds_state — passive while the old service holds the state — ?
- live_endpoint_damaged_runtime_is_a_notice_not_an_abort — damaged runtime gives a failure status — ?
- live_import_movie_in_place_makes_no_source_copy_and_extracts — in-place import: no Source/ copy, extraction works — ?
- live_import_hint_tab_extract_works — Hint-tab Extract re-extracts from the movie — ?
- live_import_missing_original_is_named_and_recovers — missing original is named; renaming back recovers — ?

Env overrides: KEYLOOM_LIVE_CLIP, KEYLOOM_LIVE_PYTHON, KEYLOOM_LIVE_MODELS, KEYLOOM_LIVE_INSTALL, KEYLOOM_LIVE_BIREFNET (variant), KEYLOOM_LIVE_FRAMES (capped at 6).
