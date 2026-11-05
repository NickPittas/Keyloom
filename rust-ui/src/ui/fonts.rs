//! Embedded fonts (DESIGN-BIBLE.md §3): Noto Sans Regular/Medium/SemiBold and IBM Plex Mono Regular, both SIL OFL
//! (licences beside the files in `assets/fonts/`). egui's bundled fonts stay AFTER ours in every family list.
use eframe::egui::{self, FontData, FontFamily};

/// Installs the families once at startup: `Proportional` = Regular, `Name("Medium")`, `Name("SemiBold")`, `Monospace` = Plex Mono.
pub fn install(ctx: &egui::Context) {
    let mut defs = egui::FontDefinitions::default(); // keeps the default_fonts (emoji, fallback)
    let ours = [
        ("NotoSans-Regular", &include_bytes!("../../assets/fonts/NotoSans-Regular.ttf")[..]),
        ("NotoSans-Medium", &include_bytes!("../../assets/fonts/NotoSans-Medium.ttf")[..]),
        ("NotoSans-SemiBold", &include_bytes!("../../assets/fonts/NotoSans-SemiBold.ttf")[..]),
        ("IBMPlexMono-Regular", &include_bytes!("../../assets/fonts/IBMPlexMono-Regular.ttf")[..]),
    ];
    // FontTweak stays at its defaults (scale 1, no y offset): measured against the Chromium reference, glyph size, advance
    // widths and baseline already match with hinting and sub-pixel binning on (egui's defaults); see theme.rs for the weight.
    for (name, bytes) in ours {
        defs.font_data.insert(name.to_owned(), std::sync::Arc::new(FontData::from_static(bytes)));
    }
    // The defaults of Proportional/Monospace are the fallback tail behind our first entry.
    let tail = |f: &FontFamily| defs.families.get(f).cloned().unwrap_or_default();
    let (prop_tail, mono_tail) = (tail(&FontFamily::Proportional), tail(&FontFamily::Monospace));
    let with = |first: &str, tail: &[String]| std::iter::once(first.to_owned()).chain(tail.iter().cloned()).collect::<Vec<_>>();
    defs.families.insert(FontFamily::Proportional, with("NotoSans-Regular", &prop_tail));
    defs.families.insert(FontFamily::Name("Medium".into()), with("NotoSans-Medium", &prop_tail));
    defs.families.insert(FontFamily::Name("SemiBold".into()), with("NotoSans-SemiBold", &prop_tail));
    // Monospace (11 px mono role) is Plex Mono UNHINTED, scale 1 (advances unchanged): hinted egui draws text.primary numbers on the
    // Light rows 1.11-1.18x too heavy vs Chromium; unhinted is 1.00-1.05 there and 0.91-0.95 in Dark (both inside 0.90-1.10).
    let mono_tweak = egui::FontTweak { hinting: Some(false), ..Default::default() };
    defs.font_data.insert("IBMPlexMono-Mono".to_owned(), std::sync::Arc::new(FontData::from_static(include_bytes!("../../assets/fonts/IBMPlexMono-Regular.ttf")).tweak(mono_tweak)));
    defs.families.insert(FontFamily::Monospace, with("IBMPlexMono-Mono", &mono_tail));
    // 12 px mono role only (the frame field): own Plex Mono entry, same bytes. Measured against the Chromium reference this
    // keeps its ink mass in 0.95-1.05 in both themes (hinted egui draws it 1.08 too heavy in Light); y_offset 0.5 keeps the top edge.
    let tweak = egui::FontTweak { scale: 0.99, hinting: Some(false), y_offset: 0.5, ..Default::default() };
    defs.font_data.insert("IBMPlexMono-Frame".to_owned(), std::sync::Arc::new(FontData::from_static(include_bytes!("../../assets/fonts/IBMPlexMono-Regular.ttf")).tweak(tweak)));
    defs.families.insert(FontFamily::Name("MonoFrame".into()), with("IBMPlexMono-Frame", &mono_tail));
    ctx.set_fonts(defs);
}

