//! The docked assistant panel: transcript, context chips and the input, with
//! its keys and drawing. The app feeds it what the assistant says and does,
//! and acts on the [`ChatAction`] a key produces.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use edtui::LineNumbers;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::editor::CellEditor;
use crate::render::render_markdown;
use crate::theme::Theme;
use crate::ui::cell::{EditorOpts, draw_editor};
use crate::ui::text::{clean_text, cut, wrap_line, wrap_words};

/// Narrowest panel that is still usable.
pub const MIN_WIDTH: u16 = 40;
/// Most rows the input grows to before it scrolls.
const MAX_INPUT_ROWS: u16 = 8;
/// Columns left of the text (the user bar and the prompt sit in them).
const PAD_LEFT: u16 = 3;
const PAD_RIGHT: u16 = 1;
const HINTS: &str = "⏎ send  ⌥⏎ newline  esc read";
const EMPTY: &str = "Ask about the focused cell, its output or the project.";

/// Panel width for a main area `width` columns wide, or `None` when the
/// notebook and the panel do not fit side by side.
pub fn docked_width(width: u16) -> Option<u16> {
    let w = (width as u32 * 42 / 100) as u16;
    let w = w.clamp(MIN_WIDTH, 80);
    (width >= w + 56).then_some(w)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolState {
    Running,
    Done(String),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    User(String),
    Assistant(String),
    Tool {
        id: String,
        name: String,
        detail: String,
        state: ToolState,
    },
    Notice {
        text: String,
        error: bool,
    },
}

/// The assistant asks whether a tool may run.
#[derive(Debug, Clone, PartialEq)]
pub struct Ask {
    pub id: String,
    /// What wants to run ("Bash").
    pub title: String,
    /// With what ("rm -rf build").
    pub detail: String,
    /// What "always" passes back with the answer.
    pub remember: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Once,
    /// Allow, and do not ask again in this session.
    Always,
    Deny,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ChatAction {
    None,
    /// The answer to the [`Ask`] that was showing.
    Permission {
        ask: Ask,
        answer: Answer,
    },
    /// Send this prompt.
    Send(String),
    /// Stop the turn that is running.
    Interrupt,
    /// Give the keys back to the notebook; the panel stays (Esc).
    Blur,
    /// Hide the panel (Ctrl+L, or `q` in normal mode).
    Close,
}

struct Row {
    line: Line<'static>,
    /// The user's own message: a bar in the left margin.
    bar: bool,
}

/// Which part of the transcript the panel shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    /// The newest rows; the view follows what is written.
    End,
    /// The item with this index at the top, so a reply reads from its
    /// question down. Until the rows from there fill the panel this is the
    /// same as `End`.
    Pinned(usize),
    /// This row at the top: where the user scrolled to.
    Row(usize),
}

pub struct ChatState {
    items: Vec<Item>,
    /// Wrapped rows per item at `rows_width`; `None` is stale.
    rows: Vec<Option<Vec<Row>>>,
    rows_width: u16,
    pub input: CellEditor,
    /// What the next prompt is about ("cell 7 · code · figure").
    pub chips: Vec<String>,
    view: View,
    /// The top row and the highest possible top row as last drawn; scrolling
    /// starts from them.
    top_row: usize,
    max_top: usize,
    /// Transcript rows on screen, for paging.
    view_rows: usize,
    /// Reading mode: the keys scroll the transcript instead of typing.
    reading: bool,
    /// `g` was pressed in reading mode and waits for the second one.
    pending_g: bool,
    /// A permission request waiting for an answer; it takes the keys.
    pub ask: Option<Ask>,
    /// A turn is running.
    pub busy: bool,
    pub model: String,
    /// Reasoning effort, when one is set ("high").
    pub effort: String,
    /// Context use, as shown in the header ("12k").
    pub usage: String,
}

impl Default for ChatState {
    fn default() -> Self {
        Self::new()
    }
}

impl ChatState {
    pub fn new() -> Self {
        let mut input = CellEditor::new("");
        input.enter_insert();
        ChatState {
            items: Vec::new(),
            rows: Vec::new(),
            rows_width: 0,
            input,
            chips: Vec::new(),
            view: View::End,
            top_row: 0,
            max_top: 0,
            view_rows: 1,
            reading: false,
            pending_g: false,
            ask: None,
            busy: false,
            model: String::new(),
            effort: String::new(),
            usage: String::new(),
        }
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// The keys scroll the transcript (after Esc) rather than type.
    pub fn reading(&self) -> bool {
        self.reading
    }

    fn push(&mut self, item: Item) {
        self.items.push(item);
        self.rows.push(None);
    }

    /// The user's prompt. The view rests on it, so the reply that follows
    /// is read from its start however long it gets.
    pub fn push_user(&mut self, text: impl Into<String>) {
        self.push(Item::User(text.into()));
        self.view = View::Pinned(self.items.len() - 1);
        self.reading = false;
    }

    /// Streamed assistant text: extends the message being written.
    pub fn push_delta(&mut self, text: &str) {
        if let Some(Item::Assistant(t)) = self.items.last_mut() {
            t.push_str(text);
            if let Some(r) = self.rows.last_mut() {
                *r = None;
            }
        } else {
            self.push(Item::Assistant(text.to_string()));
        }
    }

    pub fn tool_start(
        &mut self,
        id: impl Into<String>,
        name: impl Into<String>,
        detail: impl Into<String>,
    ) {
        self.push(Item::Tool {
            id: id.into(),
            name: name.into(),
            detail: detail.into(),
            state: ToolState::Running,
        });
    }

    pub fn tool_end(&mut self, id: &str, ok: bool, note: impl Into<String>) {
        for (i, item) in self.items.iter_mut().enumerate().rev() {
            if let Item::Tool { id: tid, state, .. } = item
                && tid == id
            {
                *state = if ok {
                    ToolState::Done(note.into())
                } else {
                    ToolState::Failed(note.into())
                };
                self.rows[i] = None;
                return;
            }
        }
    }

    pub fn notice(&mut self, text: impl Into<String>, error: bool) {
        self.push(Item::Notice {
            text: text.into(),
            error,
        });
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.rows.clear();
        self.view = View::End;
    }

    pub fn paste(&mut self, text: String) {
        self.input.paste(text);
    }

    /// Put row `row` at the top (clamped). Reaching the end follows it again.
    fn scroll_to(&mut self, row: isize) {
        let row = row.clamp(0, self.max_top as isize) as usize;
        // Kept current here too: several keys can arrive between two frames.
        self.top_row = row;
        self.view = if row >= self.max_top {
            View::End
        } else {
            View::Row(row)
        };
    }

    fn scroll_by(&mut self, rows: isize) {
        self.scroll_to(self.top_row as isize + rows);
    }

    /// Keys of reading mode: vim's motions scroll the transcript.
    fn reading_key(&mut self, key: KeyEvent) -> ChatAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let page = self.view_rows.max(1) as isize;
        let half = (page / 2).max(1);
        let pending_g = std::mem::take(&mut self.pending_g);
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.scroll_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.scroll_by(-1),
            KeyCode::Char('d') if ctrl => self.scroll_by(half),
            KeyCode::Char('u') if ctrl => self.scroll_by(-half),
            KeyCode::Char('f') if ctrl => self.scroll_by(page),
            KeyCode::Char('b') if ctrl => self.scroll_by(-page),
            KeyCode::Char('e') if ctrl => self.scroll_by(1),
            KeyCode::Char('y') if ctrl => self.scroll_by(-1),
            KeyCode::Char('G') | KeyCode::End => self.scroll_to(isize::MAX),
            KeyCode::Char('g') if pending_g => self.scroll_to(0),
            KeyCode::Char('g') => self.pending_g = true,
            KeyCode::Home => self.scroll_to(0),
            KeyCode::Char('i' | 'a' | 'o') | KeyCode::Enter if !ctrl => self.reading = false,
            KeyCode::Char('q') => return ChatAction::Close,
            // Back to the notebook; the panel takes input again next time.
            KeyCode::Esc => {
                self.reading = false;
                return ChatAction::Blur;
            }
            _ => {}
        }
        ChatAction::None
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> ChatAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let page = self.view_rows.max(1) as isize;
        if self.ask.is_some() {
            let answer = match key.code {
                KeyCode::Char('l') if ctrl => return ChatAction::Close,
                KeyCode::Char('c') if ctrl => return ChatAction::Interrupt,
                KeyCode::Char('y') | KeyCode::Enter => Answer::Once,
                KeyCode::Char('a') => Answer::Always,
                KeyCode::Char('n') | KeyCode::Esc => Answer::Deny,
                _ => return ChatAction::None,
            };
            return match self.ask.take() {
                Some(ask) => ChatAction::Permission { ask, answer },
                None => ChatAction::None,
            };
        }
        match key.code {
            KeyCode::Char('l') if ctrl => return ChatAction::Close,
            KeyCode::Char('c') if ctrl => {
                return if self.busy {
                    ChatAction::Interrupt
                } else {
                    ChatAction::None
                };
            }
            KeyCode::PageUp => self.scroll_by(-page),
            KeyCode::PageDown => self.scroll_by(page),
            _ if self.reading => return self.reading_key(key),
            KeyCode::Esc => {
                self.reading = true;
                self.pending_g = false;
            }
            KeyCode::Enter
                if key
                    .modifiers
                    .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) =>
            {
                self.input
                    .key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                let text = self.input.text().trim().to_string();
                if text.is_empty() {
                    return ChatAction::None;
                }
                self.input = CellEditor::new("");
                self.input.enter_insert();
                return ChatAction::Send(text);
            }
            _ => {
                self.input.key(key);
                // The draft is only ever typed into: Esc reads the
                // transcript, so the editor never leaves insert mode.
                self.input.take_requests();
            }
        }
        ChatAction::None
    }

    fn ensure_rows(&mut self, width: u16, theme: &Theme, tab: usize) {
        if self.rows_width != width {
            self.rows_width = width;
            self.rows.iter_mut().for_each(|r| *r = None);
        }
        let w = width as usize;
        for (item, slot) in self.items.iter().zip(self.rows.iter_mut()) {
            if slot.is_none() {
                *slot = Some(item_rows(item, w, theme, tab));
            }
        }
    }
}

