//! Turning cell outputs into things the UI can draw, with caching.

pub mod html;
pub mod image;
pub mod plotly;

use std::collections::HashMap;

use ansi_to_tui::IntoText;
use jupyter_protocol::{Media, MediaType};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Text};

use crate::config::Config;
use crate::notebook::{CellId, Output, StreamName};
use crate::ui::text::{expand_tabs, owned_text, plain, wrap_text};
use image::ImageStore;
use plotly::PlotlySidecar;

pub enum Rendered {
    /// Already wrapped to the target width: one line per row.
    Text(Text<'static>),
    Image { hash: u64, cols: u16, rows: u16 },
    Placeholder(Text<'static>),
}

impl Rendered {
    pub fn rows(&self) -> u16 {
        match self {
            Rendered::Text(t) | Rendered::Placeholder(t) => t.lines.len().max(1) as u16,
            Rendered::Image { rows, .. } => (*rows).max(1),
        }
    }
}

pub struct CachedOutput {
    pub key: u64,
    pub width: u16,
    pub rendered: Rendered,
}

#[derive(Default)]
pub struct RenderCache {
    outputs: HashMap<(CellId, usize), CachedOutput>,
    markdown: HashMap<CellId, (u64, u16, Text<'static>)>,
    code: HashMap<CellId, (u64, u16, Text<'static>)>,
}

pub struct OutputCtx<'a> {
    pub cfg: &'a Config,
    pub images: &'a mut ImageStore,
    pub plotly: &'a mut PlotlySidecar,
    /// Show the whole stream even if longer than `stream_max_lines`.
    pub expanded: bool,
}

pub fn hash_bytes(b: &[u8]) -> u64 {
    let h = blake3::hash(b);
    u64::from_le_bytes(h.as_bytes()[..8].try_into().unwrap())
}

/// Hash a JSON value as its serialization, without building the string
/// (plotly figures can run to megabytes and are hashed every frame while
/// they render).
pub fn hash_json(v: &serde_json::Value) -> u64 {
    let mut h = blake3::Hasher::new();
    hash_json_into(&mut h, v);
    u64::from_le_bytes(h.finalize().as_bytes()[..8].try_into().unwrap())
}

fn hash_json_into(h: &mut blake3::Hasher, v: &serde_json::Value) {
    // Writing into the hasher cannot fail.
    let _ = serde_json::to_writer(h, v);
}

impl RenderCache {
    pub fn invalidate_cell(&mut self, id: &CellId) {
        self.outputs.retain(|(cid, _), _| cid != id);
        self.markdown.remove(id);
        self.code.remove(id);
    }

    pub fn invalidate_outputs(&mut self, id: &CellId) {
        self.outputs.retain(|(cid, _), _| cid != id);
    }

    pub fn invalidate_all(&mut self) {
        self.outputs.clear();
        self.markdown.clear();
        self.code.clear();
    }

    pub fn markdown(&mut self, id: &CellId, source: &str, width: u16, tab_width: usize) -> &Text<'static> {
        let key = hash_bytes(source.as_bytes());
        let stale = match self.markdown.get(id) {
            Some((k, w, _)) => *k != key || *w != width,
            None => true,
        };
        if stale {
            let text = render_markdown(source, width, tab_width);
            self.markdown.insert(id.clone(), (key, width, text));
        }
        &self.markdown.get(id).unwrap().2
    }

    #[allow(clippy::too_many_arguments)]
    pub fn code(
        &mut self,
        id: &CellId,
        source: &str,
        width: u16,
        ext: &str,
        theme: &str,
        tab_width: usize,
        wrap: bool,
    ) -> &Text<'static> {
        let mut hasher = blake3::Hasher::new();
        hasher.update(source.as_bytes());
        hasher.update(ext.as_bytes());
        hasher.update(&[wrap as u8]);
        let key = u64::from_le_bytes(hasher.finalize().as_bytes()[..8].try_into().unwrap());
        let stale = match self.code.get(id) {
            Some((k, w, _)) => *k != key || *w != width,
            None => true,
        };
        if stale {
            let text = crate::ui::text::highlight(source, ext, theme, tab_width);
            let text = wrap_text(text, width as usize, tab_width, wrap);
            self.code.insert(id.clone(), (key, width, text));
        }
        &self.code.get(id).unwrap().2
    }

    pub fn output(
        &mut self,
        id: &CellId,
        idx: usize,
        output: &Output,
        width: u16,
        ctx: &mut OutputCtx,
    ) -> &Rendered {
        let key = output_key(output, ctx.expanded);
        let k = (id.clone(), idx);
        let stale = match self.outputs.get(&k) {
            Some(c) => c.key != key || c.width != width,
            None => true,
        };
        // Pending images/plotly figures need re-checking until they resolve.
        let pending = matches!(
            self.outputs.get(&k),
            Some(CachedOutput { rendered: Rendered::Placeholder(_), .. })
        ) && output.media().is_some();
        if stale || pending {
            let rendered = render_output(output, width, ctx);
            self.outputs.insert(k.clone(), CachedOutput { key, width, rendered });
        }
        &self.outputs.get(&k).unwrap().rendered
    }
}

pub fn output_key(o: &Output, expanded: bool) -> u64 {
    let mut h = blake3::Hasher::new();
    h.update(&[expanded as u8]);
    match o {
        Output::Stream { name, text } => {
            h.update(name.as_str().as_bytes());
            h.update(text.as_bytes());
        }
        Output::Error { ename, evalue, traceback } => {
            h.update(ename.as_bytes());
            h.update(evalue.as_bytes());
            for t in traceback {
                h.update(t.as_bytes());
            }
        }
        Output::DisplayData { data, .. } | Output::ExecuteResult { data, .. } => {
            for m in &data.content {
                h.update(m.mime_type().as_bytes());
                match m {
                    MediaType::Plain(s)
                    | MediaType::Html(s)
                    | MediaType::Latex(s)
                    | MediaType::Javascript(s)
                    | MediaType::Markdown(s)
                    | MediaType::Svg(s)
                    | MediaType::Png(s)
                    | MediaType::Jpeg(s)
                    | MediaType::Gif(s) => {
                        h.update(s.as_bytes());
                    }
                    MediaType::Json(v) | MediaType::Plotly(v) => hash_json_into(&mut h, v),
                    MediaType::Other((_, v)) => hash_json_into(&mut h, v),
                    _ => {}
                }
            }
        }
    }
    u64::from_le_bytes(h.finalize().as_bytes()[..8].try_into().unwrap())
}

fn placeholder(msg: impl Into<String>) -> Rendered {
    let mut t = Text::from(Line::from(msg.into()));
    t.style = Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC);
    Rendered::Placeholder(t)
}

