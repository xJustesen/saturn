//! Color themes: a UI palette plus syntax highlighting, described in TOML.
//!
//! A theme is looked up by name in `~/.config/saturn/themes/<name>.toml`, then
//! among the built-in themes (`assets/themes/`), then among syntect's bundled
//! themes (syntax only). A theme can `extends` another one and override keys.
//! `saturn theme import-nvim` writes a theme from a Neovim colorscheme.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use edtui::syntect::highlighting::{
    Color as SynColor, FontStyle, ScopeSelectors, StyleModifier, Theme as SynTheme, ThemeItem, ThemeSettings,
};
use ratatui::style::{Color, Modifier, Style};
use serde::Deserialize;

/// Built-in themes, compiled in from `assets/themes/`.
pub const BUILTIN: &[(&str, &str)] = &[
    ("tokyo-night", include_str!("../assets/themes/tokyo-night.toml")),
    ("tokyo-night-storm", include_str!("../assets/themes/tokyo-night-storm.toml")),
    ("monokai", include_str!("../assets/themes/monokai.toml")),
    ("catppuccin-mocha", include_str!("../assets/themes/catppuccin-mocha.toml")),
    ("dracula", include_str!("../assets/themes/dracula.toml")),
    ("gruvbox-dark", include_str!("../assets/themes/gruvbox-dark.toml")),
    ("nord", include_str!("../assets/themes/nord.toml")),
    ("one-dark", include_str!("../assets/themes/one-dark.toml")),
    ("solarized-dark", include_str!("../assets/themes/solarized-dark.toml")),
    ("solarized-light", include_str!("../assets/themes/solarized-light.toml")),
    ("github-dark", include_str!("../assets/themes/github-dark.toml")),
    ("terminal", include_str!("../assets/themes/terminal.toml")),
];

/// Semantic syntax keys a theme may set, with the syntect scope selectors each
/// one colors. Ordered general → specific: for equal match power the later
/// item wins, so more specific keys override broader ones.
pub const SYNTAX_KEYS: &[(&str, &str)] = &[
    ("comment", "comment"),
    ("comment.doc", "comment.block.documentation, string.quoted.docstring"),
    ("keyword", "keyword, storage.type, storage.modifier"),
    ("keyword.control", "keyword.control"),
    ("keyword.import", "keyword.control.import"),
    ("keyword.return", "keyword.control.flow.return, keyword.control.return"),
    (
        "keyword.exception",
        "keyword.control.flow.raise, keyword.control.flow.try, keyword.control.flow.except, keyword.control.flow.finally, keyword.control.exception, keyword.control.trycatch",
    ),
    (
        "keyword.function",
        "storage.type.function, storage.type.class, storage.modifier.async, keyword.declaration.function, keyword.declaration.class",
    ),
    ("operator", "keyword.operator"),
    ("keyword.operator", "keyword.operator.logical, keyword.operator.word, keyword.operator.wordlike, keyword.other.await"),
    ("string", "string"),
    ("string.escape", "constant.character.escape"),
    ("string.regexp", "string.regexp"),
    ("number", "constant.numeric"),
    ("constant", "constant.other, variable.other.constant, entity.name.constant"),
    ("constant.builtin", "constant.language, support.constant"),
    ("boolean", "constant.language.boolean"),
    ("variable", "variable.other"),
    ("variable.builtin", "variable.language"),
    ("parameter", "variable.parameter"),
    ("property", "variable.other.member, variable.other.property, meta.property.object"),
    ("function", "entity.name.function"),
    ("function.call", "variable.function, meta.function-call.generic"),
    ("function.builtin", "support.function"),
    ("method", "entity.name.function.method, variable.function.method"),
    ("constructor", "entity.name.function.constructor, meta.function-call.constructor"),
    (
        "type",
        "entity.name.type, entity.name.class, entity.name.struct, entity.name.enum, entity.name.trait, entity.other.inherited-class, support.class, storage.type.primitive",
    ),
    ("type.builtin", "support.type, storage.type.primitive"),
    ("module", "entity.name.namespace, entity.name.module, meta.import-path, meta.import-name"),
    (
        "decorator",
        "meta.annotation, variable.annotation, punctuation.definition.annotation, entity.name.function.decorator, punctuation.definition.decorator",
    ),
    (
        "punctuation",
        "punctuation - punctuation.definition.string - punctuation.definition.comment - punctuation.definition.heading - punctuation.definition.annotation - punctuation.definition.decorator",
    ),
    (
        "punctuation.bracket",
        "punctuation.section.brackets, punctuation.section.parens, punctuation.section.braces, punctuation.section.arguments, punctuation.section.parameters, punctuation.section.list, punctuation.section.dictionary-or-set, punctuation.section.group, punctuation.section.sequence, punctuation.section.mapping, punctuation.section.set, punctuation.section.inheritance, punctuation.definition.parameters, punctuation.definition.arguments",
    ),
    ("punctuation.special", "punctuation.section.interpolation, punctuation.definition.interpolation, punctuation.definition.template-expression"),
    ("tag", "entity.name.tag, punctuation.definition.tag"),
    ("attribute", "entity.other.attribute-name"),
    ("markup.heading", "markup.heading, entity.name.section"),
    ("markup.strong", "markup.bold"),
    ("markup.emphasis", "markup.italic"),
    ("markup.raw", "markup.raw"),
    ("markup.link", "markup.underline.link, meta.link, string.other.link"),
    ("markup.list", "markup.list, punctuation.definition.list_item"),
    ("markup.quote", "markup.quote"),
    ("invalid", "invalid"),
];

