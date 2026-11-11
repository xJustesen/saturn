//! saturn — a keyboard-first TUI Jupyter notebook.

pub mod app;
pub mod commands;
pub mod config;
pub mod editor;
pub mod files;
pub mod keymap;
pub mod kernel;
pub mod notebook;
pub mod recent;
pub mod render;
pub mod search;
pub mod syntax;
pub mod theme;
pub mod ui;
pub mod vim;

use std::path::PathBuf;

/// Command line options.
#[derive(Debug, Clone, Default)]
pub struct Opts {
    pub path: Option<PathBuf>,
    pub kernel: Option<String>,
    pub log: Option<PathBuf>,
    /// When `main` started, for the dashboard's "ready in" figure.
    pub started: Option<std::time::Instant>,
    /// The terminal supports SGR-pixel mouse reporting (set by `main`).
    pub pixel_mouse: bool,
}
