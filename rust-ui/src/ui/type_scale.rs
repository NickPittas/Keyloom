//! Text roles from DESIGN-BIBLE.md §3 (sizes 11 / 12 / 13 / 15 / 22). Each returns the role's `FontId`.
//! Not used by the existing screens yet. Line heights (28/20/18/16/14) are applied by the text helpers in a later step.
use eframe::egui::{FontFamily, FontId};

fn name(n: &str, size: f32) -> FontId { FontId::new(size, FontFamily::Name(n.into())) }
fn regular(size: f32) -> FontId { FontId::new(size, FontFamily::Proportional) }

/// Start screen title: 22 SemiBold.
pub fn display() -> FontId { name("SemiBold", 22.0) }
/// Page / dialog title: 15 SemiBold.
pub fn title() -> FontId { name("SemiBold", 15.0) }
/// Shot header name: 13 SemiBold.
pub fn shot_name() -> FontId { name("SemiBold", 13.0) }
/// Panel header: 12 SemiBold.
pub fn panel_header() -> FontId { name("SemiBold", 12.0) }
/// Section header: 12 Medium.
pub fn section_header() -> FontId { name("Medium", 12.0) }
/// Property label, menu item: 12 Regular.
pub fn label() -> FontId { regular(12.0) }
/// Button and tab label: 12 Medium.
pub fn button() -> FontId { name("Medium", 12.0) }
/// Field / dropdown content: 12 Regular.
pub fn value() -> FontId { regular(12.0) }
/// Small / meta: 11 Regular.
pub fn small() -> FontId { regular(11.0) }
/// Chip / badge: 11 Medium.
pub fn chip() -> FontId { name("Medium", 11.0) }
/// Mono (paths, ids, timecode, status-bar numbers): 11 Plex Mono.
pub fn mono() -> FontId { FontId::new(11.0, FontFamily::Monospace) }
/// Frame number field: 12 Plex Mono.
pub fn frame_field() -> FontId { name("MonoFrame", 12.0) }

/// A `TextFormat` with the role's bible line height (28/20/18/16/14). Chrome rounds the half-leading of the 12 px roles
/// (Noto 17 px / Plex 15 px content in a 16 / 14 px box) down, egui rounds it up (1 px too low), so 12 px roles get
/// `lh - 2` here. NOTE: the galley is then 2 px shorter than the role's box; callers allocate the bible height themselves.
pub fn format(font_id: FontId, color: eframe::egui::Color32, line_height: f32) -> eframe::egui::TextFormat {
    let lh = if font_id.size == 12.0 { line_height - 2.0 } else { line_height };
    eframe::egui::TextFormat { font_id, color, line_height: Some(lh), ..Default::default() }
}
