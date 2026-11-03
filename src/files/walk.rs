//! The scanner: one walk of the root with every `ignore` filter off. saturn
//! applies git's ignore rules itself, so an ignored file is tagged instead of
//! dropped (gitignored notebooks stay listed, and the ignored toggle is an
//! instant refilter).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::{Match, WalkBuilder};
use nucleo_matcher::Utf32String;
use tracing::{info, warn};

use crate::files::root::find_root;
use crate::files::{FileEntry, FilesEvent, PickerPrefs, ScanLimits, ScanSummary, Sink, skip};
use crate::ui::text::clean_name;

/// The most entries listed by default; the walk stops when it is reached.
pub const SHOWN_CAP: u32 = 50_000;
/// The most ignored non-notebook entries kept; later ones are only counted.
pub const EXTRA_CAP: u32 = 50_000;
/// The most walker entries visited, folders included; the walk stops there.
pub const VISIT_CAP: u32 = 1_000_000;

const BATCH_LEN: usize = 512;
const BATCH_EVERY: Duration = Duration::from_millis(30);

/// Where the global excludes file (`core.excludesFile`) comes from.
#[derive(Clone, Debug)]
pub enum GlobalExcludes {
    /// Read git's configuration (the app).
    Load,
    /// Load nothing (most tests).
    Skip,
    /// Load this file (one test).
    File(PathBuf),
}

#[derive(Clone, Debug)]
pub struct ScanRequest {
    pub scan_id: u64,
    pub start: PathBuf,
    pub open_path: Option<PathBuf>,
    pub prefs: PickerPrefs,
    pub limits: ScanLimits,
    pub home: Option<PathBuf>,
    pub global: GlobalExcludes,
    pub cancel: Arc<AtomicBool>,
}

/// Run `scan` on a detached thread. If the thread cannot start, report an
/// empty `Done` straight away.
pub fn spawn_scan(req: ScanRequest, sink: Sink) {
    let scan_id = req.scan_id;
    let thread_sink = sink.clone();
    let spawned = thread::Builder::new()
        .name("saturn-scan".into())
        .spawn(move || scan(req, &*thread_sink));
    if let Err(e) = spawned {
        warn!("cannot start scan thread: {e}");
        sink(FilesEvent::Done {
            scan_id,
            summary: ScanSummary::default(),
        });
    }
}

/// One level of the ignore-rule stack: a folder on the path to the current
/// entry, its `.gitignore`, and whether the folder itself is ignored.
struct Frame {
    depth: usize,
    gi: Option<Gitignore>,
    ignored: bool,
}

/// Ignore-rule verdict from one matcher: `Some(true)` ignore, `Some(false)`
/// re-include, `None` no rule matched.
fn verdict(gi: &Gitignore, path: &Path, is_dir: bool) -> Option<bool> {
    match gi.matched(path, is_dir) {
        Match::Ignore(_) => Some(true),
        Match::Whitelist(_) => Some(false),
        Match::None => None,
    }
}

fn load_gitignore(dir: &Path) -> Option<Gitignore> {
    let file = dir.join(".gitignore");
    if !file.is_file() {
        return None;
    }
    let (gi, err) = Gitignore::new(&file);
    if let Some(e) = err {
        warn!("{}: {e}", file.display());
    }
    Some(gi)
}

fn build_or_warn(builder: &GitignoreBuilder, what: &str) -> Gitignore {
    builder.build().unwrap_or_else(|e| {
        warn!("{what}: {e}");
        Gitignore::empty()
    })
}

fn error_path(e: &ignore::Error) -> Option<&Path> {
    match e {
        ignore::Error::WithPath { path, .. } => Some(path),
        ignore::Error::WithDepth { err, .. } | ignore::Error::WithLineNumber { err, .. } => {
            error_path(err)
        }
        ignore::Error::Loop { child, .. } => Some(child),
        ignore::Error::Partial(v) => v.iter().find_map(error_path),
        _ => None,
    }
}

/// `path` relative to `root`, lossy UTF-8 and `/`-separated, with control
/// characters replaced.
fn display_of(path: &Path, root: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    let joined = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    clean_name(&joined).into_owned()
}

