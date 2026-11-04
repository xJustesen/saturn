//! The files picker's listing and previews: root discovery, the scanner and
//! its ignore rules, and (in `preview`) the preview worker. No drawing code.

pub mod outline;
pub mod preview;
pub mod projects;
pub mod root;
pub mod skip;
pub mod walk;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use nucleo_matcher::Utf32String;

pub use walk::{EXTRA_CAP, SHOWN_CAP, VISIT_CAP};

/// One file found by the scanner. Folders are not entries.
#[derive(Clone, Debug)]
pub struct FileEntry {
    /// The original path, used to open the file.
    pub path: PathBuf,
    /// The path relative to the root: lossy UTF-8, `/`-separated, control
    /// characters replaced one-for-one.
    pub display: Box<str>,
    /// Fuzzy haystack built from `display`.
    pub hay: Utf32String,
    /// Byte offset in `display` where the file name starts.
    pub name_at: u32,
    pub ignored: bool,
    /// The name ends in `.ipynb`.
    pub notebook: bool,
}

impl FileEntry {
    /// An entry for a path found some other way than the scanner (the
    /// recent-notebooks list). `display` is shown and matched as is.
    pub fn new(path: PathBuf, display: String, ignored: bool) -> Self {
        let name_at = display.rfind('/').map_or(0, |i| i + 1) as u32;
        let notebook = path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().ends_with(".ipynb"));
        FileEntry {
            path,
            hay: Utf32String::from(display.as_str()),
            display: display.into_boxed_str(),
            name_at,
            ignored,
            notebook,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PickerPrefs {
    pub show_hidden: bool,
    pub show_ignored: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct ScanLimits {
    pub shown: u32,
    pub extra: u32,
    pub visit: u32,
}

impl Default for ScanLimits {
    fn default() -> Self {
        ScanLimits {
            shown: SHOWN_CAP,
            extra: EXTRA_CAP,
            visit: VISIT_CAP,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ScanSummary {
    /// Entries listed by default (`!ignored || notebook`).
    pub shown: u32,
    /// Ignored non-notebook entries kept.
    pub kept_extra: u32,
    /// Ignored non-notebook entries counted but not kept (over `EXTRA_CAP`).
    pub dropped_extra: u32,
    /// Walker entries visited, folders included.
    pub visited: u32,
    pub unreadable: u32,
    pub hit_shown_cap: bool,
    pub hit_extra_cap: bool,
    pub hit_visit_cap: bool,
    pub elapsed: Duration,
}

#[derive(Debug)]
pub enum FilesEvent {
    /// The first event of a scan: the canonical root and open notebook path.
    Root {
        scan_id: u64,
        root: PathBuf,
        open_path: Option<PathBuf>,
    },
    Batch {
        scan_id: u64,
        entries: Vec<FileEntry>,
    },
    Done {
        scan_id: u64,
        summary: ScanSummary,
    },
    /// A preview built for the request with this `id`.
    Preview {
        id: u64,
        preview: preview::Preview,
    },
}

/// Where the workers report. The app wraps each event in `AppEvent::Files`;
/// tests collect them.
pub type Sink = Arc<dyn Fn(FilesEvent) + Send + Sync>;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// A process-wide id for scans and preview requests, so an event from a
/// closed picker never carries a number a new picker is waiting for.
pub fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "K", "M", "G"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1}{}", UNITS[i])
    }
}

/// `p` with the home folder written as `~`.
pub fn tilde(p: &Path, home: Option<&Path>) -> String {
    if let Some(home) = home
        && let Ok(rest) = p.strip_prefix(home)
    {
        if rest.as_os_str().is_empty() {
            return "~".to_string();
        }
        return format!("~/{}", rest.display());
    }
    p.display().to_string()
}

pub fn shorten_path(p: &Path, max: usize) -> String {
    let s = tilde(p, dirs::home_dir().as_deref());
    if s.chars().count() <= max {
        return s;
    }
    let parts: Vec<&str> = s.split('/').collect();
    let mut tail = String::new();
    for part in parts.iter().rev() {
        let candidate = if tail.is_empty() {
            part.to_string()
        } else {
            format!("{part}/{tail}")
        };
        if candidate.chars().count() + 2 > max {
            break;
        }
        tail = candidate;
    }
    format!("…/{tail}")
}

/// A fresh, empty folder under the temp dir, named after the process and `tag`.
#[cfg(test)]
pub(crate) fn scratch_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("saturn-files-test-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tilde_shortens_paths_under_home() {
        let home = Path::new("/Users/me");
        assert_eq!(tilde(Path::new("/Users/me/dev/x"), Some(home)), "~/dev/x");
        assert_eq!(tilde(Path::new("/Users/me"), Some(home)), "~");
        assert_eq!(tilde(Path::new("/opt/x"), Some(home)), "/opt/x");
        assert_eq!(tilde(Path::new("/Users/me/x"), None), "/Users/me/x");
    }

    #[test]
    fn file_entry_new_finds_name_and_kind() {
        let e = FileEntry::new(PathBuf::from("/a/b/nb.ipynb"), "~/b/nb.ipynb".into(), false);
        assert_eq!(&e.display[e.name_at as usize..], "nb.ipynb");
        assert!(e.notebook);
        let e = FileEntry::new(PathBuf::from("/a/x.py"), "x.py".into(), true);
        assert_eq!(e.name_at, 0);
        assert!(!e.notebook);
        assert!(e.ignored);
    }
}
