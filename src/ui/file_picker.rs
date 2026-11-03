//! The files picker (`Ctrl+O`): every file under the root in one fuzzy list on
//! the left, and a preview of the selected file on the right. A scanner and a
//! preview worker (see [`crate::files`]) feed it events; this module only
//! holds state, handles keys and draws.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use edtui::syntect::highlighting::Theme as SynTheme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Widget};
use tracing::debug;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::files::preview::{Preview, PreviewRequest, PreviewWorker, UiColors};
use crate::files::walk::{GlobalExcludes, ScanRequest, spawn_scan};
use crate::files::{
    FileEntry, FilesEvent, PickerPrefs, ScanLimits, ScanSummary, Sink, next_id, shorten_path,
};
use crate::theme::Theme;
use crate::ui::fuzzy::Fuzzy;
use crate::ui::icons::icon;
use crate::ui::text::{clean_name, cut, cut_left};

const NOTICE_FOR: Duration = Duration::from_secs(3);
const ONLY_NOTEBOOKS: &str = "saturn opens only .ipynb files";
const UNSAVED_NOTE: &str = "on-disk version — unsaved edits not shown";

/// How the picker reaches its workers; tests record the calls instead.
pub trait PickerIo {
    fn scan(&self, req: ScanRequest);
    fn preview(&self, req: PreviewRequest);
}

/// The app's workers: a scan thread per scan, one preview thread per picker.
pub struct ThreadIo {
    pub sink: Sink,
    pub worker: PreviewWorker,
}

impl PickerIo for ThreadIo {
    fn scan(&self, req: ScanRequest) {
        spawn_scan(req, self.sink.clone());
    }

    fn preview(&self, req: PreviewRequest) {
        self.worker.request(req);
    }
}

#[cfg(test)]
#[derive(Debug)]
pub enum IoCall {
    Scan(ScanRequest),
    Preview(PreviewRequest),
}

#[cfg(test)]
pub struct RecordIo {
    pub calls: std::rc::Rc<std::cell::RefCell<Vec<IoCall>>>,
}

#[cfg(test)]
impl PickerIo for RecordIo {
    fn scan(&self, req: ScanRequest) {
        self.calls.borrow_mut().push(IoCall::Scan(req));
    }

    fn preview(&self, req: PreviewRequest) {
        self.calls.borrow_mut().push(IoCall::Preview(req));
    }
}

/// What the picker needs from the app, fixed for its lifetime.
pub struct PickerCtx {
    pub home: Option<PathBuf>,
    pub syn: Arc<SynTheme>,
    pub ui: UiColors,
    pub tab_width: usize,
    pub icons: bool,
}

pub enum FileAction {
    None,
    Close,
    Open(PathBuf),
    PrefsChanged(PickerPrefs),
}

pub struct FilePickerState {
    // scan
    io: Box<dyn PickerIo>,
    pub start: PathBuf,
    root: Option<PathBuf>,
    open_path: Option<PathBuf>,
    open_dirty: bool,
    scan_id: u64,
    cancel: Arc<AtomicBool>,
    scanning: bool,
    summary: Option<ScanSummary>,
    scan_failed: bool,
    // list
    entries: Vec<FileEntry>,
    filtered: Vec<u32>,
    filtered_stale: bool,
    selected: Option<u32>,
    user_moved: bool,
    preselect: Option<PathBuf>,
    list_top: usize,
    list_rows: u16,
    // query
    query: String,
    fuzzy: Fuzzy,
    prefs: PickerPrefs,
    // preview
    last_preview_id: u64,
    last_req: Option<(u32, u16)>,
    was_visible: bool,
    current: Option<Preview>,
    current_is_stale: bool,
    preview_scroll: u16,
    preview_visible: bool,
    preview_width: u16,
    preview_rows: u16,
    // other
    notice: Option<(String, Instant)>,
    stale_drops: u32,
    ctx: PickerCtx,
}

impl FilePickerState {
    pub fn new(
        start: PathBuf,
        open_path: Option<PathBuf>,
        open_dirty: bool,
        preselect: Option<PathBuf>,
        prefs: PickerPrefs,
        ctx: PickerCtx,
        io: Box<dyn PickerIo>,
    ) -> Self {
        let p = FilePickerState {
            io,
            start,
            root: None,
            open_path,
            open_dirty,
            scan_id: next_id(),
            cancel: Arc::new(AtomicBool::new(false)),
            scanning: true,
            summary: None,
            scan_failed: false,
            entries: Vec::new(),
            filtered: Vec::new(),
            filtered_stale: true,
            selected: None,
            user_moved: false,
            preselect,
            list_top: 0,
            list_rows: 1,
            query: String::new(),
            fuzzy: Fuzzy::new(true),
            prefs,
            last_preview_id: 0,
            last_req: None,
            was_visible: false,
            current: None,
            current_is_stale: false,
            preview_scroll: 0,
            preview_visible: false,
            preview_width: 0,
            preview_rows: 0,
            notice: None,
            stale_drops: 0,
            ctx,
        };
        p.send_scan();
        p
    }

