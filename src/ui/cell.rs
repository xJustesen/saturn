//! Measuring and drawing a single cell (gutter, bar, source, outputs).

use edtui::{EditorTheme, EditorView, LineNumbers, SyntaxHighlighter};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use ratatui_image::sliced::{SignedPosition, SlicedImage};

use crate::config::Config;
use crate::editor::text_rows;
use crate::notebook::{Cell, CellKind, ExecState};
use crate::render::image::{ImageState, ImageStore};
use crate::render::plotly::PlotlySidecar;
use crate::render::{OutputCtx, RenderCache, Rendered};
use crate::ui::layout::CellLayout;
use crate::ui::text::expand_tabs;

pub const GUTTER_W: u16 = 7;
pub const BAR_W: u16 = 2;
pub const RIGHT_PAD: u16 = 1;

pub fn content_width(width: u16) -> u16 {
    width.saturating_sub(GUTTER_W + BAR_W + RIGHT_PAD).max(1)
}

pub struct CellCtx<'a> {
    pub cfg: &'a Config,
    pub cache: &'a mut RenderCache,
    pub images: &'a mut ImageStore,
    pub plotly: &'a mut PlotlySidecar,
    pub lang_ext: &'a str,
    pub line_numbers: bool,
}

fn digits(n: usize) -> usize {
    n.max(1).to_string().len()
}

fn editor_gutter(line_count: usize, line_numbers: bool) -> usize {
    if line_numbers { digits(line_count) + 1 } else { 0 }
}

pub fn measure(cell: &mut Cell, editing: bool, width: u16, ctx: &mut CellCtx) -> CellLayout {
    let cw = content_width(width);
    let tab = ctx.cfg.tab_width;
    let wrap = ctx.cfg.wrap;
    let source_rows = match (&cell.editor, editing) {
        (Some(ed), true) => {
            let g = editor_gutter(ed.line_count(), ctx.line_numbers);
            ed.rows((cw as usize).saturating_sub(g).max(1), tab, wrap) as u16
        }
        _ => match cell.kind {
            CellKind::Markdown => {
                if cell.source.trim().is_empty() {
                    1
                } else {
                    ctx.cache.markdown(&cell.id, &cell.source, cw, tab).lines.len().max(1) as u16
                }
            }
            // Heights come from the same character wrapping the renderer
            // uses, so no highlighting is needed to measure a cell.
            CellKind::Code | CellKind::Raw => text_rows(&expand_tabs(&cell.source, tab), cw as usize, tab, wrap) as u16,
        },
    };
    let mut outputs = Vec::new();
    let mut collapsed_rows = 0;
    if cell.kind == CellKind::Code && !cell.outputs.is_empty() {
        if cell.outputs_collapsed {
            collapsed_rows = 1;
        } else {
            let mut octx = OutputCtx {
                cfg: ctx.cfg,
                images: ctx.images,
                plotly: ctx.plotly,
                expanded: cell.outputs_expanded,
            };
            for (i, o) in cell.outputs.iter().enumerate() {
                outputs.push(ctx.cache.output(&cell.id, i, o, cw, &mut octx).rows());
            }
        }
    }
    let total = source_rows + outputs.iter().sum::<u16>() + collapsed_rows + 1;
    CellLayout { source_rows, outputs, collapsed_rows, total }
}

