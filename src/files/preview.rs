//! Previews for the files picker: classification, building, and the preview
//! worker thread that keeps a small cache. The UI thread never touches the
//! disk for a preview.

use std::collections::VecDeque;
use std::fs::{File, Metadata};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Instant, SystemTime};

use edtui::syntect::highlighting::Theme as SynTheme;
use ratatui::style::Color;
use ratatui::text::Line;
use tracing::{debug, warn};

use crate::files::{FilesEvent, Sink, human_size, outline};
use crate::theme::from_syn;
use crate::ui::text::{clean_text, highlight_with};

/// Text files larger than this get no preview.
pub const TEXT_MAX: u64 = 256 * 1024;
/// Notebooks larger than this get no preview.
pub const NB_MAX: u64 = 32 * 1024 * 1024;
/// Bytes read to look for a NUL byte.
pub const SNIFF: usize = 8 * 1024;
pub const MAX_LINES: usize = 1_000;
pub const MAX_LINE_CHARS: usize = 1_000;
pub const NB_MAX_LINES: usize = 2_000;
pub const CACHE_LEN: usize = 32;

#[derive(Clone, Copy, Debug)]
pub struct UiColors {
    pub muted: Color,
    pub input_bg: Option<Color>,
    pub line_numbers: Color,
}

#[derive(Clone, Debug)]
pub struct PreviewRequest {
    pub id: u64,
    pub path: PathBuf,
    /// Inner width of the preview pane; rendered markdown wraps to it.
    pub width: u16,
    pub syn: Arc<SynTheme>,
    pub ui: UiColors,
    pub tab_width: usize,
}

#[derive(Clone, Debug)]
pub enum Preview {
    Doc {
        lines: Vec<Line<'static>>,
        line_numbers: bool,
        note: Option<String>,
    },
    Placeholder(String),
}

#[derive(Clone, PartialEq, Eq)]
pub struct CacheKey {
    pub path: PathBuf,
    pub mtime: Option<SystemTime>,
    pub len: u64,
    /// Set for notebooks only, whose markdown wraps to the pane width.
    pub width: Option<u16>,
}

fn is_notebook(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|n| n.to_string_lossy().ends_with(".ipynb"))
}

fn key_for(req: &PreviewRequest) -> std::io::Result<(CacheKey, Metadata)> {
    let meta = std::fs::metadata(&req.path)?;
    let key = CacheKey {
        path: req.path.clone(),
        mtime: meta.modified().ok(),
        len: meta.len(),
        width: is_notebook(&req.path).then_some(req.width),
    };
    Ok((key, meta))
}

/// `stat` the path, then build its preview. A failed `stat` has no key.
pub fn build_path(req: &PreviewRequest) -> (Option<CacheKey>, Preview) {
    match key_for(req) {
        Ok((key, meta)) => (Some(key), build(req, &meta)),
        Err(e) => (None, Preview::Placeholder(format!("cannot read: {e}"))),
    }
}

/// What a build did, for the log line.
struct Built {
    preview: Preview,
    kind: &'static str,
    bytes: u64,
}

fn placeholder(kind: &'static str, text: String) -> Built {
    Built {
        preview: Preview::Placeholder(text),
        kind,
        bytes: 0,
    }
}

/// Classify the file and build its preview (rows 2–8 of the design's table).
pub fn build(req: &PreviewRequest, meta: &Metadata) -> Preview {
    build_counted(req, meta).preview
}

