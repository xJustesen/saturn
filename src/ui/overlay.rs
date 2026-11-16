//! Modal overlays: fuzzy palette (commands / kernels / themes), prompts,
//! confirmations and the help screen.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nucleo_matcher::Utf32String;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Widget, Wrap};

use crate::commands::Command;
use crate::theme::Theme;
use crate::ui::cell_picker::CellPickerState;
use crate::ui::file_picker::FilePickerState;
use crate::ui::fuzzy::Fuzzy;
use crate::ui::popout::PopoutState;

pub enum Overlay {
    Palette(PaletteState),
    Prompt(PromptState),
    Confirm(ConfirmState),
    Help,
    /// `gt`: every cell on one row; drawn by [`crate::ui::cell_picker`].
    Cells(CellPickerState),
    /// Drawn by [`crate::ui::popout`], not here.
    Popout(PopoutState),
    /// `Ctrl+O`: drawn by [`crate::ui::file_picker`], not here.
    Files(Box<FilePickerState>),
}

impl Overlay {
    /// Pasted text: appended (trimmed) to a picker's query, or to a prompt's
    /// value without trailing newlines. Other overlays ignore it.
    pub fn paste(&mut self, text: &str) {
        match self {
            Overlay::Palette(p) => {
                p.query.push_str(text.trim());
                p.filter();
            }
            Overlay::Cells(p) => {
                p.query.push_str(text.trim());
                p.filter();
            }
            Overlay::Files(p) => p.push_query(text.trim()),
            Overlay::Prompt(p) => p.value.push_str(text.trim_end_matches(['\n', '\r'])),
            Overlay::Confirm(_) | Overlay::Help | Overlay::Popout(_) => {}
        }
    }
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
    /// `reopen`: the files picker's `start`, to reopen it on "no".
    OpenDiscard { path: PathBuf, reopen: Option<PathBuf> },
    NewDiscard(PathBuf),
    ReloadDiscard,
    /// Close the notebook and go back to the start dashboard.
    DashboardDiscard,
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
    /// A repository from the dashboard's Projects list.
    Project(PathBuf),
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
    Projects,
}

pub struct PaletteState {
    pub kind: PaletteKind,
    pub title: String,
    pub query: String,
    pub items: Vec<PaletteItem>,
    pub filtered: Vec<usize>,
    pub selected: usize,
    fuzzy: Fuzzy,
}

pub enum PaletteAction {
    None,
    Close,
    Accept(ItemData),
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
            fuzzy: Fuzzy::new(false),
        };
        s.filter();
        s
    }

    pub fn filter(&mut self) {
        if self.query.is_empty() {
            self.filtered = (0..self.items.len()).collect();
        } else {
            self.fuzzy.set_query(&self.query);
            let hays: Vec<Utf32String> =
                self.items.iter().map(|item| Utf32String::from(format!("{} {}", item.label, item.detail))).collect();
            let ranked = self.fuzzy.rank(hays.iter().enumerate().map(|(i, h)| (i as u32, h.slice(..), 0)));
            self.filtered = ranked.into_iter().map(|i| i as usize).collect();
        }
        self.selected = self.selected.min(self.filtered.len().saturating_sub(1));
    }

    /// Replace the items (a list that arrived later), keeping the query.
    pub fn set_items(&mut self, items: Vec<PaletteItem>) {
        self.items = items;
        self.filter();
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
            KeyCode::Backspace => {
                self.query.pop();
                self.filter();
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
        Overlay::Cells(p) => crate::ui::cell_picker::render(buf, area, p, theme),
        Overlay::Popout(_) | Overlay::Files(_) => {}
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
            // Long details (paths) keep their end, and stay inside the border.
            let detail = crate::ui::text::cut_left(&item.detail, detail_w as usize);
            buf.set_stringn(detail_x, y, detail, detail_w as usize, detail_st);
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
  gg / G         first / last cell    i       edit cell (insert mode)
  gt             go to cell (picker)
  Ctrl+D / U     half page           Esc     normal → back to notebook
  Ctrl+E / Y     scroll a line       vim grammar: counts, operators,
  a / b          insert above/below   motions, text objects, registers,
  dd / x / c     delete / cut / copy  visual modes, . repeat, macros,
                                      / ? * # search, :s, gc comments
                                    RUN
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
  Ctrl+P         command palette      Ctrl+O   find file
  ?              this help            q        quit
ASSISTANT (your Claude Code login)
  Ctrl+L         chat panel: open, close; Esc keeps it open, back to the notebook
  Ctrl+G         ask for a change to this cell, inline
  Tab / X / Enter  on a proposed change: accept / reject / accept and edit
  in the panel   Enter send · Alt+Enter new line · Ctrl+C stop · q close
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
