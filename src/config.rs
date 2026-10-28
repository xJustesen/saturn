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
    /// Theme name: a built-in (`tokyo-night`, `monokai`, …), a file in
    /// `~/.config/saturn/themes/`, a path, or a syntect theme name.
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
            theme: "tokyo-night".to_string(),
            line_numbers: false,
            tab_width: 4,
            wrap: true,
            show_hidden: false,
        }
    }
}

/// `$XDG_CONFIG_HOME/saturn`, else `~/.config/saturn` (on macOS too), else the
/// platform config directory.
pub fn config_root() -> Option<std::path::PathBuf> {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Some(std::path::PathBuf::from(x).join("saturn"));
    }
    if let Some(home) = dirs::home_dir() {
        return Some(home.join(".config").join("saturn"));
    }
    dirs::config_dir().map(|d| d.join("saturn"))
}

impl Config {
    pub fn path() -> Option<std::path::PathBuf> {
        let p = config_root()?.join("config.toml");
        if p.exists() {
            return Some(p);
        }
        // Earlier versions read the platform directory (~/Library/Application Support on macOS).
        match dirs::config_dir().map(|d| d.join("saturn").join("config.toml")) {
            Some(legacy) if legacy.exists() => Some(legacy),
            _ => Some(p),
        }
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
