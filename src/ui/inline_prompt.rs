//! The one-line prompt that opens under a cell's source: ask the assistant
//! for a change to that cell without opening the panel. The answer comes
//! back as a proposed edit on the cell.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::theme::Theme;
use crate::ui::text::{clean_text, cut, cut_left};

/// Rows the prompt takes under the source: the input and the keys.
pub const ROWS: u16 = 2;
const HINTS: &str = "⏎ send   esc cancel   ⇥ open in chat";
const SPINNER: [&str; 4] = ["◐", "◓", "◑", "◒"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptAction {
    None,
    Send(String),
    Cancel,
    /// Continue in the panel with what was typed.
    ToChat(String),
}

#[derive(Debug, Clone, Default)]
pub struct InlinePrompt {
    pub text: String,
    /// The part of the cell that was selected when the prompt opened.
    pub selection: Option<String>,
    /// The request is with the assistant.
    pub busy: bool,
    /// What the assistant said instead of proposing an edit, or what failed.
    pub note: Option<(String, bool)>,
}

impl InlinePrompt {
    pub fn new(selection: Option<String>) -> Self {
        InlinePrompt {
            selection: selection.filter(|s| !s.trim().is_empty()),
            ..Default::default()
        }
    }

    pub fn paste(&mut self, text: &str) {
        if !self.busy {
            self.text.push_str(&text.replace(['\n', '\r'], " "));
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> PromptAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return PromptAction::Cancel,
            KeyCode::Char('c' | 'g') if ctrl => return PromptAction::Cancel,
            _ if self.busy => {}
            KeyCode::Enter => {
                let text = self.text.trim().to_string();
                if !text.is_empty() {
                    self.note = None;
                    return PromptAction::Send(text);
                }
            }
            KeyCode::Tab => return PromptAction::ToChat(self.text.trim().to_string()),
            KeyCode::Backspace => {
                self.text.pop();
            }
            KeyCode::Char('u') if ctrl => self.text.clear(),
            KeyCode::Char('w') if ctrl => {
                let kept = self.text.trim_end().rfind(' ').map_or(0, |i| i + 1);
                self.text.truncate(kept);
            }
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                self.note = None;
                self.text.push(c);
            }
            _ => {}
        }
        PromptAction::None
    }
}

/// Draw the prompt in `area` (`ROWS` high; its first column carries the
/// bar). Returns where the terminal cursor goes while it takes input.
pub fn render(
    buf: &mut Buffer,
    area: Rect,
    p: &InlinePrompt,
    model: &str,
    spinner: usize,
    theme: &Theme,
) -> Option<Position> {
    if area.height < ROWS || area.width < 12 {
        return None;
    }
    let ui = &theme.ui;
    buf.set_style(area, theme.float());
    let accent = Style::default().fg(ui.mode_chat);
    for dy in 0..ROWS {
        buf.set_string(area.x, area.y + dy, "┃", accent);
    }
    buf.set_string(area.x + 2, area.y, "❯", accent.add_modifier(Modifier::BOLD));
    let x = area.x + 4;
    let w = area.width.saturating_sub(5) as usize;
    // The end of a long prompt stays in view, like a shell line.
    let shown = cut_left(&p.text, w.saturating_sub(1));
    let text_style = if p.busy {
        theme.muted()
    } else {
        Style::default()
    };
    buf.set_string(x, area.y, &shown, text_style);
    let cursor = (!p.busy).then(|| Position::new(x + shown.width() as u16, area.y));

    let y = area.y + 1;
    let right = match &p.selection {
        Some(s) => format!("selection · {} lines   {model}", s.lines().count().max(1)),
        None => model.to_string(),
    };
    let rw = right.width();
    let left: Line = match (&p.note, p.busy) {
        (_, true) => Line::from(vec![
            Span::styled(SPINNER[spinner % SPINNER.len()], accent),
            Span::styled(" claude is working   esc cancel", theme.muted()),
        ]),
        (Some((note, error)), _) => {
            let st = if *error {
                theme.error()
            } else {
                Style::default().fg(ui.info)
            };
            Line::from(Span::styled(
                cut(
                    &clean_text(note).replace('\n', " "),
                    w.saturating_sub(rw + 2),
                ),
                st,
            ))
        }
        _ => Line::from(Span::styled(HINTS, Style::default().fg(ui.line_numbers))),
    };
    buf.set_line(x, y, &left, w as u16);
    if rw + 2 < w && left.width() + rw + 2 <= w {
        buf.set_string(
            x + (w - rw) as u16,
            y,
            &right,
            Style::default().fg(ui.line_numbers),
        );
    }
    cursor
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn typed(p: &mut InlinePrompt, s: &str) {
        for c in s.chars() {
            assert_eq!(p.handle_key(key(KeyCode::Char(c))), PromptAction::None);
        }
    }

    fn screen(p: &InlinePrompt, w: u16) -> (Vec<String>, Option<Position>) {
        let area = Rect::new(0, 0, w, ROWS);
        let mut buf = Buffer::empty(area);
        let cur = render(&mut buf, area, p, "claude · haiku", 0, &Theme::default());
        let rows = (0..ROWS)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect();
        (rows, cur)
    }

    #[test]
    fn typing_editing_and_sending() {
        let mut p = InlinePrompt::new(None);
        assert_eq!(p.handle_key(key(KeyCode::Enter)), PromptAction::None);
        typed(&mut p, "plot pnl per tradex");
        p.handle_key(key(KeyCode::Backspace));
        assert_eq!(p.text, "plot pnl per trade");
        p.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(p.text, "plot pnl per ");
        typed(&mut p, "zone");
        assert_eq!(
            p.handle_key(key(KeyCode::Enter)),
            PromptAction::Send("plot pnl per zone".into())
        );
        assert_eq!(
            p.handle_key(key(KeyCode::Tab)),
            PromptAction::ToChat("plot pnl per zone".into())
        );
        assert_eq!(p.handle_key(key(KeyCode::Esc)), PromptAction::Cancel);
    }

    #[test]
    fn a_busy_prompt_only_cancels() {
        let mut p = InlinePrompt::new(None);
        typed(&mut p, "x");
        p.busy = true;
        assert_eq!(p.handle_key(key(KeyCode::Char('y'))), PromptAction::None);
        assert_eq!(p.handle_key(key(KeyCode::Enter)), PromptAction::None);
        assert_eq!(p.text, "x");
        assert_eq!(p.handle_key(key(KeyCode::Esc)), PromptAction::Cancel);
    }

    #[test]
    fn draws_the_input_the_keys_and_the_model() {
        let mut p = InlinePrompt::new(Some("a\nb".into()));
        typed(&mut p, "make it per trade");
        let (rows, cur) = screen(&p, 90);
        assert_eq!(rows[0], "┃ ❯ make it per trade");
        assert!(
            rows[1].starts_with("┃   ⏎ send   esc cancel   ⇥ open in chat"),
            "{rows:?}"
        );
        assert!(
            rows[1].ends_with("selection · 2 lines   claude · haiku"),
            "{rows:?}"
        );
        assert_eq!(cur, Some(Position::new(21, 0)));
        p.busy = true;
        let (rows, cur) = screen(&p, 90);
        assert!(rows[1].contains("claude is working"), "{rows:?}");
        assert_eq!(cur, None);
        p.busy = false;
        p.note = Some(("That cell has no plot.".into(), false));
        assert!(screen(&p, 90).0[1].contains("That cell has no plot."));
    }
}