    fn send_scan(&self) {
        self.io.scan(ScanRequest {
            scan_id: self.scan_id,
            start: self.start.clone(),
            open_path: self.open_path.clone(),
            prefs: self.prefs,
            limits: ScanLimits::default(),
            home: self.ctx.home.clone(),
            global: GlobalExcludes::Load,
            cancel: self.cancel.clone(),
        });
    }

    fn visible(&self, e: &FileEntry) -> bool {
        self.prefs.show_ignored || !e.ignored || e.notebook
    }

    fn visible_total(&self) -> usize {
        self.entries.iter().filter(|e| self.visible(e)).count()
    }

    /// Rebuild `filtered` and apply the selection rule.
    fn refilter(&mut self) {
        let prefs = self.prefs;
        let visible = |e: &FileEntry| prefs.show_ignored || !e.ignored || e.notebook;
        self.filtered = if self.query.is_empty() {
            (0..self.entries.len() as u32)
                .filter(|&i| visible(&self.entries[i as usize]))
                .collect()
        } else {
            let items = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| visible(e))
                .map(|(i, e)| (i as u32, e.hay.slice(..), e.display.len() as u32));
            self.fuzzy.rank(items)
        };
        self.filtered_stale = false;

        let find = |path: &Option<PathBuf>| {
            let path = path.as_deref()?;
            self.filtered
                .iter()
                .copied()
                .find(|&i| self.entries[i as usize].path == path)
        };
        self.selected =
            if self.user_moved && self.selected.is_some_and(|s| self.filtered.contains(&s)) {
                self.selected
            } else if let Some(i) = self
                .query
                .is_empty()
                .then(|| find(&self.preselect))
                .flatten()
            {
                Some(i)
            } else if let Some(i) = self
                .query
                .is_empty()
                .then(|| find(&self.open_path))
                .flatten()
            {
                Some(i)
            } else {
                self.filtered.first().copied()
            };
    }

    fn ensure_filtered(&mut self) {
        if self.filtered_stale {
            self.refilter();
        }
    }

    fn selected_entry(&self) -> Option<&FileEntry> {
        self.selected.map(|i| &self.entries[i as usize])
    }

    pub fn on_event(&mut self, ev: FilesEvent) -> bool {
        let stale = match &ev {
            FilesEvent::Root { scan_id, .. }
            | FilesEvent::Batch { scan_id, .. }
            | FilesEvent::Done { scan_id, .. } => *scan_id != self.scan_id,
            FilesEvent::Preview { id, .. } => *id != self.last_preview_id,
        };
        if stale {
            self.stale_drops += 1;
            return false;
        }
        match ev {
            FilesEvent::Root {
                root, open_path, ..
            } => {
                self.root = Some(root);
                self.open_path = open_path;
                self.filtered_stale = true;
            }
            FilesEvent::Batch { entries, .. } => {
                self.entries.extend(entries);
                self.filtered_stale = true;
            }
            FilesEvent::Done { summary, .. } => {
                // Every scan that ran sent `Root` first; a failed spawn did not.
                self.scan_failed = self.root.is_none();
                self.summary = Some(summary);
                self.scanning = false;
            }
            FilesEvent::Preview { preview, .. } => {
                self.current = Some(preview);
                self.current_is_stale = false;
            }
        }
        true
    }

    fn query_edited(&mut self) {
        self.fuzzy.set_query(&self.query);
        self.filtered_stale = true;
        self.user_moved = false;
    }

    /// Append pasted text to the query.
    pub fn push_query(&mut self, s: &str) {
        self.query.push_str(s);
        self.query_edited();
    }

    fn move_sel(&mut self, delta: i64) {
        self.ensure_filtered();
        if self.filtered.is_empty() {
            return;
        }
        let pos = self
            .selected
            .and_then(|s| self.filtered.iter().position(|&i| i == s))
            .unwrap_or(0) as i64;
        let to = (pos + delta).clamp(0, self.filtered.len() as i64 - 1) as usize;
        self.selected = Some(self.filtered[to]);
        self.user_moved = true;
    }

    fn scroll_preview(&mut self, down: bool) {
        let step = (self.preview_rows / 2).max(1);
        let lines = match &self.current {
            Some(Preview::Doc { lines, .. }) => lines.len(),
            _ => 0,
        };
        let max = lines
            .saturating_sub(self.preview_rows as usize)
            .min(u16::MAX as usize) as u16;
        self.preview_scroll = if down {
            self.preview_scroll.saturating_add(step)
        } else {
            self.preview_scroll.saturating_sub(step)
        }
        .min(max);
    }

    fn rescan(&mut self) {
        self.prefs.show_hidden = !self.prefs.show_hidden;
        self.cancel.store(true, Ordering::Relaxed);
        self.cancel = Arc::new(AtomicBool::new(false));
        self.scan_id = next_id();
        self.entries.clear();
        self.filtered.clear();
        self.selected = None;
        self.summary = None;
        self.list_top = 0;
        self.preview_scroll = 0;
        self.current = None;
        self.last_req = None;
        self.user_moved = false;
        self.scanning = true;
        self.scan_failed = false;
        self.filtered_stale = true;
        self.send_scan();
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> FileAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let page = self.list_rows.max(1) as i64;
        match key.code {
            KeyCode::Esc => return FileAction::Close,
            KeyCode::Enter => {
                self.ensure_filtered();
                let Some(e) = self.selected_entry() else {
                    return FileAction::None;
                };
                if e.notebook {
                    return FileAction::Open(e.path.clone());
                }
                self.notice = Some((ONLY_NOTEBOOKS.to_string(), Instant::now()));
            }
            KeyCode::Up | KeyCode::BackTab => self.move_sel(-1),
            KeyCode::Down | KeyCode::Tab => self.move_sel(1),
            KeyCode::PageUp => self.move_sel(-page),
            KeyCode::PageDown => self.move_sel(page),
            KeyCode::Char('p') | KeyCode::Char('k') if ctrl => self.move_sel(-1),
            KeyCode::Char('n') | KeyCode::Char('j') if ctrl => self.move_sel(1),
            KeyCode::Char('u') if ctrl => {
                self.query.clear();
                self.query_edited();
            }
            KeyCode::Char('f') if ctrl => self.scroll_preview(true),
            KeyCode::Char('b') if ctrl => self.scroll_preview(false),
            KeyCode::Char('h') if ctrl || alt => {
                self.rescan();
                return FileAction::PrefsChanged(self.prefs);
            }
            KeyCode::Char('i') if alt => {
                self.prefs.show_ignored = !self.prefs.show_ignored;
                self.filtered_stale = true;
                self.user_moved = false;
                return FileAction::PrefsChanged(self.prefs);
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.query_edited();
            }
            KeyCode::Char(c) if !ctrl && !alt => {
                self.query.push(c);
                self.query_edited();
            }
            _ => {}
        }
        FileAction::None
    }

    /// Clears an old notice; asks for redraws while the scan runs.
    pub fn on_tick(&mut self) -> bool {
        let mut redraw = self.scanning;
        if self
            .notice
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() >= NOTICE_FOR)
        {
            self.notice = None;
            redraw = true;
        }
        redraw
    }

    pub fn is_open_dirty(&self, path: &Path) -> bool {
        self.open_dirty && Some(path) == self.open_path.as_deref()
    }
}

