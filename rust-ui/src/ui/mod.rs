//! egui front end (provisional toolkit). Core modules never import this, so a toolkit swap stays here.
pub mod ai;
pub mod annotate;
pub mod clients;
pub mod app;
pub mod graph;
pub mod import;
pub mod metrics_bar;
pub mod models;
pub mod picker;
pub mod platform;
pub mod settings;
pub mod theme;
pub mod thumbs;
pub mod viewer;

pub use app::{App, AppOptions, CloseState, Tab};
