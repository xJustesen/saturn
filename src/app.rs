//! Application state, the event loop and command dispatch.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::style::Print;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, KeyEventKind};
use edtui::EditorMode;
use futures::StreamExt;
use jupyter_zmq_client::KernelspecDir;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::sliced::SlicedProtocol;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::Opts;
use crate::commands::{Command, PALETTE};
use crate::config::Config;
use crate::theme::Theme;
use crate::editor::{CellEditor, language_extension};
use crate::files::projects::Project;
use crate::files::{FileEntry, PickerPrefs};
use crate::files::preview::{PreviewWorker, UiColors};
use crate::kernel::{KernelCommand, KernelEvent, KernelHandle, KernelStatus, ReplyStatus, startup};
use crate::keymap::{self, Key, KeyAction};
use crate::notebook::undo::CellOp;
use crate::notebook::{Cell, CellKind, ExecState, Notebook, Output, io as nbio};
use crate::render::RenderCache;
use crate::render::image::ImageStore;
use crate::render::plotly::PlotlySidecar;
use crate::recent::Recent;
use crate::search::{self, Match, SearchInput, SearchState};
use crate::ui;
use crate::ui::cell::content_width;
use crate::ui::cell_picker::{CellPickerState, PickerAction};
use crate::ui::dashboard::{DashAction, DashItem, DashboardState};
use crate::ui::layout::LayoutCache;
use crate::ui::file_picker::{FileAction, FilePickerState, PickerCtx, ThreadIo};
use crate::ui::overlay::{
    ConfirmKind, ConfirmState, ItemData, Overlay, PaletteAction, PaletteItem, PaletteKind,
    PaletteState, PromptKind, PromptState,
};
use crate::ui::popout::{PopoutAction, PopoutState};

pub enum AppEvent {
    Terminal(Event),
    Kernel(KernelEvent),
    ImageReady { hash: u64, result: Result<SlicedProtocol, String> },
    PlotlyDone { hash: u64, result: Result<Vec<u8>, String> },
    PlotlyFatal(String),
    /// A frame (or the failure) of the popout's interactive plotly figure.
    PlotlyLive { sid: u64, result: Result<Vec<u8>, String> },
    Kernelspecs(Vec<KernelspecDir>),
    /// From the files picker's scanner and preview worker.
    Files(crate::files::FilesEvent),
    /// The dashboard's projects scan finished.
    Projects(Vec<Project>),
    Tick,
}

/// Whether mouse reporting is switched on in the terminal (it is only while
/// an interactive figure is showing, so text selection works otherwise).
pub static MOUSE_ON: AtomicBool = AtomicBool::new(false);

/// Switch mouse reporting off if it is on (also used when leaving the terminal).
pub fn disable_mouse() {
    if MOUSE_ON.swap(false, Ordering::SeqCst) {
        let _ = crossterm::execute!(std::io::stdout(), Print(PIXEL_MOUSE_OFF), DisableMouseCapture);
    }
}

/// SGR-pixel mouse reporting (DEC mode 1016): positions in pixels, not cells.
const PIXEL_MOUSE_ON: &str = "\x1b[?1016h";
const PIXEL_MOUSE_OFF: &str = "\x1b[?1016l";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Command,
    Edit,
}

/// What sits under the overlays: the start dashboard or the notebook.
pub enum Screen {
    Dashboard(DashboardState),
    Notebook,
}

pub struct Flash {
    pub text: String,
    pub error: bool,
    pub at: Instant,
}

const FLASH_SECS: u64 = 5;
const SCROLL_MARGIN: usize = 2;

pub struct App {
    pub screen: Screen,
    pub nb: Notebook,
    pub mode: Mode,
    pub focused: usize,
    pub scroll_row: usize,
    pub layout: LayoutCache,
    pub cache: RenderCache,
    pub images: ImageStore,
    pub plotly: PlotlySidecar,
    pub kernel: Option<KernelHandle>,
    pub kernelspecs: Option<Vec<KernelspecDir>>,
    pub cfg: Config,
    pub theme: Theme,
    /// Theme in use before the theme picker opened; restored if the picker is cancelled.
    theme_before_pick: Option<Theme>,
    pub pending: Vec<Key>,
    pub clipboard: Vec<Cell>,
    pub flash: Option<Flash>,
    pub overlay: Option<Overlay>,
    pub search: SearchState,
    pub lang_ext: String,
    pub line_numbers: bool,
    pub view: Rect,
    /// Screen position of the insert-mode cursor, set while drawing.
    pub edit_cursor: Option<ratatui::layout::Position>,
    pub log_path: PathBuf,
    pub spinner: usize,
    pub graphics_label: String,
    picker_prefs: PickerPrefs,
    /// The terminal reports mouse positions in pixels when asked (mode 1016).
    pixel_mouse: bool,
    edit_snapshot: Option<(usize, Cell)>,
    opts_kernel: Option<String>,
    want_kernel: bool,
    tx: UnboundedSender<AppEvent>,
    rx: UnboundedReceiver<AppEvent>,
    dirty_ui: bool,
    quit: bool,
    recent_path: Option<PathBuf>,
    projects: Option<Vec<Project>>,
    projects_scanning: bool,
    started: Option<Instant>,
    /// Startup to the first frame, for the dashboard footer.
    pub ready_in: Option<Duration>,
}

