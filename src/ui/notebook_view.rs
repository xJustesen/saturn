//! Virtualised list of cells.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState, StatefulWidget};

use crate::app::{App, Mode};
use crate::ui::cell::{self, CellCtx};

pub fn render(buf: &mut Buffer, area: Rect, app: &mut App) {
    if area.width < 12 || area.height == 0 {
        return;
    }
    app.ensure_layout(area.width);
    app.clamp_scroll(area.height as usize);
    let n = app.nb.cells.len();
    if n == 0 {
        return;
    }
    let first = app.layout.cell_at_row(app.scroll_row);
    let mut top = app.layout.row_of(first) as i64 - app.scroll_row as i64;
    let editing = app.mode == Mode::Edit;
    let total_rows = app.layout.total_rows();
    let scroll_row = app.scroll_row;
    let focused = app.focused;
    let App { nb, layout, cache, images, plotly, cfg, theme, lang_ext, line_numbers, .. } = app;
    let mut ctx = CellCtx { cfg, theme, cache, images, plotly, lang_ext, line_numbers: *line_numbers };
    for i in first..n {
        let Some(l) = layout.layout(i) else { break };
        if top >= area.height as i64 {
            break;
        }
        let h = l.total as i64;
        if top + h > 0 {
            cell::render(&mut nb.cells[i], l, i == focused, editing && i == focused, area, top as i32, &mut ctx, buf);
        }
        top += h;
    }
    if total_rows > area.height as usize {
        let mut state = ScrollbarState::new(total_rows.saturating_sub(area.height as usize)).position(scroll_row);
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(None)
            .thumb_style(Style::default().fg(ctx.theme.ui.muted))
            .render(area, buf, &mut state);
    }
}
