//! Board › Key selected shots batch dialog (bible §8.5, mockup page 12).
use super::ai::{AiUi, BatchDialog, BatchDialogJob, BatchDialogRow};
use super::board::{Fixture, FxShot};
use super::controls::{
    checkbox::Check,
    chips::{self, Job, Stage},
    dialog::{self, DialogSize},
    dropdown::{Dropdown, Item},
    line, paint_line, Vis,
};
use super::{
    icons::{self, Icon, IconSize},
    tokens::tokens,
    type_scale as ty,
};
use eframe::egui::{
    self, pos2, vec2, Align, Id, Layout, Order, Rect, Sense, Shape, Stroke, Ui, UiBuilder,
};

const COLS: [f32; 6] = [24.0, 132.0, 72.0, 84.0, 120.0, 94.0];
const TABLE_W: f32 = 526.0;
const ROW_H: f32 = 24.0;
const QUEUED_REASON: &str = "Already queued\nSend again from the card";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Counts {
    sent: usize,
    blocked: usize,
    queued: usize,
    skipped: usize,
}

fn counts(cards: &[FxShot]) -> Counts {
    let mut out = Counts::default();
    for shot in cards {
        if shot.q != 0 || shot.r != 0 {
            out.queued += 1;
        } else if shot.pc.is_empty() {
            out.blocked += 1;
        } else {
            out.sent += 1;
        }
    }
    out
}

fn six_cells(ui: &mut Ui, id: Id, height: f32, width: f32, add: impl FnOnce(&mut [Ui; 6])) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let mut row = ui.new_child(UiBuilder::new().id_salt(id).max_rect(rect));
    let completed_width = (line(ui.painter(), "Completed", ty::chip(), 14.0, f32::INFINITY).size().x + 28.0).round();
    let mut col_widths = COLS;
    col_widths[1] -= (completed_width - col_widths[2]).max(0.0);
    col_widths[2] = completed_width;
    let mut x = rect.left();
    let mut cells = std::array::from_fn(|i| {
        let width = if i == 5 {
            width - col_widths[..5].iter().sum::<f32>()
        } else {
            col_widths[i]
        };
        let cell_rect = Rect::from_min_size(pos2(x, rect.top()), vec2(width, height));
        x += width;
        row.new_child(
            UiBuilder::new()
                .id_salt(Id::unique(("board-batch-cell", i)))
                .max_rect(cell_rect)
                .layout(Layout::left_to_right(Align::Center)),
        )
    });
    add(&mut cells);
}

fn header(ui: &mut Ui, width: f32) {
    six_cells(
        ui,
        Id::unique("board-batch-header"),
        ROW_H,
        width,
        |cells| {
            let rect = Rect::from_min_size(cells[0].max_rect().min, vec2(width, ROW_H));
            let t = tokens(&cells[0]);
            cells[0].painter().rect_filled(rect, 0, t.bg_raised);
            cells[0].painter().hline(
                rect.x_range(),
                rect.bottom() - 0.5,
                Stroke::new(1.0, t.border_overlay),
            );
            let mut mixed = None;
            cells[0].add(Check::mixed(&mut mixed, None));
            for (i, label) in ["Shot", "Stage", "Job", "Machine", "Status"]
                .into_iter()
                .enumerate()
            {
                let cell = &mut cells[i + 1];
                let xpad = if i == 0 { 8.0 } else { 0.0 };
                let r =
                    Rect::from_min_max(cell.max_rect().min + vec2(xpad, 0.0), cell.max_rect().max);
                let g = line(cell.painter(), label, ty::chip(), 14.0, r.width());
                paint_line(
                    cell.painter(),
                    r,
                    g,
                    14.0,
                    Align::Min,
                    t.text_secondary,
                    0.0,
                );
            }
        },
    );
}