/// UI keys a theme may set (all optional; see [`Ui::resolve`] for defaults).
pub const UI_KEYS: &[&str] = &[
    "background",
    "foreground",
    "muted",
    "line_numbers",
    "selection",
    "cursor_line",
    "input_bg",
    "focus",
    "edit",
    "error",
    "warning",
    "info",
    "success",
    "status_bg",
    "status_fg",
    "border",
    "float_bg",
    "match_bg",
    "match_fg",
    "search_bg",
    "search_fg",
    "search_current_bg",
    "mode_fg",
    "mode_command",
    "mode_normal",
    "mode_insert",
    "mode_visual",
    "mode_search",
];

// ----- theme files ----------------------------------------------------------

/// A style in a theme file: either a foreground color or a table.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum StyleSpec {
    Color(String),
    Table(StyleTable),
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct StyleTable {
    pub fg: Option<String>,
    pub bg: Option<String>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

impl StyleSpec {
    fn table(&self) -> StyleTable {
        match self {
            StyleSpec::Color(c) => StyleTable { fg: Some(c.clone()), ..Default::default() },
            StyleSpec::Table(t) => t.clone(),
        }
    }
}

/// The on-disk shape of a theme.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ThemeSpec {
    pub name: Option<String>,
    pub author: Option<String>,
    /// Name of a theme whose keys this one inherits.
    pub extends: Option<String>,
    pub ui: BTreeMap<String, String>,
    pub syntax: BTreeMap<String, StyleSpec>,
    /// Raw syntect scope selectors, for anything the semantic keys don't cover.
    pub scopes: BTreeMap<String, StyleSpec>,
}

/// Parse a color: `#rrggbb`, `#rgb`, an ANSI name (`red`, `dark gray`,
/// `light_blue`), a 0–255 index, or `none` for the terminal default.
pub fn parse_color(s: &str) -> Result<Color> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("none") || s.eq_ignore_ascii_case("default") {
        return Ok(Color::Reset);
    }
    if let Some(hex) = s.strip_prefix('#') {
        let v = u32::from_str_radix(hex, 16).with_context(|| format!("bad color {s:?}"))?;
        return match hex.len() {
            6 => Ok(Color::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)),
            3 => {
                let (r, g, b) = ((v >> 8) as u8 & 0xf, (v >> 4) as u8 & 0xf, v as u8 & 0xf);
                Ok(Color::Rgb(r * 17, g * 17, b * 17))
            }
            _ => bail!("bad color {s:?}"),
        };
    }
    Color::from_str(s).map_err(|_| anyhow::anyhow!("unknown color {s:?}"))
}

pub fn user_dir() -> Option<PathBuf> {
    crate::config::config_root().map(|d| d.join("themes"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    BuiltIn,
    User(PathBuf),
    /// One of syntect's bundled themes (syntax colors only).
    Syntect,
}

impl Source {
    pub fn label(&self) -> String {
        match self {
            Source::BuiltIn => "built-in".into(),
            Source::User(p) => p.display().to_string(),
            Source::Syntect => "syntect".into(),
        }
    }
}

/// Every theme that can be named in the config, user themes first.
pub fn available() -> Vec<(String, Source)> {
    let mut out: Vec<(String, Source)> = Vec::new();
    if let Some(dir) = user_dir()
        && let Ok(rd) = std::fs::read_dir(&dir)
    {
        let mut users: Vec<PathBuf> =
            rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "toml")).collect();
        users.sort();
        for p in users {
            if let Some(stem) = p.file_stem() {
                out.push((stem.to_string_lossy().into_owned(), Source::User(p.clone())));
            }
        }
    }
    for (name, _) in BUILTIN {
        if !out.iter().any(|(n, _)| n == name) {
            out.push((name.to_string(), Source::BuiltIn));
        }
    }
    let mut syn: Vec<&String> = edtui::THEME_SET.themes.keys().collect();
    syn.sort();
    for name in syn {
        if !out.iter().any(|(n, _)| n == name) {
            out.push((name.clone(), Source::Syntect));
        }
    }
    out
}

fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(p)
}

/// Locate a theme file by name (or path) and return its text.
fn spec_text(name: &str) -> Result<String> {
    if name.ends_with(".toml") || name.contains('/') {
        let p = expand_tilde(name);
        return std::fs::read_to_string(&p).with_context(|| format!("cannot read theme {}", p.display()));
    }
    if let Some(dir) = user_dir() {
        let p = dir.join(format!("{name}.toml"));
        if p.exists() {
            return std::fs::read_to_string(&p).with_context(|| format!("cannot read theme {}", p.display()));
        }
    }
    if let Some((_, text)) = BUILTIN.iter().find(|(n, _)| *n == name) {
        return Ok((*text).to_string());
    }
    bail!("theme {name:?} not found")
}

/// Load a theme spec, following `extends`.
pub fn load_spec(name: &str, depth: usize) -> Result<ThemeSpec> {
    if depth > 8 {
        bail!("theme {name:?}: `extends` chain too deep (cycle?)");
    }
    let text = spec_text(name)?;
    let mut spec: ThemeSpec = toml::from_str(&text).with_context(|| format!("theme {name:?}"))?;
    if let Some(parent) = spec.extends.take() {
        let mut base = load_spec(&parent, depth + 1).with_context(|| format!("theme {name:?} extends {parent:?}"))?;
        base.ui.extend(spec.ui);
        base.syntax.extend(spec.syntax);
        base.scopes.extend(spec.scopes);
        base.name = spec.name.or(base.name);
        base.author = spec.author.or(base.author);
        spec = base;
    }
    Ok(spec)
}