impl Drop for FilePickerState {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        debug!(
            "files picker closed; {} stale events dropped",
            self.stale_drops
        );
    }
}

// ----- drawing -----------------------------------------------------------------

/// Where the parts of the popup go.
struct Areas {
    popup: Rect,
    header: Rect,
    list: Rect,
    preview: Option<Rect>,
    footer: Rect,
}

fn layout(area: Rect) -> Areas {
    let w = area.width * 90 / 100;
    let h = area.height * 85 / 100;
    let popup = Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    );
    let inner = Rect::new(
        popup.x + 1,
        popup.y + 1,
        popup.width.saturating_sub(2),
        popup.height.saturating_sub(2),
    );
    let header = Rect::new(inner.x, inner.y, inner.width, inner.height.min(1));
    let footer = Rect::new(
        inner.x,
        inner.y + inner.height.saturating_sub(1),
        inner.width,
        inner.height.min(1),
    );
    let body = Rect::new(
        inner.x,
        inner.y + 1,
        inner.width,
        inner.height.saturating_sub(2),
    );
    let (list, preview) = if inner.width >= 100 {
        let lw = body.width * 45 / 100;
        (
            Rect::new(body.x, body.y, lw, body.height),
            Some(Rect::new(body.x + lw, body.y, body.width - lw, body.height)),
        )
    } else if inner.height >= 20 {
        let lh = body.height * 40 / 100;
        (
            Rect::new(body.x, body.y, body.width, lh),
            Some(Rect::new(body.x, body.y + lh, body.width, body.height - lh)),
        )
    } else {
        (body, None)
    };
    Areas {
        popup,
        header,
        list,
        preview,
        footer,
    }
}

fn preview_block(title: String) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(title)
}