fn item_rows(item: &Item, w: usize, theme: &Theme, tab: usize) -> Vec<Row> {
    let ui = &theme.ui;
    let wrapped = |text: &str, st: Style, bar: bool| -> Vec<Row> {
        let clean = clean_text(text);
        clean
            .split('\n')
            .flat_map(|l| wrap_words(&Line::from(Span::styled(l.to_string(), st)), w))
            .map(|line| Row { line, bar })
            .collect()
    };
    match item {
        Item::User(t) => wrapped(t, Style::default().add_modifier(Modifier::BOLD), true),
        // Rendered unwrapped, then wrapped at words rather than characters.
        Item::Assistant(t) => render_markdown(&clean_text(t), u16::MAX, tab)
            .lines
            .iter()
            .flat_map(|l| wrap_words(l, w))
            .map(|line| Row { line, bar: false })
            .collect(),
        Item::Tool {
            name,
            detail,
            state,
            ..
        } => {
            let mut spans = vec![
                Span::styled("▸ ", theme.muted()),
                Span::styled(clean_text(name).into_owned(), theme.muted()),
            ];
            if !detail.is_empty() {
                spans.push(Span::styled(
                    format!(" {}", clean_text(detail)),
                    Style::default().fg(ui.line_numbers),
                ));
            }
            match state {
                ToolState::Running => spans.push(Span::styled("  …", theme.muted())),
                ToolState::Done(n) => spans.push(Span::styled(
                    format!("  ✓ {n}").trim_end().to_string(),
                    Style::default().fg(ui.success),
                )),
                ToolState::Failed(n) => spans.push(Span::styled(format!("  ✖ {n}"), theme.error())),
            }
            let line = Line::from(spans);
            let line = if line.width() > w {
                Line::from(Span::styled(cut(&line.to_string(), w), theme.muted()))
            } else {
                line
            };
            vec![Row { line, bar: false }]
        }
        Item::Notice { text, error } => {
            let st = if *error {
                theme.error()
            } else {
                theme.placeholder()
            };
            wrapped(text, st, false)
        }
    }
}