fn ansi_text(s: &str) -> Text<'static> {
    match s.as_bytes().into_text() {
        Ok(t) => t,
        Err(_) => plain(s, Style::default()),
    }
}

fn truncate_stream(mut text: Text<'static>, max: usize, expanded: bool) -> Text<'static> {
    if expanded || text.lines.len() <= max {
        return text;
    }
    let hidden = text.lines.len() - max;
    text.lines.truncate(max);
    let mut l = Line::from(format!("… {hidden} more lines (press o to expand)"));
    l.style = Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC);
    text.lines.push(l);
    text
}

/// Pick the mimetype we can best present.
pub fn richest(media: &Media) -> Option<&MediaType> {
    media.richest(|m| match m {
        MediaType::Png(_) => 100,
        MediaType::Jpeg(_) => 95,
        MediaType::Gif(_) => 90,
        MediaType::Plotly(_) => 85,
        MediaType::Markdown(_) => 70,
        MediaType::Plain(_) => 60,
        MediaType::Html(_) => 40,
        MediaType::Latex(_) => 35,
        MediaType::Json(_) => 30,
        MediaType::Svg(_) => 20,
        MediaType::Other(_) => 10,
        _ => 5,
    })
}

fn plain_alt(media: &Media) -> Option<&str> {
    media.content.iter().find_map(|m| match m {
        MediaType::Plain(s) => Some(s.as_str()),
        _ => None,
    })
}