fn row(ui: &mut Ui, index: usize, shot: &FxShot, items: &[Item<'_>], width: f32) {
    let t = tokens(ui);
    six_cells(
        ui,
        Id::unique(("board-batch-row", index)),
        ROW_H,
        width,
        |cells| {
            let busy = shot.q != 0 || shot.r != 0;
            let mut include = !busy;
            let checkbox = if busy {
                Check::new(&mut include, None).disabled(QUEUED_REASON)
            } else {
                Check::new(&mut include, None)
            };
            cells[0].add(checkbox);

            let name_cell = &mut cells[1];
            let name_rect = name_cell.max_rect();
            let name = line(
                name_cell.painter(),
                &shot.n,
                ty::mono(),
                16.0,
                name_rect.width() - 8.0,
            );
            paint_line(
                name_cell.painter(),
                name_rect,
                name,
                16.0,
                Align::Min,
                t.text_primary,
                1.0,
            );

            let stage = match shot.st.as_str() {
                "mask" => Stage::Mask,
                "ready" => Stage::Ready,
                "done" => Stage::Completed,
                _ => Stage::Raw,
            };
            // Mockup: rows after the first sit the chip 4 px below the row top (row 0 has no top rule, so it stays at 3).
            let chip_rect = cells[2].max_rect().translate(vec2(0.0, if index > 0 { 1.0 } else { 0.0 }));
            let mut chip_cell = cells[2].new_child(
                UiBuilder::new()
                    .max_rect(chip_rect)
                    .layout(Layout::left_to_right(Align::Center)),
            );
            chips::stage_chip(&mut chip_cell, stage);

            if shot.q != 0 || shot.r != 0 {
                let job = if shot.q != 0 {
                    Job::Queued
                } else {
                    Job::Rendering(shot.prog / 100.0)
                };
                let look = chips::job_look(t, job);
                // Same 1 px rule as the stage chip: rows after the first sit lower in the mockup.
                let job_rect = cells[3].max_rect().translate(vec2(0.0, if index > 0 { 1.0 } else { 0.0 }));
                let mut job_cell = cells[3].new_child(
                    UiBuilder::new()
                        .max_rect(job_rect)
                        .layout(Layout::left_to_right(Align::Center)),
                );
                let badge_rect = job_cell
                    .allocate_exact_size(vec2(72.0, 18.0), Sense::hover())
                    .0;
                job_cell.painter().rect_filled(badge_rect, 3, look.fill);
                let mut badge = cells[3].new_child(UiBuilder::new().max_rect(badge_rect));
                chips::job_badge_motion(&mut badge, job, true);
            } else {
                let rect = cells[3].max_rect();
                let g = line(cells[3].painter(), "—", ty::label(), 16.0, rect.width());
                paint_line(
                    cells[3].painter(),
                    rect,
                    g,
                    16.0,
                    Align::Min,
                    t.text_tertiary,
                    0.0,
                );
            }

            let selected = if shot.pc == "Automatic" {
                0
            } else {
                items
                    .iter()
                    .position(|item| item.label == shot.pc)
                    .unwrap_or(items.len())
            };
            let mut pick = selected;
            let machine_rect = cells[4].max_rect();
            let mut dd = Dropdown::new(("board-batch-machine", index), &mut pick, items, 112.0)
                .name("Machine")
                .placeholder("Assign…")
                .disabled_if(busy, QUEUED_REASON)
                // Row 0 has no top rule, so its dropdown sits 1 px higher than the rest (mockup).
                .at(pos2(machine_rect.left(), machine_rect.center().y - if index == 0 { 11.0 } else { 10.0 }));
            if shot.pc == "Automatic" {
                dd = dd.tip("Automatic\nRuns on the next free machine");
            }
            dd.show(&mut cells[4]);

            // Mockup: status text in rows after the first sits 1 px lower (row 0 has no top rule).
            let status_cell = &mut cells[5];
            let rect = status_cell.max_rect().translate(vec2(0.0, if index > 0 { 1.0 } else { 0.0 }));
            let status = if shot.q != 0 {
                "In queue"
            } else if shot.r != 0 {
                "Rendering"
            } else if shot.pc.is_empty() {
                let icon_rect =
                    Rect::from_min_size(pos2(rect.left(), rect.center().y - 6.0), vec2(12.0, 12.0));
                icons::paint(
                    status_cell.painter(),
                    icon_rect,
                    Icon::Error,
                    IconSize::S12,
                    t.error,
                );
                let g = line(
                    status_cell.painter(),
                    "No machine",
                    ty::label(),
                    16.0,
                    (rect.width() - 16.0).max(0.0),
                );
                paint_line(
                    status_cell.painter(),
                    Rect::from_min_max(pos2(rect.left() + 16.0, rect.top()), rect.max),
                    g,
                    16.0,
                    Align::Min,
                    t.text_primary,
                    0.0,
                );
                return;
            } else if shot.st == "mask" {
                "Hint first"
            } else {
                "—"
            };
            let g = line(
                status_cell.painter(),
                status,
                ty::label(),
                16.0,
                rect.width(),
            );
            paint_line(
                status_cell.painter(),
                rect,
                g,
                16.0,
                Align::Min,
                t.text_tertiary,
                0.0,
            );
        },
    );
}

fn machine_list<'a>(fx: &'a Fixture, t: &super::tokens::Tokens) -> Vec<Item<'a>> {
    std::iter::once(Item::new("Automatic"))
        .chain(fx.machines.iter().map(|m| {
            Item::new(&m.name).dot(if m.state == "offline" {
                t.error
            } else {
                t.success
            })
        }))
        .collect::<Vec<_>>()
}