// ----- resolved theme -------------------------------------------------------

/// Colors for everything that isn't source code.
#[derive(Debug, Clone)]
pub struct Ui {
    /// Painted under the whole frame when set; `None` keeps the terminal's.
    pub background: Option<Color>,
    pub foreground: Color,
    /// Hints, placeholders, raw cells, idle gutter labels.
    pub muted: Color,
    pub line_numbers: Color,
    /// Background of a visual selection in the editor.
    pub selection: Color,
    /// Background of the cursor line while editing.
    pub cursor_line: Option<Color>,
    /// Full-row tint behind code and raw cell sources, so inputs stand out
    /// from their outputs. Derived from `background` when unset; `None`
    /// (no background, or `none`) falls back to a dim bar on every cell.
    pub input_bg: Option<Color>,
    /// Focused-cell bar, borders, the command-mode label.
    pub focus: Color,
    /// The cell bar while editing, the insert-mode label.
    pub edit: Color,
    pub error: Color,
    pub warning: Color,
    pub info: Color,
    pub success: Color,
    pub status_bg: Color,
    pub status_fg: Color,
    pub border: Color,
    pub float_bg: Option<Color>,
    /// The selected row in a picker.
    pub match_bg: Color,
    pub match_fg: Option<Color>,
    /// Notebook search (`/`): every match, and the one `n`/`N` landed on.
    pub search_bg: Color,
    pub search_fg: Option<Color>,
    pub search_current_bg: Color,
    /// Text color of the mode label in the status bar.
    pub mode_fg: Color,
    pub mode_command: Color,
    pub mode_normal: Color,
    pub mode_insert: Color,
    pub mode_visual: Color,
    pub mode_search: Color,
}

impl Ui {
    pub fn resolve(map: &BTreeMap<String, String>) -> Ui {
        let get = |k: &str| -> Option<Color> {
            let v = map.get(k)?;
            match parse_color(v) {
                Ok(c) => Some(c),
                Err(e) => {
                    tracing::warn!("theme ui.{k}: {e}");
                    None
                }
            }
        };
        for k in map.keys() {
            if !UI_KEYS.contains(&k.as_str()) {
                tracing::warn!("theme: unknown ui key {k:?}");
            }
        }
        let foreground = get("foreground").unwrap_or(Color::Reset);
        let background = get("background").filter(|c| *c != Color::Reset);
        let muted = get("muted").unwrap_or(Color::DarkGray);
        let focus = get("focus").unwrap_or(Color::Blue);
        let edit = get("edit").unwrap_or(Color::Green);
        let success = get("success").unwrap_or(Color::Green);
        let info = get("info").unwrap_or(Color::Cyan);
        let warning = get("warning").unwrap_or(Color::Yellow);
        let mode_fg = get("mode_fg").unwrap_or(background.unwrap_or(Color::Black));
        let mode_search = get("mode_search").unwrap_or(info);
        Ui {
            background,
            foreground,
            muted,
            line_numbers: get("line_numbers").unwrap_or(muted),
            selection: get("selection").unwrap_or(Color::Rgb(80, 80, 30)),
            cursor_line: get("cursor_line").filter(|c| *c != Color::Reset),
            input_bg: match map.get("input_bg") {
                Some(_) => get("input_bg").filter(|c| *c != Color::Reset),
                None => background.and_then(|b| shade_toward(b, foreground)),
            },
            focus,
            edit,
            error: get("error").unwrap_or(Color::Red),
            warning,
            info,
            success,
            status_bg: get("status_bg").unwrap_or(Color::Rgb(30, 30, 40)),
            status_fg: get("status_fg").unwrap_or(foreground),
            border: get("border").unwrap_or(focus),
            float_bg: get("float_bg").filter(|c| *c != Color::Reset).or(background),
            match_bg: get("match_bg").unwrap_or(Color::Rgb(50, 60, 90)),
            match_fg: get("match_fg").filter(|c| *c != Color::Reset),
            search_bg: get("search_bg").unwrap_or(mode_search),
            search_fg: match map.get("search_fg") {
                Some(_) => get("search_fg").filter(|c| *c != Color::Reset),
                None => Some(mode_fg),
            },
            search_current_bg: get("search_current_bg").unwrap_or(warning),
            mode_fg,
            mode_command: get("mode_command").unwrap_or(focus),
            mode_normal: get("mode_normal").unwrap_or(success),
            mode_insert: get("mode_insert").unwrap_or(edit),
            mode_visual: get("mode_visual").unwrap_or(Color::Magenta),
            mode_search,
        }
    }
}