fn build_counted(req: &PreviewRequest, meta: &Metadata) -> Built {
    let ft = meta.file_type();
    if ft.is_dir() {
        return placeholder("folder", "[folder]".into());
    }
    if !ft.is_file() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileTypeExt;
            if ft.is_fifo() {
                return placeholder("special", "[fifo]".into());
            }
            if ft.is_socket() {
                return placeholder("special", "[socket]".into());
            }
            if ft.is_block_device() || ft.is_char_device() {
                return placeholder("special", "[device]".into());
            }
        }
        return placeholder("special", "[device]".into());
    }
    let size = meta.len();
    let mut file = match File::open(&req.path) {
        Ok(f) => f,
        Err(e) => return placeholder("error", format!("cannot read: {e}")),
    };
    let mut head = Vec::with_capacity(SNIFF);
    if let Err(e) = (&mut file).take(SNIFF as u64).read_to_end(&mut head) {
        return placeholder("error", format!("cannot read: {e}"));
    }
    if head.contains(&0) {
        return Built {
            bytes: head.len() as u64,
            ..placeholder("binary", format!("binary · {}", human_size(size)))
        };
    }
    let notebook = is_notebook(&req.path);
    let max = if notebook { NB_MAX } else { TEXT_MAX };
    if size > max {
        let what = if notebook {
            "notebook too large to preview"
        } else {
            "too large to preview"
        };
        return Built {
            bytes: head.len() as u64,
            ..placeholder("too-large", format!("{what} ({})", human_size(size)))
        };
    }
    let mut bytes = head;
    if let Err(e) = file.take(max - bytes.len() as u64).read_to_end(&mut bytes) {
        return placeholder("error", format!("cannot read: {e}"));
    }
    let read = bytes.len() as u64;
    if notebook {
        let preview = outline::build(&bytes, req.width, &req.syn, req.tab_width, req.ui);
        return Built {
            preview,
            kind: "notebook",
            bytes: read,
        };
    }
    let name = req
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Built {
        preview: text_preview(&bytes, &name, req),
        kind: "text",
        bytes: read,
    }
}