/// How much room an output may take.
#[derive(Clone, Copy)]
pub struct Box {
    pub width: u16,
    /// Tallest image, in rows.
    pub max_rows: u16,
    /// Popout rendering: fill the box (images are enlarged, plotly figures are
    /// re-rendered at the box's pixel size) and never truncate text.
    pub popout: bool,
}

fn render_output(o: &Output, width: u16, ctx: &mut OutputCtx) -> Rendered {
    let max_rows = if ctx.images.is_halfblocks() {
        ctx.cfg.max_image_rows_halfblocks
    } else {
        ctx.cfg.max_image_rows
    };
    render_output_in(o, Box { width, max_rows, popout: false }, ctx)
}

pub fn render_output_in(o: &Output, bx: Box, ctx: &mut OutputCtx) -> Rendered {
    let tab = ctx.cfg.tab_width;
    let wrap = ctx.cfg.wrap;
    let width = bx.width;
    let w = width as usize;
    match o {
        Output::Stream { name, text } => {
            let mut t = ansi_text(&expand_tabs(text.trim_end_matches('\n'), tab));
            if *name == StreamName::Stderr {
                t.style = Style::default().fg(Color::LightRed);
            }
            let t = truncate_stream(t, ctx.cfg.stream_max_lines, ctx.expanded);
            Rendered::Text(wrap_text(t, w, tab, wrap))
        }
        Output::Error { ename, evalue, traceback } => {
            let raw = if traceback.is_empty() {
                format!("{ename}: {evalue}")
            } else {
                traceback.join("\n")
            };
            let t = ansi_text(&expand_tabs(&raw, tab));
            let t = truncate_stream(t, ctx.cfg.stream_max_lines, ctx.expanded);
            Rendered::Text(wrap_text(t, w, tab, wrap))
        }
        Output::DisplayData { data, .. } | Output::ExecuteResult { data, .. } => render_media(data, bx, ctx),
    }
}

/// Key under which the popout stores an image rendered for a `cols`×`rows` box.
pub fn sized_key(hash: u64, cols: u16, rows: u16) -> u64 {
    let mut h = blake3::Hasher::new();
    h.update(&hash.to_le_bytes());
    h.update(&cols.to_le_bytes());
    h.update(&rows.to_le_bytes());
    u64::from_le_bytes(h.finalize().as_bytes()[..8].try_into().unwrap())
}

