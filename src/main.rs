//! saturn — a keyboard-first TUI Jupyter notebook.

use std::io::{self, IsTerminal};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui_image::picker::Picker;
use saturn::{Opts, app, config};

const USAGE: &str = "\
saturn — keyboard-first TUI Jupyter notebook

usage: saturn [PATH] [--kernel NAME] [--log FILE]
       saturn theme list | show NAME | import-nvim [--colorscheme NAME] [--name NAME] [--force]

  PATH            notebook (.ipynb) to open, or a directory to find files in
                  (default: the start dashboard)
  --kernel NAME   kernelspec name to start (overrides the notebook's metadata)
  --log FILE      log file (default: $XDG_CACHE_HOME/saturn/saturn.log)
  -h, --help      show this help

  theme list          themes that `theme = \"…\"` in config.toml accepts
  theme show NAME     print a theme's resolved colors
  theme import-nvim   write ~/.config/saturn/themes/<colorscheme>.toml from your
                      Neovim colorscheme (runs `nvim --headless` with your config)
";

fn theme_cli(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("list") => {
            for (name, src) in saturn::theme::available() {
                println!("{name:<28} {}", src.label());
            }
        }
        Some("show") => {
            let name = args.get(1).context("theme show needs a NAME")?;
            print!("{}", saturn::theme::describe(name)?);
        }
        Some("import-nvim") => {
            let mut colorscheme = None;
            let mut name = None;
            let mut force = false;
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--colorscheme" | "-c" => colorscheme = Some(it.next().context("--colorscheme needs a value")?.clone()),
                    "--name" | "-n" => name = Some(it.next().context("--name needs a value")?.clone()),
                    "--force" | "-f" => force = true,
                    s => bail!("unknown option {s}\n{USAGE}"),
                }
            }
            let path = saturn::theme::import_nvim(colorscheme.as_deref(), name.as_deref(), force)?;
            let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            println!("wrote {}\nuse it with  theme = {stem:?}  in config.toml, or pick it with “Select theme…” (Ctrl+P)", path.display());
        }
        _ => bail!("usage: saturn theme list | show NAME | import-nvim [--colorscheme NAME] [--name NAME] [--force]"),
    }
    Ok(())
}

static KBD_ENHANCED: AtomicBool = AtomicBool::new(false);
static LOG_GUARD: OnceLock<tracing_appender::non_blocking::WorkerGuard> = OnceLock::new();

fn parse_args() -> Result<Opts> {
    let mut opts = Opts { started: Some(std::time::Instant::now()), ..Opts::default() };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("saturn {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--kernel" | "-k" => opts.kernel = Some(args.next().context("--kernel needs a value")?),
            "--log" => opts.log = Some(PathBuf::from(args.next().context("--log needs a value")?)),
            s if s.starts_with("--kernel=") => opts.kernel = Some(s["--kernel=".len()..].to_string()),
            s if s.starts_with("--log=") => opts.log = Some(PathBuf::from(&s["--log=".len()..])),
            s if s.starts_with('-') => bail!("unknown option {s}\n{USAGE}"),
            s => opts.path = Some(PathBuf::from(s)),
        }
    }
    Ok(opts)
}

/// Set up file logging and redirect fd 2 to the log file so that nothing a
/// dependency or a kernel prints to stderr can corrupt the screen.
fn init_logging(log: Option<PathBuf>) -> Result<PathBuf> {
    let path = match log {
        Some(p) => p,
        None => {
            let dir = dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("saturn");
            std::fs::create_dir_all(&dir).ok();
            dir.join("saturn.log")
        }
    };
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("cannot open log file {}", path.display()))?;
    // SAFETY: dup2 on valid descriptors; both stay open for the program's life.
    unsafe {
        libc::dup2(file.as_raw_fd(), 2);
    }
    let (writer, guard) = tracing_appender::non_blocking(file);
    let _ = LOG_GUARD.set(guard);
    let filter = tracing_subscriber::EnvFilter::try_from_env("SATURN_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(false)
        .init();
    Ok(path)
}

pub fn restore_terminal() {
    let mut out = io::stdout();
    if KBD_ENHANCED.swap(false, Ordering::SeqCst) {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    let _ = execute!(
        out,
        DisableBracketedPaste,
        LeaveAlternateScreen,
        crossterm::cursor::SetCursorStyle::DefaultUserShape,
        crossterm::cursor::Show
    );
    let _ = disable_raw_mode();
}

fn main() -> Result<()> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().map(String::as_str) == Some("theme") {
        return theme_cli(&argv[1..]);
    }
    let opts = parse_args()?;
    if !io::stdout().is_terminal() {
        bail!("saturn needs an interactive terminal");
    }
    let log_path = init_logging(opts.log.clone())?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = rt.block_on(run(opts, log_path));
    restore_terminal();
    if let Err(e) = &result {
        eprintln!("saturn: {e:#}");
        println!("saturn: {e:#}");
    }
    result
}

async fn run(opts: Opts, log_path: PathBuf) -> Result<()> {
    let cfg = config::Config::load();
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let orig_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Background task panics are contained by tokio; only a panic on the
        // UI thread takes the terminal down with it.
        let on_main = std::thread::current().name() == Some("main");
        if on_main {
            restore_terminal();
        }
        tracing::error!("panic: {info}");
        if on_main {
            orig_hook(info);
        }
    }));

    // Must run after raw mode / alternate screen and before the event stream
    // is created: it reads the terminal's responses from stdin.
    let picker = match Picker::from_query_stdio() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("terminal graphics query failed ({e}); using halfblocks");
            Picker::halfblocks()
        }
    };
    tracing::info!(protocol = ?picker.protocol_type(), font = ?picker.font_size(), "graphics");

    if supports_keyboard_enhancement().unwrap_or(false) {
        execute!(
            io::stdout(),
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
            )
        )?;
        KBD_ENHANCED.store(true, Ordering::SeqCst);
    }
    // The terminal cursor is shown only while typing in a cell (insert
    // mode), where it should read as a line; it stays hidden otherwise.
    execute!(io::stdout(), EnableBracketedPaste, crossterm::cursor::SetCursorStyle::SteadyBar)?;

    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let app = app::App::new(opts, cfg, picker, log_path).await?;
    app.run(&mut terminal).await
}
