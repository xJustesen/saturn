//! `gt`: a picker listing every cell on one row (number, first source line,
//! output glyph and excerpt) so a cell can be found at a glance. Digits jump
//! to a cell number, letters fuzzy-filter on the source and output text.

use base64::Engine;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jupyter_protocol::MediaType;
use nucleo_matcher::Utf32String;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Widget};

use crate::notebook::{Cell, CellKind, ExecState, Output};
use crate::render::{html, richest};
use crate::theme::Theme;
use crate::ui::fuzzy::Fuzzy;
use crate::ui::text::{cut, expand_tabs};

/// What kind of output a cell's excerpt describes; picks the glyph and colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutKind {
    Stream,
    Result,
    Figure,
    Error,
}

impl OutKind {
    pub fn glyph(self) -> &'static str {
        match self {
            OutKind::Stream => "≡",
            OutKind::Result => "▤",
            OutKind::Figure => "▦",
            OutKind::Error => "✗",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowState {
    Idle,
    Running,
    Error,
}

#[derive(Debug, Clone)]
pub struct CellRow {
    pub kind: CellKind,
    /// First non-blank source line, tabs expanded.
    pub source: String,
    pub out: Option<(OutKind, String)>,
    pub state: RowState,
}

pub enum PickerAction {
    None,
    Close,
    /// Jump to this cell index.
    Go(usize),
}

pub struct CellPickerState {
    pub rows: Vec<CellRow>,
    pub query: String,
    pub filtered: Vec<usize>,
    pub selected: usize,
    pub focused: usize,
    /// Cells currently on screen (inclusive).
    pub visible: (usize, usize),
    fuzzy: Fuzzy,
}

impl CellPickerState {
    pub fn new(cells: &[Cell], tab_width: usize, focused: usize, visible: (usize, usize)) -> Self {
        let rows = cells.iter().map(|c| summarize(c, tab_width)).collect::<Vec<_>>();
        let mut s = CellPickerState {
            rows,
            query: String::new(),
            filtered: Vec::new(),
            selected: focused,
            focused,
            visible,
            fuzzy: Fuzzy::new(false),
        };
        s.filter();
        s
    }

    fn numeric(&self) -> Option<usize> {
        if self.query.is_empty() || !self.query.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        self.query.parse().ok()
    }

    pub fn filter(&mut self) {
        let n = self.rows.len();
        if let Some(k) = self.numeric() {
            // Digits address a cell number; the whole list stays so the
            // target lights up in place.
            self.filtered = (0..n).collect();
            if (1..=n).contains(&k) {
                self.selected = k - 1;
            }
            return;
        }
        if self.query.is_empty() {
            self.filtered = (0..n).collect();
            self.selected = self.selected.min(n.saturating_sub(1));
            return;
        }
        self.fuzzy.set_query(&self.query);
        let hays: Vec<Utf32String> = self
            .rows
            .iter()
            .map(|row| match &row.out {
                Some((_, x)) => Utf32String::from(format!("{} {}", row.source, x)),
                None => Utf32String::from(row.source.as_str()),
            })
            .collect();
        let ranked = self.fuzzy.rank(hays.iter().enumerate().map(|(i, h)| (i as u32, h.slice(..), 0)));
        self.filtered = ranked.into_iter().map(|i| i as usize).collect();
        self.selected = 0;
    }

    fn move_sel(&mut self, delta: i32) {
        if self.filtered.is_empty() {
            self.selected = 0;
            return;
        }
        let n = self.filtered.len() as i32;
        let pos = self.filtered.iter().position(|&i| i == self.selected).unwrap_or(0) as i32;
        self.selected = self.filtered[((pos + delta).rem_euclid(n)) as usize];
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> PickerAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => return PickerAction::Close,
            KeyCode::Enter => {
                return if self.filtered.contains(&self.selected) || self.numeric().is_some() {
                    PickerAction::Go(self.selected.min(self.rows.len().saturating_sub(1)))
                } else {
                    PickerAction::None
                };
            }
            KeyCode::Up => self.move_sel(-1),
            KeyCode::Down => self.move_sel(1),
            KeyCode::PageUp => self.move_sel(-10),
            KeyCode::PageDown => self.move_sel(10),
            KeyCode::BackTab => self.move_sel(-1),
            KeyCode::Tab => self.move_sel(1),
            KeyCode::Char('p') | KeyCode::Char('k') if ctrl => self.move_sel(-1),
            KeyCode::Char('n') | KeyCode::Char('j') if ctrl => self.move_sel(1),
            // Plain j/k browse while nothing is typed; once a query starts
            // they are letters like any other.
            KeyCode::Char('k') if self.query.is_empty() && !alt => self.move_sel(-1),
            KeyCode::Char('j') if self.query.is_empty() && !alt => self.move_sel(1),
            KeyCode::Char('u') if ctrl => {
                self.query.clear();
                self.filter();
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.filter();
            }
            KeyCode::Char(c) if !ctrl && !alt => {
                self.query.push(c);
                self.filter();
            }
            _ => {}
        }
        PickerAction::None
    }
}

// ----- summaries ---------------------------------------------------------------

pub fn summarize(cell: &Cell, tab_width: usize) -> CellRow {
    let source = cell
        .source
        .lines()
        .map(|l| l.trim_end())
        .find(|l| !l.trim().is_empty())
        .map(|l| expand_tabs(l, tab_width))
        .unwrap_or_default();
    let out = if cell.kind == CellKind::Code { summarize_outputs(&cell.outputs) } else { None };
    let state = match cell.exec {
        ExecState::Error => RowState::Error,
        ExecState::Running | ExecState::Queued => RowState::Running,
        _ if cell.outputs.iter().any(|o| matches!(o, Output::Error { .. })) => RowState::Error,
        _ => RowState::Idle,
    };
    CellRow { kind: cell.kind, source, out, state }
}

/// One excerpt for a cell: an error wins, then the richest display/result,
/// then the first stream.
fn summarize_outputs(outputs: &[Output]) -> Option<(OutKind, String)> {
    if let Some(Output::Error { ename, .. }) = outputs.iter().find(|o| matches!(o, Output::Error { .. })) {
        return Some((OutKind::Error, ename.clone()));
    }
    let mut best: Option<(u8, OutKind, String)> = None;
    for o in outputs {
        let (rank, kind, text) = match o {
            Output::Stream { text, .. } => {
                let mut lines = text.lines().filter(|l| !l.trim().is_empty());
                let first = lines.next().unwrap_or("").trim_end().to_string();
                let n = text.lines().count();
                let text = if n > 1 { format!("{first} ({n} lines)") } else { first };
                (1, OutKind::Stream, text)
            }
            Output::DisplayData { data, .. } | Output::ExecuteResult { data, .. } => match richest(data) {
                Some(MediaType::Png(b64)) => (3, OutKind::Figure, match png_dims(b64) {
                    Some((w, h)) => format!("figure {w}×{h}"),
                    None => "figure".to_string(),
                }),
                Some(MediaType::Jpeg(_)) | Some(MediaType::Gif(_)) | Some(MediaType::Svg(_)) => (3, OutKind::Figure, "figure".to_string()),
                Some(MediaType::Plotly(v)) => (3, OutKind::Figure, match plotly_dims(v) {
                    Some((w, h)) => format!("plotly {w}×{h}"),
                    None => "plotly figure".to_string(),
                }),
                Some(MediaType::Html(s)) => (2, OutKind::Result, first_line(&html::to_text(s))),
                Some(MediaType::Markdown(s)) | Some(MediaType::Plain(s)) | Some(MediaType::Latex(s)) => (2, OutKind::Result, first_line(s)),
                Some(MediaType::Json(_)) => (2, OutKind::Result, "json".to_string()),
                Some(MediaType::Other((mime, _))) => (2, OutKind::Result, mime.clone()),
                _ => (2, OutKind::Result, String::new()),
            },
            Output::Error { .. } => unreachable!(),
        };
        if best.as_ref().is_none_or(|(r, _, _)| rank > *r) {
            best = Some((rank, kind, text));
        }
    }
    best.map(|(_, k, t)| (k, t))
}

fn first_line(s: &str) -> String {
    s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_string()
}

/// Width and height from a base64 PNG's IHDR chunk (first 24 bytes).
pub fn png_dims(b64: &str) -> Option<(u32, u32)> {
    let head: String = b64.chars().filter(|c| !c.is_whitespace()).take(32).collect();
    let bytes = base64::engine::general_purpose::STANDARD.decode(&head).ok()?;
    if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((w, h))
}

fn plotly_dims(v: &serde_json::Value) -> Option<(u64, u64)> {
    let layout = v.get("layout")?;
    Some((layout.get("width")?.as_u64()?, layout.get("height")?.as_u64()?))
}

// ----- drawing -----------------------------------------------------------------

pub fn render(buf: &mut Buffer, area: Rect, p: &CellPickerState, theme: &Theme) {
    let n = p.rows.len();
    let list_h = p.filtered.len().clamp(1, 20) as u16;
    let w = (area.width * 70 / 100).clamp(48.min(area.width), area.width);
    let h = (list_h + 3).min(area.height);
    let rect = Rect::new(area.x + (area.width - w) / 2, area.y + area.height.saturating_sub(h) / 3, w, h);
    Clear.render(rect, buf);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .style(theme.float())
        .border_style(Style::default().fg(theme.ui.border))
        .title(format!(" Go to cell (1-{n}) "));
    let inner = block.inner(rect);
    block.render(rect, buf);
    if inner.height < 2 || inner.width < 12 {
        return;
    }
    let list = Rect::new(inner.x, inner.y, inner.width, inner.height - 1);
    let num_w = n.max(1).to_string().len() as u16;
    let text_x = list.x + 1 + num_w + 1;
    let text_w = list.width.saturating_sub(1 + num_w + 1 + 1) as usize;
    let src_w = text_w * 56 / 100;
    let out_w = text_w.saturating_sub(src_w + 1);

    let visible = list.height as usize;
    let pos = p.filtered.iter().position(|&i| i == p.selected).unwrap_or(0);
    let start = pos.saturating_sub(visible.saturating_sub(1)).min(p.filtered.len().saturating_sub(visible));
    let band = theme.ui.input_bg;
    for (row, &idx) in p.filtered.iter().enumerate().skip(start).take(visible) {
        let r = &p.rows[idx];
        let y = list.y + (row - start) as u16;
        let selected = idx == p.selected;
        let on_screen = idx >= p.visible.0 && idx <= p.visible.1;
        let mut base = Style::default();
        if selected {
            base = theme.selected_row();
        } else if on_screen && let Some(bg) = band {
            base = base.bg(bg);
        }
        buf.set_style(Rect::new(list.x, y, list.width, 1), base);
        // Colour carries state; a selected row keeps the selection colours.
        let state_fg = match r.state {
            RowState::Error => Some(theme.ui.error),
            RowState::Running => Some(theme.ui.warning),
            RowState::Idle => None,
        };
        let tint = |st: Style, fg: ratatui::style::Color| if selected { st } else { st.fg(fg) };
        if idx == p.focused {
            buf.set_string(list.x, y, "┃", tint(base, theme.ui.focus));
        }
        let num_st = match state_fg {
            Some(c) => tint(base, c),
            None if idx == p.focused => tint(base, theme.ui.focus).add_modifier(Modifier::BOLD),
            None => tint(base, theme.ui.muted),
        };
        buf.set_string(list.x + 1, y, format!("{:>w$}", idx + 1, w = num_w as usize), num_st);
        let (src, src_st) = if r.source.is_empty() {
            ("(empty)".to_string(), tint(base, theme.ui.muted).add_modifier(Modifier::ITALIC))
        } else {
            let st = match (r.kind, state_fg) {
                (_, Some(c)) => tint(base, c),
                (CellKind::Markdown, None) => tint(base, theme.ui.info).add_modifier(Modifier::BOLD),
                (CellKind::Raw, None) => tint(base, theme.ui.muted),
                _ => base,
            };
            (r.source.clone(), st)
        };
        let src_max = if r.out.is_some() { src_w } else { text_w };
        buf.set_string(text_x, y, cut(&src, src_max), src_st);
        if let Some((kind, excerpt)) = &r.out
            && out_w > 2
        {
            let gx = text_x + src_w as u16 + 1;
            let (glyph, glyph_fg) = match (kind, r.state) {
                (_, RowState::Running) => ("◐", theme.ui.warning),
                (OutKind::Error, _) => (kind.glyph(), theme.ui.error),
                (OutKind::Figure, _) => (kind.glyph(), theme.ui.info),
                _ => (kind.glyph(), theme.ui.muted),
            };
            buf.set_string(gx, y, glyph, tint(base, glyph_fg));
            let ex_fg = state_fg.unwrap_or(theme.ui.muted);
            buf.set_string(gx + 2, y, cut(excerpt, out_w.saturating_sub(2)), tint(base, ex_fg));
        }
    }
    if p.filtered.is_empty() {
        buf.set_string(list.x + 2, list.y, "no matches", theme.muted());
    }

    let qy = inner.y + inner.height - 1;
    let hint = if p.query.is_empty() {
        "digits jump · letters filter · j/k · Enter · Esc"
    } else {
        "Enter go · Esc back · Ctrl+U clear"
    };
    let line = Line::from(vec![
        Span::raw(p.query.clone()),
        Span::styled("▏", theme.muted()),
        Span::styled(format!("  {hint}"), theme.muted()),
    ]);
    buf.set_line(inner.x + 1, qy, &line, inner.width.saturating_sub(1));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_header_dims() {
        // 1x1 PNG
        let b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
        assert_eq!(png_dims(b64), Some((1, 1)));
        assert_eq!(png_dims("not png"), None);
    }
}