/// A subtle shade of `bg` nudged toward `fg` (or, when `fg` is not RGB, away
/// from black/white by luminance), for surfaces that must read as "on the
/// background but not the background". `None` when `bg` is not RGB.
fn shade_toward(bg: Color, fg: Color) -> Option<Color> {
    let Color::Rgb(r, g, b) = bg else { return None };
    let toward = match fg {
        Color::Rgb(..) => fg,
        _ => {
            let lum = (r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000;
            let t = if lum < 128 { 255 } else { 0 };
            Color::Rgb(t, t, t)
        }
    };
    mix(bg, toward, 8)
}

/// `a` moved `pct` percent of the way toward `b`. `None` unless both are RGB.
pub fn mix(a: Color, b: Color, pct: u8) -> Option<Color> {
    let (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) = (a, b) else {
        return None;
    };
    let p = pct.min(100) as i32;
    let m = |x: u8, y: u8| -> u8 { ((x as i32 * (100 - p) + y as i32 * p) / 100) as u8 };
    Some(Color::Rgb(m(ar, br), m(ag, bg), m(ab, bb)))
}

/// A loaded theme: UI colors plus a syntect theme for code.
#[derive(Debug, Clone)]
pub struct Theme {
    pub name: String,
    pub ui: Ui,
    pub syntax: SynTheme,
}

impl Default for Theme {
    fn default() -> Self {
        Theme::load("terminal").expect("built-in terminal theme is valid")
    }
}

impl Theme {
    /// Load by name, path, or (syntax only) syntect theme name.
    pub fn load(name: &str) -> Result<Theme> {
        match load_spec(name, 0) {
            Ok(spec) => Ok(Theme::from_spec(name, spec)),
            Err(e) => match edtui::THEME_SET.themes.get(name).or_else(|| edtui::THEME_SET.themes.get(&name.replace('.', "-"))) {
                Some(t) => {
                    let mut syntax = t.clone();
                    // Syntax colors only: keep the terminal's background.
                    syntax.settings.background = Some(to_syn(Color::Reset));
                    Ok(Theme { name: name.to_string(), ui: Ui::resolve(&BTreeMap::new()), syntax })
                }
                None => Err(e),
            },
        }
    }

    pub fn from_spec(name: &str, spec: ThemeSpec) -> Theme {
        let ui = Ui::resolve(&spec.ui);
        let display = spec.name.clone().unwrap_or_else(|| name.to_string());
        let mut syntax = SynTheme {
            name: Some(display.clone()),
            author: spec.author.clone(),
            settings: ThemeSettings {
                foreground: Some(to_syn(ui.foreground)),
                // No background → the terminal-default marker, which the
                // span conversion leaves unset.
                background: Some(to_syn(ui.background.unwrap_or(Color::Reset))),
                selection: Some(to_syn(ui.selection)),
                line_highlight: ui.cursor_line.map(to_syn),
                ..Default::default()
            },
            scopes: Vec::new(),
        };
        for (key, selectors) in SYNTAX_KEYS {
            if let Some(spec) = spec.syntax.get(*key) {
                push_item(&mut syntax, selectors, &spec.table(), key);
            }
        }
        for key in spec.syntax.keys() {
            if !SYNTAX_KEYS.iter().any(|(k, _)| k == key) {
                tracing::warn!("theme {name:?}: unknown syntax key {key:?} (use [scopes] for raw selectors)");
            }
        }
        for (selectors, spec) in &spec.scopes {
            push_item(&mut syntax, selectors, &spec.table(), selectors);
        }
        Theme { name: name.to_string(), ui, syntax }
    }

    /// Foreground + background of the frame.
    pub fn base(&self) -> Style {
        let mut s = Style::default().fg(self.ui.foreground);
        if let Some(bg) = self.ui.background {
            s = s.bg(bg);
        }
        s
    }

    pub fn muted(&self) -> Style {
        Style::default().fg(self.ui.muted)
    }

    /// Hints and placeholders.
    pub fn placeholder(&self) -> Style {
        Style::default().fg(self.ui.muted).add_modifier(Modifier::ITALIC)
    }

    pub fn error(&self) -> Style {
        Style::default().fg(self.ui.error)
    }

    pub fn border(&self) -> Style {
        Style::default().fg(self.ui.border)
    }

    /// Background of floating windows (pickers, prompts, the popout).
    pub fn float(&self) -> Style {
        let mut s = Style::default().fg(self.ui.foreground);
        if let Some(bg) = self.ui.float_bg {
            s = s.bg(bg);
        }
        s
    }

    /// A search match in the notebook.
    pub fn search(&self) -> Style {
        let mut s = Style::default().bg(self.ui.search_bg);
        if let Some(fg) = self.ui.search_fg {
            s = s.fg(fg);
        }
        s
    }

    /// The search match the cursor is on.
    pub fn search_current(&self) -> Style {
        let mut s = Style::default().bg(self.ui.search_current_bg).add_modifier(Modifier::BOLD);
        if let Some(fg) = self.ui.search_fg {
            s = s.fg(fg);
        }
        s
    }

    pub fn selected_row(&self) -> Style {
        let mut s = Style::default().bg(self.ui.match_bg).add_modifier(Modifier::BOLD);
        if let Some(fg) = self.ui.match_fg {
            s = s.fg(fg);
        }
        s
    }
}

fn push_item(theme: &mut SynTheme, selectors: &str, st: &StyleTable, key: &str) {
    let scope = match ScopeSelectors::from_str(selectors) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("theme: bad scope selector for {key:?}: {e:?}");
            return;
        }
    };
    let color = |v: &Option<String>| -> Option<SynColor> {
        let v = v.as_deref()?;
        match parse_color(v) {
            Ok(c) => Some(to_syn(c)),
            Err(e) => {
                tracing::warn!("theme syntax.{key}: {e}");
                None
            }
        }
    };
    let mut fs = FontStyle::empty();
    if st.bold {
        fs |= FontStyle::BOLD;
    }
    if st.italic {
        fs |= FontStyle::ITALIC;
    }
    if st.underline {
        fs |= FontStyle::UNDERLINE;
    }
    let style = StyleModifier {
        foreground: color(&st.fg),
        background: color(&st.bg),
        font_style: if st.bold || st.italic || st.underline { Some(fs) } else { None },
    };
    theme.scopes.push(ThemeItem { scope, style });
}