impl App {
    pub async fn new(opts: Opts, cfg: Config, picker: Picker, log_path: PathBuf) -> Result<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        let graphics_label = match picker.protocol_type() {
            ProtocolType::Kitty => "kitty".to_string(),
            ProtocolType::Iterm2 => "iterm2".to_string(),
            ProtocolType::Sixel => "sixel".to_string(),
            ProtocolType::Halfblocks => "halfblocks".to_string(),
        };
        let images = ImageStore::new(picker, tx.clone());
        let plotly = PlotlySidecar::new(tx.clone(), cfg.plotly_scale);
        let theme = match Theme::load(&cfg.theme) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("theme {:?}: {e:#}; using terminal colors", cfg.theme);
                Theme::default()
            }
        };
        let mut app = App {
            screen: Screen::Notebook,
            nb: Notebook::empty(),
            mode: Mode::Command,
            focused: 0,
            scroll_row: 0,
            layout: LayoutCache::default(),
            cache: RenderCache::default(),
            images,
            plotly,
            kernel: None,
            kernelspecs: None,
            line_numbers: cfg.line_numbers,
            picker_prefs: PickerPrefs { show_hidden: cfg.show_hidden, show_ignored: cfg.show_ignored },
            pixel_mouse: opts.pixel_mouse,
            cfg,
            theme,
            theme_before_pick: None,
            pending: Vec::new(),
            clipboard: Vec::new(),
            flash: None,
            overlay: None,
            search: SearchState::default(),
            lang_ext: "py".to_string(),
            view: Rect::default(),
            edit_cursor: None,
            log_path,
            spinner: 0,
            graphics_label,
            edit_snapshot: None,
            opts_kernel: opts.kernel.clone(),
            want_kernel: false,
            tx,
            rx,
            dirty_ui: true,
            quit: false,
            recent_path: crate::recent::default_path(),
            projects: None,
            projects_scanning: false,
            started: opts.started,
            ready_in: None,
        };
        {
            let tx = app.tx.clone();
            tokio::spawn(async move {
                let specs = crate::kernel::discover_kernelspecs().await;
                let _ = tx.send(AppEvent::Kernelspecs(specs));
            });
        }
        match opts.path {
            Some(p) if p.is_dir() => app.open_file_picker(p, None),
            Some(p) => {
                if let Err(e) = app.load_notebook(&p) {
                    if p.exists() {
                        app.flash_err(format!("{e:#}"));
                    } else {
                        // New notebook at that path.
                        app.nb = Notebook::empty();
                        app.nb.path = Some(p);
                        app.want_kernel = true;
                        app.flash("new notebook");
                    }
                }
            }
            None => app.show_dashboard(),
        }
        Ok(app)
    }

    // ----- event loop -------------------------------------------------------

    pub async fn run(mut self, terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>) -> Result<()> {
        {
            let tx = self.tx.clone();
            tokio::spawn(async move {
                let mut stream = EventStream::new();
                while let Some(ev) = stream.next().await {
                    match ev {
                        Ok(ev) => {
                            if tx.send(AppEvent::Terminal(ev)).is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            tracing::error!("input error: {e}");
                            break;
                        }
                    }
                }
            });
        }
        {
            let tx = self.tx.clone();
            tokio::spawn(async move {
                let mut iv = tokio::time::interval(Duration::from_millis(200));
                loop {
                    iv.tick().await;
                    if tx.send(AppEvent::Tick).is_err() {
                        break;
                    }
                }
            });
        }
        loop {
            if self.dirty_ui {
                let t0 = Instant::now();
                terminal.draw(|f| ui::draw(f, &mut self))?;
                if let Some(s) = self.started.take() {
                    self.ready_in = Some(s.elapsed());
                    // The footer shows the figure from the next frame on.
                    self.dirty_ui = true;
                    let dt = t0.elapsed();
                    tracing::debug!("first frame after {:?} (draw {dt:?})", self.ready_in);
                    continue;
                }
                let dt = t0.elapsed();
                if dt > Duration::from_millis(8) {
                    tracing::debug!("slow frame: {dt:?}");
                }
                self.dirty_ui = false;
            }
            self.sync_mouse();
            let Some(ev) = self.rx.recv().await else { break };
            self.handle(ev);
            let mut n = 0;
            while let Ok(ev) = self.rx.try_recv() {
                self.handle(ev);
                n += 1;
                if n > 256 {
                    break;
                }
            }
            if self.quit {
                break;
            }
        }
        if let Some(k) = self.kernel.take() {
            k.send(KernelCommand::Shutdown);
            let _ = tokio::time::timeout(Duration::from_secs(3), k.wait()).await;
        }
        Ok(())
    }

    /// Mouse reporting follows the popout: on while it shows an interactive
    /// figure, off otherwise. A session left behind by a replaced overlay is
    /// closed here too.
    fn sync_mouse(&mut self) {
        let live = matches!(&self.overlay, Some(Overlay::Popout(p)) if p.is_live());
        if !live {
            self.plotly.live_close();
            disable_mouse();
        } else if !MOUSE_ON.swap(true, Ordering::SeqCst) {
            let mut out = std::io::stdout();
            let _ = crossterm::execute!(out, EnableMouseCapture);
            if self.pixel_mouse {
                let _ = crossterm::execute!(out, Print(PIXEL_MOUSE_ON));
            }
        }
    }

    fn handle(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Terminal(Event::Mouse(m)) => {
                if let Some(Overlay::Popout(p)) = &mut self.overlay {
                    p.handle_mouse(m, self.images.font_size(), self.pixel_mouse, &self.plotly);
                }
            }
            AppEvent::PlotlyLive { sid, result } => match result {
                Ok(png) => {
                    if let Some(Overlay::Popout(p)) = &mut self.overlay {
                        p.live_frame(sid, png, &mut self.images);
                    }
                }
                Err(e) => tracing::warn!("plotly live figure: {e}"),
            },
            AppEvent::Terminal(Event::Key(key)) => {
                self.on_key(key);
                self.dirty_ui = true;
            }
            AppEvent::Terminal(Event::Paste(text)) => {
                if self.mode == Mode::Edit {
                    let idx = self.focused;
                    if let Some(ed) = self.nb.cells.get_mut(idx).and_then(|c| c.editor.as_mut()) {
                        ed.paste(text);
                    }
                    self.after_edit();
                } else if let Some(inp) = &mut self.search.input {
                    inp.text.push_str(text.trim_end_matches(['\n', '\r']));
                    self.search_preview();
                } else if let Some(ov) = &mut self.overlay {
                    ov.paste(&text);
                }
                self.dirty_ui = true;
            }
            AppEvent::Terminal(Event::Resize(_, _)) => {
                self.layout.invalidate_all();
                self.dirty_ui = true;
            }
            AppEvent::Terminal(_) => {}
            AppEvent::Kernel(ev) => {
                self.on_kernel(ev);
                self.dirty_ui = true;
            }
            AppEvent::ImageReady { hash, result } => {
                self.images.set_result(hash, result);
                // A frame of the popout's live figure changes no layout.
                if !matches!(&self.overlay, Some(Overlay::Popout(p)) if p.owns_frame(hash)) {
                    self.layout.invalidate_all();
                }
                self.dirty_ui = true;
            }
            AppEvent::PlotlyDone { hash, result } => {
                self.plotly.complete(hash, result);
                self.persist_plotly_pngs();
                self.cache.invalidate_all();
                self.layout.invalidate_all();
                self.dirty_ui = true;
            }
            AppEvent::PlotlyFatal(reason) => {
                self.plotly.fail_all(reason.clone());
                self.flash_err(format!("plotly sidecar: {reason}"));
                self.cache.invalidate_all();
                self.layout.invalidate_all();
                self.dirty_ui = true;
            }
            AppEvent::Kernelspecs(specs) => {
                self.kernelspecs = Some(specs);
                if self.want_kernel {
                    self.want_kernel = false;
                    self.resolve_kernel();
                }
                if let Some(Overlay::Palette(p)) = &self.overlay
                    && p.kind == PaletteKind::Kernels
                {
                    self.open_kernel_picker();
                }
                self.dirty_ui = true;
            }
            AppEvent::Projects(list) => {
                self.projects = Some(list);
                self.projects_scanning = false;
                if let Some(Overlay::Palette(p)) = &self.overlay
                    && p.kind == PaletteKind::Projects
                {
                    let (title, items) = self.project_items();
                    if let Some(Overlay::Palette(p)) = &mut self.overlay {
                        p.title = title;
                        p.set_items(items);
                    }
                }
                self.dirty_ui = true;
            }
            AppEvent::Files(ev) => {
                if let Some(Overlay::Files(p)) = &mut self.overlay
                    && p.on_event(ev)
                {
                    self.dirty_ui = true;
                }
            }
            AppEvent::Tick => {
                if let Some(Overlay::Files(p)) = &mut self.overlay
                    && p.on_tick()
                {
                    self.dirty_ui = true;
                }
                let busy = self
                    .kernel
                    .as_ref()
                    .map(|k| matches!(k.status, KernelStatus::Busy | KernelStatus::Starting | KernelStatus::Restarting))
                    .unwrap_or(false);
                if busy {
                    self.spinner = self.spinner.wrapping_add(1);
                    self.dirty_ui = true;
                }
                if let Some(f) = &self.flash
                    && f.at.elapsed() > Duration::from_secs(FLASH_SECS)
                {
                    self.flash = None;
                    self.dirty_ui = true;
                }
            }
        }
    }

    // ----- helpers ----------------------------------------------------------

    pub fn flash(&mut self, text: impl Into<String>) {
        self.flash = Some(Flash { text: text.into(), error: false, at: Instant::now() });
    }

    pub fn flash_err(&mut self, text: impl Into<String>) {
        let text = text.into();
        tracing::warn!("{text}");
        self.flash = Some(Flash { text, error: true, at: Instant::now() });
    }

    fn cwd(&self) -> PathBuf {
        self.nb
            .path
            .as_ref()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }

    pub fn ensure_layout(&mut self, width: u16) {
        self.layout.set_width(width);
        let n = self.nb.cells.len();
        let editing = self.mode == Mode::Edit;
        let focused = self.focused;
        let App { nb, layout, cache, images, plotly, cfg, theme, lang_ext, line_numbers, .. } = self;
        let mut ctx = ui::cell::CellCtx { cfg, theme, cache, images, plotly, lang_ext, line_numbers: *line_numbers };
        layout.ensure(n, |i| ui::cell::measure(&mut nb.cells[i], editing && i == focused, width, &mut ctx));
    }

    pub fn clamp_scroll(&mut self, view_h: usize) {
        let max = self.layout.total_rows().saturating_sub(view_h);
        if self.scroll_row > max {
            self.scroll_row = max;
        }
    }

    fn view_height(&self) -> usize {
        (self.view.height as usize).max(1)
    }

    /// Make document rows [r0, r1] visible (with margins), if possible.
    fn scroll_into_view(&mut self, r0: usize, r1: usize) {
        let h = self.view_height();
        let span = r1.saturating_sub(r0) + 1;
        if span + 2 * SCROLL_MARGIN <= h {
            if r0 < self.scroll_row + SCROLL_MARGIN {
                self.scroll_row = r0.saturating_sub(SCROLL_MARGIN);
            } else if r1 + SCROLL_MARGIN >= self.scroll_row + h {
                self.scroll_row = r1 + SCROLL_MARGIN + 1 - h;
            }
        } else if r0 < self.scroll_row || r0 >= self.scroll_row + h {
            self.scroll_row = r0;
        }
        self.clamp_scroll(h);
    }

    fn scroll_to_focused(&mut self) {
        if self.view.width == 0 {
            return;
        }
        self.ensure_layout(self.view.width);
        let f = self.focused;
        let r0 = self.layout.row_of(f);
        let total = self.layout.height(f);
        let r1 = r0 + total.saturating_sub(2); // exclude gap row
        self.scroll_into_view(r0, r1);
    }

    fn scroll_to_cursor(&mut self) {
        if self.view.width == 0 {
            return;
        }
        self.ensure_layout(self.view.width);
        let f = self.focused;
        let cw = content_width(self.view.width) as usize;
        let Some(cell) = self.nb.cells.get(f) else { return };
        let Some(ed) = &cell.editor else { return };
        let gutter = if self.line_numbers { ed.line_count().to_string().len() + 1 } else { 0 };
        let row = self.layout.row_of(f) + ed.cursor_visual_row(cw.saturating_sub(gutter).max(1), self.cfg.tab_width, self.cfg.wrap);
        self.scroll_into_view(row, row);
    }

    fn after_edit(&mut self) {
        let idx = self.focused;
        if let Some(cell) = self.nb.cells.get_mut(idx)
            && cell.sync_from_editor()
        {
            self.nb.dirty = true;
        }
        self.layout.invalidate_cell(idx);
        self.scroll_to_cursor();
    }

    fn set_focus(&mut self, idx: usize) {
        let n = self.nb.cells.len();
        if n == 0 {
            self.focused = 0;
            return;
        }
        let old = self.focused;
        self.focused = idx.min(n - 1);
        if old != self.focused {
            self.layout.invalidate_cell(old);
            self.layout.invalidate_cell(self.focused);
            // Like vim, `n` searches from the cursor, not from the last hit.
            self.search.current = None;
        }
        self.scroll_to_focused();
    }

    // ----- keys -------------------------------------------------------------

    fn on_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        if self.overlay.is_some() {
            self.on_overlay_key(key);
            return;
        }
        if let Screen::Dashboard(d) = &mut self.screen {
            let action = d.handle_key(key);
            self.on_dash(action);
            return;
        }
        if self.search.input.is_some() {
            self.on_search_key(key);
            return;
        }
        if let Some(cmd) = keymap::global(&key) {
            self.pending.clear();
            self.dispatch(cmd);
            return;
        }
        match self.mode {
            Mode::Edit => self.on_edit_key(key),
            Mode::Command => match keymap::command_mode(&self.pending, &key) {
                KeyAction::Command(cmd) => {
                    self.pending.clear();
                    self.dispatch(cmd);
                }
                KeyAction::Pending => self.pending.push(keymap::normalize(&key)),
                KeyAction::Unbound => {
                    if !self.pending.is_empty() {
                        self.pending.clear();
                    }
                }
            },
        }
    }

    fn on_edit_key(&mut self, key: KeyEvent) {
        let idx = self.focused;
        let Some(cell) = self.nb.cells.get_mut(idx) else {
            self.mode = Mode::Command;
            return;
        };
        let Some(ed) = cell.editor.as_mut() else {
            self.mode = Mode::Command;
            return;
        };
        if key.code == KeyCode::Esc && ed.mode() == EditorMode::Normal {
            self.leave_edit();
            return;
        }
        ed.key(key);
        self.after_edit();
    }

    fn on_overlay_key(&mut self, key: KeyEvent) {
        let Some(ov) = self.overlay.as_mut() else { return };
        match ov {
            Overlay::Help => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char('?')) {
                    self.overlay = None;
                }
            }
            Overlay::Popout(p) => {
                let n = self.nb.cells.get(p.cell).map(|c| c.outputs.len()).unwrap_or(0);
                if let PopoutAction::Close = p.handle_key(key, n) {
                    self.close_popout();
                }
            }
            Overlay::Confirm(c) => {
                let kind = c.kind.clone();
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                        self.overlay = None;
                        self.confirm(kind, true);
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') => {
                        self.overlay = None;
                        self.confirm(kind, false);
                    }
                    KeyCode::Esc | KeyCode::Char('q') => {
                        self.overlay = None;
                        // Esc means "no" only where "no" does nothing
                        // drastic; for Quit, `confirm(Quit, false)` would
                        // quit without saving.
                        if matches!(kind, ConfirmKind::OpenDiscard { .. }) {
                            self.confirm(kind, false);
                        }
                    }
                    _ => {}
                }
            }
            Overlay::Prompt(p) => match key.code {
                KeyCode::Esc => {
                    let kind = p.kind.clone();
                    self.overlay = None;
                    if matches!(kind, PromptKind::StdinReply)
                        && let Some(k) = &self.kernel
                    {
                        k.send(KernelCommand::Interrupt);
                    }
                }
                KeyCode::Enter => {
                    let kind = p.kind.clone();
                    let value = p.value.clone();
                    self.overlay = None;
                    self.submit_prompt(kind, value);
                }
                KeyCode::Backspace => {
                    p.value.pop();
                }
                KeyCode::Char('u') if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) => p.value.clear(),
                KeyCode::Char(c) if !key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) => p.value.push(c),
                _ => {}
            },
            Overlay::Files(p) => match p.handle_key(key) {
                FileAction::None => {}
                FileAction::Close => self.overlay = None,
                FileAction::Open(path) => {
                    // The dashboard's placeholder notebook holds nothing to lose.
                    let home = matches!(self.screen, Screen::Dashboard(_));
                    if self.nb.dirty && !home {
                        let start = p.start.clone();
                        self.overlay = Some(Overlay::Confirm(ConfirmState {
                            message: format!("Discard unsaved changes and open {}? [y/n]", path.display()),
                            kind: ConfirmKind::OpenDiscard { path, reopen: Some(start) },
                        }));
                    } else {
                        self.overlay = None;
                        self.open_notebook(path);
                    }
                }
                FileAction::PrefsChanged(prefs) => self.picker_prefs = prefs,
            },
            Overlay::Cells(p) => match p.handle_key(key) {
                PickerAction::None => {}
                PickerAction::Close => self.overlay = None,
                PickerAction::Go(idx) => {
                    self.overlay = None;
                    self.set_focus(idx);
                }
            },
            Overlay::Palette(p) => match p.handle_key(key) {
                PaletteAction::None => {
                    if p.kind == PaletteKind::Themes {
                        self.preview_selected_theme();
                    }
                }
                PaletteAction::Close => {
                    self.overlay = None;
                    if let Some(t) = self.theme_before_pick.take()
                        && t.name != self.theme.name
                    {
                        self.apply_theme(t);
                    }
                }
                PaletteAction::Accept(data) => {
                    self.overlay = None;
                    self.theme_before_pick = None;
                    self.on_palette_accept(data);
                }
            },
        }
    }

    // ----- commands ---------------------------------------------------------

    fn dispatch(&mut self, cmd: Command) {
        match cmd {
            Command::FocusNext => self.set_focus(self.focused + 1),
            Command::FocusPrev => self.set_focus(self.focused.saturating_sub(1)),
            Command::FocusFirst => self.set_focus(0),
            Command::FocusLast => self.set_focus(self.nb.cells.len().saturating_sub(1)),
            Command::GoToCell => {
                if self.mode == Mode::Edit {
                    return;
                }
                self.open_cell_picker();
            }
            Command::HalfPageDown | Command::HalfPageUp => {
                self.ensure_layout(self.view.width.max(1));
                let h = self.view_height() / 2;
                if cmd == Command::HalfPageDown {
                    self.scroll_row += h.max(1);
                } else {
                    self.scroll_row = self.scroll_row.saturating_sub(h.max(1));
                }
                self.clamp_scroll(self.view_height());
                let idx = self.layout.cell_at_row(self.scroll_row + SCROLL_MARGIN);
                let old = self.focused;
                self.focused = idx.min(self.nb.cells.len().saturating_sub(1));
                self.layout.invalidate_cell(old);
                self.layout.invalidate_cell(self.focused);
            }
            Command::ScrollDown => {
                self.scroll_row += 1;
                self.clamp_scroll(self.view_height());
            }
            Command::ScrollUp => self.scroll_row = self.scroll_row.saturating_sub(1),
            Command::Search => self.open_search(),
            Command::SearchNext => self.search_step(true),
            Command::SearchPrev => self.search_step(false),
            Command::EditCell => self.enter_edit(false),
            Command::EditCellInsert => self.enter_edit(true),
            Command::LeaveEdit => self.leave_edit(),
            Command::Escape => {
                self.pending.clear();
                self.flash = None;
                self.search.hl = false;
            }
            Command::InsertAbove => self.insert_cell(self.focused, CellKind::Code),
            Command::InsertBelow => self.insert_cell(self.focused + 1, CellKind::Code),
            Command::DeleteCell | Command::CutCell => {
                let idx = self.focused;
                if let Some(cell) = self.nb.cells.get(idx) {
                    let snapshot = cell.clone();
                    self.clipboard = vec![snapshot.clone()];
                    self.cache.invalidate_cell(&snapshot.id);
                    self.nb.apply(CellOp::Delete { idx, cell: snapshot });
                    self.layout.removed(idx);
                    if self.nb.cells.len() == 1 && self.layout.layout(0).is_none() {
                        self.layout.inserted(0);
                    }
                    self.set_focus(idx);
                    self.layout.invalidate_all();
                }
            }
            Command::CopyCell => {
                if let Some(cell) = self.nb.cells.get(self.focused) {
                    self.clipboard = vec![cell.clone()];
                    self.flash("cell copied");
                }
            }
            Command::PasteBelow | Command::PasteAbove => {
                if self.clipboard.is_empty() {
                    self.flash("nothing to paste");
                    return;
                }
                let first = if cmd == Command::PasteBelow { self.focused + 1 } else { self.focused };
                for (i, c) in self.clipboard.clone().into_iter().enumerate() {
                    self.nb.apply(CellOp::Insert { idx: first + i, cell: c.duplicate() });
                    self.layout.inserted(first + i);
                }
                self.set_focus(first);
            }
            Command::MoveCellUp | Command::MoveCellDown => {
                let from = self.focused;
                let to = if cmd == Command::MoveCellUp { from.checked_sub(1) } else { Some(from + 1) };
                if let Some(to) = to
                    && to < self.nb.cells.len()
                {
                    self.nb.apply(CellOp::Move { from, to });
                    self.layout.moved(from, to);
                    self.set_focus(to);
                }
            }
            Command::ToCode | Command::ToMarkdown | Command::ToRaw => {
                let to = match cmd {
                    Command::ToCode => CellKind::Code,
                    Command::ToMarkdown => CellKind::Markdown,
                    _ => CellKind::Raw,
                };
                let idx = self.focused;
                if let Some(cell) = self.nb.cells.get(idx)
                    && cell.kind != to
                {
                    let id = cell.id.clone();
                    let from = cell.kind;
                    self.nb.apply(CellOp::SetKind { idx, from, to });
                    self.cache.invalidate_cell(&id);
                    self.layout.invalidate_cell(idx);
                }
            }
            Command::UndoCellOp => {
                if self.mode == Mode::Edit {
                    self.leave_edit();
                }
                match self.nb.undo.undo(&mut self.nb.cells) {
                    Some(f) => {
                        self.nb.dirty = true;
                        self.cache.invalidate_all();
                        self.layout.invalidate_all();
                        self.set_focus(f);
                    }
                    None => self.flash("nothing to undo"),
                }
            }
            Command::RedoCellOp => match self.nb.undo.redo(&mut self.nb.cells) {
                Some(f) => {
                    self.nb.dirty = true;
                    self.cache.invalidate_all();
                    self.layout.invalidate_all();
                    self.set_focus(f);
                }
                None => self.flash("nothing to redo"),
            },
            Command::ToggleOutputs => self.toggle_outputs(),
            Command::PopoutOutput => self.open_popout(),
            Command::ClearOutputs => {
                let idx = self.focused;
                if let Some(cell) = self.nb.cells.get_mut(idx) {
                    if !cell.outputs.is_empty() || cell.execution_count.is_some() {
                        self.nb.dirty = true;
                    }
                    cell.outputs.clear();
                    cell.execution_count = None;
                    cell.exec = ExecState::Idle;
                    let id = cell.id.clone();
                    self.cache.invalidate_outputs(&id);
                    self.layout.invalidate_cell(idx);
                }
            }
            Command::ClearAllOutputs => {
                for cell in &mut self.nb.cells {
                    if !cell.outputs.is_empty() || cell.execution_count.is_some() {
                        self.nb.dirty = true;
                    }
                    cell.outputs.clear();
                    cell.execution_count = None;
                    cell.exec = ExecState::Idle;
                }
                self.cache.invalidate_all();
                self.layout.invalidate_all();
            }
            Command::MergeCellBelow => self.merge_below(),
            Command::SplitCell => self.split_cell(),
            Command::RunCell => {
                let idx = self.focused;
                self.run_cells(&[idx], false);
            }
            Command::RunAndAdvance => {
                let idx = self.focused;
                self.run_cells(&[idx], true);
                if idx + 1 >= self.nb.cells.len() {
                    self.insert_cell(idx + 1, CellKind::Code);
                } else {
                    self.set_focus(idx + 1);
                }
            }
            Command::RunAndInsert => {
                let idx = self.focused;
                self.run_cells(&[idx], true);
                self.insert_cell(idx + 1, CellKind::Code);
            }
            Command::RunAll => {
                let all: Vec<usize> = (0..self.nb.cells.len()).collect();
                self.run_cells(&all, true);
            }
            Command::RunAllAbove => {
                let upto: Vec<usize> = (0..self.focused).collect();
                self.run_cells(&upto, true);
            }
            Command::RunAllBelow => {
                let from: Vec<usize> = (self.focused..self.nb.cells.len()).collect();
                self.run_cells(&from, true);
            }
            Command::InterruptKernel => match &self.kernel {
                Some(k) => {
                    k.send(KernelCommand::Interrupt);
                    self.flash("interrupt sent");
                }
                None => self.flash("no kernel"),
            },
            Command::RestartKernel => {
                if self.kernel.is_some() {
                    self.overlay = Some(Overlay::Confirm(ConfirmState {
                        message: "Restart the kernel? All variables will be lost. [y/n]".into(),
                        kind: ConfirmKind::RestartKernel,
                    }));
                } else {
                    self.resolve_kernel();
                }
            }
            Command::RestartAndRunAll => {
                self.overlay = Some(Overlay::Confirm(ConfirmState {
                    message: "Restart the kernel and run all cells? [y/n]".into(),
                    kind: ConfirmKind::RestartAndRunAll,
                }));
            }
            Command::ShutdownKernel => {
                if let Some(k) = self.kernel.take() {
                    k.send(KernelCommand::Shutdown);
                    self.reset_exec_states();
                    self.flash("kernel shut down");
                }
            }
            Command::SelectKernel => self.open_kernel_picker(),
            Command::SelectTheme => self.open_theme_picker(),
            Command::Save => self.save(),
            Command::SaveAs => {
                let current = self.nb.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                self.overlay = Some(Overlay::Prompt(PromptState {
                    title: "Save as (path)".into(),
                    value: current,
                    password: false,
                    kind: PromptKind::SaveAs,
                }));
            }
            Command::OpenFile => self.open_file_picker(self.cwd(), None),
            Command::Reload => {
                if self.nb.path.is_none() {
                    self.flash("notebook has no file");
                } else if self.nb.dirty {
                    self.overlay = Some(Overlay::Confirm(ConfirmState {
                        message: "Discard unsaved changes and reload from disk? [y/n]".into(),
                        kind: ConfirmKind::ReloadDiscard,
                    }));
                } else {
                    self.confirm(ConfirmKind::ReloadDiscard, true);
                }
            }
            Command::NewNotebook => {
                let dir = self.cwd();
                if self.nb.dirty {
                    self.overlay = Some(Overlay::Confirm(ConfirmState {
                        message: "Discard unsaved changes and create a new notebook? [y/n]".into(),
                        kind: ConfirmKind::NewDiscard(dir),
                    }));
                } else {
                    self.confirm(ConfirmKind::NewDiscard(dir), true);
                }
            }
            Command::Dashboard => {
                if self.mode == Mode::Edit {
                    self.leave_edit();
                }
                if self.nb.dirty {
                    self.overlay = Some(Overlay::Confirm(ConfirmState {
                        message: "Discard unsaved changes and return to the dashboard? [y/n]".into(),
                        kind: ConfirmKind::DashboardDiscard,
                    }));
                } else {
                    self.show_dashboard();
                }
            }
            Command::CommandPalette => self.open_command_palette(),
            Command::Help => self.overlay = Some(Overlay::Help),
            Command::ToggleLineNumbers => {
                self.line_numbers = !self.line_numbers;
                self.layout.invalidate_all();
            }
            Command::ToggleWrap => {
                self.cfg.wrap = !self.cfg.wrap;
                self.cache.invalidate_all();
                self.layout.invalidate_all();
            }
            Command::ShowLogPath => {
                let p = self.log_path.display().to_string();
                self.flash(format!("log: {p}"));
            }
            Command::Quit => {
                if self.mode == Mode::Edit {
                    self.leave_edit();
                }
                if self.nb.dirty {
                    self.overlay = Some(Overlay::Confirm(ConfirmState {
                        message: "Unsaved changes. Save before quitting? [y] save & quit  [n] discard  [Esc] cancel".into(),
                        kind: ConfirmKind::Quit,
                    }));
                } else {
                    self.quit = true;
                }
            }
            Command::ForceQuit => self.quit = true,
        }
    }

    fn confirm(&mut self, kind: ConfirmKind, yes: bool) {
        if let Some((start, path)) = reopen_after(&kind, yes) {
            self.open_file_picker(start, Some(path));
            return;
        }
        match kind {
            ConfirmKind::Quit => {
                if yes {
                    self.save();
                    if !self.nb.dirty {
                        self.quit = true;
                    }
                } else {
                    self.quit = true;
                }
            }
            ConfirmKind::RestartKernel => {
                if yes {
                    self.restart_kernel();
                }
            }
            ConfirmKind::RestartAndRunAll => {
                if yes {
                    self.restart_kernel();
                    let all: Vec<usize> = (0..self.nb.cells.len()).collect();
                    self.run_cells(&all, true);
                }
            }
            ConfirmKind::OpenDiscard { path, .. } => {
                if yes {
                    self.open_notebook(path);
                }
            }
            ConfirmKind::NewDiscard(dir) => {
                if yes {
                    self.overlay = Some(Overlay::Prompt(PromptState {
                        title: format!("New notebook name (in {})", dir.display()),
                        value: "Untitled.ipynb".into(),
                        password: false,
                        kind: PromptKind::NewNotebook { dir },
                    }));
                }
            }
            ConfirmKind::ReloadDiscard => {
                if yes && let Some(p) = self.nb.path.clone() {
                    self.open_notebook(p);
                }
            }
            ConfirmKind::DashboardDiscard => {
                if yes {
                    self.show_dashboard();
                }
            }
        }
    }

    fn submit_prompt(&mut self, kind: PromptKind, value: String) {
        match kind {
            PromptKind::StdinReply => {
                if let Some(k) = &self.kernel {
                    k.send(KernelCommand::InputReply { value });
                }
            }
            PromptKind::SaveAs => {
                let v = value.trim();
                if v.is_empty() {
                    return;
                }
                let mut p = PathBuf::from(v);
                if p.extension().is_none() {
                    p.set_extension("ipynb");
                }
                self.nb.path = Some(p);
                self.nb.dirty = true;
                self.save();
            }
            PromptKind::NewNotebook { dir } => {
                let v = value.trim();
                if v.is_empty() {
                    return;
                }
                let mut p = dir.join(v);
                if p.extension().is_none() {
                    p.set_extension("ipynb");
                }
                self.close_notebook();
                self.nb = Notebook::empty();
                self.nb.path = Some(p);
                self.nb.dirty = true;
                self.resolve_kernel();
            }
        }
    }

    // ----- editing ----------------------------------------------------------

    fn enter_edit(&mut self, insert: bool) {
        let idx = self.focused;
        let Some(cell) = self.nb.cells.get_mut(idx) else { return };
        let snapshot = cell.clone();
        let ed = cell.editor.get_or_insert_with(|| CellEditor::new(&snapshot.source));
        ed.set_mode(if insert { EditorMode::Insert } else { EditorMode::Normal });
        self.edit_snapshot = Some((idx, snapshot));
        self.mode = Mode::Edit;
        self.pending.clear();
        self.layout.invalidate_cell(idx);
        self.scroll_to_cursor();
    }

    fn leave_edit(&mut self) {
        if self.mode != Mode::Edit {
            return;
        }
        let idx = self.focused;
        if let Some(cell) = self.nb.cells.get_mut(idx) {
            if cell.sync_from_editor() {
                self.nb.dirty = true;
            }
            if let Some(ed) = cell.editor.as_mut() {
                ed.set_mode(EditorMode::Normal);
            }
            if let Some((sidx, before)) = self.edit_snapshot.take()
                && sidx == idx
                && before.source != cell.source
            {
                let after = cell.clone();
                self.nb.undo.push(CellOp::Replace { idx, before, after });
            }
        }
        self.mode = Mode::Command;
        self.layout.invalidate_cell(idx);
        self.scroll_to_focused();
    }

    fn insert_cell(&mut self, idx: usize, kind: CellKind) {
        if self.mode == Mode::Edit {
            self.leave_edit();
        }
        let idx = idx.min(self.nb.cells.len());
        self.nb.apply(CellOp::Insert { idx, cell: Cell::new(kind, "") });
        self.layout.inserted(idx);
        self.set_focus(idx);
    }

    fn merge_below(&mut self) {
        if self.mode == Mode::Edit {
            self.leave_edit();
        }
        let idx = self.focused;
        if idx + 1 >= self.nb.cells.len() {
            self.flash("no cell below");
            return;
        }
        let below = self.nb.cells[idx + 1].clone();
        let before = self.nb.cells[idx].clone();
        let mut after = before.clone();
        after.set_source(format!("{}\n{}", before.source.trim_end_matches('\n'), below.source));
        after.outputs.extend(below.outputs.iter().cloned());
        self.nb.apply(CellOp::Replace { idx, before, after });
        self.nb.apply(CellOp::Delete { idx: idx + 1, cell: below });
        self.layout.removed(idx + 1);
        self.cache.invalidate_cell(&self.nb.cells[idx].id.clone());
        self.layout.invalidate_cell(idx);
        self.set_focus(idx);
    }

    fn split_cell(&mut self) {
        let idx = self.focused;
        let Some(cell) = self.nb.cells.get_mut(idx) else { return };
        let Some(ed) = cell.editor.as_ref() else {
            self.flash("split works while editing (cursor position)");
            return;
        };
        let text = ed.text();
        let off = ed.cursor_offset().min(text.len());
        let (head, tail) = text.split_at(off);
        let head = head.trim_end_matches('\n').to_string();
        let tail = tail.trim_start_matches('\n').to_string();
        let kind = cell.kind;
        self.leave_edit();
        let before = self.nb.cells[idx].clone();
        let mut after = before.clone();
        after.set_source(head);
        after.outputs.clear();
        self.nb.apply(CellOp::Replace { idx, before, after });
        self.nb.apply(CellOp::Insert { idx: idx + 1, cell: Cell::new(kind, tail) });
        self.layout.inserted(idx + 1);
        self.cache.invalidate_cell(&self.nb.cells[idx].id.clone());
        self.layout.invalidate_cell(idx);
        self.set_focus(idx + 1);
    }

    fn toggle_outputs(&mut self) {
        let idx = self.focused;
        let max = self.cfg.stream_max_lines;
        let Some(cell) = self.nb.cells.get(idx) else { return };
        if cell.outputs.is_empty() {
            self.flash("no outputs");
            return;
        }
        let truncated = cell.outputs.iter().any(|o| match o {
            Output::Stream { text, .. } => text.lines().count() > max,
            Output::Error { traceback, .. } => traceback.len() > max,
            _ => false,
        });
        let collapsed = cell.outputs_collapsed;
        let expanded = cell.outputs_expanded;
        if collapsed {
            self.nb.cells[idx].outputs_collapsed = false;
        } else if truncated && !expanded {
            self.nb.cells[idx].outputs_expanded = true;
        } else if self.plotly_open_in_browser(idx) {
            return;
        } else {
            let cell = &mut self.nb.cells[idx];
            cell.outputs_collapsed = true;
            cell.outputs_expanded = false;
        }
        let id = self.nb.cells[idx].id.clone();
        self.cache.invalidate_outputs(&id);
        self.layout.invalidate_cell(idx);
        self.scroll_to_focused();
    }

    fn open_popout(&mut self) {
        if self.mode == Mode::Edit {
            self.leave_edit();
        }
        let idx = self.focused;
        let Some(cell) = self.nb.cells.get(idx) else { return };
        if cell.outputs.is_empty() {
            self.flash("no outputs");
            return;
        }
        let output = PopoutState::default_output(cell);
        self.overlay = Some(Overlay::Popout(PopoutState::new(idx, cell.id.clone(), output, self.cfg.wrap)));
    }

    fn close_popout(&mut self) {
        if let Some(Overlay::Popout(p)) = self.overlay.take() {
            p.close(&mut self.images, &mut self.plotly);
        }
    }

    /// If the cell has a plotly figure that could not be rendered, write its
    /// HTML to a temp file and open it. Returns true if something was opened.
    fn plotly_open_in_browser(&mut self, idx: usize) -> bool {
        let Some(cell) = self.nb.cells.get(idx) else { return false };
        if self.plotly.fatal.is_none() {
            return false;
        }
        for o in &cell.outputs {
            let Some(media) = o.media() else { continue };
            let html = media.content.iter().find_map(|m| match m {
                jupyter_protocol::MediaType::Html(h) => Some(h.clone()),
                _ => None,
            });
            let has_plotly = media.content.iter().any(|m| matches!(m, jupyter_protocol::MediaType::Plotly(_)));
            if has_plotly && let Some(html) = html {
                let dir = dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("saturn");
                let _ = std::fs::create_dir_all(&dir);
                let path = dir.join(format!("plotly-{}.html", crate::render::hash_bytes(html.as_bytes())));
                let doc = if html.contains("<html") {
                    html
                } else {
                    format!(
                        "<!doctype html><html><head><meta charset=\"utf-8\"><script src=\"https://cdn.plot.ly/plotly-latest.min.js\"></script></head><body>{html}</body></html>"
                    )
                };
                if std::fs::write(&path, doc).is_ok() {
                    let _ = open::that_detached(&path);
                    self.flash(format!("opened {}", path.display()));
                    return true;
                }
            }
        }
        false
    }

    // ----- execution --------------------------------------------------------

    fn run_cells(&mut self, indices: &[usize], leave_edit: bool) {
        if leave_edit && self.mode == Mode::Edit {
            self.leave_edit();
        } else if self.mode == Mode::Edit {
            let idx = self.focused;
            if let Some(cell) = self.nb.cells.get_mut(idx)
                && cell.sync_from_editor()
            {
                self.nb.dirty = true;
            }
        }
        let Some(kernel) = &self.kernel else {
            self.flash_err("no kernel: press K to select one");
            return;
        };
        if kernel.status == KernelStatus::Dead {
            self.flash_err("kernel is dead: 00 restarts it");
            return;
        }
        for &idx in indices {
            let Some(cell) = self.nb.cells.get_mut(idx) else { continue };
            if cell.kind != CellKind::Code {
                continue;
            }
            let code = cell.text();
            if code.trim().is_empty() {
                continue;
            }
            cell.outputs.clear();
            cell.execution_count = None;
            cell.exec = ExecState::Queued;
            cell.clear_pending = false;
            cell.outputs_expanded = false;
            let id = cell.id.clone();
            self.nb.dirty = true;
            self.cache.invalidate_outputs(&id);
            self.layout.invalidate_cell(idx);
            kernel.send(KernelCommand::Execute { cell_id: id, code });
        }
    }

    fn reset_exec_states(&mut self) {
        for c in &mut self.nb.cells {
            if matches!(c.exec, ExecState::Queued | ExecState::Running) {
                c.exec = ExecState::Idle;
            }
        }
        self.layout.invalidate_all();
    }

    fn on_kernel(&mut self, ev: KernelEvent) {
        match ev {
            KernelEvent::Status(s) => {
                if let Some(k) = &mut self.kernel {
                    k.status = s;
                }
            }
            KernelEvent::Ready { implementation, language, version } => {
                // A kernel shut down while starting (the notebook was closed)
                // can still report in; it must not touch the notebook now open.
                let Some(k) = &mut self.kernel else { return };
                k.status = KernelStatus::Idle;
                k.language = language.clone();
                if let Some(code) = startup::startup_code(&language) {
                    k.send(KernelCommand::ExecuteSilent { code });
                }
                self.lang_ext = language_extension(&language).to_string();
                self.nb.set_language_info(&language, &version);
                self.cache.invalidate_all();
                self.layout.invalidate_all();
                self.flash(format!("kernel ready: {implementation} ({language} {version})"));
            }
            KernelEvent::ExecuteStarted { cell_id } => {
                if let Some(idx) = self.nb.index_of(&cell_id) {
                    self.nb.cells[idx].exec = ExecState::Running;
                    self.layout.invalidate_cell(idx);
                }
            }
            KernelEvent::ExecuteInput { cell_id, execution_count } => {
                if let Some(idx) = self.nb.index_of(&cell_id) {
                    self.nb.cells[idx].execution_count = Some(execution_count);
                    self.layout.invalidate_cell(idx);
                }
            }
            KernelEvent::Output { cell_id, output } => {
                if let Some(idx) = self.nb.index_of(&cell_id) {
                    let cell = &mut self.nb.cells[idx];
                    if cell.clear_pending {
                        cell.outputs.clear();
                        cell.clear_pending = false;
                        self.cache.invalidate_outputs(&cell_id);
                    }
                    match output {
                        Output::Stream { name, text } => cell.push_stream(name, &text),
                        other => cell.outputs.push(other),
                    }
                    self.layout.invalidate_cell(idx);
                    if self.focused == idx && self.mode == Mode::Command {
                        self.scroll_to_focused();
                    }
                }
            }
            KernelEvent::UpdateDisplay { display_id, data, metadata } => {
                let mut changed = Vec::new();
                for (idx, cell) in self.nb.cells.iter_mut().enumerate() {
                    for o in &mut cell.outputs {
                        if let Output::DisplayData { display_id: Some(id), data: d, metadata: m } = o
                            && *id == display_id
                        {
                            *d = data.clone();
                            *m = metadata.clone();
                            changed.push((idx, cell.id.clone()));
                        }
                    }
                }
                for (idx, id) in changed {
                    self.cache.invalidate_outputs(&id);
                    self.layout.invalidate_cell(idx);
                }
            }
            KernelEvent::ClearOutput { cell_id, wait } => {
                if let Some(idx) = self.nb.index_of(&cell_id) {
                    let cell = &mut self.nb.cells[idx];
                    if wait {
                        cell.clear_pending = true;
                    } else {
                        cell.outputs.clear();
                        self.cache.invalidate_outputs(&cell_id);
                        self.layout.invalidate_cell(idx);
                    }
                }
            }
            KernelEvent::ExecuteDone { cell_id, status, execution_count } => {
                if let Some(idx) = self.nb.index_of(&cell_id) {
                    let cell = &mut self.nb.cells[idx];
                    cell.exec = match status {
                        ReplyStatus::Ok => ExecState::Done,
                        ReplyStatus::Error => ExecState::Error,
                        ReplyStatus::Aborted => ExecState::Idle,
                    };
                    if let Some(c) = execution_count {
                        cell.execution_count = Some(c);
                    }
                    self.layout.invalidate_cell(idx);
                }
                if status != ReplyStatus::Ok {
                    self.reset_exec_states();
                }
            }
            KernelEvent::InputRequest { cell_id, prompt, password } => {
                if let Some(idx) = cell_id.and_then(|id| self.nb.index_of(&id)) {
                    self.set_focus(idx);
                }
                self.overlay = Some(Overlay::Prompt(PromptState {
                    title: if prompt.is_empty() { "input".into() } else { prompt },
                    value: String::new(),
                    password,
                    kind: PromptKind::StdinReply,
                }));
            }
            KernelEvent::QueueChanged { queued } => {
                for (idx, c) in self.nb.cells.iter_mut().enumerate() {
                    if c.exec == ExecState::Queued && !queued.contains(&c.id) && c.exec != ExecState::Running {
                        c.exec = ExecState::Idle;
                        self.layout.invalidate_cell(idx);
                    }
                }
            }
            KernelEvent::Died { reason } => {
                if let Some(k) = &mut self.kernel {
                    k.status = KernelStatus::Dead;
                }
                self.reset_exec_states();
                self.flash_err(format!("kernel died: {reason} (00 to restart)"));
            }
            KernelEvent::Log(msg) => self.flash(msg),
        }
    }

    // ----- kernels ----------------------------------------------------------

    fn resolve_kernel(&mut self) {
        let Some(specs) = &self.kernelspecs else {
            self.want_kernel = true;
            return;
        };
        if specs.is_empty() {
            self.flash_err("no Jupyter kernels found (pip install ipykernel)");
            return;
        }
        let wanted = self
            .opts_kernel
            .clone()
            .or_else(|| self.nb.kernelspec_name().map(str::to_string));
        let pick = wanted
            .as_deref()
            .and_then(|w| specs.iter().find(|s| s.kernel_name == w))
            .or_else(|| specs.iter().find(|s| s.kernel_name == "python3"))
            .or_else(|| if specs.len() == 1 { specs.first() } else { None })
            .cloned();
        match pick {
            Some(spec) => self.start_kernel(spec),
            None => {
                self.flash("select a kernel with K");
                self.open_kernel_picker();
            }
        }
    }

    fn start_kernel(&mut self, spec: KernelspecDir) {
        if let Some(old) = self.kernel.take() {
            old.send(KernelCommand::Shutdown);
        }
        self.reset_exec_states();
        self.nb.set_kernelspec(&spec.kernel_name, &spec.kernelspec.display_name, &spec.kernelspec.language);
        self.lang_ext = language_extension(&spec.kernelspec.language).to_string();
        let cwd = self.cwd();
        self.kernel = Some(KernelHandle::launch(spec, cwd, self.tx.clone()));
        self.cache.invalidate_all();
        self.layout.invalidate_all();
        if self.nb.mentions_plotly() {
            self.plotly.warm();
        }
    }

    fn restart_kernel(&mut self) {
        match &self.kernel {
            Some(k) if k.is_alive() => {
                k.send(KernelCommand::Restart);
                self.reset_exec_states();
            }
            Some(k) => {
                // dead actor: relaunch from spec
                let spec = k.spec.clone();
                self.start_kernel(spec);
            }
            None => self.resolve_kernel(),
        }
    }

    /// `gt`: list every cell with its number, first source line and output
    /// excerpt; the band marks the cells on screen.
    fn open_cell_picker(&mut self) {
        if self.nb.cells.is_empty() {
            return;
        }
        if self.view.width > 0 {
            self.ensure_layout(self.view.width);
        }
        let h = self.view_height();
        let first = self.layout.cell_at_row(self.scroll_row);
        let last = self.layout.cell_at_row(self.scroll_row + h.saturating_sub(1));
        let state = CellPickerState::new(&self.nb.cells, self.cfg.tab_width, self.focused, (first, last));
        self.overlay = Some(Overlay::Cells(state));
    }

    fn open_kernel_picker(&mut self) {
        let current = self.kernel.as_ref().map(|k| k.name().to_string());
        let items: Vec<PaletteItem> = match &self.kernelspecs {
            Some(specs) => specs
                .iter()
                .enumerate()
                .map(|(i, s)| PaletteItem {
                    label: s.kernelspec.display_name.clone(),
                    detail: format!(
                        "{} · {}{}",
                        s.kernel_name,
                        s.kernelspec.language,
                        if current.as_deref() == Some(s.kernel_name.as_str()) { " · current" } else { "" }
                    ),
                    data: ItemData::Kernel(i),
                })
                .collect(),
            None => Vec::new(),
        };
        let title = if self.kernelspecs.is_none() { "Select kernel (scanning…)" } else { "Select kernel" };
        self.overlay = Some(Overlay::Palette(PaletteState::new(PaletteKind::Kernels, title, items)));
    }

    /// Theme picker with live preview: the theme follows the cursor, `Esc`
    /// restores the one in use before, `Enter` keeps the selection.
    fn open_theme_picker(&mut self) {
        let current = self.theme.name.clone();
        self.theme_before_pick = Some(self.theme.clone());
        let items = crate::theme::available()
            .into_iter()
            .map(|(name, src)| PaletteItem {
                detail: format!("{}{}", src.label(), if name == current { " · current" } else { "" }),
                data: ItemData::Theme(name.clone()),
                label: name,
            })
            .collect();
        let mut p = PaletteState::new(PaletteKind::Themes, "Select theme ⋅ ↑↓ preview, Enter keep, Esc revert", items);
        if let Some(i) = p.items.iter().position(|it| it.label == current) {
            p.selected = i;
        }
        self.overlay = Some(Overlay::Palette(p));
    }

    /// Apply the theme under the picker's cursor without closing it.
    fn preview_selected_theme(&mut self) {
        let Some(Overlay::Palette(p)) = &self.overlay else { return };
        let Some(PaletteItem { data: ItemData::Theme(name), .. }) = p.selected_item() else { return };
        if *name == self.theme.name {
            return;
        }
        match Theme::load(name) {
            Ok(t) => self.apply_theme(t),
            Err(e) => tracing::warn!("theme preview {name}: {e:#}"),
        }
    }

    fn apply_theme(&mut self, theme: Theme) {
        self.theme = theme;
        self.cache.invalidate_all();
        self.layout.invalidate_all();
        self.dirty_ui = true;
    }

    /// Switch themes and remember the choice in config.toml, so the next
    /// launch starts with it.
    fn set_theme(&mut self, name: &str) {
        match Theme::load(name) {
            Ok(t) => {
                self.apply_theme(t);
                self.cfg.theme = name.to_string();
                match self.cfg.save_theme() {
                    Ok(path) => {
                        let file = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
                        self.flash(format!("theme {name} · saved to {file}"));
                    }
                    Err(e) => self.flash_err(format!("theme {name} applied but not saved: {e:#}")),
                }
            }
            Err(e) => self.flash_err(format!("theme {name}: {e:#}")),
        }
    }

    fn open_command_palette(&mut self) {
        let items = PALETTE
            .iter()
            .map(|c| PaletteItem { label: c.name.to_string(), detail: c.keys.to_string(), data: ItemData::Command(c.cmd) })
            .collect();
        self.overlay = Some(Overlay::Palette(PaletteState::new(PaletteKind::Commands, "Command palette", items)));
    }

    /// What every files picker needs: its context and its worker threads.
    fn picker_parts(&self) -> (PickerCtx, Box<ThreadIo>) {
        let tx = self.tx.clone();
        let sink: crate::files::Sink = std::sync::Arc::new(move |ev| {
            let _ = tx.send(AppEvent::Files(ev));
        });
        let io = ThreadIo { worker: PreviewWorker::spawn(sink.clone()), sink };
        let ui = &self.theme.ui;
        let ctx = PickerCtx {
            home: dirs::home_dir(),
            syn: std::sync::Arc::new(self.theme.syntax.clone()),
            ui: UiColors { muted: ui.muted, input_bg: ui.input_bg, line_numbers: ui.line_numbers },
            tab_width: self.cfg.tab_width,
            icons: self.cfg.icons,
        };
        (ctx, Box::new(io))
    }

    fn open_file_picker(&mut self, start: PathBuf, preselect: Option<PathBuf>) {
        let (ctx, io) = self.picker_parts();
        let picker = FilePickerState::new(
            start,
            self.nb.path.clone(),
            self.nb.dirty,
            preselect,
            self.picker_prefs,
            ctx,
            io,
        );
        self.overlay = Some(Overlay::Files(Box::new(picker)));
    }

    /// The files picker over the recent-notebooks list, newest first.
    fn open_recent_picker(&mut self) {
        let recent = self.recent_path.as_deref().map(Recent::load).unwrap_or_default();
        let home = dirs::home_dir();
        let entries = recent
            .paths()
            .map(|p| FileEntry::new(p.to_path_buf(), crate::files::tilde(p, home.as_deref()), false))
            .collect();
        let (ctx, io) = self.picker_parts();
        let picker = FilePickerState::fixed("Recent notebooks".into(), entries, self.nb.path.clone(), self.nb.dirty, ctx, io);
        self.overlay = Some(Overlay::Files(Box::new(picker)));
    }

    /// Record `path` in the recent-notebooks list. Failures are only logged.
    fn remember(&self, path: &Path) {
        let Some(store) = &self.recent_path else { return };
        if let Err(e) = Recent::remember(store, path) {
            tracing::warn!("recent notebooks: {e:#}");
        }
    }

    // ----- dashboard --------------------------------------------------------

    /// Close the notebook (and its kernel) and show the start dashboard.
    fn show_dashboard(&mut self) {
        self.close_notebook();
        self.nb = Notebook::empty();
        self.lang_ext = "py".to_string();
        self.want_kernel = false;
        self.overlay = None;
        self.screen = Screen::Dashboard(DashboardState::default());
        self.spawn_project_scan();
        self.dirty_ui = true;
    }

    fn on_dash(&mut self, action: DashAction) {
        let DashAction::Run(item) = action else { return };
        match item {
            DashItem::FindFiles => self.open_file_picker(self.cwd(), None),
            DashItem::NewNotebook => {
                self.close_notebook();
                self.nb = Notebook::empty();
                self.lang_ext = "py".to_string();
                self.screen = Screen::Notebook;
                self.resolve_kernel();
            }
            DashItem::Projects => self.open_projects_picker(),
            DashItem::Recent => self.open_recent_picker(),
            DashItem::Theme => self.open_theme_picker(),
            DashItem::Quit => self.quit = true,
        }
    }

    fn spawn_project_scan(&mut self) {
        if self.projects_scanning {
            return;
        }
        self.projects_scanning = true;
        let roots = self.cfg.project_root_paths();
        let tx = self.tx.clone();
        let spawned = std::thread::Builder::new().name("saturn-projects".into()).spawn(move || {
            let home = dirs::home_dir();
            let list = crate::files::projects::scan(&roots, crate::files::projects::PROJECT_DEPTH, home.as_deref());
            let _ = tx.send(AppEvent::Projects(list));
        });
        if let Err(e) = spawned {
            tracing::warn!("projects scan: {e}");
            self.projects_scanning = false;
        }
    }

    /// The Projects palette's title and rows.
    fn project_items(&self) -> (String, Vec<PaletteItem>) {
        let home = dirs::home_dir();
        let now = std::time::SystemTime::now();
        let items: Vec<PaletteItem> = self
            .projects
            .iter()
            .flatten()
            .map(|p| {
                let mut detail = crate::files::tilde(&p.path, home.as_deref());
                if let Some(t) = p.modified {
                    detail.push_str(" · ");
                    detail.push_str(&crate::files::projects::ago(t, now));
                }
                if p.worktree_of.is_some() {
                    detail.push_str(" · worktree");
                }
                PaletteItem { label: p.name.clone(), detail, data: ItemData::Project(p.path.clone()) }
            })
            .collect();
        let title = if self.projects_scanning && items.is_empty() {
            "Projects (scanning…)".to_string()
        } else if items.is_empty() {
            "Projects: none found (project_roots in config.toml)".to_string()
        } else {
            format!("Projects ({})", items.len())
        };
        (title, items)
    }

    fn open_projects_picker(&mut self) {
        if self.projects.is_none() && !self.projects_scanning {
            self.spawn_project_scan();
        }
        let (title, items) = self.project_items();
        self.overlay = Some(Overlay::Palette(PaletteState::new(PaletteKind::Projects, title, items)));
    }

    fn on_palette_accept(&mut self, data: ItemData) {
        match data {
            ItemData::Command(cmd) => self.dispatch(cmd),
            ItemData::Theme(name) => self.set_theme(&name),
            ItemData::Project(path) => self.open_file_picker(path, None),
            ItemData::Kernel(i) => {
                let spec = self.kernelspecs.as_ref().and_then(|s| s.get(i)).cloned();
                if let Some(spec) = spec {
                    if self.kernel.as_ref().map(|k| k.name() == spec.kernel_name && k.is_alive()).unwrap_or(false) {
                        self.flash("kernel already running");
                    } else {
                        self.start_kernel(spec);
                    }
                }
            }
        }
    }

    // ----- files ------------------------------------------------------------

    fn close_notebook(&mut self) {
        if self.mode == Mode::Edit {
            self.leave_edit();
        }
        if let Some(k) = self.kernel.take() {
            k.send(KernelCommand::Shutdown);
        }
        self.cache.invalidate_all();
        self.layout = LayoutCache::default();
        self.focused = 0;
        self.scroll_row = 0;
        self.edit_snapshot = None;
        self.mode = Mode::Command;
        self.search.matches.clear();
        self.search.current = None;
        self.search.generation = None;
    }

    fn load_notebook(&mut self, path: &Path) -> Result<()> {
        let nb = nbio::load(path)?;
        self.close_notebook();
        self.nb = nb;
        self.screen = Screen::Notebook;
        self.remember(path);
        if let Some(lang) = self.nb.language() {
            self.lang_ext = language_extension(lang).to_string();
        }
        self.want_kernel = true;
        // Launching the sidecar's browser takes over a second; get it going
        // while the notebook is still being laid out.
        if self.nb.has_plotly_outputs() {
            self.plotly.warm();
        }
        Ok(())
    }

    fn open_notebook(&mut self, path: PathBuf) {
        match self.load_notebook(&path) {
            Ok(()) => {
                let t = self.nb.title();
                self.flash(format!("opened {t}"));
                if self.kernelspecs.is_some() {
                    self.want_kernel = false;
                    self.resolve_kernel();
                }
            }
            Err(e) => self.flash_err(format!("{e:#}")),
        }
    }

    fn save(&mut self) {
        if self.mode == Mode::Edit {
            let idx = self.focused;
            if let Some(cell) = self.nb.cells.get_mut(idx)
                && cell.sync_from_editor()
            {
                self.nb.dirty = true;
            }
        }
        let Some(path) = self.nb.path.clone() else {
            self.dispatch(Command::SaveAs);
            return;
        };
        match nbio::save(&self.nb, &path) {
            Ok(()) => {
                self.nb.dirty = false;
                self.remember(&path);
                self.flash(format!("saved {}", path.display()));
            }
            Err(e) => self.flash_err(format!("save failed: {e:#}")),
        }
    }

    /// Store sidecar-rendered PNGs into the plotly outputs so they persist.
    fn persist_plotly_pngs(&mut self) {
        if !self.cfg.persist_plotly_png {
            return;
        }
        use jupyter_protocol::MediaType;
        let mut changed = false;
        for cell in &mut self.nb.cells {
            for o in &mut cell.outputs {
                let Some(media) = o.media_mut() else { continue };
                let has_png = media.content.iter().any(|m| matches!(m, MediaType::Png(_)));
                if has_png {
                    continue;
                }
                let fig = media.content.iter().find_map(|m| match m {
                    MediaType::Plotly(v) => Some(v.clone()),
                    _ => None,
                });
                let Some(fig) = fig else { continue };
                let hash = crate::render::hash_bytes(fig.to_string().as_bytes());
                if let Some(png) = self.plotly.cached(hash) {
                    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &png[..]);
                    media.content.push(MediaType::Png(b64));
                    changed = true;
                }
            }
        }
        if changed {
            self.nb.dirty = true;
        }
    }
}

