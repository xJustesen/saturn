//! Bottom status line: mode, document, kernel, pending keys, flash messages.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::{App, Focus, Mode, Screen};
use crate::kernel::KernelStatus;
use crate::vim::Mode as VimMode;

const SPINNER: [&str; 4] = ["◐", "◓", "◑", "◒"];

/// `[3/12]`, `[12]` when the cursor is on none of them, `no match`.
fn search_count(app: &App) -> String {
    let n = app.search.matches.len();
    if app.search.query.is_empty() {
        String::new()
    } else if n == 0 {
        "no match".into()
    } else {
        match app.search.current {
            Some(k) => format!("[{}/{n}]", k + 1),
            None => format!("[{n}]"),
        }
    }
}

pub fn render(buf: &mut Buffer, area: Rect, app: &App) {
    let t = &app.theme;
    let ui = &t.ui;
    buf.set_style(area, Style::default().bg(ui.status_bg).fg(ui.status_fg));
    let editor = match app.mode {
        Mode::Edit => app.nb.cells.get(app.focused).and_then(|c| c.editor.as_ref()),
        Mode::Command => None,
    };
    let mode = |label: &'static str, bg: Color| (label, Style::default().bg(bg).fg(ui.mode_fg).add_modifier(Modifier::BOLD));
    let (mode_label, mode_style) = match app.mode {
        _ if app.focus == Focus::Chat && app.chat.is_some() => mode(" CHAT ", ui.mode_chat),
        Mode::Command if app.search.input.is_some() => mode(" SEARCH ", ui.mode_search),
        Mode::Command => mode(" COMMAND ", ui.mode_command),
        Mode::Edit => {
            match editor.map(|e| (e.mode(), e.vim.cmdline().map(|c| c.starts_with(':')))) {
                Some((_, Some(true))) => mode(" EX ", ui.mode_search),
                Some((_, Some(false))) => mode(" SEARCH ", ui.mode_search),
                Some((VimMode::Insert, _)) => mode(" INSERT ", ui.mode_insert),
                Some((VimMode::Replace, _)) => mode(" REPLACE ", ui.mode_insert),
                Some((VimMode::Visual, _)) => mode(" VISUAL ", ui.mode_visual),
                Some((VimMode::VisualLine, _)) => mode(" V-LINE ", ui.mode_visual),
                Some((VimMode::VisualBlock, _)) => mode(" V-BLOCK ", ui.mode_visual),
                _ => mode(" NORMAL ", ui.mode_normal),
            }
        }
    };
    if matches!(app.screen, Screen::Dashboard(_)) {
        let mut left = vec![Span::styled(" HOME ", mode(" HOME ", ui.mode_command).1), Span::raw(" ")];
        if let Some(f) = &app.flash {
            let st = if f.error { t.error() } else { Style::default().fg(ui.info) };
            left.push(Span::styled(f.text.clone(), st));
        }
        let right = Line::from(Span::styled(format!("  {} ", app.graphics_label), t.muted()));
        let rw = right.width() as u16;
        buf.set_line(area.x, area.y, &Line::from(left), area.width.saturating_sub(rw + 1));
        if rw < area.width {
            buf.set_line(area.x + area.width - rw, area.y, &right, rw);
        }
        return;
    }
    let mut left: Vec<Span> = vec![Span::styled(mode_label, mode_style), Span::raw(" ")];
    if let Some(inp) = &app.search.input {
        // The prompt takes the line, like vim's command line.
        left.push(Span::styled("/", Style::default().fg(ui.mode_search)));
        left.push(Span::raw(inp.text.clone()));
        left.push(Span::styled("▏", t.muted()));
        left.push(Span::styled(format!("  {}", search_count(app)), t.muted()));
        left.push(Span::styled("  Enter keep · Esc back · ^G/^T next/prev", t.muted()));
        buf.set_line(area.x, area.y, &Line::from(left), area.width);
        return;
    }
    if let Some(cmd) = editor.and_then(|e| e.vim.cmdline()) {
        // The editor's `/`, `?` or `:` prompt takes the line, like vim's.
        left.push(Span::raw(cmd));
        left.push(Span::styled("▏", t.muted()));
        buf.set_line(area.x, area.y, &Line::from(left), area.width);
        return;
    }
    let title = app.nb.title();
    left.push(Span::styled(title, Style::default().add_modifier(Modifier::BOLD)));
    if app.nb.dirty {
        left.push(Span::styled(" [+]", Style::default().fg(ui.warning)));
    }
    left.push(Span::styled(format!("  cell {}/{}", app.focused + 1, app.nb.cells.len()), t.muted()));
    if !app.pending.is_empty() {
        left.push(Span::styled(format!("  {}", crate::keymap::describe(&app.pending)), Style::default().fg(ui.warning)));
    }
    if let Some(ed) = editor {
        if let Some(reg) = ed.vim.recording() {
            left.push(Span::styled(format!("  recording @{reg}"), Style::default().fg(ui.warning)));
        }
        if !ed.vim.pending().is_empty() {
            left.push(Span::styled(format!("  {}", ed.vim.pending()), Style::default().fg(ui.warning)));
        }
    }
    if app.search.active() {
        left.push(Span::styled(format!("  /{} {}", app.search.query, search_count(app)), t.muted()));
    }
    if let Some(f) = &app.flash {
        let st = if f.error { t.error() } else { Style::default().fg(ui.info) };
        left.push(Span::raw("  "));
        left.push(Span::styled(f.text.clone(), st));
    }

    let mut right: Vec<Span> = Vec::new();
    match &app.kernel {
        Some(k) => {
            let (icon, st) = match k.status {
                KernelStatus::Idle => ("●", Style::default().fg(ui.success)),
                KernelStatus::Busy => (SPINNER[app.spinner % SPINNER.len()], Style::default().fg(ui.warning)),
                KernelStatus::Starting | KernelStatus::Restarting => (SPINNER[app.spinner % SPINNER.len()], Style::default().fg(ui.info)),
                KernelStatus::Dead => ("✖", t.error()),
            };
            right.push(Span::styled(k.display_name().to_string(), Style::default()));
            right.push(Span::raw(" "));
            right.push(Span::styled(icon, st));
            right.push(Span::styled(format!(" {}", k.status.label()), Style::default()));
        }
        None => right.push(Span::styled("no kernel (K to select)", t.muted())),
    }
    right.push(Span::styled(format!("  {} ", app.graphics_label), t.muted()));

    let left_line = Line::from(left);
    let right_line = Line::from(right);
    let rw = right_line.width() as u16;
    buf.set_line(area.x, area.y, &left_line, area.width.saturating_sub(rw + 1));
    if rw < area.width {
        buf.set_line(area.x + area.width - rw, area.y, &right_line, rw);
    }
}