pub fn render(buf: &mut Buffer, area: Rect, p: &mut FilePickerState, theme: &Theme) {
    // 1. Rebuild the list.
    p.ensure_filtered();

    // 2. Layout.
    let a = layout(area);
    p.list_rows = a.list.height;
    p.preview_visible = a.preview.is_some_and(|r| r.width > 2 && r.height > 2);
    if let Some(r) = a.preview {
        let inner = preview_block(String::new()).inner(r);
        p.preview_width = inner.width;
        p.preview_rows = inner.height;
    }

    // 3. Reset the preview scroll when the selection changed.
    if p.selected.is_some() && p.selected != p.last_req.map(|(i, _)| i) {
        p.preview_scroll = 0;
    }

    // 4. Request a preview.
    if p.preview_visible
        && let Some(sel) = p.selected
    {
        let notebook = p.entries[sel as usize].notebook;
        let resend = match p.last_req {
            None => true,
            Some((i, w)) => i != sel || (w != p.preview_width && notebook),
        };
        if !p.was_visible || p.current.is_none() || resend {
            let id = next_id();
            p.io.preview(PreviewRequest {
                id,
                path: p.entries[sel as usize].path.clone(),
                width: p.preview_width,
                syn: p.ctx.syn.clone(),
                ui: p.ctx.ui,
                tab_width: p.ctx.tab_width,
            });
            p.last_preview_id = id;
            p.last_req = Some((sel, p.preview_width));
            p.current_is_stale = p.current.is_some();
        }
    }

    // 5. Remember visibility.
    p.was_visible = p.preview_visible;

    // 6. Draw.
    Clear.render(a.popup, buf);
    let title_w = a.popup.width.saturating_sub(6) as usize;
    let root = p.root.as_deref().unwrap_or(&p.start);
    let root_text = clean_name(&root.to_string_lossy()).into_owned();
    let title = format!(" {} ", shorten_path(Path::new(&root_text), title_w));
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .style(theme.float())
        .border_style(theme.border())
        .title(title)
        .render(a.popup, buf);
    if a.header.height == 0 {
        return;
    }
    draw_header(buf, a.header, p, theme);
    draw_list(buf, a.list, p, theme, &root_text);
    if let Some(r) = a.preview
        && p.preview_visible
    {
        draw_preview(buf, r, p, theme);
    }
    draw_footer(buf, a.footer, p, theme);
}

fn draw_header(buf: &mut Buffer, r: Rect, p: &FilePickerState, theme: &Theme) {
    let capped = p
        .summary
        .as_ref()
        .is_some_and(|s| s.hit_shown_cap || s.hit_extra_cap || s.hit_visit_cap);
    let counter = format!(
        "{}/{}{}",
        p.filtered.len(),
        p.visible_total(),
        if capped { "+" } else { "" }
    );
    let right = if p.scanning {
        format!("scanning…  {counter}")
    } else {
        counter
    };
    let rw = right.width() as u16;
    let query = Line::from(vec![
        Span::styled("> ", Style::default().fg(theme.ui.focus)),
        Span::raw(clean_name(&p.query).into_owned()),
        Span::styled("▏", theme.muted()),
    ]);
    buf.set_line(r.x, r.y, &query, r.width.saturating_sub(rw + 1));
    if rw < r.width {
        buf.set_string(r.x + r.width - rw, r.y, right, theme.muted());
    }
}

/// The characters of a list row, each with its byte offset in `display`
/// (`None` for the ellipsis and padding). The file name stays whole when it
/// fits; the folder part is cut from the left.
fn row_chars(display: &str, name_at: usize, avail: usize) -> Vec<(char, Option<usize>)> {
    let from = |s: &str, base: usize| {
        s.char_indices()
            .map(move |(i, c)| (c, Some(base + i)))
            .collect::<Vec<_>>()
    };
    if display.width() <= avail {
        return from(display, 0);
    }
    let name = &display[name_at..];
    if name.width() < avail {
        // `…`, padding, then a suffix of `display`.
        let s = cut_left(display, avail);
        let rest = s.strip_prefix('…').unwrap_or(&s);
        let j = (0..=rest.len())
            .filter(|&j| rest.is_char_boundary(j))
            .find(|&j| display.ends_with(&rest[j..]))
            .unwrap_or(rest.len());
        let mut out = vec![('…', None)];
        out.extend(rest[..j].chars().map(|c| (c, None)));
        out.extend(from(&rest[j..], display.len() - (rest.len() - j)));
        out
    } else {
        // A prefix of the file name, then `…` and padding.
        let s = cut(name, avail);
        let k = if s == name {
            s.len()
        } else {
            s.rfind('…').unwrap_or(s.len())
        };
        let mut out = from(&s[..k], name_at);
        out.extend(s[k..].chars().map(|c| (c, None)));
        out
    }
}