// ----- search -------------------------------------------------------------

impl App {
    /// Recompute the matches if cells, widths or wrapping changed since the
    /// last time (called before every draw, cheap when nothing changed).
    pub fn ensure_matches(&mut self) {
        if self.search.query.is_empty() {
            if !self.search.matches.is_empty() {
                self.search.matches.clear();
                self.search.current = None;
            }
            return;
        }
        if self.search.generation == Some(self.layout.generation()) {
            return;
        }
        self.refresh_matches();
    }

    /// Find every occurrence of the pattern in the rendered rows of every
    /// cell's source; keeps the current match if it still exists.
    fn refresh_matches(&mut self) {
        if self.view.width == 0 {
            return;
        }
        self.ensure_layout(self.view.width);
        let cur = self.search.current_match();
        let mut matches = Vec::new();
        if !self.search.query.is_empty() {
            let pat = search::pattern(&self.search.query);
            let sensitive = search::case_sensitive(&self.search.query);
            let cw = content_width(self.view.width);
            let tab = self.cfg.tab_width;
            let wrap = self.cfg.wrap;
            let App { nb, cache, .. } = self;
            for (i, cell) in nb.cells.iter().enumerate() {
                let plain;
                let lines: &[ratatui::text::Line<'static>] = match cell.kind {
                    CellKind::Markdown => {
                        if cell.source.trim().is_empty() {
                            continue;
                        }
                        &cache.markdown(&cell.id, &cell.source, cw, tab).lines
                    }
                    // Same geometry as the highlighted text (character wrap
                    // after tab expansion), without highlighting every cell.
                    CellKind::Code | CellKind::Raw => {
                        let text = ui::text::plain(&ui::text::expand_tabs(&cell.source, tab), ratatui::style::Style::default());
                        plain = ui::text::wrap_text(text, cw as usize, tab, wrap);
                        &plain.lines
                    }
                };
                for (row, line) in lines.iter().enumerate() {
                    for (x, w) in search::find_in_line(line, &pat, sensitive) {
                        matches.push(Match { cell: i, row: row as u16, x, w });
                    }
                }
            }
        }
        self.search.current = cur.and_then(|c| matches.iter().position(|m| *m == c));
        self.search.matches = matches;
        self.search.generation = Some(self.layout.generation());
    }