fn render_media(data: &Media, bx: Box, ctx: &mut OutputCtx) -> Rendered {
    let tab = ctx.cfg.tab_width;
    let wrap = ctx.cfg.wrap;
    let Box { width, max_rows, popout } = bx;
    let w = width as usize;
    // Figures are sized to the box the protocol can actually draw.
    let (width, max_rows) = ctx.images.clamp_box(width, max_rows);
    let Some(mut m) = richest(data) else {
        return placeholder("[no displayable output]");
    };
    // In the popout a plotly figure is re-rendered at the popout's own pixel
    // size, which beats any (small) PNG that came with it.
    if popout && ctx.plotly.usable()
        && let Some(fig) = data.content.iter().find(|m| matches!(m, MediaType::Plotly(_)))
    {
        m = fig;
    }
    let store_image = |images: &mut ImageStore, hash: u64, bytes: Vec<u8>| -> Option<(u64, u16, u16)> {
        if popout {
            let key = sized_key(hash, width, max_rows);
            images.ensure_fit(key, bytes, width, max_rows, true).map(|(c, r)| (key, c, r))
        } else {
            images.ensure_bytes(hash, bytes, width, max_rows).map(|(c, r)| (hash, c, r))
        }
    };
    match m {
        MediaType::Png(b64) | MediaType::Jpeg(b64) | MediaType::Gif(b64) => {
            let hash = hash_bytes(b64.as_bytes());
            if !popout && let Some(e) = ctx.images.get(hash) {
                return Rendered::Image { hash, cols: e.cols, rows: e.rows };
            }
            let cleaned: String = b64.chars().filter(|c| !c.is_whitespace()).collect();
            let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, cleaned.as_bytes()).ok();
            match bytes.and_then(|b| store_image(ctx.images, hash, b)) {
                Some((hash, cols, rows)) => Rendered::Image { hash, cols, rows },
                None => placeholder("[image: cannot decode]"),
            }
        }
        MediaType::Plotly(fig) => {
            let fig_hash = hash_json(fig);
            let fs = ctx.images.font_size();
            let (hash, px_w, px_h) = if popout {
                let px_w = u32::from(width) * u32::from(fs.width.max(1));
                let px_h = u32::from(max_rows) * u32::from(fs.height.max(1));
                (sized_key(fig_hash, width, max_rows), px_w, px_h)
            } else {
                let px_w = u32::from(width.min(120)) * u32::from(fs.width.max(1));
                let px_h = u32::from(max_rows) * u32::from(fs.height.max(1));
                (fig_hash, px_w, px_h)
            };
            match ctx.plotly.request(hash, fig, px_w, px_h) {
                plotly::PlotlyState::Ready(png) => {
                    let img_hash = hash_bytes(&png);
                    match store_image(ctx.images, img_hash, png.to_vec()) {
                        Some((hash, cols, rows)) => Rendered::Image { hash, cols, rows },
                        None => placeholder("[plotly: bad png from sidecar]"),
                    }
                }
                plotly::PlotlyState::Pending => placeholder("[plotly figure: rendering…]"),
                plotly::PlotlyState::Failed(e) => {
                    placeholder(format!("[plotly figure: {e} — press o to open in browser]"))
                }
            }
        }
        MediaType::Markdown(s) => Rendered::Text(render_markdown(s, width, tab)),
        MediaType::Plain(s) => {
            let t = ansi_text(&expand_tabs(s.trim_end_matches('\n'), tab));
            let t = truncate_stream(t, ctx.cfg.stream_max_lines, ctx.expanded);
            Rendered::Text(wrap_text(t, w, tab, wrap))
        }
        MediaType::Html(s) => {
            let text = match plain_alt(data) {
                Some(p) => expand_tabs(p.trim_end_matches('\n'), tab),
                None => html::to_text(s),
            };
            let t = truncate_stream(plain(&text, Style::default()), ctx.cfg.stream_max_lines, ctx.expanded);
            Rendered::Text(wrap_text(t, w, tab, wrap))
        }
        MediaType::Latex(s) => Rendered::Text(wrap_text(plain(s, Style::default()), w, tab, wrap)),
        MediaType::Json(v) => {
            let s = serde_json::to_string_pretty(v).unwrap_or_default();
            let t = truncate_stream(plain(&s, Style::default()), ctx.cfg.stream_max_lines, ctx.expanded);
            Rendered::Text(wrap_text(t, w, tab, wrap))
        }
        MediaType::Svg(_) => match plain_alt(data) {
            Some(p) => Rendered::Text(wrap_text(plain(p, Style::default()), w, tab, wrap)),
            None => placeholder("[svg image]"),
        },
        other => match plain_alt(data) {
            Some(p) => Rendered::Text(wrap_text(plain(p, Style::default()), w, tab, wrap)),
            None => placeholder(format!("[{}]", other.mime_type())),
        },
    }
}

pub fn render_markdown(source: &str, width: u16, tab_width: usize) -> Text<'static> {
    let source = expand_tabs(source, tab_width);
    let text = owned_text(tui_markdown::from_str(&source));
    wrap_text(text, width as usize, tab_width, true)
}
