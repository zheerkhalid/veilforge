//! The window: screens, dialogs and the small password prompt.
pub mod app;
mod bidi;
mod dialogs;
mod list;
mod main_view;
pub mod ops;
mod pages;
pub mod prompt;
mod protect;
pub mod start;
pub mod theme;
pub mod widgets;

pub use app::{run, Options};