// ----- terminal colors through syntect --------------------------------------
//
// syntect colors are RGBA. Terminal colors (ANSI names, 0–255 indices, the
// default) are smuggled through as `a == 0` with a marker in g/b; edtui's
// highlighter drops the alpha and emits `Color::Rgb(r, g, b)`, which
// [`decode_rgb`] recognises by the marker.

const MARK_G: u8 = 0xA5;
const MARK_INDEXED: u8 = 0x51;
const MARK_RESET: u8 = 0x52;

fn ansi_index(c: Color) -> Option<u8> {
    Some(match c {
        Color::Black => 0,
        Color::Red => 1,
        Color::Green => 2,
        Color::Yellow => 3,
        Color::Blue => 4,
        Color::Magenta => 5,
        Color::Cyan => 6,
        Color::Gray => 7,
        Color::DarkGray => 8,
        Color::LightRed => 9,
        Color::LightGreen => 10,
        Color::LightYellow => 11,
        Color::LightBlue => 12,
        Color::LightMagenta => 13,
        Color::LightCyan => 14,
        Color::White => 15,
        Color::Indexed(i) => i,
        _ => return None,
    })
}

pub fn to_syn(c: Color) -> SynColor {
    match c {
        Color::Rgb(r, g, b) => SynColor { r, g, b, a: 255 },
        Color::Reset => SynColor { r: 0, g: MARK_G, b: MARK_RESET, a: 0 },
        other => match ansi_index(other) {
            Some(i) => SynColor { r: i, g: MARK_G, b: MARK_INDEXED, a: 0 },
            None => SynColor { r: 0, g: MARK_G, b: MARK_RESET, a: 0 },
        },
    }
}

pub fn from_syn(c: SynColor) -> Color {
    if c.a == 0 { decode_rgb(c.r, c.g, c.b) } else { Color::Rgb(c.r, c.g, c.b) }
}

/// Undo the marker encoding on an RGB triple (identity for real colors).
pub fn decode_rgb(r: u8, g: u8, b: u8) -> Color {
    match (g, b) {
        (MARK_G, MARK_INDEXED) => Color::Indexed(r),
        (MARK_G, MARK_RESET) => Color::Reset,
        _ => Color::Rgb(r, g, b),
    }
}

pub fn decode_color(c: Color) -> Color {
    match c {
        Color::Rgb(r, g, b) => decode_rgb(r, g, b),
        other => other,
    }
}

// ----- neovim import --------------------------------------------------------

/// UI keys and the nvim highlight groups they are taken from (first hit wins);
/// `bg:` reads the group's background instead of its foreground.
const NVIM_UI: &[(&str, &[&str])] = &[
    ("background", &["bg:Normal"]),
    ("foreground", &["Normal"]),
    ("muted", &["Comment", "NonText"]),
    ("line_numbers", &["LineNr"]),
    ("selection", &["bg:Visual"]),
    ("cursor_line", &["bg:CursorLine"]),
    ("focus", &["Function", "Directory", "Title"]),
    ("edit", &["String", "DiagnosticOk"]),
    ("error", &["DiagnosticError", "ErrorMsg", "Error"]),
    ("warning", &["DiagnosticWarn", "WarningMsg"]),
    ("info", &["DiagnosticInfo", "Directory"]),
    ("success", &["DiagnosticOk", "Added", "String"]),
    ("status_bg", &["bg:StatusLine"]),
    ("status_fg", &["StatusLine"]),
    ("border", &["FloatBorder", "WinSeparator"]),
    ("float_bg", &["bg:NormalFloat", "bg:Pmenu"]),
    ("match_bg", &["bg:PmenuSel"]),
    ("match_fg", &["PmenuSel"]),
    ("search_bg", &["bg:Search"]),
    ("search_fg", &["Search"]),
    ("search_current_bg", &["bg:CurSearch", "bg:IncSearch"]),
    ("mode_visual", &["@keyword", "Keyword", "Statement"]),
    ("mode_search", &["bg:Search", "WarningMsg"]),
];

