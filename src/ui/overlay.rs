//! Modal overlays: fuzzy palette (commands / kernels / files), prompts,
//! confirmations and the help screen.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as MatcherConfig, Matcher, Utf32Str};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Widget, Wrap};

use crate::commands::Command;
use crate::theme::Theme;
use crate::ui::popout::PopoutState;

pub enum Overlay {
    Palette(PaletteState),
    Prompt(PromptState),
    Confirm(ConfirmState),
    Help,
    /// Drawn by [`crate::ui::popout`], not here.
    Popout(PopoutState),
}

#[derive(Debug, Clone)]
pub enum PromptKind {
    StdinReply,
    SaveAs,
    NewNotebook { dir: PathBuf },
}

pub struct PromptState {
    pub title: String,
    pub value: String,
    pub password: bool,
    pub kind: PromptKind,
}

#[derive(Debug, Clone)]
pub enum ConfirmKind {
    /// y = save and quit, n = quit without saving
    Quit,
    RestartKernel,
    RestartAndRunAll,
    OpenDiscard(PathBuf),
    NewDiscard(PathBuf),
    ReloadDiscard,
}

pub struct ConfirmState {
    pub message: String,
    pub kind: ConfirmKind,
}

#[derive(Debug, Clone)]
pub enum ItemData {
    Command(Command),
    Kernel(usize),
    Theme(String),
    Dir(PathBuf),
    File(PathBuf),
    NewNotebook(PathBuf),
}

#[derive(Debug, Clone)]
pub struct PaletteItem {
    pub label: String,
    pub detail: String,
    pub data: ItemData,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteKind {
    Commands,
    Kernels,
    Themes,
    Files(PathBuf),
}

pub struct PaletteState {
    pub kind: PaletteKind,
    pub title: String,
    pub query: String,
    pub items: Vec<PaletteItem>,
    pub filtered: Vec<usize>,
    pub selected: usize,
    matcher: Matcher,
}

pub enum PaletteAction {
    None,
    Close,
    Accept(ItemData),
    /// File browser: go to the parent directory.
    Parent,
    /// File browser: go to the home directory.
    Home,
    /// File browser: toggle hidden files.
    ToggleHidden,
}

impl PaletteState {
    pub fn new(kind: PaletteKind, title: impl Into<String>, items: Vec<PaletteItem>) -> Self {
        let mut s = PaletteState {
            kind,
            title: title.into(),
            query: String::new(),
            items,
            filtered: Vec::new(),
            selected: 0,
            matcher: Matcher::new(MatcherConfig::DEFAULT),
        };
        s.filter();
        s
    }

    pub fn filter(&mut self) {
        if self.query.is_empty() {
            self.filtered = (0..self.items.len()).collect();
        } else {
            let pat = Pattern::parse(&self.query, CaseMatching::Ignore, Normalization::Smart);
            let mut buf = Vec::new();
            let mut scored: Vec<(usize, u32)> = Vec::new();
            for (i, item) in self.items.iter().enumerate() {
                let hay = format!("{} {}", item.label, item.detail);
                let h = Utf32Str::new(&hay, &mut buf);
                if let Some(score) = pat.score(h, &mut self.matcher) {
                    scored.push((i, score));
                }
            }
            scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            self.filtered = scored.into_iter().map(|(i, _)| i).collect();
        }
        self.selected = self.selected.min(self.filtered.len().saturating_sub(1));
    }

    pub fn selected_item(&self) -> Option<&PaletteItem> {
        self.filtered.get(self.selected).and_then(|&i| self.items.get(i))
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> PaletteAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return PaletteAction::Close,
            KeyCode::Enter => {
                return match self.selected_item() {
                    Some(item) => PaletteAction::Accept(item.data.clone()),
                    None => PaletteAction::None,
                };
            }
            KeyCode::Up => self.move_sel(-1),
            KeyCode::Down => self.move_sel(1),
            KeyCode::PageUp => self.move_sel(-10),
            KeyCode::PageDown => self.move_sel(10),
            KeyCode::Tab => self.move_sel(1),
            KeyCode::BackTab => self.move_sel(-1),
            KeyCode::Char('p') | KeyCode::Char('k') if ctrl => self.move_sel(-1),
            KeyCode::Char('n') | KeyCode::Char('j') if ctrl => self.move_sel(1),
            KeyCode::Char('u') if ctrl => {
                self.query.clear();
                self.filter();
            }
            KeyCode::Char('h') if ctrl => return PaletteAction::ToggleHidden,
            KeyCode::Backspace => {
                if self.query.is_empty() {
                    if matches!(self.kind, PaletteKind::Files(_)) {
                        return PaletteAction::Parent;
                    }
                } else {
                    self.query.pop();
                    self.filter();
                }
            }
            KeyCode::Char('~') if self.query.is_empty() && matches!(self.kind, PaletteKind::Files(_)) => {
                return PaletteAction::Home;
            }
            KeyCode::Char('-') if self.query.is_empty() && matches!(self.kind, PaletteKind::Files(_)) => {
                return PaletteAction::Parent;
            }
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                self.query.push(c);
                self.filter();
            }
            _ => {}
        }
        PaletteAction::None
    }

    fn move_sel(&mut self, delta: i32) {
        if self.filtered.is_empty() {
            self.selected = 0;
            return;
        }
        let n = self.filtered.len() as i32;
        self.selected = ((self.selected as i32 + delta).rem_euclid(n)) as usize;
    }
}

