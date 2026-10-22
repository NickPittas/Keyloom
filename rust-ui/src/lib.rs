//! CorridorKey native viewer core. Everything here is read-only with respect to project data and
//! free of UI types so the toolkit can be swapped (see `ui`).
pub mod contracts;
pub mod decode;
pub mod frames;
pub mod import;
pub mod jobs;
pub mod natural_sort;
pub mod project;
pub mod scheduler;
pub mod settings;
pub mod ui;
pub mod worker;
pub mod workflow;
