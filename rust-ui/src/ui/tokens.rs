//! Keyloom design-bible colour tokens (DESIGN-BIBLE.md §2, §5.14), Dark and Light. One field per token, named after
//! the bible token (dots and hyphens become underscores). Alpha tokens use `from_rgba_unmultiplied_const`.
use eframe::egui::{self, Color32};

/// `0xRRGGBB` -> opaque colour.
const fn x(h: u32) -> Color32 { Color32::from_rgb((h >> 16) as u8, (h >> 8) as u8, h as u8) }
/// `0xRRGGBB` at alpha `a` (0..=255), unmultiplied.
const fn xa(h: u32, a: u8) -> Color32 { Color32::from_rgba_unmultiplied_const((h >> 16) as u8, (h >> 8) as u8, h as u8, a) }

#[derive(Clone, Copy, Debug)]
pub struct Tokens {
    // 2.1 Surfaces
    pub bg_app: Color32, pub bg_panel: Color32, pub bg_raised: Color32, pub bg_hover: Color32, pub bg_pressed: Color32,
    pub bg_menu_hover: Color32, pub bg_sunken: Color32, pub bg_overlay: Color32, pub bg_selected: Color32,
    pub viewer_surround: Color32, pub graph_canvas: Color32, pub graph_grid: Color32, pub scrim: Color32,
    // 2.2 Borders
    pub border_divider: Color32, pub border_default: Color32, pub border_control: Color32, pub border_hover: Color32,
    pub border_overlay: Color32,
    // 2.3 Text
    pub text_primary: Color32, pub text_secondary: Color32, pub text_tertiary: Color32, pub text_disabled: Color32,
    pub text_link: Color32, pub text_on_accent: Color32, pub text_on_danger: Color32,
    // 2.4 Accent, focus, selection
    pub accent: Color32, pub accent_hover: Color32, pub accent_pressed: Color32, pub accent_bright: Color32,
    pub accent_tint: Color32, pub busy_fill: Color32, pub busy_queued: Color32, pub focus_ring: Color32,
    pub text_selection: Color32,
    // 2.5 Status
    pub success: Color32, pub success_tint: Color32, pub warning: Color32, pub warning_tint: Color32,
    pub error: Color32, pub error_tint: Color32, pub info: Color32, pub info_tint: Color32,
    pub danger_fill: Color32, pub danger_fill_hover: Color32, pub danger_fill_pressed: Color32,
    // 2.6 Stage chips and bars; job badge label colours (badge fills are busy_queued / busy_fill)
    pub stage_raw_chip_fill: Color32, pub stage_raw_chip_text: Color32, pub stage_raw_bar: Color32,
    pub stage_mask_chip_fill: Color32, pub stage_mask_chip_text: Color32, pub stage_mask_bar: Color32,
    pub stage_ready_chip_fill: Color32, pub stage_ready_chip_text: Color32, pub stage_ready_bar: Color32,
    pub stage_completed_chip_fill: Color32, pub stage_completed_chip_text: Color32, pub stage_completed_bar: Color32,
    pub stage_failed_chip_fill: Color32, pub stage_failed_chip_text: Color32, pub stage_failed_bar: Color32,
    pub badge_queued_text: Color32, pub badge_rendering_text: Color32,
    // 2.7 Node graph
    pub node_kind_source: Color32, pub node_kind_mask: Color32, pub node_kind_key: Color32, pub node_kind_output: Color32,
    pub node_fill: Color32, pub node_border: Color32, pub node_selected: Color32,
    pub wire_image: Color32, pub wire_matte: Color32,
    // 5.14 Scrollbar thumb
    pub scroll_thumb: Color32, pub scroll_thumb_hover: Color32, pub scroll_thumb_drag: Color32,
}

pub const DARK: Tokens = Tokens {
    bg_app: x(0x181818), bg_panel: x(0x202020), bg_raised: x(0x292929), bg_hover: x(0x383838), bg_pressed: x(0x4C4C4C),
    bg_menu_hover: x(0x505A6A), bg_sunken: x(0x141414), bg_overlay: x(0x2C2C2C), bg_selected: x(0x474747),
    viewer_surround: x(0x2E2E2E), graph_canvas: x(0x1A1A1A), graph_grid: x(0x2A2A2A), scrim: xa(0x000000, 128),
    border_divider: x(0x303030), border_default: x(0x383838), border_control: x(0x757575), border_hover: x(0xA0A0A0),
    border_overlay: x(0x454545),
    text_primary: x(0xE8E8E8), text_secondary: x(0xB0B0B0), text_tertiary: x(0x969696), text_disabled: x(0x7A7A7A),
    text_link: x(0x82B1FF), text_on_accent: x(0xFFFFFF), text_on_danger: x(0xFFFFFF),
    accent: x(0x2D6BD0), accent_hover: x(0x3572D8), accent_pressed: x(0x1D4E9E), accent_bright: x(0x4C8DF6),
    accent_tint: x(0x1D3553), busy_fill: x(0xD0D0D0), busy_queued: x(0xA6A6A6), focus_ring: x(0x8AB8FF),
    text_selection: xa(0x2D6BD0, 115),
    success: x(0x4CC38A), success_tint: x(0x163A29), warning: x(0xE8A93A), warning_tint: x(0x3D2E12),
    error: x(0xF2665E), error_tint: x(0x42201E), info: x(0xC4C4C4), info_tint: x(0x2F2F2F),
    danger_fill: x(0xC2352C), danger_fill_hover: x(0xCC3F35), danger_fill_pressed: x(0xA82C24),
    stage_raw_chip_fill: x(0x3A3A3A), stage_raw_chip_text: x(0xD6D6D6), stage_raw_bar: x(0x757575),
    stage_mask_chip_fill: x(0x33294D), stage_mask_chip_text: x(0xCDBDFF), stage_mask_bar: x(0xA78BFA),
    stage_ready_chip_fill: x(0x12363C), stage_ready_chip_text: x(0x7FDDEA), stage_ready_bar: x(0x1A9AAD),
    stage_completed_chip_fill: x(0x163A29), stage_completed_chip_text: x(0x8BE0B4), stage_completed_bar: x(0x4CC38A),
    stage_failed_chip_fill: x(0x42201E), stage_failed_chip_text: x(0xFF9A92), stage_failed_bar: x(0xF2665E),
    badge_queued_text: x(0x1A1A1A), badge_rendering_text: x(0x1A1A1A),
    node_kind_source: x(0x8E8E8E), node_kind_mask: x(0xA78BFA), node_kind_key: x(0x22B8CF), node_kind_output: x(0x4CC38A),
    node_fill: x(0x292929), node_border: x(0x3F3F3F), node_selected: x(0x4C8DF6),
    wire_image: x(0x9A9A9A), wire_matte: x(0xA78BFA),
    scroll_thumb: x(0x767676), scroll_thumb_hover: x(0x8A8A8A), scroll_thumb_drag: x(0xA3A3A3),
};