fn centered(area: Rect, width_pct: u16, max_height: u16) -> Rect {
    let w = (area.width * width_pct / 100).clamp(20.min(area.width), area.width);
    let h = max_height.min(area.height);
    let x = area.x + (area.width - w) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 3;
    Rect::new(x, y, w, h)
}

pub fn render(buf: &mut Buffer, area: Rect, overlay: &mut Overlay, theme: &Theme) {
    match overlay {
        Overlay::Palette(p) => render_palette(buf, area, p, theme),
        Overlay::Prompt(p) => render_prompt(buf, area, p, theme),
        Overlay::Confirm(c) => render_confirm(buf, area, c, theme),
        Overlay::Help => render_help(buf, area, theme),
        Overlay::Popout(_) => {}
    }
}

fn float_block(theme: &Theme, border: Color) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .style(theme.float())
        .border_style(Style::default().fg(border))
}

fn render_palette(buf: &mut Buffer, area: Rect, p: &PaletteState, theme: &Theme) {
    let list_h = p.filtered.len().clamp(1, 18) as u16;
    let rect = centered(area, 70, list_h + 4);
    Clear.render(rect, buf);
    let block = float_block(theme, theme.ui.border).title(format!(" {} ", p.title));
    let inner = block.inner(rect);
    block.render(rect, buf);
    if inner.height == 0 {
        return;
    }
    let query_line = Line::from(vec![
        Span::styled("> ", Style::default().fg(theme.ui.focus)),
        Span::raw(p.query.clone()),
        Span::styled("▏", theme.muted()),
    ]);
    buf.set_line(inner.x, inner.y, &query_line, inner.width);
    let list = Rect::new(inner.x, inner.y + 1, inner.width, inner.height.saturating_sub(1));
    if list.height == 0 {
        return;
    }
    let visible = list.height as usize;
    let start = p.selected.saturating_sub(visible.saturating_sub(1)).min(p.filtered.len().saturating_sub(visible));
    for (row, &idx) in p.filtered.iter().enumerate().skip(start).take(visible) {
        let item = &p.items[idx];
        let y = list.y + (row - start) as u16;
        let selected = row == p.selected;
        let st = if selected { theme.selected_row() } else { Style::default() };
        buf.set_style(Rect::new(list.x, y, list.width, 1), st);
        let marker = if selected { "▶ " } else { "  " };
        let label = format!("{marker}{}", item.label);
        let detail_w = (item.detail.chars().count() as u16).min(list.width / 2);
        buf.set_string(list.x, y, &label, st);
        if detail_w > 0 {
            let detail_x = list.x + list.width - detail_w;
            let detail_st = if selected && theme.ui.match_fg.is_some() { st } else { st.fg(theme.ui.muted) };
            buf.set_string(detail_x, y, &item.detail, detail_st);
        }
    }
    if p.filtered.is_empty() {
        buf.set_string(list.x + 2, list.y, "no matches", theme.muted());
    }
}

fn render_prompt(buf: &mut Buffer, area: Rect, p: &PromptState, theme: &Theme) {
    let rect = centered(area, 60, 3);
    Clear.render(rect, buf);
    let block = float_block(theme, theme.ui.warning).title(format!(" {} ", p.title));
    let inner = block.inner(rect);
    block.render(rect, buf);
    let shown = if p.password { "•".repeat(p.value.chars().count()) } else { p.value.clone() };
    let line = Line::from(vec![Span::raw(shown), Span::styled("▏", theme.muted())]);
    buf.set_line(inner.x, inner.y, &line, inner.width);
}

fn render_confirm(buf: &mut Buffer, area: Rect, c: &ConfirmState, theme: &Theme) {
    let rect = centered(area, 60, 5);
    Clear.render(rect, buf);
    let block = float_block(theme, theme.ui.warning).title(" confirm ");
    let inner = block.inner(rect);
    block.render(rect, buf);
    Paragraph::new(c.message.clone()).wrap(Wrap { trim: false }).render(inner, buf);
}

pub const HELP: &str = "\
NOTEBOOK (command mode)            CELL EDITING
  j / k, ↓ / ↑   focus next/prev     Enter   edit cell (vim normal mode)
  gg / G         first / last        i       edit cell (insert mode)
  Ctrl+D / U     half page           Esc     normal → back to notebook
  Ctrl+E / Y     scroll a line       (vim: hjkl, dd, yy, p, ciw, v, /, u …)
  a / b          insert above/below
  dd / x / c     delete / cut / copy   RUN
  v / V          paste below/above    Shift+Enter  run & advance
  Ctrl+K / J     move cell up/down    Ctrl+Enter   run
  m / y / r      → markdown/code/raw  Alt+Enter    run & insert below
  M              merge with below     Ctrl+C / I   interrupt kernel
  Ctrl+_         split at cursor      0 0          restart kernel
  u / Ctrl+R     undo / redo cell op
  o / O          toggle / clear outputs
  z              inspect output (popout)
  / · n · N      search · next / prev match
  Esc            clear search highlight
                                    FILES & UI
  K              select kernel        Ctrl+S   save
  Ctrl+P         command palette      Ctrl+O   open file / browse
  ?              this help            q        quit
";

fn render_help(buf: &mut Buffer, area: Rect, theme: &Theme) {
    let lines = HELP.lines().count() as u16 + 2;
    let rect = centered(area, 86, lines);
    Clear.render(rect, buf);
    let block = float_block(theme, theme.ui.border).title(" saturn — keys (Esc to close) ");
    let inner = block.inner(rect);
    block.render(rect, buf);
    Paragraph::new(HELP).render(inner, buf);
}
