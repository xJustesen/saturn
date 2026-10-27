//! Output popout: one output of a cell in a modal that fills the terminal,
//! with figures re-rendered at the popout's size.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Scrollbar, ScrollbarOrientation, ScrollbarState, StatefulWidget, Widget};
use ratatui_image::sliced::{SignedPosition, SlicedImage};

use crate::config::Config;
use crate::notebook::{Cell, CellId, Output};
use crate::render::image::{ImageState, ImageStore};
use crate::render::plotly::PlotlySidecar;
use crate::render::{self, Box as RenderBox, OutputCtx, Rendered};

/// Space around the modal.
const MARGIN_X: u16 = 2;
const MARGIN_Y: u16 = 1;

pub struct PopoutState {
    pub cell: usize,
    pub cell_id: CellId,
    pub output: usize,
    pub scroll: usize,
    pub hscroll: usize,
    pub wrap: bool,
    /// Rendered output for (output key, inner width, inner height).
    cached: Option<(u64, u16, u16, Rendered)>,
    /// Popout-sized image entries created in the [`ImageStore`]; dropped on close.
    image_keys: Vec<u64>,
    /// Rows of content and the viewport height at the last draw (for paging).
    content_rows: usize,
    view_rows: usize,
    view_cols: usize,
    content_cols: usize,
}

pub enum PopoutAction {
    None,
    Close,
}

impl PopoutState {
    pub fn new(cell: usize, cell_id: CellId, output: usize, wrap: bool) -> Self {
        PopoutState {
            cell,
            cell_id,
            output,
            scroll: 0,
            hscroll: 0,
            wrap,
            cached: None,
            image_keys: Vec::new(),
            content_rows: 0,
            view_rows: 1,
            view_cols: 1,
            content_cols: 0,
        }
    }

    /// The output to open first: the first figure, else the first output.
    pub fn default_output(cell: &Cell) -> usize {
        cell.outputs
            .iter()
            .position(|o| o.media().map(|m| render::richest(m).map(is_figure).unwrap_or(false)).unwrap_or(false))
            .unwrap_or(0)
    }

    pub fn handle_key(&mut self, key: KeyEvent, n_outputs: usize) -> PopoutAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let page = self.view_rows.saturating_sub(1).max(1);
        let half = (self.view_rows / 2).max(1);
        let max_scroll = self.content_rows.saturating_sub(self.view_rows);
        let max_h = self.content_cols.saturating_sub(self.view_cols);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('z') | KeyCode::Enter => return PopoutAction::Close,
            KeyCode::Char('j') | KeyCode::Down if !ctrl => self.scroll = (self.scroll + 1).min(max_scroll),
            KeyCode::Char('k') | KeyCode::Up if !ctrl => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Char('d') if ctrl => self.scroll = (self.scroll + half).min(max_scroll),
            KeyCode::Char('u') if ctrl => self.scroll = self.scroll.saturating_sub(half),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll = (self.scroll + page).min(max_scroll),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(page),
            KeyCode::Char('g') | KeyCode::Home => self.scroll = 0,
            KeyCode::Char('G') | KeyCode::End => self.scroll = max_scroll,
            KeyCode::Char('h') | KeyCode::Left if !ctrl => self.hscroll = self.hscroll.saturating_sub(8),
            KeyCode::Char('l') | KeyCode::Right if !ctrl => self.hscroll = (self.hscroll + 8).min(max_h),
            KeyCode::Char('0') => self.hscroll = 0,
            KeyCode::Char('$') => self.hscroll = max_h,
            KeyCode::Char('w') => {
                self.wrap = !self.wrap;
                self.hscroll = 0;
                self.cached = None;
            }
            KeyCode::Tab | KeyCode::Char('n') if n_outputs > 0 => self.select((self.output + 1) % n_outputs),
            KeyCode::BackTab | KeyCode::Char('p') if n_outputs > 0 => {
                self.select((self.output + n_outputs - 1) % n_outputs)
            }
            _ => {}
        }
        PopoutAction::None
    }

    fn select(&mut self, output: usize) {
        if output != self.output {
            self.output = output;
            self.scroll = 0;
            self.hscroll = 0;
            self.cached = None;
        }
    }

    /// Forget popout-sized images once the popout closes.
    pub fn close(self, images: &mut ImageStore) {
        for k in self.image_keys {
            images.remove(k);
        }
    }
}

fn is_figure(m: &jupyter_protocol::MediaType) -> bool {
    use jupyter_protocol::MediaType;
    matches!(m, MediaType::Png(_) | MediaType::Jpeg(_) | MediaType::Gif(_) | MediaType::Plotly(_))
}

fn output_label(o: &Output, plotly_ok: bool) -> String {
    use jupyter_protocol::MediaType;
    match o {
        Output::Stream { name, .. } => name.as_str().to_string(),
        Output::Error { ename, .. } => format!("error: {ename}"),
        Output::DisplayData { data, .. } | Output::ExecuteResult { data, .. } => {
            if plotly_ok && data.content.iter().any(|m| matches!(m, MediaType::Plotly(_))) {
                return "plotly figure".into();
            }
            render::richest(data).map(|m| m.mime_type().to_string()).unwrap_or_else(|| "display".into())
        }
    }
}

pub struct PopoutCtx<'a> {
    pub cfg: &'a Config,
    pub images: &'a mut ImageStore,
    pub plotly: &'a mut PlotlySidecar,
}