pub const LIGHT: Tokens = Tokens {
    bg_app: x(0xE2E2E2), bg_panel: x(0xEFEFEF), bg_raised: x(0xFAFAFA), bg_hover: x(0xDCDCDC), bg_pressed: x(0xC6C6C6),
    bg_menu_hover: x(0xC9D6EA), bg_sunken: x(0xFFFFFF), bg_overlay: x(0xFCFCFC), bg_selected: x(0xF4F4F4),
    viewer_surround: x(0x2E2E2E), graph_canvas: x(0xE8E8E8), graph_grid: x(0xD4D4D4), scrim: xa(0x000000, 77),
    border_divider: x(0xD6D6D6), border_default: x(0xCFCFCF), border_control: x(0x848484), border_hover: x(0x5E5E5E),
    border_overlay: x(0xC4C4C4),
    text_primary: x(0x1A1A1A), text_secondary: x(0x474747), text_tertiary: x(0x5C5C5C), text_disabled: x(0x7E7E7E),
    text_link: x(0x1A5BC4), text_on_accent: x(0xFFFFFF), text_on_danger: x(0xFFFFFF),
    accent: x(0x1F62CC), accent_hover: x(0x2C6EDA), accent_pressed: x(0x164A9E), accent_bright: x(0x2F6FDB),
    accent_tint: x(0xBFD4F5), busy_fill: x(0x333333), busy_queued: x(0x5A5A5A), focus_ring: x(0x0B3D91),
    text_selection: xa(0x1F62CC, 64),
    success: x(0x17703F), success_tint: x(0xDDF2E6), warning: x(0x8A5A00), warning_tint: x(0xFCEFD6),
    error: x(0xB42318), error_tint: x(0xFBE3E0), info: x(0x404040), info_tint: x(0xE6E6E6),
    danger_fill: x(0xB42318), danger_fill_hover: x(0xC2342A), danger_fill_pressed: x(0x9A1D14),
    stage_raw_chip_fill: x(0xE0E0E0), stage_raw_chip_text: x(0x363636), stage_raw_bar: x(0x8A8A8A),
    stage_mask_chip_fill: x(0xECE6FD), stage_mask_chip_text: x(0x5434AE), stage_mask_bar: x(0x7C5CE0),
    stage_ready_chip_fill: x(0xDAF2F5), stage_ready_chip_text: x(0x0B6370), stage_ready_bar: x(0x138FA3),
    stage_completed_chip_fill: x(0xDDF2E6), stage_completed_chip_text: x(0x156A3B), stage_completed_bar: x(0x17703F),
    stage_failed_chip_fill: x(0xFBE3E0), stage_failed_chip_text: x(0x9E1F18), stage_failed_bar: x(0xD23B30),
    badge_queued_text: x(0xFFFFFF), badge_rendering_text: x(0xFFFFFF),
    node_kind_source: x(0x7A7A7A), node_kind_mask: x(0x7C5CE0), node_kind_key: x(0x138FA3), node_kind_output: x(0x2A955C),
    node_fill: x(0xFAFAFA), node_border: x(0xC4C4C4), node_selected: x(0x2F6FDB),
    wire_image: x(0x6E6E6E), wire_matte: x(0x7C5CE0),
    scroll_thumb: x(0x878787), scroll_thumb_hover: x(0x6E6E6E), scroll_thumb_drag: x(0x575757),
};

/// Something that knows the current theme: a `Context` (global style) or a `Ui` (its own scope).
pub trait HasTheme { fn is_dark(&self) -> bool; }
impl HasTheme for egui::Context { fn is_dark(&self) -> bool { self.global_style().visuals.dark_mode } }
impl HasTheme for egui::Ui { fn is_dark(&self) -> bool { self.visuals().dark_mode } }

/// The token set for the current theme.
pub fn tokens(src: &impl HasTheme) -> &'static Tokens { if src.is_dark() { &DARK } else { &LIGHT } }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spot_check_values() {
        assert_eq!(DARK.bg_panel, Color32::from_rgb(0x20, 0x20, 0x20));
        assert_eq!(LIGHT.focus_ring, Color32::from_rgb(0x0B, 0x3D, 0x91));
        assert_eq!(DARK.text_selection, Color32::from_rgba_unmultiplied(0x2D, 0x6B, 0xD0, 115));
        assert_eq!(LIGHT.scrim, Color32::from_rgba_unmultiplied(0, 0, 0, 77));
    }
}