enum BatchEdit {
    Include(usize, bool),
    Machine(usize, Option<String>),
    Reuse(usize, bool),
    Reset(String),
}

fn machine_id(batch: &BatchDialog, pick: usize) -> Option<String> {
    batch.machines.get(pick).and_then(|machine| machine.id.clone())
}

fn status_label(text: &str) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(8).collect();
    if chars.next().is_some() { format!("{head}…") } else { head }
}

fn live_counts(rows: &[BatchDialogRow]) -> Counts {
    let queued = |row: &&BatchDialogRow| matches!(row.job, Some(BatchDialogJob::Queued | BatchDialogJob::Rendering));
    Counts {
        sent: rows.iter().filter(|r| r.included && r.queueable && !queued(r) && !r.blocked).count(),
        blocked: rows.iter().filter(|r| r.blocked && !queued(r)).count(),
        queued: rows.iter().filter(queued).count(),
        skipped: rows.iter().filter(|r| !r.included && !r.blocked && !queued(r)).count(),
    }
}

fn live_machine_list<'a>(batch: &'a BatchDialog, t: &super::tokens::Tokens) -> Vec<Item<'a>> {
    batch.machines.iter().map(|machine| {
        Item::new(&machine.label).dot(if machine.available { t.success } else { t.error })
    }).collect()
}

