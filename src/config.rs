//! User configuration (`$XDG_CONFIG_HOME/saturn/config.toml`).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Maximum height of an inline image, in terminal rows.
    pub max_image_rows: u16,
    /// Same, when only unicode halfblocks are available.
    pub max_image_rows_halfblocks: u16,
    /// Streams longer than this are truncated until expanded with `o`.
    pub stream_max_lines: usize,
    /// Write the PNG produced by the plotly sidecar into the notebook output.
    pub persist_plotly_png: bool,
    /// Pixel scale factor for plotly renders.
    pub plotly_scale: f64,
    /// Syntect theme name used for code cells.
    pub theme: String,
    /// Show line numbers inside the focused cell editor.
    pub line_numbers: bool,
    /// Tab width used by the editor.
    pub tab_width: usize,
    /// Wrap long lines in cells.
    pub wrap: bool,
    /// Fuzzy file browser: show hidden entries.
    pub show_hidden: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_image_rows: 20,
            max_image_rows_halfblocks: 12,
            stream_max_lines: 500,
            persist_plotly_png: true,
            plotly_scale: 2.0,
            theme: "base16-ocean.dark".to_string(),
            line_numbers: false,
            tab_width: 4,
            wrap: true,
            show_hidden: false,
        }
    }
}

impl Config {
    pub fn path() -> Option<std::path::PathBuf> {
        dirs::config_dir().map(|d| d.join("saturn").join("config.toml"))
    }

    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(s) => match toml::from_str(&s) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("invalid config {}: {e}", path.display());
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }
}