/// Syntax keys and the nvim (treesitter first) groups they are taken from.
const NVIM_SYNTAX: &[(&str, &[&str])] = &[
    ("comment", &["@comment", "Comment"]),
    ("comment.doc", &["@string.documentation", "@comment.documentation", "Comment"]),
    ("keyword", &["@keyword", "Keyword", "Statement"]),
    ("keyword.control", &["@keyword.conditional", "@conditional", "Conditional", "Statement"]),
    ("keyword.import", &["@keyword.import", "Include", "PreProc"]),
    ("keyword.return", &["@keyword.return", "@keyword", "Keyword"]),
    ("keyword.exception", &["@keyword.exception", "@exception", "Exception", "Statement"]),
    ("keyword.function", &["@keyword.function", "@keyword", "Keyword"]),
    ("operator", &["@operator", "Operator"]),
    ("keyword.operator", &["@keyword.operator", "@operator", "Operator"]),
    ("string", &["@string", "String"]),
    ("string.escape", &["@string.escape", "SpecialChar", "Special"]),
    ("string.regexp", &["@string.regexp", "@string.regex", "@string.special", "SpecialChar"]),
    ("number", &["@number", "Number", "Constant"]),
    ("constant", &["@constant", "Constant"]),
    ("constant.builtin", &["@constant.builtin", "@boolean", "Boolean", "Constant"]),
    ("boolean", &["@boolean", "Boolean", "Constant"]),
    ("variable", &["@variable", "Identifier"]),
    ("variable.builtin", &["@variable.builtin", "Special"]),
    ("parameter", &["@variable.parameter", "@parameter", "Identifier"]),
    ("property", &["@variable.member", "@property", "@field", "Identifier"]),
    ("function", &["@function", "Function"]),
    ("function.call", &["@function.call", "@function", "Function"]),
    ("function.builtin", &["@function.builtin", "@function", "Function"]),
    ("method", &["@function.method", "@method", "@function", "Function"]),
    ("constructor", &["@constructor", "@type", "Type"]),
    ("type", &["@type", "Type"]),
    ("type.builtin", &["@type.builtin", "@type", "Type"]),
    ("module", &["@module", "@namespace", "Structure", "Type"]),
    ("decorator", &["@attribute", "@function.macro", "PreProc"]),
    ("punctuation", &["@punctuation.delimiter", "@punctuation", "Delimiter"]),
    ("punctuation.bracket", &["@punctuation.bracket", "@punctuation", "Delimiter"]),
    ("punctuation.special", &["@punctuation.special", "Special"]),
    ("tag", &["@tag", "Tag"]),
    ("attribute", &["@tag.attribute", "@attribute"]),
    ("markup.heading", &["@markup.heading", "Title"]),
    ("markup.strong", &["@markup.strong"]),
    ("markup.emphasis", &["@markup.italic", "@markup.emphasis"]),
    ("markup.raw", &["@markup.raw", "@markup.raw.block"]),
    ("markup.link", &["@markup.link.url", "@markup.link", "Underlined"]),
    ("markup.list", &["@markup.list", "Special"]),
    ("markup.quote", &["@markup.quote", "Comment"]),
    ("invalid", &["@error", "Error"]),
];

const NVIM_LUA: &str = r#"
local groups = GROUPS
local out = {}
for _, g in ipairs(groups) do
  local ok, v = pcall(vim.api.nvim_get_hl, 0, { name = g, link = false })
  if ok and type(v) == "table" and next(v) ~= nil then
    out[g] = v
  end
end
io.stdout:write("SATURN_THEME_BEGIN" .. vim.json.encode({ colors_name = vim.g.colors_name, background = vim.o.background, groups = out }) .. "SATURN_THEME_END\n")
"#;

/// Ask a headless Neovim (with the user's config) for its highlight groups.
/// Returns the colorscheme name and the groups as JSON.
pub fn nvim_highlights(colorscheme: Option<&str>) -> Result<(String, serde_json::Value)> {
    let mut groups: Vec<&str> = Vec::new();
    for (_, gs) in NVIM_UI.iter().chain(NVIM_SYNTAX) {
        for g in *gs {
            let g = g.strip_prefix("bg:").unwrap_or(g);
            if !groups.contains(&g) {
                groups.push(g);
            }
        }
    }
    let list = groups.iter().map(|g| format!("{g:?}")).collect::<Vec<_>>().join(", ");
    let lua = NVIM_LUA.replace("GROUPS", &format!("{{ {list} }}"));
    let dir = std::env::temp_dir();
    let script = dir.join(format!("saturn-nvim-theme-{}.lua", std::process::id()));
    std::fs::write(&script, lua).with_context(|| format!("cannot write {}", script.display()))?;
    let mut cmd = std::process::Command::new("nvim");
    cmd.arg("--headless").arg("-i").arg("NONE");
    if let Some(cs) = colorscheme {
        cmd.arg("-c").arg(format!("colorscheme {cs}"));
    }
    cmd.arg("-c").arg(format!("luafile {}", script.display())).arg("-c").arg("qa!");
    cmd.stdin(std::process::Stdio::null());
    let out = cmd.output().context("cannot run nvim (is it on PATH?)");
    let _ = std::fs::remove_file(&script);
    let out = out?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let Some(start) = stdout.find("SATURN_THEME_BEGIN") else {
        bail!("nvim did not report its highlights{}{}", if stderr.trim().is_empty() { "" } else { ":\n" }, stderr.trim());
    };
    let rest = &stdout[start + "SATURN_THEME_BEGIN".len()..];
    let end = rest.find("SATURN_THEME_END").unwrap_or(rest.len());
    let v: serde_json::Value = serde_json::from_str(&rest[..end]).context("bad JSON from nvim")?;
    let name = v
        .get("colors_name")
        .and_then(|s| s.as_str())
        .map(str::to_string)
        .or_else(|| colorscheme.map(str::to_string))
        .unwrap_or_else(|| "nvim".to_string());
    Ok((name, v))
}

fn hex(v: u64) -> String {
    format!("#{:06x}", v & 0xff_ffff)
}

fn nvim_color(groups: &serde_json::Value, group: &str, bg: bool) -> Option<String> {
    let g = groups.get(group)?;
    let (rgb, cterm) = if bg { ("bg", "ctermbg") } else { ("fg", "ctermfg") };
    if let Some(v) = g.get(rgb).and_then(|v| v.as_u64()) {
        return Some(hex(v));
    }
    g.get(cterm).and_then(|v| v.as_u64()).map(|i| i.to_string())
}