    fn open_search(&mut self) {
        if self.mode == Mode::Edit {
            return;
        }
        self.pending.clear();
        self.search.input = Some(SearchInput {
            text: String::new(),
            from_focus: self.focused,
            from_scroll: self.scroll_row,
            prev_query: self.search.query.clone(),
            prev_hl: self.search.hl,
        });
    }

    fn on_search_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(crossterm::event::KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => self.cancel_search(),
            KeyCode::Char('c') if ctrl => self.cancel_search(),
            KeyCode::Enter => self.confirm_search(),
            KeyCode::Backspace => {
                let Some(inp) = &mut self.search.input else { return };
                if inp.text.is_empty() {
                    self.cancel_search();
                } else {
                    inp.text.pop();
                    self.search_preview();
                }
            }
            KeyCode::Char('u') if ctrl => {
                if let Some(inp) = &mut self.search.input {
                    inp.text.clear();
                }
                self.search_preview();
            }
            KeyCode::Char('w') if ctrl => {
                if let Some(inp) = &mut self.search.input {
                    let t = inp.text.trim_end_matches(' ');
                    let cut = t.rfind(' ').map(|i| i + 1).unwrap_or(0);
                    inp.text.truncate(cut);
                }
                self.search_preview();
            }
            KeyCode::Char('g') if ctrl => self.search_step_in_prompt(true),
            KeyCode::Char('t') if ctrl => self.search_step_in_prompt(false),
            KeyCode::Char(c) if !ctrl && !alt => {
                if let Some(inp) = &mut self.search.input {
                    inp.text.push(c);
                }
                self.search_preview();
            }
            _ => {}
        }
    }

    /// Incremental search: show the first match at or after where `/` was
    /// pressed, or go back there while nothing matches.
    fn search_preview(&mut self) {
        let Some(inp) = &self.search.input else { return };
        let from = inp.from_focus;
        let from_scroll = inp.from_scroll;
        self.search.query = inp.text.clone();
        self.search.hl = true;
        self.refresh_matches();
        let first = self.search.matches.iter().position(|m| m.cell >= from).or(if self.search.matches.is_empty() { None } else { Some(0) });
        match first {
            Some(k) => self.goto_match(k),
            None => {
                self.set_focus(from);
                self.scroll_row = from_scroll;
                self.clamp_scroll(self.view_height());
            }
        }
    }

    /// Ctrl+G / Ctrl+T: move between matches without leaving the prompt.
    fn search_step_in_prompt(&mut self, forward: bool) {
        let Some(inp) = &self.search.input else { return };
        if inp.text.is_empty() {
            return;
        }
        self.search_step(forward);
    }

    fn cancel_search(&mut self) {
        let Some(inp) = self.search.input.take() else { return };
        self.search.query = inp.prev_query;
        self.search.hl = inp.prev_hl;
        self.search.generation = None;
        self.set_focus(inp.from_focus);
        self.scroll_row = inp.from_scroll;
        self.clamp_scroll(self.view_height());
    }

    fn confirm_search(&mut self) {
        let Some(inp) = self.search.input.take() else { return };
        if inp.text.is_empty() {
            // `/` + Enter repeats the previous search, like vim.
            self.search.query = inp.prev_query;
            self.search.hl = inp.prev_hl;
            self.search.generation = None;
            self.search_step(true);
            return;
        }
        self.search.query = inp.text;
        self.search.hl = true;
        self.refresh_matches();
        if self.search.matches.is_empty() {
            let q = self.search.query.clone();
            self.flash_err(format!("pattern not found: {q}"));
        }
    }

    /// `n` / `N`: the next match after (before) the current one, or from the
    /// focused cell when the cursor moved since the last hit.
    fn search_step(&mut self, forward: bool) {
        if self.mode == Mode::Edit {
            return;
        }
        if self.search.query.is_empty() {
            self.flash_err("no previous search");
            return;
        }
        self.search.hl = true;
        self.refresh_matches();
        let n = self.search.matches.len();
        if n == 0 {
            let q = self.search.query.clone();
            self.flash_err(format!("pattern not found: {q}"));
            return;
        }
        let focused = self.focused;
        let (next, wrapped) = match self.search.current {
            Some(k) if k < n => {
                if forward {
                    ((k + 1) % n, k + 1 == n)
                } else {
                    ((k + n - 1) % n, k == 0)
                }
            }
            _ => {
                let m = &self.search.matches;
                if forward {
                    match m.iter().position(|x| x.cell >= focused) {
                        Some(k) => (k, false),
                        None => (0, true),
                    }
                } else {
                    match m.iter().rposition(|x| x.cell <= focused) {
                        Some(k) => (k, false),
                        None => (n - 1, true),
                    }
                }
            }
        };
        self.goto_match(next);
        if wrapped {
            self.flash(if forward { "search hit BOTTOM, continuing at TOP" } else { "search hit TOP, continuing at BOTTOM" });
        }
    }

    /// Focus the match's cell and bring its row into view.
    fn goto_match(&mut self, k: usize) {
        let Some(m) = self.search.matches.get(k).copied() else { return };
        self.set_focus(m.cell);
        self.ensure_layout(self.view.width.max(1));
        let row = self.layout.row_of(m.cell) + m.row as usize;
        self.scroll_into_view(row, row);
        self.search.current = Some(k);
    }
}

/// After "no" to discarding changes for a notebook chosen in the files
/// picker: the picker's `start` and the chosen path, to reopen the picker
/// with that file selected.
fn reopen_after(kind: &ConfirmKind, yes: bool) -> Option<(PathBuf, PathBuf)> {
    match kind {
        ConfirmKind::OpenDiscard { path, reopen: Some(start) } if !yes => Some((start.clone(), path.clone())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reopen_after_declined_discard() {
        let kind = ConfirmKind::OpenDiscard { path: PathBuf::from("/r/a.ipynb"), reopen: Some(PathBuf::from("/r")) };
        assert_eq!(reopen_after(&kind, false), Some((PathBuf::from("/r"), PathBuf::from("/r/a.ipynb"))));
        assert_eq!(reopen_after(&kind, true), None);
        assert_eq!(reopen_after(&ConfirmKind::NewDiscard(PathBuf::from("/r")), false), None);
    }
}
