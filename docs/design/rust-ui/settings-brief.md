# Native Settings page — 2026-10-22

User-requested addition to the import/playback repair work. Settings must be reachable from both Batch and an open clip and persist between launches.

- Playback: decoded RAM preview budget, decode worker count with Auto, and default looping. RAM/thread changes apply when the next clip opens, so applying preferences does not discard the current clip's In/Out edits or playhead. Explicit CLI cache settings override the saved default.
- Interface: UI scale, preserving the selected muted theme and readable controls. Apply scale immediately.
- AI: use the backend's actual VRAM profiles and inference parameters. A VRAM profile is an engine policy, not a promise to enforce a memory allocation cap. Until native inference is integrated, clearly identify these as saved future defaults.
- Masks/hints: use existing supported tracker and hint defaults; identify values awaiting native mask-tool integration.
- Defaults seed new work; they must not silently replace explicit project or clip settings.

Use Apply, Cancel, and Reset defaults. Reset changes the form until Apply. Validate before saving, write atomically outside project data, show failures, and preserve unreadable or unsupported-version settings. Tests must use temporary settings files and never modify the user's preferences.

Acceptance: page opens from Batch and workspace; Apply survives restart; Cancel leaves saved/live settings unchanged; invalid/unsupported files are preserved; RAM budget and worker count reach the actual viewer; UI remains usable at narrow widths and supported scales. AI acceleration evaluation remains deferred per `rust-path.md`.