fn first_color(groups: &serde_json::Value, candidates: &[&str]) -> Option<String> {
    candidates.iter().find_map(|c| match c.strip_prefix("bg:") {
        Some(g) => nvim_color(groups, g, true),
        None => nvim_color(groups, c, false),
    })
}

/// Turn nvim's highlight groups into a saturn theme file.
pub fn theme_toml_from_nvim(colors_name: &str, dump: &serde_json::Value) -> String {
    let groups = dump.get("groups").cloned().unwrap_or(serde_json::Value::Null);
    let mut out = String::new();
    out.push_str(&format!(
        "# saturn theme imported from the Neovim colorscheme {colors_name:?}\n# (saturn theme import-nvim). Edit freely; keys are documented in the README.\n\nname = {:?}\n\n[ui]\n",
        format!("{colors_name} (nvim)")
    ));
    for (key, candidates) in NVIM_UI {
        if let Some(c) = first_color(&groups, candidates) {
            out.push_str(&format!("{key} = {c:?}\n"));
        }
    }
    out.push_str("\n[syntax]\n");
    for (key, candidates) in NVIM_SYNTAX {
        let Some(group) = candidates.iter().find(|g| groups.get(**g).is_some()) else { continue };
        let g = &groups[*group];
        let fg = nvim_color(&groups, group, false);
        let flag = |k: &str| g.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
        let (bold, italic, underline) = (flag("bold"), flag("italic"), flag("underline"));
        if fg.is_none() && !bold && !italic && !underline {
            continue;
        }
        if !bold && !italic && !underline {
            out.push_str(&format!("{key:?} = {:?}\n", fg.unwrap()));
            continue;
        }
        let mut parts = Vec::new();
        if let Some(fg) = fg {
            parts.push(format!("fg = {fg:?}"));
        }
        for (name, on) in [("bold", bold), ("italic", italic), ("underline", underline)] {
            if on {
                parts.push(format!("{name} = true"));
            }
        }
        out.push_str(&format!("{key:?} = {{ {} }}\n", parts.join(", ")));
    }
    out
}

/// `saturn theme import-nvim`: write `~/.config/saturn/themes/<name>.toml`.
pub fn import_nvim(colorscheme: Option<&str>, name: Option<&str>, force: bool) -> Result<PathBuf> {
    let (colors_name, dump) = nvim_highlights(colorscheme)?;
    let text = theme_toml_from_nvim(&colors_name, &dump);
    // Validate before writing.
    let spec: ThemeSpec = toml::from_str(&text).context("generated theme does not parse")?;
    let _ = Theme::from_spec(&colors_name, spec);
    let name = name.map(str::to_string).unwrap_or_else(|| colors_name.replace(['/', ' '], "-"));
    let dir = user_dir().context("no config directory")?;
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let path = dir.join(format!("{name}.toml"));
    if path.exists() && !force {
        bail!("{} exists (use --force to overwrite)", path.display());
    }
    std::fs::write(&path, text).with_context(|| format!("cannot write {}", path.display()))?;
    Ok(path)
}

