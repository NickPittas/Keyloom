# Finish the current UI

Status: design review, not an application update. The current application is the baseline; older HTML proposals are background only. The user considers the design about 60% complete and asks to finish it, preserving the working pipeline.

## Preserve

- Board cards, selection, full-workflow preset selector and per-video queue sections.
- Media / Hint / Key / Review / Deliver workspace tabs.
- Viewer, vertical contextual SAM tools, transport and timeline.
- Right Properties inspector. Settings stay there; processing actions do not.
- Shared status-left / module-actions-right panel below the timeline and above the graph.
- Resizable inspector, queue and graph; collapsible graph; narrow Properties sheet.
- Current precomputed SAM/Chroma, automatic GVM/BiRefNet, direct/refined hint, Key and per-output sequence/video delivery semantics.

## Finish

| Surface | Required correction |
|---|---|
| Settings | Left category navigation; only the selected category in the right content pane. Playback, Interface, AI defaults, AI worker targets, Masks and hints, paths/information, Models link. Apply/Cancel/Reset remain visible. Preserve all real preferences and target configuration. |
| Models | Compact model/family navigation, selected artifact summary and visible actions. Technical catalog/provenance data lives in collapsed Details. Runtime dependencies remain accessible. Download, Verify, Repair and the requested Delete have distinct intent and a visible operation state. |
| Queue | Left-aligned clip title and stage names, consistent label/progress/control columns, padding inside the divider, separate clip cards, contextual controls. No raw debug dump on normal startup. Detailed failures and recovery remain reachable. |
| Key | Grouped client, temporal, VRAM, despill, outputs and preset settings. Compact result summary. One clear Key action in the shared panel. Resume/Replace/lock recovery appears only when relevant. |
| Hint | Separate Pre-matte, final Hint, cleanup and consumption settings. Module-specific action groups. SAM actions only with SAM; companion import actions only with an import source selected. |
| Deliver | Preserve one card per enabled output and its sequence/video format/codec/quality. Distinguish rendering the workflow from exporting an existing result. |
| All panes | Distinct dark/raised/inset grey surfaces, muted grey-green primary accents, fixed label/control grids, consistent button sizes and spacing, concise help with technical details disclosed. |

## Project navigation and references (latest user additions)

- Use the supplied settings screenshot as the structural reference: search at the top, a bounded category list on the left, a clearly selected row, and aligned fields in the right pane. Use CorridorKey's actual categories and settings.
- Use the supplied compact monochrome toolbar as an icon-quality reference. Draw only CorridorKey's actual tools with consistent strokes, spacing, tooltips and selected states; do not import unrelated image-editor tools.
- Add a proper start page with Open project, New project and Recent projects. Make loading another project obvious after opening one.
- Provide discoverable Open, Save project and Quit commands, a visible saved/unsaved status and clear shortcuts. Save/Quit in the prototype are simulated flows; do not close the user's browser.
- Expose **Project output folder** prominently in the project header/Board and Deliver. Both edit the same project-wide output root. Distinguish it from optional per-video export-folder overrides and show a resolved example path. Do not require users to discover a buried template editor to change the project destination.
- These are additions to the current design, not a reason to remove the working batch workflow or rearrange the agreed viewer/action/graph structure.

## Visible recompute guidance

A persistent amber outline must link the changed inspector group, affected graph node and required action. A brief pulse draws attention once; respect reduced motion. Labels supplement colour rather than replacing it. Tool selection emphasizes the relevant action group; changing prompts marks output stale.

| Edit | Actions marked pending |
|---|---|
| Pre-matte producer, producer settings, SAM model/prompts, grow/shrink or fill holes | Pre-matte, downstream Hint, Key and delivery |
| Refiner or refiner settings; final feather | Hint, Key and delivery; preserve a valid pre-matte |
| Hint mode/quality/carry quality | Key and delivery |
| Temporal/VRAM/despill/output settings | Key and delivery |
| Video container/codec/quality or destination | Delivery |

Recomputing a stage clears only that stage's pending state. Downstream output remains stale until updated. An upstream pending stage blocks a downstream action from pretending to produce a current result. Queued/running jobs retain their frozen parameters.

## Design acceptance

Review the actual HTML screens and interactions at 1440×900 and a narrower width. Check Settings categories, Models selection/details/actions, Board/queue with long names and multiple statuses, Media, Hint with SAM/GVM/BiRefNet/Chroma and Direct/MatAnyone2/VideoMaMa/Import, Key, Review, Deliver, Batch review and Clients. Exercise producer/refiner/prompt/cleanup/Key/delivery edits and verify the exact highlighted action. Verify processing actions are above the graph, never hidden at the end of the inspector. Review geometry and overflow, not only DOM presence.

After design approval, application acceptance requires matching private rendered screenshots for each pane and changed state, plus relevant functional checks. Functional tests alone cannot approve visual fidelity. Record discrepancies explicitly; do not call the application pixel-perfect without a measured comparison.

## Implementation work the design must not conceal

- The model helper currently supports catalog/check/verify/download/repair/cancel, not deletion. Delete in the prototype is a proposed confirmation flow; actual managed-asset deletion, ownership/dependency checks and in-use protection must be implemented before shipping it.
- The worker already saves hint publication provenance including submitted parameters. The Rust UI does not currently compare it with the edited configuration. Reuse that evidence and add missing input/prompt/upstream fingerprints where needed for reliable cross-session stale detection; do not claim old files are current merely because they exist.
- All prototype jobs, progress and model statuses are simulated. The HTML does not read or change the installed application, user settings, models or projects.
