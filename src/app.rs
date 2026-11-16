//! Application state, the event loop and command dispatch.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::style::Print;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, KeyEventKind};
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
use crate::editor::{CellEditor, comment_leader, language_extension};
use crate::vim::Request;
use crate::files::projects::Project;
use crate::files::{FileEntry, PickerPrefs};
use crate::files::preview::{PreviewWorker, UiColors};
use crate::ai::{AiCommand, AiConfig, AiEvent, AiHandle, Image, Permission, ToolContent, TurnOutcome, context, tools};
use crate::kernel::{KernelCommand, KernelEvent, KernelHandle, KernelStatus, ReplyStatus, startup};
use crate::keymap::{self, Key, KeyAction};
use crate::notebook::undo::CellOp;
use crate::notebook::{Cell, CellId, CellKind, ExecState, Notebook, Output, io as nbio};
use crate::render::RenderCache;
use crate::render::image::ImageStore;
use crate::render::plotly::PlotlySidecar;
use crate::recent::Recent;
use crate::search::{self, Match, SearchInput, SearchState};
use crate::ui;
use crate::ui::cell::content_width;
use crate::ui::cell_picker::{CellPickerState, PickerAction};
use crate::ui::chat::{Answer, Ask, ChatAction, ChatState};
use crate::ui::dashboard::{DashAction, DashItem, DashboardState};
use crate::ui::inline_prompt::{InlinePrompt, PromptAction};
use crate::ui::diff::{Kind as ProposalKind, Proposal};
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
    /// From the assistant's `claude` process.
    Ai(AiEvent),
    /// From the lean `claude` process behind the inline prompt.
    AiQuick(AiEvent),
    ImageReady { hash: u64, result: Result<SlicedProtocol, String> },
    PlotlyDone { hash: u64, result: Result<Vec<u8>, String> },
    PlotlyFatal(String),
    /// A frame (or the failure) of the popout's interactive plotly figure.
    /// The frame is a base64 PNG.
    PlotlyLive { sid: u64, result: Result<String, String> },
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

/// Which of the two assistant processes something belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lane {
    /// The panel's conversation.
    Chat,
    /// The inline prompt.
    Quick,
}

/// A tool call of the assistant that is answered later.
enum ToolWait {
    /// When the user settles this proposal.
    Proposal { lane: Lane, tool: String, proposal: u64 },
    /// When these cells have run.
    Run { lane: Lane, tool: String, cells: Vec<CellId> },
}

/// What a disabled assistant answers its keys with.
const ASSISTANT_OFF: &str = "the assistant is off: set enabled = true under [assistant] in config.toml";

