//! User configuration (`$XDG_CONFIG_HOME/saturn/config.toml`).
//!
//! The file is read once at startup. Settings changed from inside saturn
//! (currently the theme) are written back with [`write_key`], which edits the
//! one key in place so hand-written comments and other keys survive.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
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
    /// With `line_numbers`: number lines relative to the cursor, as vim's
    /// `relativenumber` does.
    pub relative_numbers: bool,
    /// Insert the closing bracket or quote while typing in a cell.
    pub auto_pairs: bool,
    /// Wrap long lines in cells.
    pub wrap: bool,
    /// Files picker: list hidden files and folders (dot names). Toggle with Ctrl+H.
    pub show_hidden: bool,
    /// Files picker: list files matched by .gitignore. Notebooks are always
    /// listed. Toggle with Ctrl+I.
    pub show_ignored: bool,
    /// Files picker: show Nerd Font file-type icons. Needs a Nerd Font, which
    /// Ghostty includes.
    pub icons: bool,
    /// Dashboard "Projects": folders searched (a few levels deep) for git
    /// repositories. `~` is expanded; folders that don't exist are skipped.
    pub project_roots: Vec<String>,
    /// The built-in assistant (`[assistant]`).
    pub assistant: Assistant,
}

/// The built-in assistant, which runs on the user's own `claude` CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Assistant {
    /// Off: the panel and the inline prompt do not open.
    pub enabled: bool,
    /// The Claude Code program to run.
    pub command: String,
    /// Model of the panel's conversation; empty is the account's default.
    pub model: String,
    /// Model behind the inline prompt, where speed matters most.
    pub inline_model: String,
    /// Reasoning effort of the panel's conversation (`low` … `max`); empty
    /// is the CLI's default.
    pub effort: String,
    /// Also load the MCP servers configured for Claude Code. Slower to start.
    pub user_mcp: bool,
    /// Keep `ANTHROPIC_API_KEY` in the CLI's environment, which makes it
    /// bill the API instead of using the subscription login.
    pub use_api_key: bool,
}

impl Default for Assistant {
    fn default() -> Self {
        Self {
            enabled: true,
            command: "claude".to_string(),
            model: String::new(),
            inline_model: "haiku".to_string(),
            effort: String::new(),
            user_mcp: false,
            use_api_key: false,
        }
    }
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
            relative_numbers: true,
            auto_pairs: true,
            wrap: true,
            show_hidden: false,
            show_ignored: false,
            icons: true,
            project_roots: ["~/dev", "~/code", "~/projects", "~/src"]
                .map(String::from)
                .to_vec(),
            assistant: Assistant::default(),
        }
    }
}

/// `$XDG_CONFIG_HOME/saturn`, else `~/.config/saturn` (on macOS too), else the
/// platform config directory.
pub fn config_root() -> Option<PathBuf> {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(x).join("saturn"));
    }
    if let Some(home) = dirs::home_dir() {
        return Some(home.join(".config").join("saturn"));
    }
    dirs::config_dir().map(|d| d.join("saturn"))
}

/// `$XDG_STATE_HOME/saturn`, else `~/.local/state/saturn` (on macOS too, like
/// [`config_root`]). Holds the recent-notebooks list.
pub fn state_root() -> Option<PathBuf> {
    if let Some(x) = std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(x).join("saturn"));
    }
    dirs::home_dir().map(|h| h.join(".local").join("state").join("saturn"))
}

/// `~` or `~/rest` with the home folder filled in; anything else unchanged.
pub fn expand_tilde(s: &str) -> PathBuf {
    let home = dirs::home_dir();
    match (s, home) {
        ("~", Some(h)) => h,
        (s, Some(h)) if s.starts_with("~/") => h.join(&s[2..]),
        (s, _) => PathBuf::from(s),
    }
}

impl Config {
    /// `project_roots` with `~` expanded, in order, without duplicates.
    pub fn project_root_paths(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        for r in &self.project_roots {
            let p = expand_tilde(r);
            if !out.contains(&p) {
                out.push(p);
            }
        }
        out
    }

