//! Drawing: notebook view, status bar and overlays.

pub mod cell;
pub mod cell_picker;
pub mod layout;
pub mod notebook_view;
pub mod overlay;
pub mod popout;
pub mod status_bar;
pub mod text;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};

use crate::app::App;
use crate::ui::overlay::Overlay;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let [main, status] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);
    app.view = main;
    let buf = frame.buffer_mut();
    buf.set_style(area, app.theme.base());
    // The popout covers the notebook; not drawing it underneath keeps inline
    // graphics (sixel/iTerm2 draw outside the cell grid) from bleeding through.
    if matches!(app.overlay, Some(Overlay::Popout(_))) {
        status_bar::render(buf, status, app);
        let App { nb, images, plotly, cfg, theme, overlay, .. } = app;
        let Some(Overlay::Popout(p)) = overlay else { return };
        let cell = nb.cells.get(p.cell).filter(|c| c.id == p.cell_id);
        popout::render(buf, main, p, cell, popout::PopoutCtx { cfg, theme, images, plotly });
        return;
    }
    notebook_view::render(buf, main, app);
    status_bar::render(buf, status, app);
    if let Some(ov) = &mut app.overlay {
        overlay::render(buf, area, ov, &app.theme);
    }
}
