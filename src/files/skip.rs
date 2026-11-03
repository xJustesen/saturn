//! The built-in skip list: entries the scanner never enters or lists.

use std::ffi::OsStr;
use std::path::Path;

use crate::files::PickerPrefs;

/// Folder names pruned even with both toggles on.
const SKIP_NAMES: &[&str] = &[
    "node_modules",
    ".venv",
    "venv",
    "__pycache__",
    ".ipynb_checkpoints",
    "site-packages",
    ".tox",
];

/// Marker files: Cargo's `target/` and the pytest, ruff and mypy caches write
/// `CACHEDIR.TAG`; every Python virtual environment holds `pyvenv.cfg`.
const MARKERS: &[&str] = &["CACHEDIR.TAG", "pyvenv.cfg"];

/// True when the walker must not enter or list this entry.
pub fn prune(
    path: &Path,
    name: &OsStr,
    is_dir: bool,
    depth: usize,
    prefs: PickerPrefs,
    home: Option<&Path>,
) -> bool {
    if name == ".git" {
        return true;
    }
    if depth > 0 && !prefs.show_hidden && name.as_encoded_bytes().starts_with(b".") {
        return true;
    }
    if !is_dir {
        return false;
    }
    if SKIP_NAMES.iter().any(|n| name == *n) {
        return true;
    }
    if let Some(home) = home
        && (path == home.join("Library") || path == home.join(".Trash"))
    {
        return true;
    }
    MARKERS
        .iter()
        .any(|m| path.join(m).symlink_metadata().is_ok())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::files::scratch_dir;

    fn pruned(dir: &Path, name: &str, is_dir: bool, depth: usize, prefs: PickerPrefs) -> bool {
        prune(
            &dir.join(name),
            OsStr::new(name),
            is_dir,
            depth,
            prefs,
            None,
        )
    }

    #[test]
    fn listed_names_are_pruned() {
        let d = scratch_dir("skip-names");
        let all = PickerPrefs {
            show_hidden: true,
            show_ignored: true,
        };
        for n in SKIP_NAMES {
            assert!(pruned(&d, n, true, 1, all), "{n}");
        }
        assert!(pruned(&d, ".git", true, 1, all));
        // A file that happens to share a name is kept.
        assert!(!pruned(&d, "venv", false, 1, all));
    }

    #[test]
    fn git_file_is_pruned() {
        let d = scratch_dir("skip-gitfile");
        assert!(pruned(
            &d,
            ".git",
            false,
            1,
            PickerPrefs {
                show_hidden: true,
                show_ignored: true
            }
        ));
    }

    #[test]
    fn marker_files_prune_their_folder() {
        let d = scratch_dir("skip-markers");
        fs::create_dir_all(d.join("target")).unwrap();
        fs::write(
            d.join("target/CACHEDIR.TAG"),
            "Signature: 8a477f597d28d172789f06886806bc55",
        )
        .unwrap();
        fs::create_dir_all(d.join("my-env")).unwrap();
        fs::write(d.join("my-env/pyvenv.cfg"), "home = /usr/bin").unwrap();
        fs::create_dir_all(d.join("build/target")).unwrap();
        let p = PickerPrefs::default();
        assert!(pruned(&d, "target", true, 1, p));
        assert!(pruned(&d, "my-env", true, 1, p));
        assert!(!pruned(&d.join("build"), "target", true, 2, p));
    }

    #[test]
    fn home_library_and_trash_are_pruned() {
        let home = scratch_dir("skip-home");
        let p = PickerPrefs {
            show_hidden: true,
            show_ignored: true,
        };
        assert!(prune(
            &home.join("Library"),
            OsStr::new("Library"),
            true,
            1,
            p,
            Some(&home)
        ));
        assert!(prune(
            &home.join(".Trash"),
            OsStr::new(".Trash"),
            true,
            1,
            p,
            Some(&home)
        ));
        assert!(!prune(
            &home.join("x/Library"),
            OsStr::new("Library"),
            true,
            2,
            p,
            Some(&home)
        ));
    }

    #[test]
    fn dot_names_depend_on_depth_and_toggle() {
        let d = scratch_dir("skip-hidden");
        let off = PickerPrefs::default();
        let on = PickerPrefs {
            show_hidden: true,
            show_ignored: false,
        };
        assert!(pruned(&d, ".env", false, 1, off));
        assert!(pruned(&d, ".config", true, 1, off));
        assert!(!pruned(&d, ".dotfiles", true, 0, off));
        assert!(!pruned(&d, ".env", false, 1, on));
        assert!(!pruned(&d, ".config", true, 1, on));
    }
}