fn draw_list(buf: &mut Buffer, r: Rect, p: &mut FilePickerState, theme: &Theme, root_text: &str) {
    if r.height == 0 || r.width == 0 {
        return;
    }
    if p.filtered.is_empty() {
        let msg = if p.scan_failed {
            "cannot start scan".to_string()
        } else if p.entries.is_empty() && p.summary.is_some() {
            format!("no files under {root_text} — Esc, then Ctrl+P → New notebook")
        } else if p.entries.is_empty() {
            String::new()
        } else if !p.prefs.show_ignored && !p.query.is_empty() {
            let hidden = p
                .entries
                .iter()
                .filter(|e| e.ignored && !e.notebook)
                .filter(|e| p.fuzzy.score(e.hay.slice(..)).is_some())
                .count();
            if hidden > 0 {
                format!("no matches — {hidden} ignored not shown (M-i)")
            } else {
                "no matches".into()
            }
        } else {
            "no matches".into()
        };
        buf.set_stringn(
            r.x + 1,
            r.y,
            msg,
            r.width.saturating_sub(1) as usize,
            theme.muted(),
        );
        return;
    }

    let rows = r.height as usize;
    let pos = p
        .selected
        .and_then(|s| p.filtered.iter().position(|&i| i == s))
        .unwrap_or(0);
    if pos < p.list_top {
        p.list_top = pos;
    } else if pos >= p.list_top + rows {
        p.list_top = pos + 1 - rows;
    }
    p.list_top = p.list_top.min(p.filtered.len().saturating_sub(rows));

    let slot = if p.ctx.icons { 2 } else { 0 };
    let text_x = r.x + 1 + slot;
    let avail = r.width.saturating_sub(2 + slot) as usize;
    let mut indices = Vec::new();
    for (row, &idx) in p.filtered.iter().enumerate().skip(p.list_top).take(rows) {
        let e = &p.entries[idx as usize];
        let y = r.y + (row - p.list_top) as u16;
        let selected = Some(idx) == p.selected;
        let base = if selected {
            theme.selected_row()
        } else {
            Style::default()
        };
        buf.set_style(Rect::new(r.x, y, r.width, 1), base);
        let tint = |fg| if selected { base } else { base.fg(fg) };
        let display = clean_name(&e.display);
        let name_at = e.name_at as usize;
        if p.ctx.icons {
            let (glyph, color) = icon(&display[name_at..], e.notebook, &theme.ui);
            buf.set_string(r.x + 1, y, glyph.to_string(), tint(color));
        }
        indices.clear();
        if display.is_ascii() && !p.query.is_empty() {
            p.fuzzy.indices(e.hay.slice(..), &mut indices);
        }
        let mut x = text_x;
        for (c, off) in row_chars(&display, name_at, avail) {
            let st = match off {
                Some(o) if indices.binary_search(&(o as u32)).is_ok() => {
                    base.fg(theme.ui.focus).add_modifier(Modifier::BOLD)
                }
                Some(o) if o >= name_at => base,
                _ => tint(theme.ui.muted),
            };
            buf.set_string(x, y, c.to_string(), st);
            x += UnicodeWidthChar::width(c).unwrap_or(0) as u16;
        }
    }
}

fn draw_preview(buf: &mut Buffer, r: Rect, p: &FilePickerState, theme: &Theme) {
    let Some(e) = p.selected_entry() else {
        preview_block(String::new())
            .border_style(theme.border())
            .render(r, buf);
        return;
    };
    let name = clean_name(&e.display[e.name_at as usize..]).into_owned();
    let mut notes: Vec<String> = Vec::new();
    if p.is_open_dirty(&e.path) {
        notes.push(UNSAVED_NOTE.into());
    }
    if let Some(Preview::Doc { note: Some(n), .. }) = &p.current {
        notes.push(n.clone());
    }
    let mut block = preview_block(format!(" {name} ")).border_style(theme.border());
    if !notes.is_empty() {
        block = block.title_bottom(Line::styled(
            format!(" {} ", notes.join(" · ")),
            theme.muted().add_modifier(Modifier::DIM),
        ));
    }
    let inner = block.inner(r);
    block.render(r, buf);
    match &p.current {
        None => {}
        Some(Preview::Placeholder(s)) => {
            buf.set_stringn(
                inner.x + 1,
                inner.y,
                s,
                inner.width.saturating_sub(1) as usize,
                theme.muted(),
            );
        }
        Some(Preview::Doc {
            lines,
            line_numbers,
            ..
        }) => {
            let gutter = if *line_numbers {
                lines.len().max(1).to_string().len() as u16 + 1
            } else {
                0
            };
            let tx = inner.x + gutter;
            let tw = inner.width.saturating_sub(gutter);
            let top = p.preview_scroll as usize;
            for (row, line) in lines
                .iter()
                .skip(top)
                .take(inner.height as usize)
                .enumerate()
            {
                let y = inner.y + row as u16;
                if *line_numbers {
                    let n = format!("{:>w$} ", top + row + 1, w = gutter as usize - 1);
                    buf.set_string(inner.x, y, n, Style::default().fg(p.ctx.ui.line_numbers));
                }
                if line.style != Style::default() {
                    buf.set_style(Rect::new(tx, y, tw, 1), line.style);
                }
                buf.set_line(tx, y, line, tw);
            }
        }
    }
    if p.current_is_stale {
        buf.set_style(inner, Style::default().add_modifier(Modifier::DIM));
    }
}