/// Draw the panel into `area`. Returns where the terminal cursor goes when
/// the panel has the keys and its input is in insert mode.
pub fn render(
    buf: &mut Buffer,
    area: Rect,
    st: &mut ChatState,
    theme: &Theme,
    focused: bool,
    tab: usize,
) -> Option<Position> {
    if area.width < 12 || area.height < 8 {
        return None;
    }
    let ui = &theme.ui;
    let float = theme.float();
    buf.set_style(area, float);
    let x = area.x + PAD_LEFT;
    let w = area.width - PAD_LEFT - PAD_RIGHT;
    let rule = Style::default().fg(ui.border);
    let hrule =
        |buf: &mut Buffer, y: u16| buf.set_string(area.x, y, "─".repeat(area.width as usize), rule);

    // header
    let title = if focused {
        Style::default().fg(ui.focus)
    } else {
        theme.muted()
    };
    buf.set_string(
        area.x + 2,
        area.y,
        "claude",
        title.add_modifier(Modifier::BOLD),
    );
    let info = [st.model.as_str(), st.effort.as_str(), st.usage.as_str()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    let info = cut(&info, area.width.saturating_sub(12) as usize);
    let iw = Line::from(info.as_str()).width() as u16;
    buf.set_string(
        area.x + area.width - PAD_RIGHT - iw,
        area.y,
        &info,
        theme.muted(),
    );
    hrule(buf, area.y + 1);

    // footer: rule, then a permission request or chips, input, gap and hints
    let mut cursor = None;
    let foot_y;
    if let Some(ask) = &st.ask {
        let detail = wrap_line(
            &Line::from(Span::styled(
                clean_text(&ask.detail).into_owned(),
                theme.muted(),
            )),
            w as usize,
            tab,
        );
        let detail_rows = (detail.len() as u16).min(4);
        foot_y = area.y + area.height - (1 + 1 + detail_rows + 2);
        hrule(buf, foot_y);
        let warn = Style::default().fg(ui.warning).add_modifier(Modifier::BOLD);
        buf.set_string(area.x + 1, foot_y + 1, "?", warn);
        buf.set_string(
            x,
            foot_y + 1,
            cut(&format!("allow {}", clean_text(&ask.title)), w as usize),
            warn,
        );
        for (i, l) in detail.iter().take(detail_rows as usize).enumerate() {
            buf.set_line(x, foot_y + 2 + i as u16, l, w);
        }
        let key = |s: &'static str| {
            Span::styled(
                s,
                Style::default().fg(ui.focus).add_modifier(Modifier::BOLD),
            )
        };
        let dim = |s: &'static str| Span::styled(s, theme.muted());
        let keys = Line::from(vec![
            key("y"),
            dim(" once   "),
            key("a"),
            dim(" always   "),
            key("n"),
            dim(" deny"),
        ]);
        buf.set_line(x, area.y + area.height - 1, &keys, w);
    } else {
        let total_rows = st.input.rows(w as usize, tab, true) as u16;
        let input_rows = total_rows.clamp(1, MAX_INPUT_ROWS.min(area.height / 3).max(1));
        let chip_rows = u16::from(!st.chips.is_empty());
        foot_y = area.y + area.height - (1 + chip_rows + input_rows + 2);
        hrule(buf, foot_y);
        if chip_rows == 1 {
            let mut cx = x;
            for chip in &st.chips {
                let label = format!(" ▣ {} ", cut(chip, w.saturating_sub(4) as usize));
                let lw = Line::from(label.as_str()).width() as u16;
                if cx + lw > x + w {
                    break;
                }
                buf.set_string(cx, foot_y + 1, &label, Style::default().bg(ui.selection));
                cx += lw + 1;
            }
        }
        let input_y = foot_y + 1 + chip_rows;
        let reading = st.reading;
        let prompt = if focused && !reading {
            Style::default().fg(ui.focus)
        } else {
            theme.muted()
        };
        buf.set_string(
            area.x + 1,
            input_y,
            "❯",
            prompt.add_modifier(Modifier::BOLD),
        );
        let cursor_row = st.input.cursor_visual_row(w as usize, tab, true) as u16;
        let first = (cursor_row + 1)
            .saturating_sub(input_rows)
            .min(total_rows.saturating_sub(input_rows));
        let opts = EditorOpts {
            theme,
            base: float,
            ext: "md",
            numbers: LineNumbers::None,
            wrap: true,
            tab,
        };
        let clip = Rect::new(area.x, input_y, area.width, input_rows);
        cursor = draw_editor(
            &mut st.input,
            &opts,
            x,
            w,
            total_rows,
            clip,
            -(first as i32),
            buf,
        );
        if reading {
            // The draft waits, dimmed, and takes no cursor.
            cursor = None;
            buf.set_style(
                Rect::new(area.x, foot_y + 1, area.width, chip_rows + input_rows),
                theme.muted(),
            );
        }
        if st.input.text().is_empty() {
            let hint = match st.chips.first() {
                _ if reading => "i to type".to_string(),
                Some(c) => format!("ask about {}", c.split(" · ").next().unwrap_or(c)),
                None => "ask claude".to_string(),
            };
            buf.set_string(x, input_y, cut(&hint, w as usize), theme.placeholder());
        }
        let hints_y = area.y + area.height - 1;
        if reading && focused {
            let key = |s: &'static str| {
                Span::styled(
                    s,
                    Style::default().fg(ui.focus).add_modifier(Modifier::BOLD),
                )
            };
            let dim = |s: &'static str| Span::styled(s, theme.muted());
            let keys = Line::from(vec![
                key("j k"),
                dim(" scroll "),
                key("^D ^U"),
                dim(" page "),
                key("gg G"),
                dim(" ends "),
                key("i"),
                dim(" type"),
            ]);
            buf.set_line(x, hints_y, &keys, w);
        } else {
            buf.set_string(
                x,
                hints_y,
                cut(HINTS, w as usize),
                Style::default().fg(ui.line_numbers),
            );
        }
    }

    // transcript
    let top = area.y + 2;
    let h = foot_y.saturating_sub(top) as usize;
    st.view_rows = h.max(1);
    st.ensure_rows(w, theme, tab);
    if st.items.is_empty() {
        let lines = wrap_line(
            &Line::from(Span::styled(EMPTY, theme.placeholder())),
            w as usize,
            tab,
        );
        for (i, l) in lines.iter().enumerate().take(h.saturating_sub(1)) {
            buf.set_line(x, top + 1 + i as u16, l, w);
        }
    } else {
        let blank = Row {
            line: Line::default(),
            bar: false,
        };
        let mut all: Vec<&Row> = Vec::new();
        // The row each item starts at.
        let mut starts: Vec<usize> = Vec::with_capacity(st.items.len());
        let mut prev_tool = false;
        for (item, rows) in st.items.iter().zip(&st.rows) {
            let tool = matches!(item, Item::Tool { .. });
            if !all.is_empty() && !(tool && prev_tool) {
                all.push(&blank);
            }
            starts.push(all.len());
            all.extend(rows.iter().flatten());
            prev_tool = tool;
        }
        let max_top = all.len().saturating_sub(h);
        let first = match st.view {
            View::End => max_top,
            View::Pinned(i) => starts.get(i).copied().unwrap_or(max_top).min(max_top),
            View::Row(r) => r.min(max_top),
        };
        st.top_row = first;
        st.max_top = max_top;
        // One spare row under the header when the transcript does not fill it.
        let lead = usize::from(all.len() < h);
        for (i, row) in all.iter().skip(first).take(h - lead).enumerate() {
            let y = top + (i + lead) as u16;
            if row.bar {
                buf.set_string(area.x + 1, y, "┃", Style::default().fg(ui.mode_visual));
            }
            buf.set_line(x, y, &row.line, w);
        }
        // What is out of view, on the rules above and below, and a scroll
        // bar in the right margin.
        let right = area.x + area.width - 1;
        let mut mark = |y: u16, text: String, st: Style| {
            let text = format!(" {text} ");
            buf.set_string(
                right.saturating_sub(text.chars().count() as u16),
                y,
                &text,
                st,
            );
        };
        if first > 0 {
            mark(area.y + 1, format!("↑ {first} above"), theme.muted());
        }
        if first < max_top {
            mark(
                foot_y,
                format!("↓ {} more", max_top - first),
                Style::default().fg(ui.warning),
            );
        }
        if max_top > 0 && h > 0 {
            let size = (h * h / all.len()).clamp(1, h);
            let pos = first * (h - size) / max_top;
            let thumb = if st.reading && focused {
                Style::default().fg(ui.focus)
            } else {
                theme.muted()
            };
            for i in 0..h {
                let (glyph, style) = if (pos..pos + size).contains(&i) {
                    ("┃", thumb)
                } else {
                    ("│", rule)
                };
                buf.set_string(right, top + i as u16, glyph, style);
            }
        }
    }
    if focused { cursor } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vim::Mode as VimMode;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn typed(st: &mut ChatState, s: &str) {
        for c in s.chars() {
            assert_eq!(st.handle_key(key(KeyCode::Char(c))), ChatAction::None);
        }
    }

    fn screen(st: &mut ChatState, w: u16, h: u16, focused: bool) -> String {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, st, &Theme::default(), focused, 4);
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn enter_sends_and_clears_the_input() {
        let mut st = ChatState::new();
        assert_eq!(st.handle_key(key(KeyCode::Enter)), ChatAction::None);
        typed(&mut st, "why is DK2 low?");
        assert_eq!(
            st.handle_key(key(KeyCode::Enter)),
            ChatAction::Send("why is DK2 low?".into())
        );
        assert_eq!(st.input.text(), "");
        assert!(matches!(st.input.mode(), VimMode::Insert));
    }

    #[test]
    fn alt_enter_adds_a_line() {
        let mut st = ChatState::new();
        typed(&mut st, "a");
        st.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        typed(&mut st, "b");
        assert_eq!(st.input.text(), "a\nb");
    }

    #[test]
    fn esc_reads_then_goes_back_to_the_notebook() {
        let mut st = ChatState::new();
        typed(&mut st, "draft");
        assert_eq!(st.handle_key(key(KeyCode::Esc)), ChatAction::None);
        assert!(st.reading());
        // Reading mode types nothing; `i` goes back to the draft.
        assert_eq!(st.handle_key(key(KeyCode::Char('x'))), ChatAction::None);
        assert_eq!(st.handle_key(key(KeyCode::Char('i'))), ChatAction::None);
        assert!(!st.reading());
        typed(&mut st, "!");
        assert_eq!(st.input.text(), "draft!");
        st.handle_key(key(KeyCode::Esc));
        assert_eq!(st.handle_key(key(KeyCode::Char('q'))), ChatAction::Close);
        assert_eq!(st.handle_key(key(KeyCode::Esc)), ChatAction::Blur);
        assert!(
            !st.reading(),
            "the panel takes input again when it gets the keys back"
        );
        assert_eq!(
            st.handle_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL)),
            ChatAction::Close
        );
    }

    /// A conversation with a reply far longer than the panel.
    fn long_reply() -> ChatState {
        let mut st = ChatState::new();
        st.notice("earlier", false);
        st.push_user("walk me through it");
        let reply: Vec<String> = (1..=40).map(|i| format!("reply line {i}")).collect();
        st.push_delta(&reply.join("\n\n"));
        st
    }

    #[test]
    fn a_long_reply_rests_on_its_question() {
        let mut st = long_reply();
        let s = screen(&mut st, 44, 20, true);
        let rows: Vec<&str> = s.lines().collect();
        assert!(rows[2].contains("┃ walk me through it"), "{s}");
        assert!(
            s.contains("reply line 1 ") && !s.contains("reply line 40"),
            "{s}"
        );
        assert!(rows[1].contains("↑ 2 above"), "{s}");
        assert!(s.contains("↓ ") && s.contains(" more"), "{s}");
        // More text arriving does not move the view.
        st.push_delta("\n\nthe end");
        let again = screen(&mut st, 44, 20, true);
        assert!(
            again.lines().nth(2).unwrap().contains("walk me through it"),
            "{again}"
        );
    }

    #[test]
    fn a_short_reply_keeps_the_history_in_view() {
        let mut st = ChatState::new();
        st.notice("earlier", false);
        st.push_user("hi");
        st.push_delta("hello");
        let s = screen(&mut st, 44, 20, true);
        assert!(s.contains("earlier") && s.contains("hello"), "{s}");
        assert!(!s.contains("above") && !s.contains("more"), "{s}");
    }

    #[test]
    fn reading_mode_scrolls_with_vim_motions() {
        let mut st = long_reply();
        screen(&mut st, 44, 20, true);
        st.handle_key(key(KeyCode::Esc));
        let s = screen(&mut st, 44, 20, true);
        assert!(
            s.lines()
                .last()
                .unwrap()
                .contains("j k scroll ^D ^U page gg G ends i type"),
            "{s}"
        );
        assert!(s.contains("i to type"), "{s}");

        st.handle_key(key(KeyCode::Char('G')));
        let s = screen(&mut st, 44, 20, true);
        assert!(
            s.contains("reply line 40") && !s.contains("walk me through it"),
            "{s}"
        );
        assert!(!s.contains(" more"), "{s}");

        st.handle_key(key(KeyCode::Char('g')));
        st.handle_key(key(KeyCode::Char('g')));
        let s = screen(&mut st, 44, 20, true);
        assert!(s.contains("earlier") && !s.contains("above"), "{s}");

        st.handle_key(key(KeyCode::Char('j')));
        st.handle_key(key(KeyCode::Char('j')));
        let s = screen(&mut st, 44, 20, true);
        assert!(s.lines().nth(1).unwrap().contains("↑ 2 above"), "{s}");
        st.handle_key(key(KeyCode::Char('k')));
        st.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        let s = screen(&mut st, 44, 20, true);
        assert!(s.lines().nth(1).unwrap().contains("↑ 8 above"), "{s}");

        // Sending a new prompt leaves reading mode and rests on that prompt.
        st.handle_key(key(KeyCode::Char('i')));
        typed(&mut st, "next");
        assert_eq!(
            st.handle_key(key(KeyCode::Enter)),
            ChatAction::Send("next".into())
        );
        st.push_user("next");
        let s = screen(&mut st, 44, 20, true);
        assert!(s.contains("┃ next") && s.contains("reply line 40"), "{s}");
    }

    #[test]
    fn ctrl_c_interrupts_only_a_running_turn() {
        let mut st = ChatState::new();
        let cc = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(st.handle_key(cc), ChatAction::None);
        st.busy = true;
        assert_eq!(st.handle_key(cc), ChatAction::Interrupt);
    }

    #[test]
    fn deltas_extend_one_message_and_tools_update_in_place() {
        let mut st = ChatState::new();
        st.push_user("hi");
        st.push_delta("DK2 has ");
        st.push_delta("fewer trades.");
        st.tool_start("t1", "read_cells", "6–7");
        st.tool_end("t1", true, "0.4s");
        st.push_delta("Done.");
        assert_eq!(
            st.items(),
            &[
                Item::User("hi".into()),
                Item::Assistant("DK2 has fewer trades.".into()),
                Item::Tool {
                    id: "t1".into(),
                    name: "read_cells".into(),
                    detail: "6–7".into(),
                    state: ToolState::Done("0.4s".into())
                },
                Item::Assistant("Done.".into()),
            ]
        );
    }

    #[test]
    fn draws_header_transcript_chips_and_hints() {
        let mut st = ChatState::new();
        st.model = "fable".into();
        st.usage = "12k".into();
        st.chips.push("cell 7 · code · figure".into());
        st.push_user("why is DK2 so much lower?");
        st.push_delta("It has **fewer** trades.");
        st.tool_start("t1", "run_cells", "8");
        let s = screen(&mut st, 44, 20, true);
        assert!(s.lines().next().unwrap().contains("claude"), "{s}");
        assert!(s.contains("fable · 12k"), "{s}");
        assert!(s.contains("┃ why is DK2 so much lower?"), "{s}");
        assert!(s.contains("It has fewer trades."), "{s}");
        assert!(s.contains("▸ run_cells 8  …"), "{s}");
        assert!(s.contains("▣ cell 7 · code · figure"), "{s}");
        assert!(s.contains("❯"), "{s}");
        assert!(s.contains("ask about cell 7"), "{s}");
        assert!(s.lines().last().unwrap().contains("⏎ send"), "{s}");
    }

    #[test]
    fn long_transcripts_follow_the_newest_row_and_scroll_back() {
        let mut st = ChatState::new();
        for i in 0..40 {
            st.notice(format!("line {i}"), false);
        }
        let s = screen(&mut st, 44, 20, true);
        assert!(s.contains("line 39") && !s.contains("line 0\n"), "{s}");
        st.handle_key(key(KeyCode::PageUp));
        st.handle_key(key(KeyCode::PageUp));
        st.handle_key(key(KeyCode::PageUp));
        st.handle_key(key(KeyCode::PageUp));
        st.handle_key(key(KeyCode::PageUp));
        st.handle_key(key(KeyCode::PageUp));
        st.handle_key(key(KeyCode::PageUp));
        let s = screen(&mut st, 44, 20, true);
        assert!(s.contains("line 0 ") && !s.contains("line 39"), "{s}");
        assert!(s.contains("↓"), "{s}");
    }

    #[test]
    fn the_transcript_wraps_at_words() {
        let mut st = ChatState::new();
        st.push_delta("It streams a few chunks of markdown so the panel has something to draw.");
        let s = screen(&mut st, 44, 20, true);
        assert!(
            s.contains("   It streams a few chunks of markdown so  "),
            "{s}"
        );
        assert!(s.contains("   the panel has something to draw."), "{s}");
    }

    #[test]
    fn a_permission_request_takes_the_keys() {
        let mut st = ChatState::new();
        let ask = Ask {
            id: "p1".into(),
            title: "Bash".into(),
            detail: "rm -rf build".into(),
            remember: Vec::new(),
        };
        st.ask = Some(ask.clone());
        let s = screen(&mut st, 44, 20, true);
        assert!(s.contains("? allow Bash"), "{s}");
        assert!(s.contains("rm -rf build"), "{s}");
        assert!(
            s.lines()
                .last()
                .unwrap()
                .contains("y once   a always   n deny"),
            "{s}"
        );
        assert_eq!(st.handle_key(key(KeyCode::Char('x'))), ChatAction::None);
        assert_eq!(st.input.text(), "");
        assert_eq!(
            st.handle_key(key(KeyCode::Char('a'))),
            ChatAction::Permission {
                ask: ask.clone(),
                answer: Answer::Always
            }
        );
        assert!(st.ask.is_none());
        st.ask = Some(ask.clone());
        assert_eq!(
            st.handle_key(key(KeyCode::Esc)),
            ChatAction::Permission {
                ask,
                answer: Answer::Deny
            }
        );
    }

    #[test]
    fn docks_only_when_the_notebook_keeps_room() {
        assert_eq!(docked_width(90), None);
        assert_eq!(docked_width(100), Some(42));
        assert_eq!(docked_width(300), Some(80));
    }
}
