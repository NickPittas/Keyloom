//! egui front end (provisional toolkit). Core modules never import this, so a toolkit swap stays here.
pub mod ai;
pub mod app;
pub mod graph;
pub mod import;
pub mod picker;
pub mod platform;
pub mod settings;
pub mod theme;
pub mod viewer;

pub use app::{App, AppOptions, CloseState, Tab};
