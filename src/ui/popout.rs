//! Output popout: one output of a cell in a modal that fills the terminal,
//! with figures re-rendered at the popout's size.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Scrollbar, ScrollbarOrientation, ScrollbarState, StatefulWidget, Widget};
use ratatui_image::FontSize;
use ratatui_image::sliced::{SignedPosition, SlicedImage};

use crate::config::Config;
use crate::notebook::{Cell, CellId, Output};
use crate::render::image::{ImageState, ImageStore};
use crate::render::plotly::{LiveInput, PlotlySidecar};
use crate::render::{self, Box as RenderBox, OutputCtx, Rendered};
use crate::theme::Theme;

/// Space around the modal.
const MARGIN_X: u16 = 2;
const MARGIN_Y: u16 = 1;
/// Page pixels scrolled per wheel notch in a live figure.
const WHEEL_STEP: f64 = 100.0;

/// An interactive plotly figure: a page in the sidecar's browser that mouse
/// input is forwarded to, shown as a stream of frames.
struct Live {
    sid: u64,
    /// Figure and box this session was opened for.
    key: (u64, u16, u16),
    cols: u16,
    rows: u16,
    /// Where the figure is on screen, in cells.
    rect: Rect,
    /// Image-store keys of the frame on screen and of the one being decoded.
    shown: Option<u64>,
    next: Option<u64>,
    frames: u64,
    last_input: Option<LiveInput>,
}

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
    live: Option<Live>,
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
            live: None,
        }
    }

    /// Whether an interactive figure is showing (and wants mouse input).
    pub fn is_live(&self) -> bool {
        self.live.is_some()
    }

    /// Whether `key` is one of the live figure's frames in the [`ImageStore`].
    pub fn owns_frame(&self, key: u64) -> bool {
        self.live.as_ref().is_some_and(|l| l.shown == Some(key) || l.next == Some(key))
    }

    /// A new frame of the live figure arrived from the sidecar.
    pub fn live_frame(&mut self, sid: u64, png: Vec<u8>, images: &mut ImageStore) {
        let Some(live) = self.live.as_mut().filter(|l| l.sid == sid) else { return };
        live.frames += 1;
        let key = render::sized_key(sid ^ 0x6c69_7665_6672_616d, live.frames as u16, (live.frames >> 16) as u16);
        // Only the newest frame is worth decoding.
        if let Some(stale) = live.next.replace(key) {
            images.remove(stale);
        }
        images.ensure_frame(key, png, live.cols, live.rows);
    }

    /// Forward a mouse event to the live figure. With `pixels`, the event's
    /// position is in pixels (SGR-pixel mouse reporting) rather than cells.
    pub fn handle_mouse(&mut self, ev: MouseEvent, font: FontSize, pixels: bool, plotly: &PlotlySidecar) {
        let Some(live) = self.live.as_mut() else { return };
        let (fw, fh) = (f64::from(font.width.max(1)), f64::from(font.height.max(1)));
        let (x0, y0) = (f64::from(live.rect.x) * fw, f64::from(live.rect.y) * fh);
        let (x, y) = if pixels {
            (f64::from(ev.column) - x0, f64::from(ev.row) - y0)
        } else {
            ((f64::from(ev.column) + 0.5) * fw - x0, (f64::from(ev.row) + 0.5) * fh - y0)
        };
        let inside = x >= 0.0 && y >= 0.0 && x < f64::from(live.cols) * fw && y < f64::from(live.rows) * fh;
        let button = |b: MouseButton| match b {
            MouseButton::Left => "left",
            MouseButton::Right => "right",
            MouseButton::Middle => "middle",
        };
        let wheel = |dx: f64, dy: f64| LiveInput::Wheel { x, y, dx, dy };
        let input = match ev.kind {
            MouseEventKind::Moved | MouseEventKind::Drag(_) => LiveInput::Move { x, y },
            MouseEventKind::Down(b) if inside => LiveInput::Down { x, y, button: button(b) },
            MouseEventKind::Up(b) => LiveInput::Up { x, y, button: button(b) },
            MouseEventKind::ScrollUp if inside => wheel(0.0, -WHEEL_STEP),
            MouseEventKind::ScrollDown if inside => wheel(0.0, WHEEL_STEP),
            MouseEventKind::ScrollLeft if inside => wheel(-WHEEL_STEP, 0.0),
            MouseEventKind::ScrollRight if inside => wheel(WHEEL_STEP, 0.0),
            _ => return,
        };
        // Any-motion reporting repeats positions; the page only needs changes.
        if matches!(input, LiveInput::Move { .. }) && live.last_input == Some(input) {
            return;
        }
        live.last_input = Some(input);
        plotly.live_input(live.sid, input);
    }

    fn end_live(&mut self, images: &mut ImageStore, plotly: &mut PlotlySidecar) {
        if let Some(live) = self.live.take() {
            plotly.live_close();
            for k in live.shown.into_iter().chain(live.next) {
                images.remove(k);
            }
        }
    }

    /// Keep a live session open for `fig` drawn in `inner`; returns true when
    /// one of its frames was drawn (so the static render is not needed).
    fn draw_live(
        &mut self,
        buf: &mut Buffer,
        inner: Rect,
        fig: &serde_json::Value,
        fig_key: u64,
        images: &mut ImageStore,
        plotly: &mut PlotlySidecar,
    ) -> bool {
        let (cols, rows) = images.clamp_box(inner.width, inner.height);
        let key = (fig_key, cols, rows);
        if self.live.as_ref().is_some_and(|l| l.key != key) {
            self.end_live(images, plotly);
        }
        if self.live.is_none() {
            let font = images.font_size();
            let (w, h) = (u32::from(cols) * u32::from(font.width.max(1)), u32::from(rows) * u32::from(font.height.max(1)));
            let Some(sid) = plotly.live_open(fig, w, h) else { return false };
            self.live =
                Some(Live { sid, key, cols, rows, rect: Rect::default(), shown: None, next: None, frames: 0, last_input: None });
        }
        let Some(live) = self.live.as_mut() else { return false };
        live.rect = Rect::new(inner.x + (inner.width - cols) / 2, inner.y + (inner.height - rows) / 2, cols, rows);
        // Swap in the newest frame once it is decoded, so there is no gap.
        if let Some(next) = live.next
            && let Some(e) = images.get(next)
            && !matches!(e.state, ImageState::Pending)
        {
            if matches!(e.state, ImageState::Ready(_)) {
                if let Some(old) = live.shown.replace(next) {
                    images.remove(old);
                }
            } else {
                images.remove(next);
            }
            live.next = None;
        }
        let Some(ImageState::Ready(proto)) = live.shown.and_then(|k| images.get(k)).map(|e| &e.state) else {
            return false;
        };
        let pos = SignedPosition { x: (live.rect.x - inner.x) as i16, y: (live.rect.y - inner.y) as i16 };
        SlicedImage::new(proto, pos).render(inner, buf);
        true
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
    pub fn close(mut self, images: &mut ImageStore, plotly: &mut PlotlySidecar) {
        self.end_live(images, plotly);
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
    pub theme: &'a Theme,
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
    // A plotly figure is interactive while the sidecar can serve it.
    let live_fig = body
        .and_then(|o| o.media())
        .filter(|_| ctx.plotly.usable())
        .and_then(|data| data.content.iter().find_map(|m| match m {
            jupyter_protocol::MediaType::Plotly(v) => Some(v),
            _ => None,
        }));
    let hint = if live_fig.is_some() {
        " mouse: hover · click · drag to zoom · double-click resets  Tab next output  Esc close "
    } else {
        " j/k scroll  h/l pan  w wrap  Tab next output  Esc close "
    };
    let theme = ctx.theme;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .style(theme.float())
        .border_style(theme.border())
        .title(Line::from(Span::styled(title, Style::default().add_modifier(Modifier::BOLD))))
        .title_bottom(Line::from(Span::styled(hint, theme.muted())).right_aligned());
    let inner = block.inner(rect);
    block.render(rect, buf);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    p.view_rows = inner.height as usize;
    p.view_cols = inner.width as usize;
    let Some(o) = body else {
        p.end_live(ctx.images, ctx.plotly);
        p.content_rows = 0;
        buf.set_string(inner.x, inner.y, "(no outputs)", theme.placeholder());
        return;
    };
    match live_fig {
        Some(fig) => {
            let fig_key = render::output_key(o, true);
            if p.draw_live(buf, inner, fig, fig_key, ctx.images, ctx.plotly) {
                p.content_rows = 0;
                p.content_cols = 0;
                return;
            }
        }
        None => p.end_live(ctx.images, ctx.plotly),
    }

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
        let mut octx = OutputCtx { cfg: &cfg, theme, images: ctx.images, plotly: ctx.plotly, expanded: true };
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
                    .thumb_style(Style::default().fg(theme.ui.foreground))
                    .track_style(theme.muted())
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
                    buf.set_string(inner.x, inner.y, format!("[image: {e}]"), theme.error());
                }
                _ => {
                    buf.set_string(inner.x, inner.y, "[image: decoding…]", theme.placeholder());
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
