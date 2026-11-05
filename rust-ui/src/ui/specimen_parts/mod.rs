//! `--specimen-part NAME`: one specimen section per foundation control (parallel batch).
use eframe::egui::Ui;
pub mod dropdown;
pub mod textfield;
pub mod numeric;
pub mod checkbox;
pub mod menus;
pub mod chips;
pub mod scroll;
pub mod table;
pub mod property;
pub mod dialog;
pub mod card;
pub mod activity;
pub mod statusbar;
pub mod node;
pub mod transport;
pub mod viewer_toolbar;
pub mod machine_row;
pub fn show(name: &str, ui: &mut Ui) -> bool {
    match name {
        "dropdown" => dropdown::show(ui),
        "textfield" => textfield::show(ui),
        "numeric" => numeric::show(ui),
        "checkbox" => checkbox::show(ui),
        "menus" => menus::show(ui),
        "chips" => chips::show(ui),
        "scroll" => scroll::show(ui),
        "table" => table::show(ui),
        "property" => property::show(ui),
        "dialog" => dialog::show(ui),
        "card" => card::show(ui),
        "activity" => activity::show(ui),
        "statusbar" => statusbar::show(ui),
        "node" => node::show(ui),
        "transport" => transport::show(ui),
        "viewer_toolbar" => viewer_toolbar::show(ui),
        "machine_row" => machine_row::show(ui),
        _ => return false,
    }
    true
}