pub fn render(buf: &mut Buffer, area: Rect, p: &mut PopoutState, cell: Option<&Cell>, ctx: PopoutCtx) {
    let rect = Rect::new(
        area.x + MARGIN_X.min(area.width / 4),
        area.y + MARGIN_Y.min(area.height / 4),
        area.width.saturating_sub(2 * MARGIN_X.min(area.width / 4)),
        area.height.saturating_sub(2 * MARGIN_Y.min(area.height / 4)),
    );
    if rect.width < 4 || rect.height < 3 {
        return;
    }
    Clear.render(rect, buf);
    let (title, body) = match cell {
        Some(c) if !c.outputs.is_empty() => {
            p.output = p.output.min(c.outputs.len() - 1);
            let o = &c.outputs[p.output];
            let label = output_label(o, ctx.plotly.usable());
            let title = format!(" cell {} · output {}/{} · {} ", p.cell + 1, p.output + 1, c.outputs.len(), label);
            (title, Some(o))
        }
        _ => (format!(" cell {} · no outputs ", p.cell + 1), None),
    };
    let hint = " j/k scroll  h/l pan  w wrap  Tab next output  Esc close ";
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Blue))
        .title(Line::from(Span::styled(title, Style::default().add_modifier(Modifier::BOLD))))
        .title_bottom(Line::from(Span::styled(hint, Style::default().fg(Color::DarkGray))).right_aligned());
    let inner = block.inner(rect);
    block.render(rect, buf);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    p.view_rows = inner.height as usize;
    p.view_cols = inner.width as usize;
    let Some(o) = body else {
        p.content_rows = 0;
        buf.set_string(inner.x, inner.y, "(no outputs)", Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC));
        return;
    };

    // Render (or reuse) the output for this box. Placeholders for images and
    // plotly figures are re-checked every frame until they resolve.
    let key = render::output_key(o, true) ^ (p.wrap as u64);
    let stale = match &p.cached {
        Some((k, w, h, r)) => {
            *k != key || *w != inner.width || *h != inner.height || (matches!(r, Rendered::Placeholder(_)) && o.media().is_some())
        }
        None => true,
    };
    if stale {
        let mut cfg = ctx.cfg.clone();
        cfg.wrap = p.wrap;
        let mut octx = OutputCtx { cfg: &cfg, images: ctx.images, plotly: ctx.plotly, expanded: true };
        let bx = RenderBox { width: inner.width, max_rows: inner.height, popout: true };
        let rendered = render::render_output_in(o, bx, &mut octx);
        if let Rendered::Image { hash, .. } = &rendered
            && !p.image_keys.contains(hash)
        {
            p.image_keys.push(*hash);
        }
        p.cached = Some((key, inner.width, inner.height, rendered));
    }
    let Some((_, _, _, rendered)) = &p.cached else { return };

    match rendered {
        Rendered::Text(t) | Rendered::Placeholder(t) => {
            p.content_rows = t.lines.len();
            p.content_cols = t.lines.iter().map(|l| l.width()).max().unwrap_or(0);
            let max_scroll = p.content_rows.saturating_sub(p.view_rows);
            p.scroll = p.scroll.min(max_scroll);
            p.hscroll = p.hscroll.min(p.content_cols.saturating_sub(p.view_cols));
            let text_w = if p.content_rows > p.view_rows { inner.width.saturating_sub(1) } else { inner.width };
            for (row, line) in t.lines.iter().skip(p.scroll).take(p.view_rows).enumerate() {
                let mut line = line.clone();
                line.style = if line.style == Style::default() { t.style } else { t.style.patch(line.style) };
                let line = if p.hscroll > 0 { skip_cols(&line, p.hscroll) } else { line };
                buf.set_line(inner.x, inner.y + row as u16, &line, text_w);
            }
            if p.content_rows > p.view_rows {
                let mut st = ScrollbarState::new(max_scroll).position(p.scroll);
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None)
                    .thumb_style(Style::default().fg(Color::Gray))
                    .track_style(Style::default().fg(Color::DarkGray))
                    .render(inner, buf, &mut st);
            }
        }
        Rendered::Image { hash, cols, rows } => {
            p.content_rows = 0;
            p.content_cols = 0;
            p.scroll = 0;
            p.hscroll = 0;
            let x = inner.width.saturating_sub(*cols) / 2;
            let y = inner.height.saturating_sub(*rows) / 2;
            match ctx.images.get(*hash).map(|e| &e.state) {
                Some(ImageState::Ready(proto)) => {
                    let pos = SignedPosition { x: x as i16, y: y as i16 };
                    SlicedImage::new(proto, pos).render(inner, buf);
                }
                Some(ImageState::Failed(e)) => {
                    buf.set_string(inner.x, inner.y, format!("[image: {e}]"), Style::default().fg(Color::Red));
                }
                _ => {
                    buf.set_string(
                        inner.x,
                        inner.y,
                        "[image: decoding…]",
                        Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
                    );
                }
            }
        }
    }
}

/// Drop the first `n` columns of a line (horizontal scrolling).
fn skip_cols(line: &Line<'static>, n: usize) -> Line<'static> {
    use unicode_width::UnicodeWidthChar;
    let mut spans = Vec::new();
    let mut skip = n;
    for span in &line.spans {
        if skip == 0 {
            spans.push(span.clone());
            continue;
        }
        let mut rest = String::new();
        for ch in span.content.chars() {
            if skip > 0 {
                skip = skip.saturating_sub(UnicodeWidthChar::width(ch).unwrap_or(0));
                continue;
            }
            rest.push(ch);
        }
        if !rest.is_empty() {
            spans.push(Span::styled(rest, span.style));
        }
    }
    let mut out = Line::from(spans);
    out.style = line.style;
    out
}