fn live_row(ui: &mut Ui, index: usize, row: &BatchDialogRow, batch: &BatchDialog, items: &[Item<'_>], width: f32, edits: &mut Vec<BatchEdit>) {
    let t = tokens(ui);
    six_cells(ui, Id::unique(("board-batch-live-row", index)), ROW_H, width, |cells| {
        let mut include = row.included;
        let check = if row.can_include { Check::new(&mut include, None) }
            else { Check::new(&mut include, None).disabled(row.reason.as_deref().unwrap_or("This shot cannot be included")) };
        cells[0].add(check);
        if include != row.included { edits.push(BatchEdit::Include(index, include)); }

        let galley = line(cells[1].painter(), &row.name, ty::mono(), 16.0, cells[1].max_rect().width() - 8.0);
        paint_line(cells[1].painter(), cells[1].max_rect(), galley, 16.0, Align::Min, t.text_primary, 1.0);
        let stage = match row.stage.as_str() { "mask" => Stage::Mask, "ready" => Stage::Ready, "done" => Stage::Completed, _ => Stage::Raw };
        let stage_rect = cells[2].max_rect();
        let mut stage_cell = cells[2].new_child(UiBuilder::new().max_rect(stage_rect).layout(Layout::left_to_right(Align::Center)));
        chips::stage_chip(&mut stage_cell, stage);

        let job = match row.job {
            Some(BatchDialogJob::Queued) => Some(Job::Queued),
            Some(BatchDialogJob::Rendering) => Some(Job::Rendering(0.0)),
            Some(BatchDialogJob::Preparing) => Some(Job::Preparing),
            Some(BatchDialogJob::Blocked) => Some(Job::Failed),
            None => None,
        };
        if let Some(job) = job {
            let look = chips::job_look(t, job);
            let rect = cells[3].allocate_exact_size(vec2(72.0, 18.0), Sense::hover()).0;
            cells[3].painter().rect_filled(rect, 3, look.fill);
            let mut badge = cells[3].new_child(UiBuilder::new().max_rect(rect));
            chips::job_badge_motion(&mut badge, job, true);
        } else {
            let rect = cells[3].max_rect();
            let painter = cells[3].painter().with_clip_rect(rect);
            let galley = line(&painter, "—", ty::label(), 16.0, rect.width());
            paint_line(&painter, rect, galley, 16.0, Align::Min, t.text_tertiary, 0.0);
        }

        let mut pick = batch.machines.iter().position(|machine| machine.id.as_ref() == row.machine.as_ref()).unwrap_or(items.len());
        let before = pick;
        let r = cells[4].max_rect();
        Dropdown::new(("board-batch-live-machine", index), &mut pick, items, 112.0)
            .name("Machine")
            .placeholder("Assign…")
            .disabled_if(matches!(row.job, Some(BatchDialogJob::Queued | BatchDialogJob::Rendering)), QUEUED_REASON)
            .at(pos2(r.left(), r.center().y - 10.0))
            .show(&mut cells[4]);
        if before != pick { edits.push(BatchEdit::Machine(index, machine_id(batch, pick))); }

        let status_cell = &mut cells[5];
        let has_decisions = row.accept.is_some() || row.offer.is_some() || row.reset_to_preset;
        if has_decisions {
            egui::ComboBox::from_id_salt(("board-batch-live-status", index))
                .width(status_cell.max_rect().width())
                .selected_text(status_label(&row.status))
                .show_ui(status_cell, |menu| {
                    if let Some(reason) = &row.reason { menu.label(reason); }
                    if let Some(note) = &row.reuse_note { menu.label(note); }
                    if let Some(accept) = &row.accept {
                        if menu.selectable_label(row.included, accept).clicked() { edits.push(BatchEdit::Include(index, true)); }
                    }
                    if let Some(offer) = &row.offer {
                        menu.label(offer);
                        let label = if row.mismatch { "Reuse anyway" } else { "Reuse" };
                        let mut reuse = row.reuse;
                        if menu.selectable_value(&mut reuse, true, label).clicked() { edits.push(BatchEdit::Reuse(index, true)); }
                        if menu.selectable_value(&mut reuse, false, "Re-render").clicked() { edits.push(BatchEdit::Reuse(index, false)); }
                    }
                    if row.can_include && menu.selectable_label(!row.included, "Skip").clicked() { edits.push(BatchEdit::Include(index, false)); }
                    if row.reset_to_preset && menu.button("Use preset values").clicked() { edits.push(BatchEdit::Reset(row.folder.clone())); }
                });
        } else {
            let rect = status_cell.max_rect();
            let color = if row.blocked { t.error } else if row.status == "—" { t.text_tertiary } else { t.text_primary };
            let galley = line(status_cell.painter(), &row.status, ty::label(), 16.0, rect.width());
            paint_line(status_cell.painter(), rect, galley, 16.0, Align::Min, color, 0.0);
        }
        if let Some(note) = &row.reuse_note {
            let rect = status_cell.max_rect();
            status_cell.interact(rect, egui::Id::unique(("board-batch-reuse-note", index)), Sense::hover()).on_hover_text(note);
        }
    });
}

/// Production dialog wired to the real pending `AiUi` batch. The fixture renderer below stays isolated for sheets.
pub fn live_dialog(ui: &mut egui::Ui, ai: &mut AiUi) {
    let Some(batch) = ai.batch_dialog() else { return };
    let ctx = ui.ctx().clone();
    if batch.planning { ctx.request_repaint_after(std::time::Duration::from_millis(50)); }
    let counts = live_counts(&batch.rows);
    let key_label = format!("  Key {} shots  ", counts.sent);
    let items = live_machine_list(&batch, tokens(ui));
    let mut edits = Vec::new();
    let mut preset_change: Option<Option<String>> = None;
    let mut bulk_pick = items.len();
    let height = (2.0 + 40.0 + 32.0 + 8.0 + 24.0 + ROW_H * batch.rows.len() as f32 + 16.0 + 52.0)
        .min((ctx.content_rect().height() - 64.0).max(0.0));
    let list_height = (height - 174.0).max(0.0).min(ROW_H * batch.rows.len() as f32);
    let out = dialog::Dialog::new("board-batch-dialog", "Key selected shots", DialogSize::M)
        .body(|body| {
            if batch.planning { body.label("Planning…"); return; }
            body.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let width = body.available_width();
            let (bulk_rect, _) = body.allocate_exact_size(vec2(width, 32.0), Sense::hover());
            let mut bulk = body.new_child(UiBuilder::new().id_salt("board-batch-live-bulk").max_rect(bulk_rect)
                .layout(Layout::left_to_right(Align::Center)));
            bulk.label("Preset");
            let current = batch.preset.as_ref();
            let selected = batch.presets.iter().find(|(name, _)| name.as_ref() == current)
                .map_or("Current settings (no preset)", |(_, label)| label.as_str());
            egui::ComboBox::from_id_salt("board-batch-live-preset").selected_text(selected).width(176.0)
                .show_ui(&mut bulk, |menu| {
                    for (name, label) in &batch.presets {
                        if menu.selectable_label(name.as_ref() == current, label).clicked() { preset_change = Some(name.clone()); }
                    }
                });
            bulk.add_space(8.0);
            bulk.label("Assign all blocked to");
            let before = bulk_pick;
            Dropdown::new("board-batch-live-bulk-machine", &mut bulk_pick, &items, 126.0)
                .name("Assign all blocked to").placeholder("Assign…").tip("Applies to blocked rows")
                .forced(Vis::Rest, false, true).show(&mut bulk);
            if before != bulk_pick {
                for (index, _) in batch.rows.iter().enumerate().filter(|(_, row)| row.blocked) {
                    edits.push(BatchEdit::Machine(index, machine_id(&batch, bulk_pick)));
                }
            }
            body.add_space(8.0);
            let list_width = if list_height < ROW_H * batch.rows.len() as f32 { TABLE_W - 8.0 } else { TABLE_W };
            header(body, list_width);
            let original_style = body.style().clone();
            let track = body.painter().add(Shape::Noop);
            let t = tokens(body);
            let out = crate::ui::theme::scroll_thumb_scope(body, |scroll_ui| {
                let style = scroll_ui.style_mut();
                style.spacing.scroll = egui::style::ScrollStyle { bar_width: 6.0, bar_inner_margin: 1.0, bar_outer_margin: 1.0, handle_min_length: 24.0, ..egui::style::ScrollStyle::solid() };
                style.spacing.scroll.fade.strength = 0.0;
                style.visuals.extreme_bg_color = t.bg_overlay;
                let out = egui::ScrollArea::vertical().id_salt("board-batch-live-list").max_width(TABLE_W).max_height(list_height)
                    .auto_shrink([false, false]).scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded)
                    .show(scroll_ui, |list| {
                        list.set_width(list_width);
                        list.spacing_mut().item_spacing = vec2(0.0, 0.0);
                        list.set_style(original_style.clone());
                        for (index, row) in batch.rows.iter().enumerate() {
                            let (rect, _) = list.allocate_exact_size(vec2(list_width, ROW_H), Sense::hover());
                            if index > 0 { list.painter().hline(rect.x_range(), rect.top() + 0.5, Stroke::new(1.0, t.border_overlay)); }
                            let mut row_ui = list.new_child(UiBuilder::new().id_salt(("board-batch-live-data", index)).max_rect(rect));
                            live_row(&mut row_ui, index, row, &batch, &items, list_width, &mut edits);
                        }
                    });
                let rect = out.inner_rect;
                if out.content_size.y > rect.height() {
                    scroll_ui.painter().set(track, Shape::Vec(vec![
                        Shape::rect_filled(Rect::from_min_size(rect.right_top(), vec2(8.0, rect.height())), 0, t.bg_overlay),
                        Shape::rect_filled(Rect::from_min_size(rect.right_top(), vec2(1.0, rect.height())), 0, t.border_overlay),
                    ]));
                } else { scroll_ui.painter().set(track, Shape::Noop); }
                scroll_ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, t.border_overlay));
                out
            });
            let _ = out;
        })
        .footer(|footer| {
            let footer = footer.cancel(" Cancel ").min_widths(72.0, 112.0).button_top(14.0);
            if batch.planning { footer } else { footer.primary(&key_label) }
        })
        .show(&ctx);

    if let Some(name) = preset_change {
        ai.batch_dialog_set_preset(&ctx, name);
    } else {
        for edit in edits {
            match edit {
                BatchEdit::Include(index, on) => ai.batch_dialog_set_included(index, on),
                BatchEdit::Machine(index, id) => ai.batch_dialog_set_machine(index, id.as_deref()),
                BatchEdit::Reuse(index, reuse) => ai.batch_dialog_set_reuse(index, reuse),
                BatchEdit::Reset(folder) => ai.batch_dialog_reset_to_preset(&ctx, &folder),
            }
        }
    }
    if out.primary_clicked {
        ai.confirm_batch();
    } else if out.cancel_clicked || out.closed {
        ai.cancel_batch();
    }

    let width = DialogSize::M.width().min((ctx.content_rect().width() - 32.0).max(0.0));
    let left = ((ctx.content_rect().left() + ctx.content_rect().right() - width) * 0.5).round() + 17.0;
    let top = ((ctx.content_rect().top() + ctx.content_rect().bottom() - height) * 0.5).round() + height - 34.0;
    let mut rect = Rect::from_min_size(pos2(left, top), vec2(width - 34.0 - 192.0, 14.0));
    rect.max.x = (ctx.content_rect().right() + ctx.content_rect().left() + width) * 0.5 - 17.0 - 192.0;
    let painter = ctx.layer_painter(egui::LayerId::new(Order::Tooltip, Id::unique("board-batch-live-footer")));
    footer_count(&painter, rect, counts, tokens(ui));
}

