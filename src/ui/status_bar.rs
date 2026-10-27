//! Bottom status line: mode, document, kernel, pending keys, flash messages.

use edtui::EditorMode;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Mode};
use crate::kernel::KernelStatus;

const SPINNER: [&str; 4] = ["◐", "◓", "◑", "◒"];

pub fn render(buf: &mut Buffer, area: Rect, app: &App) {
    buf.set_style(area, Style::default().bg(Color::Rgb(30, 30, 40)));
    let (mode_label, mode_style) = match app.mode {
        Mode::Command => (" COMMAND ", Style::default().bg(Color::Blue).fg(Color::Black).add_modifier(Modifier::BOLD)),
        Mode::Edit => {
            let m = app.nb.cells.get(app.focused).and_then(|c| c.editor.as_ref()).map(|e| e.mode());
            match m {
                Some(EditorMode::Insert) => (" INSERT ", Style::default().bg(Color::Green).fg(Color::Black).add_modifier(Modifier::BOLD)),
                Some(EditorMode::Visual) => (" VISUAL ", Style::default().bg(Color::Magenta).fg(Color::Black).add_modifier(Modifier::BOLD)),
                Some(EditorMode::Search) => (" SEARCH ", Style::default().bg(Color::Cyan).fg(Color::Black).add_modifier(Modifier::BOLD)),
                _ => (" NORMAL ", Style::default().bg(Color::LightGreen).fg(Color::Black).add_modifier(Modifier::BOLD)),
            }
        }
    };
    let mut left: Vec<Span> = vec![Span::styled(mode_label, mode_style), Span::raw(" ")];
    let title = app.nb.title();
    left.push(Span::styled(title, Style::default().add_modifier(Modifier::BOLD)));
    if app.nb.dirty {
        left.push(Span::styled(" [+]", Style::default().fg(Color::Yellow)));
    }
    left.push(Span::styled(
        format!("  cell {}/{}", app.focused + 1, app.nb.cells.len()),
        Style::default().fg(Color::Gray),
    ));
    if !app.pending.is_empty() {
        left.push(Span::styled(format!("  {}", crate::keymap::describe(&app.pending)), Style::default().fg(Color::Yellow)));
    }
    if let Some(f) = &app.flash {
        let st = if f.error { Style::default().fg(Color::LightRed) } else { Style::default().fg(Color::LightCyan) };
        left.push(Span::raw("  "));
        left.push(Span::styled(f.text.clone(), st));
    }

    let mut right: Vec<Span> = Vec::new();
    match &app.kernel {
        Some(k) => {
            let (icon, st) = match k.status {
                KernelStatus::Idle => ("●", Style::default().fg(Color::Green)),
                KernelStatus::Busy => (SPINNER[app.spinner % SPINNER.len()], Style::default().fg(Color::Yellow)),
                KernelStatus::Starting | KernelStatus::Restarting => (SPINNER[app.spinner % SPINNER.len()], Style::default().fg(Color::Cyan)),
                KernelStatus::Dead => ("✖", Style::default().fg(Color::Red)),
            };
            right.push(Span::styled(k.display_name().to_string(), Style::default().fg(Color::Gray)));
            right.push(Span::raw(" "));
            right.push(Span::styled(icon, st));
            right.push(Span::styled(format!(" {}", k.status.label()), Style::default().fg(Color::Gray)));
        }
        None => right.push(Span::styled("no kernel (K to select)", Style::default().fg(Color::DarkGray))),
    }
    right.push(Span::styled(format!("  {} ", app.graphics_label), Style::default().fg(Color::DarkGray)));

    let left_line = Line::from(left);
    let right_line = Line::from(right);
    let rw = right_line.width() as u16;
    buf.set_line(area.x, area.y, &left_line, area.width.saturating_sub(rw + 1));
    if rw < area.width {
        buf.set_line(area.x + area.width - rw, area.y, &right_line, rw);
    }
    let _ = UnicodeWidthStr::width("");
}
