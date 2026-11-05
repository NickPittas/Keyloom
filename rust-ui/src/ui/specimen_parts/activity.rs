//! Reference section "8.7 · Activity popover" of specimen.html, original x, dy = -7707 px (sheet top 7723 -> 16).
use crate::ui::{
    controls::activity::{show_in, Action, ActivityItem, ActivityState, Forced, ItemKind, JobState, NoticeLevel, WIDTH},
    controls::{line, paint_line},
    tokens::tokens,
    type_scale as ty,
};
use eframe::egui::{self, pos2, vec2, Align, Rect, Stroke, StrokeKind, Ui};

const DY: f32 = -7707.0;

fn item<'a>(id: u64, kind: ItemKind, title: &'a str, time: &'a str, detail: &'a str, actions: &'a [Action]) -> ActivityItem<'a> {
    ActivityItem { id, kind, title, detail, time, actions }
}

pub fn show(ui: &mut Ui) {
    let t = tokens(ui);
    let p = ui.painter().clone();
    let sheet = Rect::from_min_size(pos2(32.0, 7723.0 + DY), vec2(939.421875, 830.0));
    p.rect_filled(sheet, 4.0, t.bg_panel);
    p.rect_stroke(sheet, 4.0, Stroke::new(1.0, t.border_divider), StrokeKind::Inside);
    let at = |x: f32, y: f32, w: f32, h: f32| Rect::from_min_size(pos2(x, y + DY), vec2(w, h));
    let h2 = at(49.0, 7740.0, 905.0, 20.0);
    paint_line(&p, h2, line(&p, "8.7 · Activity popover", ty::title(), 20.0, 905.0), 20.0, Align::Min, t.text_primary, 0.0);
    let caption = |y: f32, s: &str| {
        let r = at(49.0, y, 905.4, 14.0);
        paint_line(&p, r, line(&p, s, ty::small(), 14.0, 905.4), 14.0, Align::Min, t.text_tertiary, 0.0);
    };

    let done = |id, title, time, detail| item(id, ItemKind::Job(JobState::Done), title, time, detail, &[Action::Reveal]);
    const RETRY_DISMISS: [Action; 2] = [Action::Retry, Action::Dismiss];
    const CANCEL: [Action; 1] = [Action::Cancel];
    const CANCEL_REVEAL: [Action; 2] = [Action::Cancel, Action::Reveal];
    const DISMISS: [Action; 1] = [Action::Dismiss];

    // Short popover: running (hovered, Cancel), done, failed, notice.
    let short = [
        item(1, ItemKind::Job(JobState::Running(Some(0.34))), "Keying V1-0062", "now", "endor-omarchy · frame 22 / 65 · 34%", &CANCEL),
        done(2, "Exported V1-0046", "2 min ago", "This machine · Matte, Processed · 108 frames"),
        item(3, ItemKind::Job(JobState::Failed), "Keying failed: out of GPU memory", "14:02", "render-node-07 · V1-0071", &RETRY_DISMISS),
        item(4, ItemKind::Notice(NoticeLevel::Info), "No system tray: closing the window quits Keyloom.", "13:40", "Notice · Details", &DISMISS),
    ];
    let forced = Forced { hover: Some(0), scroll: None, reduced_motion: true };
    show_in(ui, at(49.0, 7772.0, WIDTH, 230.0), &mut ActivityState::default(), &short, forced);

    caption(8014.0, "Long content, scrolled (list body 176, header and tabs pinned, scrollbar visible) · hovered row shows its tools");
    let scrolled = [
        done(11, "Exported V1-0040", "13:12", "This machine · FG, Matte · 96 frames"),
        item(12, ItemKind::Job(JobState::Failed), "Pre-matte failed: GVM ran out of memory on frame 41 of a very long shot name V1-0031_A001C…", "", "render-node-07 · V1-0031", &RETRY_DISMISS),
        done(13, "Keyed V1-0028", "12:47", "endor-omarchy · 120 frames"),
        done(14, "Hint done V1-0025", "12:30", "This machine · 65 frames"),
        done(15, "Exported V1-0022", "12:02", "This machine · Comp · 48 frames"),
        item(26, ItemKind::Notice(NoticeLevel::Info), "No system tray: closing the window quits Keyloom.", "13:40", "Notice · Details", &DISMISS),
    ];
    let forced = Forced { hover: Some(1), scroll: Some(22.0), reduced_motion: true };
    show_in(ui, at(49.0, 8032.0, WIDTH, 230.0), &mut ActivityState::default(), &scrolled, forced);

    // Caption 3 has an inline `bg.hover` in mono; it wraps at the sheet width like the HTML.
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = 905.4;
    job.append("Long content in one frame · 480 wide, list scrolled (scrollbar 8, text column 424) · hovered running row: long title, “Yesterday”, long machine name, Cancel + Reveal on a ", 0.0, ty::format(ty::small(), t.text_tertiary, 14.0));
    job.append("bg.hover", 0.0, ty::format(ty::mono(), t.text_tertiary, 14.0));
    job.append(" backing", 0.0, ty::format(ty::small(), t.text_tertiary, 14.0));
    let g = p.layout_job(job);
    p.galley(pos2(49.0, 8274.0 + DY), g, t.text_tertiary);

    let long = [
        done(21, "Exported V1-0040", "13:12", "This machine · FG, Matte · 96 frames"),
        item(22, ItemKind::Job(JobState::Running(Some(0.25))), "Keying V1-0046_A002C008_261012_R4LF_interview_wide", "Yesterday", "render-node-07-windows-long-name · frame 1022 / 4096", &CANCEL_REVEAL),
        done(23, "Keyed V1-0028", "12:47", "endor-omarchy · 120 frames"),
        done(24, "Hint done V1-0025", "12:30", "This machine · 65 frames"),
        done(25, "Exported V1-0022", "12:02", "This machine · Comp · 48 frames"),
        item(26, ItemKind::Notice(NoticeLevel::Info), "No system tray: closing the window quits Keyloom.", "13:40", "Notice · Details", &DISMISS),
    ];
    let forced = Forced { hover: Some(1), scroll: Some(22.0), reduced_motion: true };
    show_in(ui, at(49.0, 8306.0, WIDTH, 230.0), &mut ActivityState::default(), &long, forced);
}