fn footer_count(painter: &egui::Painter, rect: Rect, counts: Counts, t: &super::tokens::Tokens) {
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_rows = 1;
    job.wrap.max_width = rect.width();
    job.append(
        &format!("{} will be sent · ", counts.sent),
        0.0,
        ty::format(ty::small(), t.text_secondary, 14.0),
    );
    job.append(
        &format!("{} need a machine", counts.blocked),
        0.0,
        ty::format(
            ty::small(),
            if counts.blocked > 0 { t.error } else { t.text_secondary },
            14.0,
        ),
    );
    job.append(
        &format!(" · {} in queue · {} skipped", counts.queued, counts.skipped),
        0.0,
        ty::format(ty::small(), t.text_secondary, 14.0),
    );
    let galley = painter.layout_job(job);
    painter.galley(rect.min, galley, t.text_secondary);
}

/// Fixture renderer for the Board's selected-shots batch dialog.
pub fn dialog(ui: &mut egui::Ui, fx: &Fixture) {
    let ctx = ui.ctx().clone();
    let counts = counts(&fx.cards);
    let key_label = format!("  Key {} shots  ", counts.sent);
    let items = machine_list(fx, tokens(ui));
    let mut bulk_pick = items.len();
    let height = (2.0 + 40.0 + 32.0 + 8.0 + 24.0 + ROW_H * fx.cards.len() as f32 + 16.0 + 52.0)
        .min((ctx.content_rect().height() - 64.0).max(0.0));
    let list_height = (height - 174.0).max(0.0).min(ROW_H * fx.cards.len() as f32);

    dialog::Dialog::new("board-batch-dialog", "Key selected shots", DialogSize::M)
        .body(|body| {
            body.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let width = body.available_width();
            let (bulk_rect, _) = body.allocate_exact_size(vec2(width, 32.0), Sense::hover());
            let t = tokens(body);
            let label = "Assign all blocked to";
            let label_galley = line(body.painter(), label, ty::small(), 14.0, f32::INFINITY);
            let label_width = label_galley.size().x;
            paint_line(
                body.painter(),
                Rect::from_min_size(bulk_rect.min, vec2(label_width, 32.0)),
                label_galley,
                14.0,
                Align::Min,
                t.text_tertiary,
                0.0,
            );
            let mut bulk = body.new_child(
                UiBuilder::new()
                    .id_salt("board-batch-bulk")
                    .max_rect(bulk_rect),
            );
            Dropdown::new("board-batch-bulk-machine", &mut bulk_pick, &items, 160.0)
                .name("Assign all blocked to")
                .placeholder("Assign…")
                .tip("Applies on pick")
                .forced(Vis::Rest, false, true)
                .at(pos2(
                    bulk_rect.left() + label_width + 12.0,
                    bulk_rect.center().y - 11.0,
                ))
                .show(&mut bulk);

            body.add_space(8.0);
            let list_width = if list_height < ROW_H * fx.cards.len() as f32 {
                TABLE_W - 8.0
            } else {
                TABLE_W
            };
            header(body, list_width);
            let original_style = body.style().clone();
            let track = body.painter().add(Shape::Noop);
            let t = tokens(body);
            let out = crate::ui::theme::scroll_thumb_scope(body, |scroll_ui| {
                let style = scroll_ui.style_mut();
                style.spacing.scroll = egui::style::ScrollStyle {
                    bar_width: 6.0,
                    bar_inner_margin: 1.0,
                    bar_outer_margin: 1.0,
                    handle_min_length: 24.0,
                    ..egui::style::ScrollStyle::solid()
                };
                style.spacing.scroll.fade.strength = 0.0;
                style.visuals.extreme_bg_color = t.bg_overlay;
                let out = egui::ScrollArea::vertical()
                    .id_salt("board-batch-list")
                    .max_width(TABLE_W)
                    .max_height(list_height)
                    .auto_shrink([false, false])
                    .scroll_bar_visibility(
                        egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded,
                    )
                    .show(scroll_ui, |list| {
                        list.set_width(list_width);
                        list.spacing_mut().item_spacing = vec2(0.0, 0.0);
                        list.set_style(original_style.clone());
                        for (index, shot) in fx.cards.iter().enumerate() {
                            let (row_rect, _) =
                                list.allocate_exact_size(vec2(list_width, ROW_H), Sense::hover());
                            // Row 0's top rule would double the header's bottom rule; mockup draws one line.
                            if index > 0 {
                                list.painter().hline(
                                    row_rect.x_range(),
                                    row_rect.top() + 0.5,
                                    Stroke::new(1.0, t.border_overlay),
                                );
                            }
                            // The row UI occupies the same rectangle already reserved for its separator and fixed height.
                            let mut row_ui = list.new_child(
                                UiBuilder::new()
                                    .id_salt(Id::unique(("batch-data", index)))
                                    .max_rect(row_rect),
                            );
                            row(&mut row_ui, index, shot, &items, list_width);
                        }
                    });
                let r = out.inner_rect;
                if out.content_size.y > r.height() {
                    scroll_ui.painter().set(
                        track,
                        Shape::Vec(vec![
                            Shape::rect_filled(
                                Rect::from_min_size(r.right_top(), vec2(8.0, r.height())),
                                0,
                                t.bg_overlay,
                            ),
                            Shape::rect_filled(
                                Rect::from_min_size(r.right_top(), vec2(1.0, r.height())),
                                0,
                                t.border_overlay,
                            ),
                        ]),
                    );
                } else {
                    scroll_ui.painter().set(track, Shape::Noop);
                }
                scroll_ui.painter().hline(
                    r.x_range(),
                    r.bottom() - 0.5,
                    Stroke::new(1.0, t.border_overlay),
                );
                out
            });
            let _ = out;
        })
        .footer(|footer| footer.cancel(" Cancel ").primary(&key_label).min_widths(72., 112.).button_top(14.))
        .show(&ctx);

    let width = DialogSize::M
        .width()
        .min((ctx.content_rect().width() - 32.0).max(0.0));
    let left =
        ((ctx.content_rect().left() + ctx.content_rect().right() - width) * 0.5).round() + 17.0;
    let top = ((ctx.content_rect().top() + ctx.content_rect().bottom() - height) * 0.5).round()
        + height
        - 34.0;
    let mut text_rect = Rect::from_min_size(pos2(left, top), vec2(width - 34.0 - 192.0, 14.0));
    text_rect.max.x =
        (ctx.content_rect().right() + ctx.content_rect().left() + width) * 0.5 - 17.0 - 192.0;
    let painter = ctx.layer_painter(egui::LayerId::new(
        Order::Tooltip,
        Id::unique("board-batch-footer"),
    ));
    footer_count(&painter, text_rect, counts, tokens(ui));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn board_12_footer_counts() {
        let fx: Fixture =
            serde_json::from_str(include_str!("../../tests/fixtures/board-12.json")).unwrap();
        assert_eq!(
            counts(&fx.cards),
            Counts {
                sent: 24,
                blocked: 4,
                queued: 2,
                skipped: 0
            }
        );
    }

    #[test]
    fn compatible_hint_reuse_note_is_visible_in_the_row() {
        let note = "Reusing the existing compatible hint";
        let row = BatchDialogRow {
            folder: "shot".into(), name: "Shot".into(), stage: "done".into(), machine: None,
            job: None, status: note.into(), reason: None, included: true, can_include: true,
            accept: None, offer: None, reuse_note: Some(note.into()), reuse: true, mismatch: false,
            reset_to_preset: false, queueable: true, blocked: false,
        };
        let batch = BatchDialog { planning: false, preset: None, presets: vec![], machines: vec![], rows: vec![row.clone()] };
        let ctx = egui::Context::default();
        let out = ctx.run_ui(
            egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 80.0))), ..Default::default() },
            |ui| live_row(ui, 0, &row, &batch, &[], 1100.0, &mut vec![]),
        );
        fn has_text(shape: &egui::epaint::Shape, wanted: &str) -> bool {
            match shape {
                egui::epaint::Shape::Text(text) => text.galley.job.text == wanted,
                egui::epaint::Shape::Vec(shapes) => shapes.iter().any(|shape| has_text(shape, wanted)),
                _ => false,
            }
        }
        assert!(out.shapes.iter().any(|shape| has_text(&shape.shape, note)), "the automatic reuse note is drawn in the row");
    }
}