/// The scanner's core loop. It reports through `emit`, so tests call it
/// directly.
pub fn scan(req: ScanRequest, emit: &dyn Fn(FilesEvent)) {
    let started = Instant::now();
    let scan_id = req.scan_id;
    let info_root = find_root(&req.start);
    let root = info_root.root.clone();
    let open_path = req.open_path.as_ref().map(|p| {
        std::fs::canonicalize(p).unwrap_or_else(|_| {
            std::env::current_dir()
                .map(|d| d.join(p))
                .unwrap_or_else(|_| p.clone())
        })
    });
    emit(FilesEvent::Root {
        scan_id,
        root: root.clone(),
        open_path,
    });

    // Base matchers: only inside a repository.
    let mut base: Vec<Gitignore> = Vec::new();
    if let Some(repo) = &info_root.repo {
        let mut b = GitignoreBuilder::new(&repo.work_tree);
        let exclude = repo.common_dir.join("info").join("exclude");
        if exclude.is_file()
            && let Some(e) = b.add(&exclude)
        {
            warn!("{}: {e}", exclude.display());
        }
        base.push(build_or_warn(&b, "info/exclude"));
        let global = match &req.global {
            GlobalExcludes::Load => {
                let (gi, err) = GitignoreBuilder::new(&repo.work_tree).build_global();
                if let Some(e) = err {
                    warn!("global excludes: {e}");
                }
                gi
            }
            GlobalExcludes::Skip => Gitignore::empty(),
            GlobalExcludes::File(path) => {
                let mut b = GitignoreBuilder::new(&repo.work_tree);
                if let Some(e) = b.add(path) {
                    warn!("{}: {e}", path.display());
                }
                build_or_warn(&b, "global excludes")
            }
        };
        base.push(global);
    }

    let prefs = req.prefs;
    let home = req.home.clone();
    let walker = WalkBuilder::new(&root)
        .standard_filters(false)
        .follow_links(false)
        .same_file_system(true)
        .sort_by_file_name(|a, b| a.cmp(b))
        .filter_entry(move |e| {
            let is_dir = e.file_type().is_some_and(|t| t.is_dir());
            !skip::prune(
                e.path(),
                e.file_name(),
                is_dir,
                e.depth(),
                prefs,
                home.as_deref(),
            )
        })
        .build();

    let limits = req.limits;
    let mut sum = ScanSummary::default();
    let mut unreadable_paths: Vec<PathBuf> = Vec::new();
    let mut frames: Vec<Frame> = Vec::new();
    let mut batch: Vec<FileEntry> = Vec::new();
    let mut last_emit = Instant::now();

    for item in walker {
        if req.cancel.load(Ordering::Relaxed) {
            return;
        }
        let entry = match item {
            Ok(e) if e.depth() == 0 => {
                frames.push(Frame {
                    depth: 0,
                    gi: load_gitignore(e.path()),
                    ignored: false,
                });
                continue;
            }
            item => {
                if sum.visited == limits.visit {
                    sum.hit_visit_cap = true;
                    break;
                }
                sum.visited += 1;
                match item {
                    Ok(e) => e,
                    Err(err) => {
                        sum.unreadable += 1;
                        if unreadable_paths.len() < 5
                            && let Some(p) = error_path(&err)
                        {
                            unreadable_paths.push(p.to_path_buf());
                        }
                        continue;
                    }
                }
            }
        };

        let depth = entry.depth();
        while frames.last().is_some_and(|f| f.depth >= depth) {
            frames.pop();
        }
        let path = entry.path();
        let is_dir = entry.file_type().is_some_and(|t| t.is_dir());
        let ignored = if frames.last().is_some_and(|f| f.ignored) {
            true
        } else {
            frames
                .iter()
                .rev()
                .filter_map(|f| f.gi.as_ref())
                .chain(base.iter())
                .find_map(|gi| verdict(gi, path, is_dir))
                .unwrap_or(false)
        };

        if is_dir {
            frames.push(Frame {
                depth,
                gi: load_gitignore(path),
                ignored,
            });
        } else {
            let display = display_of(path, &root);
            let name_at = display.rfind('/').map_or(0, |i| i + 1) as u32;
            let notebook = entry.file_name().to_string_lossy().ends_with(".ipynb");
            let fe = FileEntry {
                path: path.to_path_buf(),
                hay: Utf32String::from(display.as_str()),
                display: display.into_boxed_str(),
                name_at,
                ignored,
                notebook,
            };
            if !ignored || notebook {
                if sum.shown == limits.shown {
                    sum.hit_shown_cap = true;
                    break;
                }
                sum.shown += 1;
                batch.push(fe);
            } else if sum.kept_extra < limits.extra {
                sum.kept_extra += 1;
                batch.push(fe);
            } else {
                sum.hit_extra_cap = true;
                sum.dropped_extra += 1;
            }
        }

        if !batch.is_empty() && (batch.len() >= BATCH_LEN || last_emit.elapsed() >= BATCH_EVERY) {
            emit(FilesEvent::Batch {
                scan_id,
                entries: std::mem::take(&mut batch),
            });
            last_emit = Instant::now();
        }
    }

    if !batch.is_empty() {
        emit(FilesEvent::Batch {
            scan_id,
            entries: batch,
        });
    }
    sum.elapsed = started.elapsed();
    info!(
        "scan {}: hidden={} ignored={} kept={} shown={} caps(shown={} extra={} visit={}) unreadable={} in {:?}",
        root.display(),
        prefs.show_hidden,
        prefs.show_ignored,
        sum.shown + sum.kept_extra,
        sum.shown,
        sum.hit_shown_cap,
        sum.hit_extra_cap,
        sum.hit_visit_cap,
        sum.unreadable,
        sum.elapsed,
    );
    if sum.unreadable > 0 {
        warn!(
            "scan {}: {} unreadable, first: {:?}",
            root.display(),
            sum.unreadable,
            unreadable_paths
        );
    }
    emit(FilesEvent::Done {
        scan_id,
        summary: sum,
    });
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::fs;

    use super::*;
    use crate::files::scratch_dir;

    struct Out {
        root: Option<PathBuf>,
        open_path: Option<PathBuf>,
        entries: Vec<FileEntry>,
        summary: Option<ScanSummary>,
    }

    impl Out {
        fn find(&self, display: &str) -> Option<&FileEntry> {
            self.entries.iter().find(|e| &*e.display == display)
        }

        fn displays(&self) -> Vec<&str> {
            self.entries.iter().map(|e| &*e.display).collect()
        }
    }

    fn request(start: &Path, prefs: PickerPrefs, limits: ScanLimits) -> ScanRequest {
        ScanRequest {
            scan_id: 7,
            start: start.to_path_buf(),
            open_path: None,
            prefs,
            limits,
            home: None,
            global: GlobalExcludes::Skip,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    fn run(req: ScanRequest) -> Out {
        let out = RefCell::new(Out {
            root: None,
            open_path: None,
            entries: Vec::new(),
            summary: None,
        });
        scan(req, &|ev| {
            let mut o = out.borrow_mut();
            match ev {
                FilesEvent::Root {
                    scan_id,
                    root,
                    open_path,
                } => {
                    assert_eq!(scan_id, 7);
                    o.root = Some(root);
                    o.open_path = open_path;
                }
                FilesEvent::Batch { entries, .. } => {
                    assert!(!entries.is_empty() && entries.len() <= BATCH_LEN);
                    o.entries.extend(entries);
                }
                FilesEvent::Done { summary, .. } => o.summary = Some(summary),
                FilesEvent::Preview { .. } => unreachable!("the scanner sends no previews"),
            }
        });
        out.into_inner()
    }

    fn scan_dir(dir: &Path) -> Out {
        run(request(dir, PickerPrefs::default(), ScanLimits::default()))
    }

    fn touch(dir: &Path, rel: &str) {
        let p = dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, "x").unwrap();
    }

    fn repo(tag: &str) -> PathBuf {
        let d = scratch_dir(tag);
        fs::create_dir(d.join(".git")).unwrap();
        d
    }

    #[test]
    fn root_gitignore_ignores_folder_but_lists_notebooks() {
        let d = repo("walk-root-gi");
        fs::write(d.join(".gitignore"), "data/\n").unwrap();
        touch(&d, "data/a.csv");
        touch(&d, "data/n.ipynb");
        touch(&d, "src/main.py");
        let out = scan_dir(&d);
        assert!(out.find("data/a.csv").unwrap().ignored);
        let nb = out.find("data/n.ipynb").unwrap();
        assert!(nb.ignored && nb.notebook);
        assert!(!out.find("src/main.py").unwrap().ignored);
        let s = out.summary.unwrap();
        assert_eq!((s.shown, s.kept_extra), (2, 1));
    }

    #[test]
    fn nested_gitignore_reincludes() {
        let d = repo("walk-reinclude");
        fs::write(d.join(".gitignore"), "*.txt\n").unwrap();
        touch(&d, "sub/keep.txt");
        touch(&d, "sub/drop.txt");
        fs::write(d.join("sub/.gitignore"), "!keep.txt\n").unwrap();
        let out = scan_dir(&d);
        assert!(!out.find("sub/keep.txt").unwrap().ignored);
        assert!(out.find("sub/drop.txt").unwrap().ignored);
    }

    #[test]
    fn whitelist_inside_ignored_folder_does_not_reinclude() {
        let d = repo("walk-ignored-parent");
        fs::write(d.join(".gitignore"), "build/\n").unwrap();
        touch(&d, "build/keep.txt");
        fs::write(d.join("build/.gitignore"), "!keep.txt\n").unwrap();
        let out = scan_dir(&d);
        assert!(out.find("build/keep.txt").unwrap().ignored);
    }

    #[test]
    fn info_exclude_applies() {
        let d = repo("walk-info-exclude");
        fs::create_dir_all(d.join(".git/info")).unwrap();
        fs::write(d.join(".git/info/exclude"), "secret.txt\n").unwrap();
        touch(&d, "secret.txt");
        touch(&d, "open.txt");
        let out = scan_dir(&d);
        assert!(out.find("secret.txt").unwrap().ignored);
        assert!(!out.find("open.txt").unwrap().ignored);
    }

    #[test]
    fn info_exclude_through_worktree_git_file() {
        let d = scratch_dir("walk-worktree");
        let wt_git = d.join("main/.git/worktrees/wt");
        fs::create_dir_all(&wt_git).unwrap();
        fs::write(wt_git.join("commondir"), "../..\n").unwrap();
        fs::create_dir_all(d.join("main/.git/info")).unwrap();
        fs::write(d.join("main/.git/info/exclude"), "*.log\n").unwrap();
        let wt = d.join("wt");
        fs::create_dir_all(&wt).unwrap();
        fs::write(wt.join(".git"), format!("gitdir: {}\n", wt_git.display())).unwrap();
        touch(&wt, "run.log");
        touch(&wt, "run.py");
        let out = scan_dir(&wt);
        assert!(out.find("run.log").unwrap().ignored);
        assert!(!out.find("run.py").unwrap().ignored);
        assert!(out.find(".git").is_none());
    }

    #[test]
    fn global_excludes_file() {
        let d = repo("walk-global");
        let global = scratch_dir("walk-global-file").join("ignore");
        fs::write(&global, "*.tmp\n").unwrap();
        touch(&d, "x.tmp");
        touch(&d, "x.py");
        let mut req = request(&d, PickerPrefs::default(), ScanLimits::default());
        req.global = GlobalExcludes::File(global);
        let out = run(req);
        assert!(out.find("x.tmp").unwrap().ignored);
        assert!(!out.find("x.py").unwrap().ignored);
    }

    #[test]
    fn gitignore_applies_outside_a_repository() {
        let d = scratch_dir("walk-no-repo");
        fs::write(d.join(".gitignore"), "*.csv\n").unwrap();
        touch(&d, "a.csv");
        touch(&d, "a.py");
        let out = scan_dir(&d);
        assert!(out.find("a.csv").unwrap().ignored);
        assert!(!out.find("a.py").unwrap().ignored);
    }

    #[test]
    fn nested_repository_is_a_plain_folder() {
        let d = repo("walk-nested-repo");
        fs::create_dir_all(d.join("inner/.git/info")).unwrap();
        fs::write(d.join("inner/.git/info/exclude"), "inner_excluded.txt\n").unwrap();
        touch(&d, "inner/.git/HEAD");
        fs::write(d.join("inner/.gitignore"), "*.bin\n").unwrap();
        touch(&d, "inner/a.bin");
        touch(&d, "inner/inner_excluded.txt");
        let out = scan_dir(&d);
        assert!(out.find("inner/a.bin").unwrap().ignored);
        // The nested repository's info/exclude is not read.
        assert!(!out.find("inner/inner_excluded.txt").unwrap().ignored);
        assert!(out.entries.iter().all(|e| !e.display.contains(".git/")));
    }

    #[test]
    fn hidden_entries_follow_the_toggle() {
        let d = repo("walk-hidden");
        touch(&d, ".env");
        touch(&d, ".config/x.toml");
        touch(&d, "a.py");
        let out = scan_dir(&d);
        assert_eq!(out.displays(), vec!["a.py"]);
        let out = run(request(
            &d,
            PickerPrefs {
                show_hidden: true,
                show_ignored: false,
            },
            ScanLimits::default(),
        ));
        assert_eq!(out.displays(), vec![".config/x.toml", ".env", "a.py"]);
    }

    #[test]
    fn dot_named_root_lists_its_files() {
        let d = scratch_dir("walk-dotroot").join(".dotfiles");
        touch(&d, "zshrc");
        let out = scan_dir(&d);
        assert_eq!(out.displays(), vec!["zshrc"]);
    }

    #[test]
    fn skip_folders_are_never_entered() {
        let d = repo("walk-skip");
        touch(&d, "node_modules/x.js");
        touch(&d, "env/lib/site-packages/y.py");
        touch(&d, "env/pyvenv.cfg");
        touch(&d, "__pycache__/z.pyc");
        touch(&d, "ok.py");
        let all = PickerPrefs {
            show_hidden: true,
            show_ignored: true,
        };
        let out = run(request(&d, all, ScanLimits::default()));
        assert_eq!(out.displays(), vec!["ok.py"]);
    }

    fn limits(shown: u32, extra: u32) -> ScanLimits {
        ScanLimits {
            shown,
            extra,
            visit: VISIT_CAP,
        }
    }

    #[test]
    fn shown_cap_exactly_reached_sets_no_flag() {
        let d = scratch_dir("walk-cap-exact");
        for n in ["a", "b", "c"] {
            touch(&d, n);
        }
        let s = run(request(&d, PickerPrefs::default(), limits(3, 10)))
            .summary
            .unwrap();
        assert_eq!(s.shown, 3);
        assert!(!s.hit_shown_cap && !s.hit_extra_cap && !s.hit_visit_cap);
    }

    #[test]
    fn shown_cap_stops_the_walk() {
        let d = scratch_dir("walk-cap-over");
        for n in ["a", "b", "c", "d"] {
            touch(&d, n);
        }
        let out = run(request(&d, PickerPrefs::default(), limits(3, 10)));
        assert_eq!(out.entries.len(), 3);
        assert!(out.summary.unwrap().hit_shown_cap);
    }

    #[test]
    fn extra_cap_counts_dropped_entries() {
        let d = scratch_dir("walk-cap-extra");
        fs::write(d.join(".gitignore"), "*.csv\n").unwrap();
        for n in ["1", "2", "3", "4", "5"] {
            touch(&d, &format!("{n}.csv"));
        }
        let out = run(request(&d, PickerPrefs::default(), limits(10, 2)));
        assert_eq!(out.entries.len(), 2);
        let s = out.summary.unwrap();
        assert_eq!((s.kept_extra, s.dropped_extra), (2, 3));
        assert!(s.hit_extra_cap);
    }

    #[test]
    fn visit_cap_stops_the_walk() {
        let d = scratch_dir("walk-cap-visit");
        for n in ["a", "b", "c", "d"] {
            touch(&d, n);
        }
        let out = run(request(
            &d,
            PickerPrefs::default(),
            ScanLimits {
                shown: 10,
                extra: 10,
                visit: 2,
            },
        ));
        assert_eq!(out.entries.len(), 2);
        assert!(out.summary.unwrap().hit_visit_cap);
    }

    #[test]
    fn cancelled_scan_sends_no_done() {
        let d = scratch_dir("walk-cancel");
        touch(&d, "a");
        let req = request(&d, PickerPrefs::default(), ScanLimits::default());
        req.cancel.store(true, Ordering::Relaxed);
        let out = run(req);
        assert!(out.summary.is_none());
        assert!(out.entries.is_empty());
    }

    #[test]
    fn order_is_stable() {
        let d = scratch_dir("walk-order");
        for n in ["b/2", "a/1", "c", "a/0", "Z"] {
            touch(&d, n);
        }
        let first = scan_dir(&d)
            .displays()
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>();
        let second = scan_dir(&d)
            .displays()
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>();
        assert_eq!(first, second);
        assert_eq!(first, vec!["Z", "a/0", "a/1", "b/2", "c"]);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_loop_ends() {
        let d = scratch_dir("walk-loop");
        touch(&d, "a/f");
        std::os::unix::fs::symlink("..", d.join("a/link")).unwrap();
        let out = scan_dir(&d);
        assert_eq!(out.displays(), vec!["a/f", "a/link"]);
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_folder_is_counted() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let d = scratch_dir("walk-unreadable");
        touch(&d, "locked/x");
        touch(&d, "ok");
        fs::set_permissions(d.join("locked"), fs::Permissions::from_mode(0o000)).unwrap();
        let out = scan_dir(&d);
        fs::set_permissions(d.join("locked"), fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(out.summary.as_ref().unwrap().unreadable, 1);
        assert_eq!(out.displays(), vec!["ok"]);
    }

    #[test]
    fn root_event_carries_canonical_open_path() {
        let d = scratch_dir("walk-open-path");
        let mut req = request(&d, PickerPrefs::default(), ScanLimits::default());
        req.open_path = Some(PathBuf::from("tests/fixtures/sample.ipynb"));
        let out = run(req);
        assert_eq!(out.root.unwrap(), fs::canonicalize(&d).unwrap());
        assert_eq!(
            out.open_path.unwrap(),
            fs::canonicalize("tests/fixtures/sample.ipynb").unwrap()
        );
    }

    #[test]
    fn entry_fields() {
        let d = scratch_dir("walk-fields");
        touch(&d, "dir/sub/nb.ipynb");
        let out = scan_dir(&d);
        let e = out.find("dir/sub/nb.ipynb").unwrap();
        assert_eq!(&e.display[e.name_at as usize..], "nb.ipynb");
        assert!(e.notebook && !e.ignored);
        assert_eq!(
            e.path,
            fs::canonicalize(&d).unwrap().join("dir/sub/nb.ipynb")
        );
    }

    #[test]
    fn non_utf8_name_is_lossy_in_display_only() {
        if cfg!(target_os = "macos") {
            // APFS rejects non-UTF-8 file names.
            return;
        }
        #[cfg(unix)]
        {
            use std::ffi::OsStr;
            use std::os::unix::ffi::OsStrExt;
            let d = scratch_dir("walk-non-utf8");
            let name = OsStr::from_bytes(b"caf\xe9.txt");
            fs::write(d.join(name), "x").unwrap();
            let out = scan_dir(&d);
            let e = &out.entries[0];
            assert_eq!(&*e.display, "caf\u{fffd}.txt");
            assert_eq!(e.path.file_name().unwrap().as_bytes(), b"caf\xe9.txt");
        }
    }

    /// Compare each path's ignored flag with git's. Needs `git`.
    #[test]
    #[ignore]
    fn ignored_flags_match_git_check_ignore() {
        use std::process::Command;
        let d = scratch_dir("walk-vs-git");
        let git = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(&d)
                .args(["-c", "core.excludesFile=/dev/null"])
                .args(args)
                .status()
                .unwrap()
        };
        assert!(git(&["init", "-q"]).success());
        fs::write(
            d.join(".gitignore"),
            "*.log\ndata/\n!important.log\n/top.txt\nbuild/**/out\n",
        )
        .unwrap();
        fs::create_dir_all(d.join(".git/info")).unwrap();
        fs::write(d.join(".git/info/exclude"), "local.cfg\n").unwrap();
        for rel in [
            "a.log",
            "important.log",
            "top.txt",
            "sub/top.txt",
            "data/x.csv",
            "data/deep/y.csv",
            "sub/data/z.csv",
            "build/a/out",
            "build/a/keep",
            "local.cfg",
            "sub/local.cfg",
            "sub/n.ipynb",
            "nested/k.log",
            "nested/keep.me",
        ] {
            touch(&d, rel);
        }
        fs::write(d.join("nested/.gitignore"), "!k.log\n*.me\n").unwrap();
        let out = scan_dir(&d);
        for e in &out.entries {
            let git_ignored = git(&["check-ignore", "--no-index", "-q", &e.display]).success();
            assert_eq!(e.ignored, git_ignored, "{}", e.display);
        }
    }
}
