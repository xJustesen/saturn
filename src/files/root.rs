//! Root discovery: the git work tree that holds the starting folder, or the
//! starting folder itself.

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct RootInfo {
    pub root: PathBuf,
    pub repo: Option<Repo>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Repo {
    /// The folder that contains `.git`.
    pub work_tree: PathBuf,
    /// The shared git folder (`info/exclude` lives here). For a worktree or
    /// submodule this is found through the `.git` file.
    pub common_dir: PathBuf,
}

pub fn find_root(start: &Path) -> RootInfo {
    let start = fs::canonicalize(start).unwrap_or_else(|_| start.to_path_buf());
    for dir in start.ancestors() {
        let git = dir.join(".git");
        let Ok(meta) = fs::metadata(&git) else {
            continue;
        };
        let common_dir = if meta.is_dir() {
            Some(git)
        } else if meta.is_file() {
            common_dir_from_file(dir, &git)
        } else {
            None
        };
        if let Some(common_dir) = common_dir {
            return RootInfo {
                root: dir.to_path_buf(),
                repo: Some(Repo {
                    work_tree: dir.to_path_buf(),
                    common_dir,
                }),
            };
        }
    }
    RootInfo {
        root: start,
        repo: None,
    }
}

/// Follow a `.git` file's `gitdir:` line, then the gitdir's `commondir` file.
fn common_dir_from_file(dir: &Path, git: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(git).ok()?;
    let line = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?;
    let rel = line.trim();
    if rel.is_empty() {
        return None;
    }
    let gitdir = dir.join(rel);
    let common = match fs::read_to_string(gitdir.join("commondir")) {
        Ok(c) if !c.trim().is_empty() => gitdir.join(c.trim()),
        _ => gitdir,
    };
    Some(fs::canonicalize(&common).unwrap_or(common))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::scratch_dir;

    fn canon(p: &Path) -> PathBuf {
        fs::canonicalize(p).unwrap()
    }

    #[test]
    fn folder_with_git_dir() {
        let d = scratch_dir("root-plain");
        fs::create_dir(d.join(".git")).unwrap();
        let info = find_root(&d);
        assert_eq!(info.root, canon(&d));
        let repo = info.repo.unwrap();
        assert_eq!(repo.work_tree, canon(&d));
        assert_eq!(repo.common_dir, canon(&d.join(".git")));
    }

    #[test]
    fn nested_folder_finds_the_repository() {
        let d = scratch_dir("root-nested");
        fs::create_dir(d.join(".git")).unwrap();
        fs::create_dir_all(d.join("a/b")).unwrap();
        let info = find_root(&d.join("a/b"));
        assert_eq!(info.root, canon(&d));
        assert_eq!(info.repo.unwrap().work_tree, canon(&d));
    }

    #[test]
    fn worktree_git_file_with_relative_commondir() {
        let d = scratch_dir("root-worktree");
        let main_git = d.join("main/.git");
        let wt_git = main_git.join("worktrees/wt");
        fs::create_dir_all(&wt_git).unwrap();
        fs::write(wt_git.join("commondir"), "../..\n").unwrap();
        fs::create_dir_all(d.join("wt")).unwrap();
        fs::write(d.join("wt/.git"), format!("gitdir: {}\n", wt_git.display())).unwrap();
        let info = find_root(&d.join("wt"));
        assert_eq!(info.root, canon(&d.join("wt")));
        assert_eq!(info.repo.unwrap().common_dir, canon(&main_git));
    }

    #[test]
    fn no_repository() {
        let d = scratch_dir("root-none");
        fs::create_dir(d.join("x")).unwrap();
        let info = find_root(&d.join("x"));
        // The temp dir itself may sit inside a repository on some machines;
        // only check that nothing below the scratch folder was taken.
        if info.repo.is_none() {
            assert_eq!(info.root, canon(&d.join("x")));
        } else {
            assert!(!info.root.starts_with(canon(&d)));
        }
    }

    #[test]
    fn junk_git_file_is_skipped() {
        let d = scratch_dir("root-junk");
        fs::create_dir(d.join(".git")).unwrap();
        fs::create_dir(d.join("sub")).unwrap();
        fs::write(d.join("sub/.git"), "not a gitdir line\n").unwrap();
        let info = find_root(&d.join("sub"));
        assert_eq!(info.root, canon(&d));
    }
}