/// Write a theme's fully resolved spec, for `saturn theme show`.
pub fn describe(name: &str) -> Result<String> {
    let t = Theme::load(name)?;
    let mut out = format!("theme: {}\n\n[ui]\n", t.name);
    let u = &t.ui;
    let show = |c: Color| format!("{c:?}");
    let opt = |c: Option<Color>| c.map(show).unwrap_or_else(|| "(terminal)".into());
    for (k, v) in [
        ("background", opt(u.background)),
        ("foreground", show(u.foreground)),
        ("muted", show(u.muted)),
        ("line_numbers", show(u.line_numbers)),
        ("selection", show(u.selection)),
        ("cursor_line", opt(u.cursor_line)),
        ("input_bg", opt(u.input_bg)),
        ("focus", show(u.focus)),
        ("edit", show(u.edit)),
        ("error", show(u.error)),
        ("warning", show(u.warning)),
        ("info", show(u.info)),
        ("success", show(u.success)),
        ("status_bg", show(u.status_bg)),
        ("status_fg", show(u.status_fg)),
        ("border", show(u.border)),
        ("float_bg", opt(u.float_bg)),
        ("match_bg", show(u.match_bg)),
        ("match_fg", opt(u.match_fg)),
        ("search_bg", show(u.search_bg)),
        ("search_fg", opt(u.search_fg)),
        ("search_current_bg", show(u.search_current_bg)),
        ("mode_fg", show(u.mode_fg)),
        ("mode_command", show(u.mode_command)),
        ("mode_normal", show(u.mode_normal)),
        ("mode_insert", show(u.mode_insert)),
        ("mode_visual", show(u.mode_visual)),
        ("mode_search", show(u.mode_search)),
    ] {
        out.push_str(&format!("{k} = {v}\n"));
    }
    out.push_str(&format!("\n[syntax]  ({} scope rules)\n", t.syntax.scopes.len()));
    for item in &t.syntax.scopes {
        let sel = item
            .scope
            .selectors
            .iter()
            .map(|s| {
                let mut p = s.path.scopes.iter().map(|x| x.build_string()).collect::<Vec<_>>().join(" ");
                for ex in &s.excludes {
                    p.push_str(" - ");
                    p.push_str(&ex.scopes.iter().map(|x| x.build_string()).collect::<Vec<_>>().join(" "));
                }
                p
            })
            .collect::<Vec<_>>()
            .join(", ");
        let fg = item.style.foreground.map(|c| format!("{:?}", from_syn(c))).unwrap_or_default();
        let fs = item.style.font_style.map(|f| format!(" {f:?}")).unwrap_or_default();
        out.push_str(&format!("{fg}{fs}  <- {sel}\n"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_parse() {
        assert_eq!(parse_color("#ff8000").unwrap(), Color::Rgb(255, 128, 0));
        assert_eq!(parse_color("#f80").unwrap(), Color::Rgb(255, 136, 0));
        assert_eq!(parse_color("none").unwrap(), Color::Reset);
        assert_eq!(parse_color("dark gray").unwrap(), Color::DarkGray);
        assert_eq!(parse_color("light_red").unwrap(), Color::LightRed);
        assert_eq!(parse_color("123").unwrap(), Color::Indexed(123));
        assert!(parse_color("#12345").is_err());
        assert!(parse_color("nope").is_err());
    }

    #[test]
    fn terminal_colors_survive_syntect() {
        for c in [Color::Reset, Color::Red, Color::DarkGray, Color::Indexed(200), Color::Rgb(1, 2, 3)] {
            let s = to_syn(c);
            let back = from_syn(s);
            let expected = match c {
                Color::Red => Color::Indexed(1),
                Color::DarkGray => Color::Indexed(8),
                other => other,
            };
            assert_eq!(back, expected);
            // What edtui emits after dropping alpha.
            assert_eq!(decode_rgb(s.r, s.g, s.b), expected);
        }
    }

    #[test]
    fn builtins_load_and_selectors_parse() {
        for (name, _) in BUILTIN {
            let spec = load_spec(name, 0).unwrap_or_else(|e| panic!("{name}: {e:#}"));
            for key in spec.syntax.keys() {
                assert!(SYNTAX_KEYS.iter().any(|(k, _)| k == key), "{name}: unknown syntax key {key}");
            }
            for key in spec.ui.keys() {
                assert!(UI_KEYS.contains(&key.as_str()), "{name}: unknown ui key {key}");
            }
            let t = Theme::load(name).unwrap_or_else(|e| panic!("{name}: {e:#}"));
            assert!(!t.syntax.scopes.is_empty(), "{name}: no syntax rules");
        }
        for (_, sel) in SYNTAX_KEYS {
            ScopeSelectors::from_str(sel).unwrap_or_else(|e| panic!("{sel}: {e:?}"));
        }
    }

    #[test]
    fn extends_overrides_parent() {
        let light = load_spec("solarized-light", 0).unwrap();
        let dark = load_spec("solarized-dark", 0).unwrap();
        assert_ne!(light.ui["background"], dark.ui["background"]);
        assert_eq!(light.syntax.len(), dark.syntax.len().max(light.syntax.len()));
    }

    #[test]
    fn syntect_fallback() {
        let t = Theme::load("base16-ocean-dark").unwrap();
        assert_eq!(t.ui.background, None);
        // The old default config value.
        assert!(Theme::load("base16-ocean.dark").is_ok());
        assert!(Theme::load("no-such-theme").is_err());
    }

    #[test]
    fn python_highlighting_uses_semantic_keys() {
        let t = Theme::load("monokai").unwrap();
        let text = crate::ui::text::highlight("def foo(a):\n    return a  # hi\n", "py", &t.syntax, 4);
        let styles: Vec<(String, Color)> = text
            .lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| (s.content.to_string(), s.style.fg.unwrap_or(Color::Reset))))
            .collect();
        let of = |needle: &str| styles.iter().find(|(c, _)| c.trim() == needle).map(|(_, c)| *c).unwrap();
        assert_eq!(of("def"), Color::Rgb(0x66, 0xd9, 0xef));
        assert_eq!(of("foo"), Color::Rgb(0xa6, 0xe2, 0x2e));
        assert_eq!(of("a"), Color::Rgb(0xfd, 0x97, 0x1f));
        assert_eq!(of("return"), Color::Rgb(0xf9, 0x26, 0x72));
        assert_eq!(of("hi"), Color::Rgb(0x75, 0x71, 0x5e));
    }

    #[test]
    fn nvim_dump_to_toml() {
        let dump = serde_json::json!({
            "colors_name": "x",
            "groups": {
                "Normal": {"fg": 0xc0caf5, "bg": 0x1a1b26},
                "@keyword": {"fg": 0xbb9af7, "bold": true},
                "Comment": {"fg": 0x565f89, "italic": true},
                "DiagnosticOk": {"ctermfg": 10}
            }
        });
        let toml = theme_toml_from_nvim("x", &dump);
        assert!(toml.contains("background = \"#1a1b26\""));
        assert!(toml.contains("\"keyword\" = { fg = \"#bb9af7\", bold = true }"));
        assert!(toml.contains("\"comment\" = { fg = \"#565f89\", italic = true }"));
        assert!(toml.contains("edit = \"10\""));
        let spec: ThemeSpec = toml::from_str(&toml).unwrap();
        let t = Theme::from_spec("x", spec);
        assert_eq!(t.ui.background, Some(Color::Rgb(0x1a, 0x1b, 0x26)));
        assert_eq!(t.ui.edit, Color::Indexed(10));
    }
}