    pub fn path() -> Option<PathBuf> {
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

    /// Record the current theme in config.toml so the next launch starts with
    /// it. Returns the file written.
    pub fn save_theme(&self) -> Result<PathBuf> {
        let path = Self::path().context("no config directory")?;
        write_key(&path, "theme", self.theme.as_str())?;
        Ok(path)
    }
}

/// Set one top-level `key = value` in a TOML file, leaving every other line
/// (comments included) as it was. Creates the file and its directory if needed.
/// A file that does not parse is left untouched and reported as an error.
pub fn write_key(path: &Path, key: &str, value: impl Into<toml_edit::Value>) -> Result<()> {
    let mut doc = match std::fs::read_to_string(path) {
        Ok(s) => s
            .parse::<toml_edit::DocumentMut>()
            .with_context(|| format!("{} is not valid TOML", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml_edit::DocumentMut::new(),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    let mut value: toml_edit::Value = value.into();
    match doc.get_mut(key).and_then(toml_edit::Item::as_value_mut) {
        // Keep the key where it is, along with its spacing and trailing comment.
        Some(existing) => {
            *value.decor_mut() = existing.decor().clone();
            *existing = value;
        }
        None => {
            doc[key] = toml_edit::Item::Value(value);
        }
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, doc.to_string()).with_context(|| format!("cannot write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("cannot replace {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_assistant_table_is_optional_and_overridable() {
        let cfg: Config = toml::from_str("").unwrap();
        assert!(cfg.assistant.enabled);
        assert_eq!(cfg.assistant.inline_model, "haiku");
        let cfg: Config = toml::from_str("[assistant]\nenabled = false\nmodel = \"sonnet\"").unwrap();
        assert!(!cfg.assistant.enabled);
        assert_eq!((cfg.assistant.model.as_str(), cfg.assistant.command.as_str()), ("sonnet", "claude"));
    }

    fn temp_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("saturn-config-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("sub").join("config.toml")
    }

    #[test]
    fn picker_keys_default_and_parse() {
        let cfg: Config = toml::from_str("theme = \"nord\"\n").unwrap();
        assert!(!cfg.show_hidden && !cfg.show_ignored && cfg.icons);
        let cfg: Config = toml::from_str("icons = false\nshow_ignored = true\n").unwrap();
        assert!(!cfg.icons && cfg.show_ignored);
    }

    #[test]
    fn project_roots_default_and_parse() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.project_roots, ["~/dev", "~/code", "~/projects", "~/src"]);
        let cfg: Config = toml::from_str("project_roots = [\"~/work\", \"/opt/repos\", \"~/work\"]\n").unwrap();
        let paths = cfg.project_root_paths();
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[1], PathBuf::from("/opt/repos"));
    }

    #[test]
    fn expand_tilde_fills_in_home() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(expand_tilde("~"), home);
        assert_eq!(expand_tilde("~/dev"), home.join("dev"));
        assert_eq!(expand_tilde("/abs"), PathBuf::from("/abs"));
        assert_eq!(expand_tilde("~user/x"), PathBuf::from("~user/x"));
    }

    #[test]
    fn write_key_creates_file_and_directory() {
        let path = temp_file("create");
        write_key(&path, "theme", "nord").unwrap();
        let s = std::fs::read_to_string(&path).unwrap();
        assert_eq!(s.trim(), "theme = \"nord\"");
        let cfg: Config = toml::from_str(&s).unwrap();
        assert_eq!(cfg.theme, "nord");
    }

    #[test]
    fn write_key_keeps_comments_and_other_keys() {
        let path = temp_file("edit");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let before = "# my saturn config\nwrap = false  # keep long lines\ntheme = \"monokai\" # see Themes\n\n[unknown]\nx = 1\n";
        std::fs::write(&path, before).unwrap();
        write_key(&path, "theme", "dracula").unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            after,
            "# my saturn config\nwrap = false  # keep long lines\ntheme = \"dracula\" # see Themes\n\n[unknown]\nx = 1\n"
        );
        let cfg: Config = toml::from_str(&after).unwrap();
        assert_eq!(cfg.theme, "dracula");
        assert!(!cfg.wrap);
    }

    #[test]
    fn write_key_appends_when_missing() {
        let path = temp_file("append");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "wrap = false\n").unwrap();
        write_key(&path, "theme", "nord").unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, "wrap = false\ntheme = \"nord\"\n");
    }

    #[test]
    fn write_key_refuses_to_clobber_invalid_toml() {
        let path = temp_file("invalid");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "this is = = not toml\n").unwrap();
        assert!(write_key(&path, "theme", "nord").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "this is = = not toml\n");
    }
}