/// Keep at most `max` characters of `s`, never splitting one.
pub(crate) fn cut_chars(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// Drop span backgrounds equal to the syntax theme's own background, so the
/// pane's background (or a line's tint) shows through.
pub(crate) fn strip_plain_bg(lines: &mut [Line<'static>], syn: &SynTheme) {
    let Some(plain) = syn.settings.background.map(from_syn) else {
        return;
    };
    for line in lines {
        for span in &mut line.spans {
            if span.style.bg == Some(plain) {
                span.style.bg = None;
            }
        }
    }
}

fn text_preview(bytes: &[u8], name: &str, req: &PreviewRequest) -> Preview {
    let decoded = String::from_utf8_lossy(bytes);
    let text = clean_text(&decoded);
    let mut segments: Vec<&str> = text.split('\n').collect();
    if segments.len() > 1 && segments.last() == Some(&"") {
        segments.pop();
    }
    let total = segments.len();
    let kept: Vec<&str> = segments
        .iter()
        .take(MAX_LINES)
        .map(|l| cut_chars(l, MAX_LINE_CHARS))
        .collect();
    let first_line = kept.first().copied();
    let syntax = crate::syntax::for_file(name, first_line);
    let mut lines = highlight_with(&kept.join("\n"), syntax, &req.syn, req.tab_width).lines;
    strip_plain_bg(&mut lines, &req.syn);
    let note = (total > MAX_LINES).then(|| format!("first {MAX_LINES} of {total} lines"));
    Preview::Doc {
        lines,
        line_numbers: true,
        note,
    }
}

// ----- worker ------------------------------------------------------------------

struct SlotState {
    req: Option<PreviewRequest>,
    closed: bool,
}

struct Slot {
    state: Mutex<SlotState>,
    cv: Condvar,
}

/// One preview thread per picker. It builds the latest request only, so
/// holding a key builds at most one preview per finished build.
pub struct PreviewWorker {
    slot: Arc<Slot>,
    #[cfg(test)]
    handle: Option<thread::JoinHandle<()>>,
}

impl PreviewWorker {
    pub fn spawn(sink: Sink) -> Self {
        Self::spawn_inner(sink, None)
    }

    #[cfg(test)]
    fn spawn_counted(sink: Sink, builds: Arc<std::sync::atomic::AtomicUsize>) -> Self {
        Self::spawn_inner(sink, Some(builds))
    }

    #[cfg(test)]
    fn take_handle(&mut self) -> Option<thread::JoinHandle<()>> {
        self.handle.take()
    }

    fn spawn_inner(sink: Sink, builds: Option<Arc<std::sync::atomic::AtomicUsize>>) -> Self {
        let slot = Arc::new(Slot {
            state: Mutex::new(SlotState {
                req: None,
                closed: false,
            }),
            cv: Condvar::new(),
        });
        let thread_slot = slot.clone();
        let spawned = thread::Builder::new()
            .name("saturn-preview".into())
            .spawn(move || run(&thread_slot, &*sink, builds));
        let handle = match spawned {
            Ok(h) => Some(h),
            Err(e) => {
                warn!("cannot start preview thread: {e}");
                None
            }
        };
        // Detached: a build stuck on a hung read never holds up exit.
        #[cfg(not(test))]
        drop(handle);
        PreviewWorker {
            slot,
            #[cfg(test)]
            handle,
        }
    }

    /// Replace any request still waiting with `req`.
    pub fn request(&self, req: PreviewRequest) {
        if let Ok(mut s) = self.slot.state.lock() {
            s.req = Some(req);
        }
        self.slot.cv.notify_one();
    }
}

impl Drop for PreviewWorker {
    fn drop(&mut self) {
        if let Ok(mut s) = self.slot.state.lock() {
            s.closed = true;
        }
        self.slot.cv.notify_one();
    }
}

fn run(
    slot: &Slot,
    emit: &dyn Fn(FilesEvent),
    builds: Option<Arc<std::sync::atomic::AtomicUsize>>,
) {
    let mut cache: VecDeque<(CacheKey, Preview)> = VecDeque::with_capacity(CACHE_LEN + 1);
    loop {
        let req = {
            let Ok(guard) = slot.state.lock() else { return };
            let Ok(mut guard) = slot.cv.wait_while(guard, |s| s.req.is_none() && !s.closed) else {
                return;
            };
            if guard.closed {
                return;
            }
            guard.req.take()
        };
        let Some(req) = req else { continue };
        let started = Instant::now();
        let (preview, kind, bytes, hit) = match key_for(&req) {
            Err(e) => (
                Preview::Placeholder(format!("cannot read: {e}")),
                "error",
                0,
                false,
            ),
            Ok((key, meta)) => match cache.iter().position(|(k, _)| *k == key) {
                Some(i) => {
                    let entry = cache.remove(i).expect("index from position");
                    let preview = entry.1.clone();
                    cache.push_front(entry);
                    (preview, "cached", 0, true)
                }
                None => {
                    if let Some(b) = &builds {
                        b.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    let built = build_counted(&req, &meta);
                    cache.push_front((key, built.preview.clone()));
                    cache.truncate(CACHE_LEN);
                    (built.preview, built.kind, built.bytes, false)
                }
            },
        };
        emit(FilesEvent::Preview {
            id: req.id,
            preview,
        });
        debug!(
            "preview {}: kind={kind} bytes={bytes} cache_hit={hit} in {:?}",
            req.path.display(),
            started.elapsed()
        );
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;
    use crate::files::scratch_dir;
    use crate::theme::Theme;

    fn req(path: &Path) -> PreviewRequest {
        PreviewRequest {
            id: 1,
            path: path.to_path_buf(),
            width: 60,
            syn: Arc::new(Theme::load("monokai").unwrap().syntax),
            ui: UiColors {
                muted: Color::Gray,
                input_bg: Some(Color::Black),
                line_numbers: Color::DarkGray,
            },
            tab_width: 4,
        }
    }

    fn preview_of(path: &Path) -> Preview {
        build_path(&req(path)).1
    }

    fn placeholder_of(path: &Path) -> String {
        match preview_of(path) {
            Preview::Placeholder(s) => s,
            other => panic!("expected a placeholder, got {other:?}"),
        }
    }

    fn doc_of(path: &Path) -> (Vec<String>, bool, Option<String>) {
        match preview_of(path) {
            Preview::Doc {
                lines,
                line_numbers,
                note,
            } => (
                lines
                    .iter()
                    .map(|l| {
                        l.spans
                            .iter()
                            .map(|s| s.content.as_ref())
                            .collect::<String>()
                    })
                    .collect(),
                line_numbers,
                note,
            ),
            other => panic!("expected a doc, got {other:?}"),
        }
    }

    #[test]
    fn nul_byte_is_binary() {
        let d = scratch_dir("pv-nul");
        fs::write(d.join("a.bin"), b"abc\0def").unwrap();
        assert_eq!(placeholder_of(&d.join("a.bin")), "binary · 7 B");
    }

    #[test]
    fn large_png_is_binary_not_too_large() {
        let d = scratch_dir("pv-png");
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        bytes.resize(300 * 1024, b'x');
        fs::write(d.join("a.png"), bytes).unwrap();
        assert_eq!(placeholder_of(&d.join("a.png")), "binary · 300.0K");
    }

    #[test]
    fn invalid_utf8_is_lossy() {
        let d = scratch_dir("pv-lossy");
        fs::write(d.join("a.txt"), b"caf\xe9\n").unwrap();
        assert_eq!(doc_of(&d.join("a.txt")).0, vec!["caf\u{fffd}"]);
    }

    #[test]
    fn empty_file_is_one_empty_line() {
        let d = scratch_dir("pv-empty");
        fs::write(d.join("a.txt"), b"").unwrap();
        let (lines, numbers, note) = doc_of(&d.join("a.txt"));
        assert_eq!(lines, vec![""]);
        assert!(numbers && note.is_none());
    }

    #[test]
    fn large_text_is_too_large() {
        let d = scratch_dir("pv-large");
        fs::write(d.join("a.js"), vec![b'x'; 300 * 1024]).unwrap();
        assert_eq!(
            placeholder_of(&d.join("a.js")),
            "too large to preview (300.0K)"
        );
    }

    #[test]
    fn huge_notebook_is_too_large() {
        let d = scratch_dir("pv-huge-nb");
        let path = d.join("big.ipynb");
        // A text head past the NUL sniff, then a sparse tail.
        fs::write(&path, vec![b' '; SNIFF + 16]).unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(33 * 1024 * 1024)
            .unwrap();
        assert_eq!(
            placeholder_of(&path),
            "notebook too large to preview (33.0M)"
        );
    }

    #[test]
    fn notebook_fixture_is_a_notebook_preview() {
        match preview_of(Path::new("tests/fixtures/sample.ipynb")) {
            Preview::Doc {
                line_numbers,
                lines,
                ..
            } => {
                assert!(!line_numbers);
                assert!(!lines.is_empty());
            }
            other => panic!("expected a doc, got {other:?}"),
        }
    }

    #[test]
    fn long_file_keeps_the_first_lines() {
        let d = scratch_dir("pv-lines");
        let text: String = (0..5000).map(|i| format!("{i}\n")).collect();
        fs::write(d.join("a.txt"), text).unwrap();
        let (lines, _, note) = doc_of(&d.join("a.txt"));
        assert_eq!(lines.len(), 1000);
        assert_eq!(note.as_deref(), Some("first 1000 of 5000 lines"));
    }

    #[test]
    fn crlf_tabs_and_escapes_are_cleaned() {
        let d = scratch_dir("pv-clean");
        fs::write(d.join("a.txt"), "a\r\n\tb\r\n\x1b[31mred\x1b[0m\r\n").unwrap();
        let (lines, _, _) = doc_of(&d.join("a.txt"));
        assert_eq!(lines, vec!["a", "    b", "red"]);
    }

    #[test]
    fn long_cjk_line_is_cut_on_a_character() {
        let d = scratch_dir("pv-cjk");
        fs::write(d.join("a.txt"), "日".repeat(1200)).unwrap();
        let (lines, _, _) = doc_of(&d.join("a.txt"));
        assert_eq!(lines[0].chars().count(), 1000);
        assert!(lines[0].chars().all(|c| c == '日'));
    }

    #[cfg(unix)]
    #[test]
    fn fifo_is_never_opened() {
        let d = scratch_dir("pv-fifo");
        let p = d.join("p");
        let c = std::ffi::CString::new(p.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o644) }, 0);
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || tx.send(placeholder_of(&p)).unwrap());
        assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), "[fifo]");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_folder_is_a_folder() {
        let d = scratch_dir("pv-dirlink");
        fs::create_dir(d.join("real")).unwrap();
        std::os::unix::fs::symlink(d.join("real"), d.join("link")).unwrap();
        assert_eq!(placeholder_of(&d.join("link")), "[folder]");
    }

    #[test]
    fn missing_path_cannot_be_read() {
        let d = scratch_dir("pv-missing");
        let (key, preview) = build_path(&req(&d.join("gone.txt")));
        assert!(key.is_none());
        assert!(matches!(preview, Preview::Placeholder(s) if s.starts_with("cannot read: ")));
    }

    #[test]
    fn syntax_by_shebang_and_toml() {
        assert_eq!(
            crate::syntax::for_file("run", Some("#!/usr/bin/env python3")).name,
            "Python"
        );
        assert_eq!(crate::syntax::for_file("Cargo.toml", None).name, "TOML");
        assert_eq!(crate::syntax::for_file("Makefile", None).name, "Makefile");
    }

    #[test]
    #[ignore]
    fn big_single_line_is_rejected_fast() {
        let d = scratch_dir("pv-timed-2m");
        fs::write(d.join("a.js"), vec![b'x'; 2 * 1024 * 1024]).unwrap();
        let t = Instant::now();
        assert_eq!(
            placeholder_of(&d.join("a.js")),
            "too large to preview (2.0M)"
        );
        assert!(t.elapsed() < Duration::from_millis(50), "{:?}", t.elapsed());
    }

    #[test]
    #[ignore]
    fn long_single_line_is_cut_fast() {
        let d = scratch_dir("pv-timed-200k");
        fs::write(d.join("a.js"), vec![b'x'; 200 * 1024]).unwrap();
        let t = Instant::now();
        let (lines, _, _) = doc_of(&d.join("a.js"));
        assert!(t.elapsed() < Duration::from_millis(50), "{:?}", t.elapsed());
        assert_eq!(lines[0].len(), 1000);
    }

    // ----- worker ----------------------------------------------------------------

    fn collecting() -> (Sink, mpsc::Receiver<(u64, Preview)>) {
        let (tx, rx) = mpsc::channel();
        let sink: Sink = Arc::new(move |ev| {
            if let FilesEvent::Preview { id, preview } = ev {
                let _ = tx.send((id, preview));
            }
        });
        (sink, rx)
    }

    fn wait_for(rx: &mpsc::Receiver<(u64, Preview)>, id: u64) -> Vec<u64> {
        let mut seen = Vec::new();
        loop {
            let (got, _) = rx
                .recv_timeout(Duration::from_secs(5))
                .expect("preview event");
            seen.push(got);
            if got == id {
                return seen;
            }
        }
    }

    fn numbered(path: &Path, id: u64) -> PreviewRequest {
        PreviewRequest { id, ..req(path) }
    }

    #[test]
    fn worker_merges_requests() {
        let d = scratch_dir("pv-w-merge");
        fs::write(d.join("a.txt"), "a").unwrap();
        fs::write(d.join("b.txt"), "b").unwrap();
        let (sink, rx) = collecting();
        let w = PreviewWorker::spawn(sink);
        w.request(numbered(&d.join("a.txt"), 10));
        w.request(numbered(&d.join("b.txt"), 11));
        let seen = wait_for(&rx, 11);
        assert!(seen.iter().all(|&id| id == 10 || id == 11), "{seen:?}");
    }

    #[test]
    fn worker_caches_and_notices_changes() {
        let d = scratch_dir("pv-w-cache");
        let p = d.join("a.txt");
        fs::write(&p, "a").unwrap();
        let (sink, rx) = collecting();
        let builds = Arc::new(AtomicUsize::new(0));
        let w = PreviewWorker::spawn_counted(sink, builds.clone());
        w.request(numbered(&p, 1));
        wait_for(&rx, 1);
        w.request(numbered(&p, 2));
        wait_for(&rx, 2);
        assert_eq!(builds.load(std::sync::atomic::Ordering::Relaxed), 1);
        let later = SystemTime::now() + Duration::from_secs(10);
        File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(later)
            .unwrap();
        w.request(numbered(&p, 3));
        wait_for(&rx, 3);
        assert_eq!(builds.load(std::sync::atomic::Ordering::Relaxed), 2);
    }

    #[test]
    fn dropping_the_worker_ends_its_thread() {
        let (sink, _rx) = collecting();
        let mut w = PreviewWorker::spawn(sink);
        let handle = w.take_handle().unwrap();
        drop(w);
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            handle.join().unwrap();
            tx.send(()).unwrap();
        });
        rx.recv_timeout(Duration::from_secs(1))
            .expect("worker thread ended");
    }
}