fn draw_footer(buf: &mut Buffer, r: Rect, p: &FilePickerState, theme: &Theme) {
    if r.height == 0 {
        return;
    }
    if let Some((msg, _)) = &p.notice {
        buf.set_stringn(
            r.x + 1,
            r.y,
            msg,
            r.width.saturating_sub(1) as usize,
            Style::default().fg(theme.ui.warning),
        );
        return;
    }
    let on_off = |b: bool| if b { "on" } else { "off" };
    let items = [
        format!("M-h hidden [{}]", on_off(p.prefs.show_hidden)),
        format!("M-i ignored [{}]", on_off(p.prefs.show_ignored)),
        "^F/^B scroll".to_string(),
        "Esc close".to_string(),
    ];
    let max = r.width.saturating_sub(1) as usize;
    let mut line = String::new();
    for item in items {
        let next = if line.is_empty() {
            item
        } else {
            format!("{line}  {item}")
        };
        if next.width() > max {
            break;
        }
        line = next;
    }
    buf.set_string(r.x + 1, r.y, line, theme.muted());
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use nucleo_matcher::Utf32String;
    use ratatui::style::Color;

    use super::*;

    const ROOT: &str = "/repo";

    struct T {
        p: FilePickerState,
        calls: Rc<RefCell<Vec<IoCall>>>,
        theme: Theme,
    }

    fn ctx(icons: bool) -> PickerCtx {
        PickerCtx {
            home: None,
            syn: Arc::new(Theme::load("monokai").unwrap().syntax),
            ui: UiColors {
                muted: Color::Gray,
                input_bg: None,
                line_numbers: Color::DarkGray,
            },
            tab_width: 4,
            icons,
        }
    }

    fn picker_with(open_path: Option<&str>, open_dirty: bool, preselect: Option<&str>) -> T {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let io = RecordIo {
            calls: calls.clone(),
        };
        let p = FilePickerState::new(
            PathBuf::from(ROOT),
            open_path.map(PathBuf::from),
            open_dirty,
            preselect.map(PathBuf::from),
            PickerPrefs::default(),
            ctx(true),
            Box::new(io),
        );
        T {
            p,
            calls,
            theme: Theme::load("tokyo-night").unwrap(),
        }
    }

    fn picker() -> T {
        picker_with(None, false, None)
    }

    fn entry(display: &str, ignored: bool) -> FileEntry {
        FileEntry {
            path: Path::new(ROOT).join(display),
            display: display.into(),
            hay: Utf32String::from(display),
            name_at: display.rfind('/').map_or(0, |i| i + 1) as u32,
            ignored,
            notebook: display.ends_with(".ipynb"),
        }
    }

    impl T {
        fn scan_id(&self) -> u64 {
            self.p.scan_id
        }

        fn root(&mut self) {
            let open_path = self.p.open_path.clone();
            let id = self.scan_id();
            self.p.on_event(FilesEvent::Root {
                scan_id: id,
                root: PathBuf::from(ROOT),
                open_path,
            });
        }

        fn batch(&mut self, names: &[&str]) {
            let entries = names.iter().map(|n| entry(n, false)).collect();
            let id = self.scan_id();
            self.p.on_event(FilesEvent::Batch {
                scan_id: id,
                entries,
            });
        }

        fn done(&mut self) {
            let id = self.scan_id();
            self.p.on_event(FilesEvent::Done {
                scan_id: id,
                summary: ScanSummary::default(),
            });
        }

        fn draw(&mut self, w: u16, h: u16) -> Buffer {
            let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
            render(&mut buf, Rect::new(0, 0, w, h), &mut self.p, &self.theme);
            buf
        }

        fn key(&mut self, code: KeyCode) -> FileAction {
            self.p.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
        }

        fn mod_key(&mut self, c: char, m: KeyModifiers) -> FileAction {
            self.p.handle_key(KeyEvent::new(KeyCode::Char(c), m))
        }

        fn typed(&mut self, s: &str) {
            for c in s.chars() {
                self.key(KeyCode::Char(c));
            }
        }

        fn selected_display(&mut self) -> Option<String> {
            self.p.ensure_filtered();
            self.p.selected_entry().map(|e| e.display.to_string())
        }

        fn previews(&self) -> usize {
            self.calls
                .borrow()
                .iter()
                .filter(|c| matches!(c, IoCall::Preview(_)))
                .count()
        }

        fn scans(&self) -> Vec<ScanRequest> {
            self.calls
                .borrow()
                .iter()
                .filter_map(|c| match c {
                    IoCall::Scan(r) => Some(r.clone()),
                    IoCall::Preview(_) => None,
                })
                .collect()
        }
    }

    fn text(buf: &Buffer) -> String {
        let a = buf.area;
        let mut s = String::new();
        for y in 0..a.height {
            for x in 0..a.width {
                s.push_str(buf[(x, y)].symbol());
            }
            s.push('\n');
        }
        s
    }

    #[test]
    fn layout_side_by_side_stacked_and_list_only() {
        let mut t = picker();
        t.root();
        t.batch(&["a.py", "b.ipynb"]);
        t.draw(160, 45);
        assert!(t.p.preview_visible);
        // Side by side: the preview pane takes most of the height.
        assert!(t.p.preview_rows > 30, "{}", t.p.preview_rows);
        assert!(t.p.preview_width < 80, "{}", t.p.preview_width);

        let mut t = picker();
        t.batch(&["a.py"]);
        t.draw(100, 40);
        assert!(t.p.preview_visible);
        // Stacked: the preview spans the full width, below the list.
        assert!(
            t.p.preview_width > 80 && t.p.preview_rows < 25,
            "{}x{}",
            t.p.preview_width,
            t.p.preview_rows
        );

        let mut t = picker();
        t.batch(&["a.py"]);
        t.draw(100, 20);
        assert!(!t.p.preview_visible);
        assert_eq!(t.previews(), 0);
        t.draw(160, 45);
        assert_eq!(t.previews(), 1);
    }

    #[test]
    fn moved_selection_stays_while_better_matches_arrive() {
        let mut t = picker();
        t.batch(&["x/config_old.txt", "y/conf.txt"]);
        t.typed("conf");
        t.draw(160, 45);
        t.key(KeyCode::Down);
        let moved = t.selected_display().unwrap();
        t.batch(&["conf"]);
        t.draw(160, 45);
        assert_eq!(t.selected_display().unwrap(), moved);
    }

    #[test]
    fn open_notebook_arriving_later_is_selected() {
        let mut t = picker_with(Some("/repo/z/open.ipynb"), false, None);
        t.root();
        t.batch(&["a.py", "b.py"]);
        t.draw(160, 45);
        assert_eq!(t.selected_display().as_deref(), Some("a.py"));
        t.batch(&["z/open.ipynb"]);
        t.draw(160, 45);
        assert_eq!(t.selected_display().as_deref(), Some("z/open.ipynb"));
    }

    #[test]
    fn open_notebook_does_not_steal_a_moved_selection() {
        let mut t = picker_with(Some("/repo/z/open.ipynb"), false, None);
        t.root();
        t.batch(&["a.py", "b.py"]);
        t.draw(160, 45);
        t.key(KeyCode::Down);
        t.batch(&["z/open.ipynb"]);
        t.draw(160, 45);
        assert_eq!(t.selected_display().as_deref(), Some("b.py"));
    }

    #[test]
    fn preselect_wins_over_open_notebook() {
        let mut t = picker_with(Some("/repo/open.ipynb"), false, Some("/repo/pick.ipynb"));
        t.root();
        t.batch(&["a.py", "open.ipynb", "pick.ipynb"]);
        assert_eq!(t.selected_display().as_deref(), Some("pick.ipynb"));
    }

    #[test]
    fn stale_events_are_dropped() {
        let mut t = picker();
        let old = t.scan_id() - 1;
        assert!(!t.p.on_event(FilesEvent::Batch {
            scan_id: old,
            entries: vec![entry("a.py", false)]
        }));
        assert!(t.p.entries.is_empty());
        t.batch(&["a.py"]);
        t.draw(160, 45);
        let id = t.p.last_preview_id;
        assert!(!t.p.on_event(FilesEvent::Preview {
            id: id - 1,
            preview: Preview::Placeholder("old".into())
        }));
        assert!(t.p.current.is_none());
        assert!(t.p.on_event(FilesEvent::Preview {
            id,
            preview: Preview::Placeholder("new".into())
        }));
        assert_eq!(t.p.stale_drops, 2);
    }

    #[test]
    fn enter_opens_notebooks_only() {
        let mut t = picker();
        assert!(matches!(t.key(KeyCode::Enter), FileAction::None));
        t.batch(&["a.py", "b.ipynb"]);
        assert!(matches!(t.key(KeyCode::Enter), FileAction::None));
        assert_eq!(t.p.notice.as_ref().unwrap().0, ONLY_NOTEBOOKS);
        t.key(KeyCode::Down);
        match t.key(KeyCode::Enter) {
            FileAction::Open(p) => assert_eq!(p, Path::new("/repo/b.ipynb")),
            _ => panic!("expected Open"),
        }
    }

    #[test]
    fn escape_closes() {
        let mut t = picker();
        assert!(matches!(t.key(KeyCode::Esc), FileAction::Close));
    }

    #[test]
    fn ignored_toggle_refilters_without_a_scan() {
        let mut t = picker();
        let id = t.scan_id();
        t.p.on_event(FilesEvent::Batch {
            scan_id: id,
            entries: vec![entry("a.py", false), entry("data.csv", true)],
        });
        t.draw(160, 45);
        assert_eq!(t.p.filtered.len(), 1);
        assert!(
            matches!(t.mod_key('i', KeyModifiers::ALT), FileAction::PrefsChanged(p) if p.show_ignored)
        );
        t.draw(160, 45);
        assert_eq!(t.p.filtered.len(), 2);
        assert_eq!(t.scans().len(), 1);
    }

    #[test]
    fn hidden_toggle_rescans_and_keeps_the_query() {
        let mut t = picker();
        t.batch(&["a.py"]);
        t.typed("a");
        let first = t.scan_id();
        assert!(
            matches!(t.mod_key('h', KeyModifiers::ALT), FileAction::PrefsChanged(p) if p.show_hidden)
        );
        let scans = t.scans();
        assert_eq!(scans.len(), 2);
        assert!(scans[1].prefs.show_hidden);
        assert_ne!(scans[1].scan_id, first);
        assert!(scans[0].cancel.load(Ordering::Relaxed));
        assert_eq!(t.p.query, "a");
        assert!(t.p.entries.is_empty());
        // Ctrl+H does the same.
        t.mod_key('h', KeyModifiers::CONTROL);
        assert!(!t.scans()[2].prefs.show_hidden);
    }

    #[test]
    fn tick_clears_an_old_notice_once() {
        let mut t = picker();
        t.done();
        t.p.notice = Some((
            "x".into(),
            Instant::now().checked_sub(Duration::from_secs(4)).unwrap(),
        ));
        assert!(t.p.on_tick());
        assert!(!t.p.on_tick());
    }

    #[test]
    fn long_path_keeps_its_file_name() {
        let mut t = picker();
        let long = format!(
            "{}/the_file_name.ipynb",
            "deep/".repeat(40).trim_end_matches('/')
        );
        t.batch(&[&long]);
        let s = text(&t.draw(160, 45));
        assert!(s.contains("…"));
        assert!(s.contains("/the_file_name.ipynb"));
    }

    #[test]
    fn escape_in_a_file_name_is_drawn_as_question_mark() {
        let mut t = picker();
        t.batch(&["bad\x1b[31mname.txt"]);
        let buf = t.draw(160, 45);
        let s = text(&buf);
        assert!(s.contains("bad?[31mname.txt"));
        assert!(!s.contains('\x1b'));
    }

    fn has_focus_fg(buf: &Buffer, y: u16, theme: &Theme) -> bool {
        (0..buf.area.width).any(|x| {
            buf[(x, y)].fg == theme.ui.focus && buf[(x, y)].modifier.contains(Modifier::BOLD)
        })
    }

    fn row_of(buf: &Buffer, needle: &str) -> u16 {
        text(buf).lines().position(|l| l.contains(needle)).unwrap() as u16
    }

    #[test]
    fn non_ascii_rows_have_no_match_highlights() {
        let mut t = picker();
        t.batch(&["zz/plain.py", "zz/café.py"]);
        t.typed("py");
        // Move off the first row so neither row uses the selection style.
        t.p.selected = None;
        t.p.user_moved = true;
        let buf = t.draw(160, 45);
        let theme = Theme::load("tokyo-night").unwrap();
        assert!(has_focus_fg(&buf, row_of(&buf, "plain.py"), &theme));
        assert!(!has_focus_fg(&buf, row_of(&buf, "café.py"), &theme));
    }

    #[test]
    fn unsaved_note_only_for_the_dirty_open_notebook() {
        let mut t = picker_with(Some("/repo/open.ipynb"), true, None);
        t.root();
        t.batch(&["open.ipynb", "other.ipynb"]);
        assert!(text(&t.draw(160, 45)).contains(UNSAVED_NOTE));
        t.key(KeyCode::Down);
        assert!(!text(&t.draw(160, 45)).contains(UNSAVED_NOTE));

        let mut t = picker_with(Some("/repo/open.ipynb"), false, None);
        t.root();
        t.batch(&["open.ipynb"]);
        assert!(!text(&t.draw(160, 45)).contains(UNSAVED_NOTE));
    }

    #[test]
    fn empty_state_names_the_root() {
        let mut t = picker();
        t.root();
        t.done();
        let s = text(&t.draw(160, 45));
        assert!(
            s.contains("no files under /repo — Esc, then Ctrl+P → New notebook"),
            "{s}"
        );
    }

    #[test]
    fn failed_scan_says_so() {
        let mut t = picker();
        t.done();
        assert!(text(&t.draw(160, 45)).contains("cannot start scan"));
    }

    #[test]
    fn no_matches_counts_ignored_entries() {
        let mut t = picker();
        let id = t.scan_id();
        t.p.on_event(FilesEvent::Batch {
            scan_id: id,
            entries: vec![entry("a.py", false), entry("data.csv", true)],
        });
        t.typed("csv");
        assert!(text(&t.draw(160, 45)).contains("no matches — 1 ignored not shown (M-i)"));
    }

    #[test]
    fn row_chars_cut_forms() {
        let s: String = row_chars("abc/def.py", 4, 8)
            .iter()
            .map(|(c, _)| c)
            .collect();
        assert_eq!(s, "…/def.py");
        let r = row_chars("abc/def.py", 4, 8);
        assert_eq!(r[0].1, None);
        assert_eq!(r[1].1, Some(3));
        let s: String = row_chars("a/long_name.py", 2, 6)
            .iter()
            .map(|(c, _)| c)
            .collect();
        assert_eq!(s, "long_…");
    }
}
