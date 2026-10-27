//! Drawing: notebook view, status bar and overlays.

pub mod cell;
pub mod layout;
pub mod notebook_view;
pub mod overlay;
pub mod status_bar;
pub mod text;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};

use crate::app::App;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let [main, status] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);
    app.view = main;
    let buf = frame.buffer_mut();
    notebook_view::render(buf, main, app);
    status_bar::render(buf, status, app);
    if let Some(ov) = &mut app.overlay {
        overlay::render(buf, area, ov);
    }
}
