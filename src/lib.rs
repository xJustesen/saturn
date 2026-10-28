//! saturn — a keyboard-first TUI Jupyter notebook.

pub mod app;
pub mod commands;
pub mod config;
pub mod editor;
pub mod keymap;
pub mod kernel;
pub mod notebook;
pub mod render;
pub mod theme;
pub mod ui;

use std::path::PathBuf;

/// Command line options.
#[derive(Debug, Clone, Default)]
pub struct Opts {
    pub path: Option<PathBuf>,
    pub kernel: Option<String>,
    pub log: Option<PathBuf>,
}