fn gutter_label(cell: &Cell) -> (String, Style) {
    match cell.kind {
        CellKind::Code => match cell.exec {
            ExecState::Running => ("[*]:".to_string(), Style::default().fg(Color::Yellow)),
            ExecState::Queued => ("[·]:".to_string(), Style::default().fg(Color::DarkGray)),
            _ => match cell.execution_count {
                Some(n) => (
                    format!("[{n}]:"),
                    if cell.exec == ExecState::Error {
                        Style::default().fg(Color::Red)
                    } else {
                        Style::default().fg(Color::DarkGray)
                    },
                ),
                None => ("[ ]:".to_string(), Style::default().fg(Color::DarkGray)),
            },
        },
        CellKind::Markdown => (String::new(), Style::default()),
        CellKind::Raw => ("raw".to_string(), Style::default().fg(Color::DarkGray)),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn render(
    cell: &mut Cell,
    layout: &CellLayout,
    focused: bool,
    editing: bool,
    area: Rect,
    top: i32,
    ctx: &mut CellCtx,
    buf: &mut Buffer,
) {
    let cw = content_width(area.width);
    let tab = ctx.cfg.tab_width;
    let wrap = ctx.cfg.wrap;
    let height = area.height as i32;
    let body_rows = (layout.total - 1) as i32;
    let bar_x = area.x + GUTTER_W;
    let cx = area.x + GUTTER_W + BAR_W;

    let bar_style = if focused && editing {
        Style::default().fg(Color::Green)
    } else if focused {
        Style::default().fg(Color::Blue)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let bar = if focused { "┃" } else { "│" };

    // gutter + bar for every visible body row
    for r in 0..body_rows {
        let y = top + r;
        if y < 0 || y >= height {
            continue;
        }
        let y = area.y + y as u16;
        let st = if r < layout.source_rows as i32 { bar_style } else { bar_style.add_modifier(Modifier::DIM) };
        buf.set_string(bar_x, y, bar, st);
    }
    if top >= 0 && top < height {
        let (label, mut st) = gutter_label(cell);
        if focused {
            st = st.add_modifier(Modifier::BOLD);
        }
        let pad = (GUTTER_W as usize).saturating_sub(1).saturating_sub(label.len());
        buf.set_string(area.x + pad as u16, area.y + top as u16, &label, st);
    }

    // source
    let src_rows = layout.source_rows as i32;
    match (&mut cell.editor, editing) {
        (Some(ed), true) => {
            let g = editor_gutter(ed.line_count(), ctx.line_numbers) as u16;
            let mut scratch = Buffer::empty(Rect::new(0, 0, cw, layout.source_rows));
            let theme = EditorTheme::default()
                .base(Style::default())
                .cursor_style(Style::default().add_modifier(Modifier::REVERSED))
                .selection_style(Style::default().bg(Color::Rgb(80, 80, 30)))
                .line_numbers_style(Style::default().fg(Color::DarkGray))
                .hide_status_line();
            let mut view = EditorView::new(&mut ed.state)
                .theme(theme)
                .wrap(wrap)
                .tab_width(tab)
                .line_numbers(if ctx.line_numbers { LineNumbers::Absolute } else { LineNumbers::None });
            let ext = if cell.kind == CellKind::Markdown { "md" } else { ctx.lang_ext };
            if let Ok(h) = SyntaxHighlighter::new(&ctx.cfg.theme, ext) {
                view = view.syntax_highlighter(Some(h));
            }
            let _ = g;
            view.render(scratch.area, &mut scratch);
            for sy in 0..layout.source_rows {
                let dy = top + sy as i32;
                if dy < 0 || dy >= height {
                    continue;
                }
                let y = area.y + dy as u16;
                for x in 0..cw {
                    if let (Some(src), Some(dst)) = (scratch.cell((x, sy)), buf.cell_mut((cx + x, y))) {
                        *dst = src.clone();
                    }
                }
            }
        }
        _ => {
            let lines: Vec<Line<'static>> = match cell.kind {
                CellKind::Markdown => {
                    if cell.source.trim().is_empty() {
                        vec![Line::from(Span::styled(
                            "(empty markdown cell)",
                            Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
                        ))]
                    } else {
                        ctx.cache.markdown(&cell.id, &cell.source, cw, tab).lines.clone()
                    }
                }
                CellKind::Code => ctx
                    .cache
                    .code(&cell.id, &cell.source, cw, ctx.lang_ext, &ctx.cfg.theme, tab, wrap)
                    .lines
                    .clone(),
                CellKind::Raw => crate::ui::text::wrap_text(
                    crate::ui::text::plain(&expand_tabs(&cell.source, tab), Style::default().fg(Color::Gray)),
                    cw as usize,
                    tab,
                    wrap,
                )
                .lines,
            };
            for (i, line) in lines.iter().enumerate().take(layout.source_rows as usize) {
                let dy = top + i as i32;
                if dy < 0 || dy >= height {
                    continue;
                }
                buf.set_line(cx, area.y + dy as u16, line, cw);
            }
        }
    }

    // outputs
    if cell.kind != CellKind::Code || cell.outputs.is_empty() {
        return;
    }
    let mut y0 = top + src_rows;
    if cell.outputs_collapsed {
        if y0 >= 0 && y0 < height {
            let msg = format!("▸ {} output(s) hidden (o to show)", cell.outputs.len());
            buf.set_string(cx, area.y + y0 as u16, msg, Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC));
        }
        return;
    }
    let mut octx = OutputCtx { cfg: ctx.cfg, images: ctx.images, plotly: ctx.plotly, expanded: cell.outputs_expanded };
    for (i, o) in cell.outputs.iter().enumerate() {
        let rows = layout.outputs.get(i).copied().unwrap_or(1) as i32;
        if y0 >= height {
            break;
        }
        if y0 + rows > 0 {
            let rendered = ctx.cache.output(&cell.id, i, o, cw, &mut octx);
            match rendered {
                Rendered::Text(t) | Rendered::Placeholder(t) => {
                    for (li, line) in t.lines.iter().enumerate().take(rows as usize) {
                        let dy = y0 + li as i32;
                        if dy < 0 || dy >= height {
                            continue;
                        }
                        let mut line = line.clone();
                        if line.style == Style::default() {
                            line.style = t.style;
                        } else {
                            line.style = t.style.patch(line.style);
                        }
                        buf.set_line(cx, area.y + dy as u16, &line, cw);
                    }
                }
                Rendered::Image { hash, .. } => match octx.images.get(*hash).map(|e| &e.state) {
                    Some(ImageState::Ready(proto)) => {
                        let pos = SignedPosition { x: (cx - area.x) as i16, y: y0.clamp(i16::MIN as i32, i16::MAX as i32) as i16 };
                        SlicedImage::new(proto, pos).render(area, buf);
                    }
                    Some(ImageState::Failed(e)) => {
                        if y0 >= 0 {
                            buf.set_string(cx, area.y + y0 as u16, format!("[image: {e}]"), Style::default().fg(Color::Red));
                        }
                    }
                    _ => {
                        if y0 >= 0 {
                            buf.set_string(cx, area.y + y0 as u16, "[image: decoding…]", Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC));
                        }
                    }
                },
            }
        }
        y0 += rows;
    }
}