/// Which pane of the notebook screen has the keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Notebook,
    /// The assistant panel.
    Chat,
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
    /// The assistant's `claude` process, once something has started it.
    pub ai: Option<AiHandle>,
    /// The panel conversation's session id.
    ai_session: Option<String>,
    /// A turn has finished in that session, so there is something on disk
    /// for a restarted process to resume.
    ai_has_turn: bool,
    /// The session the next panel process continues.
    ai_resume: Option<String>,
    /// Restart the panel's process when the running turn ends (a setting
    /// that only applies at start changed).
    ai_restart: bool,
    /// The lean `claude` process behind the inline prompt.
    quick: Option<AiHandle>,
    /// What that process has said in the turn that is running.
    quick_text: String,
    /// "claude · haiku", for the inline prompt.
    pub quick_label: String,
    /// The cell whose inline prompt has the keys.
    pub ask_cell: Option<CellId>,
    pub kernelspecs: Option<Vec<KernelspecDir>>,
    pub cfg: Config,
    pub theme: Theme,
    /// Theme in use before the theme picker opened; restored if the picker is cancelled.
    theme_before_pick: Option<Theme>,
    pub pending: Vec<Key>,
    pub clipboard: Vec<Cell>,
    pub flash: Option<Flash>,
    pub overlay: Option<Overlay>,
    /// The assistant's conversation, once the panel has been opened.
    pub chat: Option<ChatState>,
    /// The assistant panel is showing.
    pub chat_open: bool,
    pub focus: Focus,
    /// Id of the last proposed edit.
    next_proposal: u64,
    /// The assistant's tool calls that wait on the user or the kernel.
    ai_waits: Vec<ToolWait>,
    /// The keys go back to the panel once the proposals it made are settled.
    return_to_chat: bool,
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
            ai: None,
            ai_session: None,
            ai_has_turn: false,
            ai_resume: None,
            ai_restart: false,
            quick: None,
            quick_text: String::new(),
            quick_label: format!("claude · {}", cfg.assistant.inline_model),
            ask_cell: None,
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
            chat: None,
            chat_open: false,
            focus: Focus::Notebook,
            next_proposal: 0,
            ai_waits: Vec::new(),
            return_to_chat: false,
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
        if let Some(ai) = self.ai.take() {
            ai.send(AiCommand::Shutdown);
            let _ = tokio::time::timeout(Duration::from_secs(2), ai.wait()).await;
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

    /// What the assistant's process says goes into the panel's transcript.
    fn on_ai(&mut self, ev: AiEvent) {
        if !matches!(ev, AiEvent::TextDelta(_)) {
            tracing::debug!("ai: {ev:?}");
        }
        let chat = self.chat.get_or_insert_with(ChatState::new);
        match ev {
            AiEvent::Ready { model, session_id, .. } => {
                if let Some(m) = model {
                    chat.model = short_model(&m);
                }
                self.ai_session = Some(session_id);
            }
            AiEvent::TextDelta(text) => chat.push_delta(&text),
            // What a subagent does stays inside its own tool row.
            AiEvent::ToolStart { parent: Some(_), .. } => {}
            // Looking up a tool's schema is plumbing, not something the model does for the user.
            AiEvent::ToolStart { name, .. } if name == "ToolSearch" => {}
            AiEvent::ToolStart { id, name, input, .. } => {
                let detail = tool_detail(&input, 60);
                let name = name.strip_prefix("mcp__saturn__").unwrap_or(&name).to_string();
                chat.tool_start(id, name, detail);
            }
            AiEvent::ToolResult { id, is_error, text, .. } => {
                let note = if is_error { ui::text::cut(text.lines().next().unwrap_or(""), 40) } else { String::new() };
                chat.tool_end(&id, !is_error, note);
            }
            AiEvent::PermissionRequest(r) => {
                let title = r.title.clone().unwrap_or_else(|| r.tool_name.clone());
                let mut detail = tool_detail(&r.input, 400);
                if detail.is_empty() {
                    detail = r.description.clone().or(r.blocked_path.clone()).unwrap_or_default();
                }
                chat.ask = Some(Ask { id: r.id, title, detail, remember: r.suggestions });
                if !self.chat_has_keys() {
                    self.flash("claude asks for permission (Ctrl+L)");
                }
            }
            AiEvent::ToolCall { id, name, arguments, .. } => match self.run_tool(Lane::Chat, &id, &name, &arguments) {
                Ok(Some((text, images))) => self.tool_reply(Lane::Chat, id, text, images, false),
                // Answered when the user or the kernel is done.
                Ok(None) => {}
                Err(e) => self.tool_reply(Lane::Chat, id, e, Vec::new(), true),
            },
            AiEvent::RequestCancelled { id } => {
                if chat.ask.as_ref().is_some_and(|a| a.id == id) {
                    chat.ask = None;
                }
                self.cancel_wait(&id);
            }
            AiEvent::TurnDone { outcome, usage, .. } => {
                chat.busy = false;
                chat.ask = None;
                if usage.context_tokens > 0 {
                    chat.usage = tokens(usage.context_tokens);
                }
                self.ai_has_turn = true;
                if std::mem::take(&mut self.ai_restart) {
                    self.restart_ai();
                }
                let Some(chat) = &mut self.chat else { return };
                match outcome {
                    TurnOutcome::Completed => {}
                    TurnOutcome::Interrupted => chat.notice("interrupted", false),
                    TurnOutcome::Error(e) => chat.notice(e, true),
                }
            }
            AiEvent::NotFound { command } => {
                chat.notice(format!("`{command}` was not found. Install Claude Code, then run `claude` once to log in."), true);
                chat.busy = false;
                self.ai = None;
            }
            AiEvent::NotLoggedIn => {
                chat.notice("Claude Code is not logged in. Run `claude` in a terminal and use /login.", true);
                chat.busy = false;
                self.ai = None;
            }
            AiEvent::Died { reason } => {
                chat.notice(format!("claude stopped: {reason}"), true);
                chat.busy = false;
                chat.ask = None;
                self.ai = None;
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
                    if let Some(Overlay::Popout(p)) = &mut self.overlay
                        && p.live_frame(sid, png, &mut self.images, &self.plotly)
                    {
                        self.dirty_ui = true;
                    }
                }
                Err(e) => tracing::warn!("plotly live figure: {e}"),
            },
            AppEvent::Terminal(Event::Key(key)) => {
                self.on_key(key);
                self.sweep_proposals();
                self.refresh_chat_chips();
                self.dirty_ui = true;
            }
            AppEvent::Terminal(Event::Paste(text)) => {
                if self.overlay.is_none() && self.ask_cell.is_some() {
                    if let Some(p) = self.inline_prompt_mut() {
                        p.paste(&text);
                    }
                } else if self.overlay.is_none() && self.chat_has_keys() {
                    if let Some(c) = &mut self.chat {
                        c.paste(text);
                    }
                } else if self.mode == Mode::Edit {
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
                self.check_runs();
                self.dirty_ui = true;
            }
            AppEvent::Ai(ev) => {
                self.on_ai(ev);
                self.dirty_ui = true;
            }
            AppEvent::AiQuick(ev) => {
                self.on_ai_quick(ev);
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
                if busy || self.ask_cell.is_some() || self.chat.as_ref().is_some_and(|c| c.busy) {
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
        let prompt = ui::cell::PromptInfo { model: "", spinner: 0 };
        let mut ctx = ui::cell::CellCtx { cfg, theme, cache, images, plotly, lang_ext, line_numbers: *line_numbers, prompt };
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
        if self.ask_cell.is_some() {
            self.on_inline_key(key);
            return;
        }
        if self.chat_has_keys() {
            // Saving, the palette, the files picker and quitting stay global.
            match keymap::global(&key) {
                Some(cmd @ (Command::Save | Command::CommandPalette | Command::OpenFile | Command::Quit)) => {
                    self.dispatch(cmd)
                }
                _ => self.on_chat_key(key),
            }
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
        if self.mode == Mode::Command && self.pending.is_empty() && self.on_proposal_key(&key) {
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

    // ----- assistant panel -----------------------------------------------------

    /// The assistant panel is open on the notebook screen and has the keys.
    fn chat_has_keys(&self) -> bool {
        self.focus == Focus::Chat && self.chat_open && self.chat.is_some() && matches!(self.screen, Screen::Notebook)
    }

    /// Where the assistant works: the project root of the notebook.
    fn ai_dir(&self) -> PathBuf {
        let dir = self
            .nb
            .path
            .as_deref()
            .and_then(Path::parent)
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        crate::files::root::find_root(&dir).root
    }

    /// How either assistant process is started, from the `[assistant]` config.
    fn ai_config(&self) -> AiConfig {
        let a = &self.cfg.assistant;
        let mut cfg = AiConfig::new(self.ai_dir());
        cfg.command = a.command.clone();
        cfg.user_mcp = a.user_mcp;
        cfg.use_api_key = a.use_api_key;
        cfg
    }

    /// Start the assistant's `claude` process unless one is running. It
    /// spends nothing until the first prompt, so the panel starts it early.
    fn ensure_ai(&mut self) -> &AiHandle {
        if !self.ai.as_ref().is_some_and(AiHandle::is_alive) {
            let mut cfg = self.ai_config();
            cfg.model = Some(self.cfg.assistant.model.clone()).filter(|m| !m.is_empty());
            cfg.effort = Some(self.cfg.assistant.effort.clone()).filter(|e| !e.is_empty());
            cfg.resume = self.ai_resume.take();
            cfg.system_prompt = Some(context::SYSTEM_PROMPT.to_string());
            cfg.disallowed_tools = vec!["NotebookEdit".to_string()];
            cfg.tools = tools::specs();
            cfg.allowed_tools = tools::allowed();
            self.ai = Some(AiHandle::spawn(cfg, self.tx.clone()));
        }
        self.ai.as_ref().expect("just started")
    }

    fn on_chat_key(&mut self, key: KeyEvent) {
        let Some(chat) = &mut self.chat else { return };
        match chat.handle_key(key) {
            ChatAction::None => {}
            ChatAction::Send(prompt) => self.send_chat(prompt),
            ChatAction::Interrupt => {
                if let Some(ai) = &self.ai {
                    ai.send(AiCommand::Interrupt);
                }
            }
            ChatAction::Permission { ask, answer } => {
                let decision = match answer {
                    Answer::Once => Permission::allow(),
                    Answer::Always => {
                        // Remembered for this session only, not in the project's settings.
                        let mut remember = ask.remember;
                        for r in &mut remember {
                            if let Some(o) = r.as_object_mut() {
                                o.insert("destination".into(), "session".into());
                            }
                        }
                        Permission::Allow { updated_input: None, remember }
                    }
                    Answer::Deny => Permission::deny("The user declined."),
                };
                if let Some(ai) = &self.ai {
                    ai.send(AiCommand::PermissionReply { id: ask.id, decision });
                }
            }
            ChatAction::Blur => self.focus = Focus::Notebook,
            ChatAction::Close => self.close_chat(),
        }
    }

    /// Send `prompt` to the panel's conversation, with the focused cell as
    /// what it is about.
    fn send_chat(&mut self, prompt: String) {
        let chat = self.chat.get_or_insert_with(ChatState::new);
        chat.push_user(prompt.clone());
        chat.busy = true;
        let plotly = &self.plotly;
        let png = |fig: &serde_json::Value| {
            plotly.cached(crate::render::hash_bytes(fig.to_string().as_bytes())).map(|p| p.to_vec())
        };
        let (ctx, images) = context::focus_context(&self.nb, self.focused, &png);
        let text = if ctx.is_empty() { prompt } else { format!("{ctx}\n\n{prompt}") };
        self.ensure_ai().send(AiCommand::Send { text, images });
    }

    /// Open the panel and ask `prompt` about the focused cell right away.
    fn ask_chat(&mut self, prompt: &str) {
        if !matches!(self.screen, Screen::Notebook) {
            return;
        }
        if !self.cfg.assistant.enabled {
            self.flash(ASSISTANT_OFF);
            return;
        }
        if self.mode == Mode::Edit {
            self.leave_edit();
        }
        self.chat.get_or_insert_with(ChatState::new);
        self.chat_open = true;
        self.focus = Focus::Chat;
        self.refresh_chat_chips();
        self.send_chat(prompt.to_string());
    }

    /// Hide the panel; the conversation stays for the next time it opens.
    fn close_chat(&mut self) {
        self.chat_open = false;
        self.focus = Focus::Notebook;
    }

    /// Forget the conversation and start the next prompt in a new session.
    fn new_chat(&mut self) {
        if let Some(ai) = self.ai.take() {
            ai.send(AiCommand::Shutdown);
        }
        self.ai_session = None;
        self.ai_has_turn = false;
        self.ai_resume = None;
        self.ai_restart = false;
        self.chat = Some(ChatState::new());
        self.chat_open = true;
        self.focus = Focus::Chat;
        self.refresh_chat_chips();
        self.ensure_ai();
    }

    /// Start the panel's process again so a start-up setting applies; the
    /// conversation carries on in the new process.
    fn restart_ai(&mut self) {
        if let Some(ai) = self.ai.take() {
            ai.send(AiCommand::Shutdown);
        }
        self.ai_resume = self.ai_session.clone().filter(|_| self.ai_has_turn);
        self.ensure_ai();
    }

    const AI_MODELS: [(&'static str, &'static str); 5] = [
        ("default", "your account's default model"),
        ("fable", "most capable"),
        ("opus", "strong at long, hard tasks"),
        ("sonnet", "balanced"),
        ("haiku", "fastest"),
    ];
    const AI_EFFORTS: [(&'static str, &'static str); 6] = [
        ("default", "the CLI's default"),
        ("low", "quickest answers"),
        ("medium", ""),
        ("high", ""),
        ("xhigh", ""),
        ("max", "slowest, most thorough"),
    ];

    /// A picker over `choices`, with the one in use marked and selected.
    fn open_assistant_picker(&mut self, title: &str, choices: &[(&str, &str)], current: &str, data: fn(String) -> ItemData) {
        if !self.cfg.assistant.enabled {
            self.flash(ASSISTANT_OFF);
            return;
        }
        let current = if current.is_empty() { "default" } else { current };
        let items: Vec<PaletteItem> = choices
            .iter()
            .map(|(name, note)| PaletteItem {
                label: name.to_string(),
                detail: match (*name == current, note.is_empty()) {
                    (true, true) => "current".to_string(),
                    (true, false) => format!("{note} · current"),
                    (false, _) => note.to_string(),
                },
                data: data(name.to_string()),
            })
            .collect();
        let mut p = PaletteState::new(PaletteKind::Assistant, title, items);
        if let Some(i) = p.items.iter().position(|it| it.label == current) {
            p.selected = i;
        }
        self.overlay = Some(Overlay::Palette(p));
    }

    /// Keep an `[assistant]` choice in the config file and say what happened.
    fn save_assistant_choice(&mut self, key: &str, value: &str, said: String) {
        match self.cfg.save_assistant(key, value) {
            Ok(_) => self.flash(said),
            Err(e) => self.flash_err(format!("{said}, but not saved: {e:#}")),
        }
    }

    /// Switch the panel's model. A running process switches in place.
    fn set_ai_model(&mut self, model: String) {
        self.cfg.assistant.model = if model == "default" { String::new() } else { model.clone() };
        if let Some(ai) = self.ai.as_ref().filter(|a| a.is_alive()) {
            ai.send(AiCommand::SetModel(model.clone()));
        }
        let value = self.cfg.assistant.model.clone();
        self.save_assistant_choice("model", &value, format!("assistant model: {model}"));
    }

    /// Switch the panel's reasoning effort. The CLI only takes it at start,
    /// so a running process is restarted and resumes the conversation, after
    /// the turn that is running.
    fn set_ai_effort(&mut self, effort: String) {
        self.cfg.assistant.effort = if effort == "default" { String::new() } else { effort.clone() };
        let busy = self.chat.as_ref().is_some_and(|c| c.busy);
        let mut said = format!("assistant effort: {effort}");
        if self.ai.as_ref().is_some_and(AiHandle::is_alive) {
            if busy {
                self.ai_restart = true;
                said.push_str(" (from the next turn)");
            } else {
                self.restart_ai();
            }
        }
        self.refresh_chat_chips();
        let value = self.cfg.assistant.effort.clone();
        self.save_assistant_choice("effort", &value, said);
    }

    /// What the next prompt is about: the focused cell. Also keeps the
    /// header's effort label current.
    fn refresh_chat_chips(&mut self) {
        let Some(chat) = &mut self.chat else { return };
        chat.effort = self.cfg.assistant.effort.clone();
        chat.chips.clear();
        let Some(cell) = self.nb.cells.get(self.focused) else { return };
        let kind = match cell.kind {
            CellKind::Code => "code",
            CellKind::Markdown => "markdown",
            CellKind::Raw => "raw",
        };
        let mut label = format!("cell {} · {kind}", self.focused + 1);
        use jupyter_protocol::MediaType as M;
        let figure = cell
            .outputs
            .iter()
            .filter_map(Output::media)
            .any(|m| m.content.iter().any(|t| matches!(t, M::Png(_) | M::Jpeg(_) | M::Gif(_) | M::Plotly(_))));
        if figure {
            label.push_str(" · figure");
        } else if !cell.outputs.is_empty() {
            label.push_str(" · output");
        }
        chat.chips.push(label);
    }

    // ----- proposed edits -------------------------------------------------------

    /// Show `after` as a proposed edit on cell `idx`, as a diff the user
    /// accepts or rejects. Returns the proposal's id.
    pub fn propose_edit(&mut self, idx: usize, after: String) -> Option<u64> {
        (idx < self.nb.cells.len()).then(|| self.propose(idx, ProposalKind::Edit, after, None))
    }

    /// Put a proposal on cell `idx`. One made by a tool call (`tool`) is
    /// answered when the user settles it, and takes the user to the cell.
    fn propose(&mut self, idx: usize, kind: ProposalKind, after: String, tool: Option<(Lane, &str)>) -> u64 {
        let typing = self.mode == Mode::Edit && self.focus == Focus::Notebook;
        if self.mode == Mode::Edit && idx == self.focused {
            self.leave_edit();
        }
        self.next_proposal += 1;
        let id = self.next_proposal;
        if let Some(old) = self.nb.cells[idx].proposal.take() {
            self.answer_proposal(old.id, "A newer proposal for the same cell replaced this one.");
        }
        let cell = &mut self.nb.cells[idx];
        cell.proposal = Some(Proposal::of(id, kind, &cell.source, after));
        self.layout.invalidate_cell(idx);
        if let Some((lane, tool)) = tool {
            self.ai_waits.push(ToolWait::Proposal { lane, tool: tool.to_string(), proposal: id });
            // Someone typing in another cell is left alone; the status bar
            // counts the proposal.
            if !typing || idx == self.focused {
                if self.mode == Mode::Edit {
                    self.leave_edit();
                }
                self.set_focus(idx);
                self.scroll_to_focused();
                if self.focus == Focus::Chat {
                    self.focus = Focus::Notebook;
                    self.return_to_chat = true;
                }
            }
        }
        id
    }

    fn delete_cell_at(&mut self, idx: usize) {
        let Some(cell) = self.nb.cells.get(idx) else { return };
        let snapshot = cell.clone();
        self.cache.invalidate_cell(&snapshot.id);
        self.nb.apply(CellOp::Delete { idx, cell: snapshot });
        self.layout.removed(idx);
        if self.nb.cells.len() == 1 && self.layout.layout(0).is_none() {
            self.layout.inserted(0);
        }
        self.set_focus(idx);
        self.layout.invalidate_all();
    }

    /// Accept or reject the proposal on the focused cell; `editing` says the
    /// user goes on to edit the cell. False when the cell has none.
    fn settle_proposal(&mut self, accept: bool, editing: bool) -> bool {
        let idx = self.focused;
        let Some(p) = self.nb.cells.get_mut(idx).and_then(|c| c.proposal.take()) else { return false };
        let answer = match (p.kind, accept) {
            (ProposalKind::Edit | ProposalKind::Insert, true) => {
                let before = self.nb.cells[idx].clone();
                let mut after = before.clone();
                after.set_source(p.after);
                self.nb.apply(CellOp::Replace { idx, before, after });
                self.cache.invalidate_cell(&self.nb.cells[idx].id.clone());
                let what = if p.kind == ProposalKind::Edit {
                    "The user accepted the edit.".to_string()
                } else {
                    format!("The user kept the new cell. It is cell {}.", idx + 1)
                };
                if editing {
                    format!("{what} They opened the cell to edit it further, so read it again before relying on its content.")
                } else {
                    what
                }
            }
            (ProposalKind::Edit, false) => {
                "The user rejected the edit; the cell is unchanged. Ask what they want instead of proposing it again.".to_string()
            }
            (ProposalKind::Insert, false) => {
                self.delete_cell_at(idx);
                "The user discarded the new cell.".to_string()
            }
            (ProposalKind::Delete, true) => {
                self.delete_cell_at(idx);
                "The user deleted the cell. The cells after it moved up by one.".to_string()
            }
            (ProposalKind::Delete, false) => "The user kept the cell.".to_string(),
        };
        self.layout.invalidate_cell(idx);
        self.scroll_to_focused();
        self.answer_proposal(p.id, &answer);
        true
    }

    /// Answer the tool call that made proposal `id`, if one did. With the
    /// last of them settled, the keys go back to the panel they came from.
    fn answer_proposal(&mut self, id: u64, answer: &str) {
        let Some(i) = self.ai_waits.iter().position(|w| matches!(w, ToolWait::Proposal { proposal, .. } if *proposal == id)) else {
            return;
        };
        if let ToolWait::Proposal { lane, tool, .. } = self.ai_waits.remove(i) {
            self.tool_reply(lane, tool, answer.to_string(), Vec::new(), false);
        }
        let open = self.ai_waits.iter().any(|w| matches!(w, ToolWait::Proposal { .. }));
        if !open && std::mem::take(&mut self.return_to_chat) && self.chat_open {
            self.focus = Focus::Chat;
        }
    }

    /// Answer proposals that went away without the user settling them (their
    /// cell was deleted, or replaced by an undo).
    fn sweep_proposals(&mut self) {
        let live: Vec<u64> = self.nb.cells.iter().filter_map(|c| c.proposal.as_ref().map(|p| p.id)).collect();
        let gone: Vec<u64> = self
            .ai_waits
            .iter()
            .filter_map(|w| match w {
                ToolWait::Proposal { proposal, .. } if !live.contains(proposal) => Some(*proposal),
                _ => None,
            })
            .collect();
        for id in gone {
            self.answer_proposal(id, "The proposal went away without an answer: its cell was changed or removed.");
        }
    }

    /// Keys of a cell that shows a proposal: Tab accepts, `X` rejects and
    /// Enter accepts and opens the cell for editing.
    fn on_proposal_key(&mut self, key: &KeyEvent) -> bool {
        let Some(kind) = self.nb.cells.get(self.focused).and_then(|c| c.proposal.as_ref()).map(|p| p.kind) else {
            return false;
        };
        match key.code {
            KeyCode::Tab => self.dispatch(Command::AcceptProposal),
            KeyCode::Char('X') => self.dispatch(Command::RejectProposal),
            KeyCode::Enter if key.modifiers.is_empty() && kind != ProposalKind::Delete => {
                self.settle_proposal(true, true);
                self.enter_edit(false);
            }
            _ => return false,
        }
        true
    }

    // ----- inline prompt ----------------------------------------------------------

    /// Start the lean `claude` process behind the inline prompt unless one
    /// is running: no built-in tools, a short system prompt, a fast model.
    fn ensure_quick(&mut self) -> &AiHandle {
        if !self.quick.as_ref().is_some_and(AiHandle::is_alive) {
            let mut cfg = self.ai_config();
            cfg.minimal = true;
            cfg.model = Some(self.cfg.assistant.inline_model.clone()).filter(|m| !m.is_empty());
            cfg.system_prompt = Some(context::INLINE_PROMPT.to_string());
            let offered = [tools::EDIT, tools::READ, tools::OUTLINE];
            cfg.tools = tools::specs().into_iter().filter(|t| offered.contains(&t.name.as_str())).collect();
            cfg.allowed_tools = tools::allowed();
            let tx = self.tx.clone();
            self.quick = Some(AiHandle::spawn_with(cfg, move |ev| {
                let _ = tx.send(AppEvent::AiQuick(ev));
            }));
        }
        self.quick.as_ref().expect("just started")
    }

    /// Open the prompt under the focused cell. A visual selection in the
    /// cell goes along as what the request is about.
    fn open_inline_prompt(&mut self) {
        if !matches!(self.screen, Screen::Notebook) {
            return;
        }
        if !self.cfg.assistant.enabled {
            self.flash(ASSISTANT_OFF);
            return;
        }
        let idx = self.focused;
        let selection = match (self.mode, self.nb.cells.get(idx).and_then(|c| c.editor.as_ref())) {
            (Mode::Edit, Some(ed)) => ed.selected_text(),
            _ => None,
        };
        if self.mode == Mode::Edit {
            self.leave_edit();
        }
        self.close_inline_prompt();
        let Some(cell) = self.nb.cells.get_mut(idx) else { return };
        cell.prompt = Some(InlinePrompt::new(selection));
        self.ask_cell = Some(cell.id.clone());
        self.focus = Focus::Notebook;
        self.layout.invalidate_cell(idx);
        self.scroll_to_focused();
        self.ensure_quick();
    }

    fn close_inline_prompt(&mut self) {
        if let Some(id) = self.ask_cell.take()
            && let Some(idx) = self.nb.index_of(&id)
        {
            self.nb.cells[idx].prompt = None;
            self.layout.invalidate_cell(idx);
        }
    }

    fn inline_prompt_mut(&mut self) -> Option<&mut InlinePrompt> {
        let idx = self.nb.index_of(self.ask_cell.as_ref()?)?;
        self.nb.cells[idx].prompt.as_mut()
    }

    fn on_inline_key(&mut self, key: KeyEvent) {
        let Some(idx) = self.ask_cell.as_ref().and_then(|id| self.nb.index_of(id)) else {
            self.ask_cell = None;
            return;
        };
        let Some(p) = self.nb.cells[idx].prompt.as_mut() else {
            self.ask_cell = None;
            return;
        };
        let was_busy = p.busy;
        match p.handle_key(key) {
            PromptAction::None => {}
            PromptAction::Cancel => {
                if was_busy && let Some(q) = &self.quick {
                    q.send(AiCommand::Interrupt);
                }
                self.close_inline_prompt();
            }
            PromptAction::ToChat(text) => {
                self.close_inline_prompt();
                if !self.chat_has_keys() {
                    self.dispatch(Command::ToggleChat);
                }
                if let Some(c) = &mut self.chat
                    && !text.is_empty()
                {
                    c.paste(text);
                }
            }
            PromptAction::Send(text) => {
                p.busy = true;
                let selection = p.selection.clone();
                // A proposal still under review is being refined: its tool
                // call is answered so the next turn can start.
                let pending = self.nb.cells[idx].proposal.as_ref().map(|p| (p.id, p.after.clone()));
                if let Some((id, _)) = &pending {
                    self.answer_proposal(*id, "The user has not accepted this proposal and asks for a change to it; their next message says what.");
                }
                let plotly = &self.plotly;
                let png = |fig: &serde_json::Value| {
                    plotly.cached(crate::render::hash_bytes(fig.to_string().as_bytes())).map(|p| p.to_vec())
                };
                let (mut msg, images) = context::focus_context(&self.nb, idx, &png);
                if let Some((_, after)) = pending {
                    msg.push_str(&format!(
                        "\n\nYour proposed source for this cell, which the user sees as a diff and has not accepted:\n```\n{after}\n```"
                    ));
                }
                if let Some(sel) = selection {
                    msg.push_str(&format!("\n\nThe user selected this part of the cell:\n```\n{sel}\n```"));
                }
                msg.push_str(&format!("\n\nInstruction: {text}"));
                self.quick_text.clear();
                self.ensure_quick().send(AiCommand::Send { text: msg, images });
            }
        }
    }

    fn inline_note(&mut self, text: String, error: bool) {
        if let Some(p) = self.inline_prompt_mut() {
            p.busy = false;
            p.note = Some((text, error));
        }
    }

    /// What the inline prompt's process says: an edit becomes a proposal
    /// and closes the prompt, anything else shows under it.
    fn on_ai_quick(&mut self, ev: AiEvent) {
        if !matches!(ev, AiEvent::TextDelta(_)) {
            tracing::debug!("ai quick: {ev:?}");
        }
        match ev {
            AiEvent::Ready { model: Some(m), .. } => self.quick_label = format!("claude · {}", short_model(&m)),
            AiEvent::TextDelta(text) => self.quick_text.push_str(&text),
            AiEvent::ToolCall { id, name, arguments, .. } => match self.run_tool(Lane::Quick, &id, &name, &arguments) {
                Ok(Some((text, images))) => self.tool_reply(Lane::Quick, id, text, images, false),
                // The proposal is on the cell: the prompt has done its job.
                Ok(None) => self.close_inline_prompt(),
                Err(e) => self.tool_reply(Lane::Quick, id, e, Vec::new(), true),
            },
            AiEvent::PermissionRequest(r) => {
                if let Some(q) = &self.quick {
                    let decision = Permission::deny("Only saturn's notebook tools are available here.");
                    q.send(AiCommand::PermissionReply { id: r.id, decision });
                }
            }
            AiEvent::RequestCancelled { id } => self.cancel_wait(&id),
            AiEvent::TurnDone { outcome, .. } => {
                let said = std::mem::take(&mut self.quick_text);
                if self.inline_prompt_mut().is_some_and(|p| p.busy) {
                    match outcome {
                        TurnOutcome::Error(e) => self.inline_note(e, true),
                        TurnOutcome::Interrupted => self.inline_note("interrupted".to_string(), false),
                        TurnOutcome::Completed if said.trim().is_empty() => {
                            self.inline_note("claude made no change".to_string(), false)
                        }
                        TurnOutcome::Completed => self.inline_note(said.trim().to_string(), false),
                    }
                }
            }
            AiEvent::NotFound { command } => {
                self.inline_note(format!("`{command}` was not found: install Claude Code and log in"), true);
                self.quick = None;
            }
            AiEvent::NotLoggedIn => {
                self.inline_note("Claude Code is not logged in: run `claude` and use /login".to_string(), true);
                self.quick = None;
            }
            AiEvent::Died { reason } => {
                self.inline_note(format!("claude stopped: {reason}"), true);
                self.quick = None;
            }
            _ => {}
        }
    }

    // ----- the assistant's tools -------------------------------------------------

    fn tool_reply(&self, lane: Lane, id: String, text: String, images: Vec<Image>, is_error: bool) {
        let handle = match lane {
            Lane::Chat => &self.ai,
            Lane::Quick => &self.quick,
        };
        let Some(ai) = handle else { return };
        let mut content = vec![ToolContent::Text(text)];
        content.extend(images.into_iter().map(ToolContent::Image));
        ai.send(AiCommand::ToolReply { id, content, is_error });
    }

    /// The cells at `indices` as the model reads them.
    fn cells_for_model(&self, indices: &[usize]) -> (String, Vec<Image>) {
        let plotly = &self.plotly;
        let png =
            |fig: &serde_json::Value| plotly.cached(crate::render::hash_bytes(fig.to_string().as_bytes())).map(|p| p.to_vec());
        let cells: Vec<(usize, &Cell)> = indices.iter().filter_map(|&i| self.nb.cells.get(i).map(|c| (i + 1, c))).collect();
        context::cells_context(&cells, &png)
    }

    /// Run one of saturn's tools for the assistant. `Ok(None)` means the
    /// answer follows later (the user reviews a proposal, the kernel runs).
    fn run_tool(
        &mut self,
        lane: Lane,
        id: &str,
        name: &str,
        args: &serde_json::Value,
    ) -> Result<Option<(String, Vec<Image>)>, String> {
        if !matches!(self.screen, Screen::Notebook) {
            return Err("no notebook is open".to_string());
        }
        let n = self.nb.cells.len();
        match name {
            tools::OUTLINE => Ok(Some((tools::outline(&self.nb, self.focused), Vec::new()))),
            tools::READ => Ok(Some(self.cells_for_model(&tools::cell_indices(args, n)?))),
            tools::EDIT => {
                let idx = tools::cell_index(args, "cell", n)?;
                let current = self.nb.cells[idx].text();
                let mut source = tools::text_arg(args, "source")?;
                // Models end a source with a newline; cells do not.
                if !current.ends_with('\n') {
                    source = source.trim_end_matches('\n');
                }
                let source = source.to_string();
                if current == source {
                    return Err("the cell already has exactly this source".to_string());
                }
                self.propose(idx, ProposalKind::Edit, source, Some((lane, id)));
                Ok(None)
            }
            tools::INSERT => {
                let after = args.get("after").and_then(|v| v.as_u64()).ok_or("`after` must be a cell number, or 0")? as usize;
                if after > n {
                    return Err(format!("there is no cell {after}: the notebook has cells 1 to {n}"));
                }
                let kind = match tools::text_arg(args, "kind")? {
                    "code" => CellKind::Code,
                    "markdown" => CellKind::Markdown,
                    other => return Err(format!("`kind` must be code or markdown, not {other}")),
                };
                let source = tools::text_arg(args, "source")?.trim_end_matches('\n').to_string();
                if self.mode == Mode::Edit {
                    self.leave_edit();
                }
                self.nb.apply(CellOp::Insert { idx: after, cell: Cell::new(kind, "") });
                self.layout.inserted(after);
                self.layout.invalidate_all();
                self.propose(after, ProposalKind::Insert, source, Some((lane, id)));
                Ok(None)
            }
            tools::DELETE => {
                let idx = tools::cell_index(args, "cell", n)?;
                self.propose(idx, ProposalKind::Delete, String::new(), Some((lane, id)));
                Ok(None)
            }
            tools::RUN => {
                let indices = tools::cell_indices(args, n)?;
                if let Some(&i) = indices.iter().find(|&&i| self.nb.cells[i].kind != CellKind::Code) {
                    return Err(format!("cell {} is not a code cell", i + 1));
                }
                match &self.kernel {
                    None => return Err("no kernel is running; the user selects one with K".to_string()),
                    Some(k) if k.status == KernelStatus::Dead => {
                        return Err("the kernel is dead; the user restarts it with 00".to_string());
                    }
                    Some(_) => {}
                }
                let cells = indices.iter().map(|&i| self.nb.cells[i].id.clone()).collect();
                self.run_cells(&indices, true);
                self.ai_waits.push(ToolWait::Run { lane, tool: id.to_string(), cells });
                self.check_runs();
                Ok(None)
            }
            _ => Err(format!("saturn has no tool named {name}")),
        }
    }

    /// Answer the `run_cells` calls whose cells have all finished.
    fn check_runs(&mut self) {
        let running = |nb: &Notebook, id: &CellId| {
            nb.index_of(id).is_some_and(|i| matches!(nb.cells[i].exec, ExecState::Queued | ExecState::Running))
        };
        let mut i = 0;
        while i < self.ai_waits.len() {
            let done = match &self.ai_waits[i] {
                ToolWait::Run { cells, .. } => !cells.iter().any(|id| running(&self.nb, id)),
                ToolWait::Proposal { .. } => false,
            };
            if !done {
                i += 1;
                continue;
            }
            if let ToolWait::Run { lane, tool, cells } = self.ai_waits.remove(i) {
                let indices: Vec<usize> = cells.iter().filter_map(|id| self.nb.index_of(id)).collect();
                let (text, images) = self.cells_for_model(&indices);
                self.tool_reply(lane, tool, text, images, false);
            }
        }
    }

    /// The assistant withdrew tool call `tool`: its proposal goes away too.
    fn cancel_wait(&mut self, tool: &str) {
        let Some(i) = self.ai_waits.iter().position(|w| match w {
            ToolWait::Proposal { tool: t, .. } | ToolWait::Run { tool: t, .. } => t == tool,
        }) else {
            return;
        };
        if let ToolWait::Proposal { proposal, .. } = self.ai_waits.remove(i)
            && let Some(idx) = self.nb.cells.iter().position(|c| c.proposal.as_ref().is_some_and(|p| p.id == proposal))
        {
            let kind = self.nb.cells[idx].proposal.take().map(|p| p.kind);
            self.layout.invalidate_cell(idx);
            if kind == Some(ProposalKind::Insert) {
                self.delete_cell_at(idx);
            }
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
        if key.code == KeyCode::Esc && ed.at_rest() {
            crate::vim::clear_search_highlight();
            self.leave_edit();
            return;
        }
        ed.vim.page = (self.view.height as usize / 2).max(1);
        ed.key(key);
        let requests = ed.take_requests();
        self.after_edit();
        for r in requests {
            match r {
                Request::Save => self.dispatch(Command::Save),
                Request::Leave => self.leave_edit(),
                Request::Message(m) => self.flash(m),
            }
        }
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
            Command::ToggleChat => {
                if !matches!(self.screen, Screen::Notebook) {
                    return;
                }
                if !self.cfg.assistant.enabled {
                    self.flash(ASSISTANT_OFF);
                } else if self.chat_has_keys() {
                    // A toggle: the panel that has the keys closes. One that
                    // is open beside the notebook takes the keys instead.
                    self.close_chat();
                } else {
                    self.chat.get_or_insert_with(ChatState::new);
                    self.chat_open = true;
                    self.focus = Focus::Chat;
                    self.refresh_chat_chips();
                    self.ensure_ai();
                }
            }
            Command::CloseChat => self.close_chat(),
            Command::AskInline => self.open_inline_prompt(),
            Command::SelectAiModel => {
                let current = self.cfg.assistant.model.clone();
                self.open_assistant_picker("Assistant model", &Self::AI_MODELS, &current, ItemData::Model);
            }
            Command::SelectAiEffort => {
                let current = self.cfg.assistant.effort.clone();
                self.open_assistant_picker("Assistant effort", &Self::AI_EFFORTS, &current, ItemData::Effort);
            }
            Command::ExplainOutput => {
                self.ask_chat("Explain what this cell's output shows. Lead with the main takeaway, then anything surprising or worth checking.")
            }
            Command::FixError => {
                let failed = self.nb.cells.get(self.focused).is_some_and(|c| c.outputs.iter().any(|o| matches!(o, Output::Error { .. })));
                if failed {
                    self.ask_chat("This cell failed. Find the cause of the error and propose a fix for the cell.");
                } else {
                    self.flash("this cell has no error");
                }
            }
            Command::NewChat => {
                if matches!(self.screen, Screen::Notebook) && self.cfg.assistant.enabled {
                    self.new_chat();
                }
            }
            Command::AcceptProposal => {
                if !self.settle_proposal(true, false) {
                    self.flash("no proposed edit on this cell");
                }
            }
            Command::RejectProposal => {
                if !self.settle_proposal(false, false) {
                    self.flash("no proposed edit on this cell");
                }
            }
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
        ed.vim.shiftwidth = self.cfg.tab_width.max(1);
        ed.vim.autopairs = self.cfg.auto_pairs;
        ed.vim.comment = comment_leader(if snapshot.kind == CellKind::Code { self.lang_ext.as_str() } else { "" }).to_string();
        if insert { ed.enter_insert() } else { ed.enter_normal() }
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
                ed.enter_normal();
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
            ItemData::Model(model) => self.set_ai_model(model),
            ItemData::Effort(effort) => self.set_ai_effort(effort),
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

/// `claude-haiku-4-5-20251001` as `haiku-4-5`.
fn short_model(model: &str) -> String {
    let m = model.strip_prefix("claude-").unwrap_or(model);
    match m.rsplit_once('-') {
        Some((head, tail)) if tail.len() == 8 && tail.bytes().all(|b| b.is_ascii_digit()) => head.to_string(),
        _ => m.to_string(),
    }
}

/// `12345` tokens as `12k`.
fn tokens(n: u64) -> String {
    if n >= 1000 { format!("{}k", (n + 500) / 1000) } else { n.to_string() }
}

/// The argument that says most about a tool call, for the transcript.
fn tool_detail(input: &serde_json::Value, max: usize) -> String {
    const KEYS: [&str; 9] = ["command", "file_path", "pattern", "path", "url", "query", "description", "prompt", "skill"];
    let text = KEYS.iter().find_map(|k| input.get(*k).and_then(|v| v.as_str())).unwrap_or("");
    // Saturn's own tools name their cells by number.
    let cells = match (input.get("cell"), input.get("cells")) {
        _ if input.get("after").is_some_and(|n| n.is_u64()) => format!("after cell {}", input["after"]),
        (Some(n), _) if n.is_u64() => format!("cell {n}"),
        (_, Some(serde_json::Value::Array(list))) => {
            format!("cell {}", list.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(", "))
        }
        _ => String::new(),
    };
    let text = if text.is_empty() { cells.as_str() } else { text };
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    ui::text::cut(&one_line, max)
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
