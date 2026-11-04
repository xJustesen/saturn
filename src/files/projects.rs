//! The dashboard's "Projects": git repositories found under the configured
//! `project_roots`, a few folder levels deep.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::files::{PickerPrefs, root, skip};

/// How many folder levels below each root are searched.
pub const PROJECT_DEPTH: usize = 3;
const VISIT_CAP: usize = 20_000;
const TIME_CAP: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq)]
pub struct Project {
    /// The work tree (canonical).
    pub path: PathBuf,
    pub name: String,
    /// Last activity: `.git/index` for a plain repository, else the folder.
    pub modified: Option<SystemTime>,
    /// For a linked worktree, the main repository's folder.
    pub worktree_of: Option<PathBuf>,
}

/// Search `roots` for repositories. A repository's own subfolders are not
/// searched, so worktrees and submodules inside it are not listed. Missing
/// roots are skipped; results are deduped and sorted newest first.
pub fn scan(roots: &[PathBuf], max_depth: usize, home: Option<&Path>) -> Vec<Project> {
    let started = Instant::now();
    let mut visited = 0usize;
    let mut found: Vec<Project> = Vec::new();
    for r in roots {
        let Ok(r) = std::fs::canonicalize(r) else { continue };
        if !r.is_dir() {
            continue;
        }
        let mut stack = vec![(r, 0usize)];
        while let Some((dir, depth)) = stack.pop() {
            visited += 1;
            if visited > VISIT_CAP || started.elapsed() > TIME_CAP {
                tracing::warn!("projects scan stopped early after {visited} folders");
                break;
            }
            if let Some(repo) = root::repo_at(&dir) {
                if !found.iter().any(|p| p.path == dir) {
                    found.push(project(&dir, &repo));
                }
                continue;
            }
            if depth >= max_depth {
                continue;
            }
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            let mut kids: Vec<PathBuf> = rd
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .filter(|e| {
                    !skip::prune(&e.path(), &e.file_name(), true, depth + 1, PickerPrefs::default(), home)
                })
                .map(|e| e.path())
                .collect();
            // Reverse so the stack visits children in name order.
            kids.sort();
            kids.reverse();
            stack.extend(kids.into_iter().map(|k| (k, depth + 1)));
        }
    }
    found.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.name.cmp(&b.name)));
    found
}

fn project(dir: &Path, repo: &root::Repo) -> Project {
    let git = dir.join(".git");
    let main = repo.common_dir.parent().map(Path::to_path_buf);
    let worktree_of = if git.is_file() {
        main.filter(|m| m != dir)
    } else {
        None
    };
    let modified = if git.is_dir() {
        std::fs::metadata(git.join("index")).and_then(|m| m.modified()).ok()
    } else {
        None
    }
    .or_else(|| std::fs::metadata(dir).and_then(|m| m.modified()).ok());
    Project {
        path: dir.to_path_buf(),
        name: dir
            .file_name()
            .map_or_else(|| dir.display().to_string(), |n| n.to_string_lossy().into_owned()),
        modified,
        worktree_of,
    }
}

/// "just now", "5m ago", "3h ago", "2d ago", "6w ago", "1y ago".
pub fn ago(t: SystemTime, now: SystemTime) -> String {
    let s = now.duration_since(t).map_or(0, |d| d.as_secs());
    match s {
        0..60 => "just now".into(),
        60..3600 => format!("{}m ago", s / 60),
        3600..86_400 => format!("{}h ago", s / 3600),
        86_400..1_209_600 => format!("{}d ago", s / 86_400),
        1_209_600..31_536_000 => format!("{}w ago", s / 604_800),
        _ => format!("{}y ago", s / 31_536_000),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::files::scratch_dir;

    fn repo(p: &Path) {
        fs::create_dir_all(p.join(".git")).unwrap();
    }

    fn names(v: &[Project]) -> Vec<&str> {
        let mut n: Vec<&str> = v.iter().map(|p| p.name.as_str()).collect();
        n.sort();
        n
    }

    #[test]
    fn finds_repos_down_to_the_depth_limit() {
        let d = scratch_dir("proj-depth");
        repo(&d.join("r0"));
        repo(&d.join("a/r1"));
        repo(&d.join("a/b/r2"));
        repo(&d.join("a/b/c/r3"));
        let got = scan(std::slice::from_ref(&d), 3, None);
        assert_eq!(names(&got), ["r0", "r1", "r2"]);
        let got = scan(std::slice::from_ref(&d), 4, None);
        assert_eq!(names(&got), ["r0", "r1", "r2", "r3"]);
    }

    #[test]
    fn root_that_is_a_repo_lists_only_itself() {
        let d = scratch_dir("proj-self");
        repo(&d);
        repo(&d.join("nested"));
        let got = scan(std::slice::from_ref(&d), 3, None);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].path, fs::canonicalize(&d).unwrap());
    }

    #[test]
    fn nested_repo_is_not_listed() {
        let d = scratch_dir("proj-nested");
        repo(&d.join("outer"));
        repo(&d.join("outer/inner"));
        assert_eq!(names(&scan(&[d], 3, None)), ["outer"]);
    }

    #[test]
    fn worktree_and_junk_git_files() {
        let d = scratch_dir("proj-wt");
        repo(&d.join("main"));
        let wt_git = d.join("main/.git/worktrees/wt");
        fs::create_dir_all(&wt_git).unwrap();
        fs::write(wt_git.join("commondir"), "../..\n").unwrap();
        fs::create_dir_all(d.join("wt")).unwrap();
        fs::write(d.join("wt/.git"), format!("gitdir: {}\n", wt_git.display())).unwrap();
        fs::create_dir_all(d.join("junk")).unwrap();
        fs::write(d.join("junk/.git"), "nonsense\n").unwrap();
        let got = scan(std::slice::from_ref(&d), 3, None);
        assert_eq!(names(&got), ["main", "wt"]);
        let wt = got.iter().find(|p| p.name == "wt").unwrap();
        assert_eq!(wt.worktree_of.as_deref(), Some(fs::canonicalize(d.join("main")).unwrap().as_path()));
        let main = got.iter().find(|p| p.name == "main").unwrap();
        assert!(main.worktree_of.is_none());
    }

    #[test]
    fn pruned_folders_are_skipped() {
        let d = scratch_dir("proj-prune");
        repo(&d.join("node_modules/pkg"));
        repo(&d.join(".hidden/r"));
        repo(&d.join("ok"));
        assert_eq!(names(&scan(&[d], 3, None)), ["ok"]);
    }

    #[test]
    fn missing_and_overlapping_roots() {
        let d = scratch_dir("proj-roots");
        repo(&d.join("a/r"));
        let got = scan(&[d.join("missing"), d.clone(), d.join("a")], 3, None);
        assert_eq!(names(&got), ["r"]);
    }

    #[test]
    fn sorted_newest_first() {
        let d = scratch_dir("proj-sort");
        repo(&d.join("old"));
        repo(&d.join("new"));
        fs::write(d.join("old/.git/index"), "").unwrap();
        std::thread::sleep(Duration::from_millis(20));
        fs::write(d.join("new/.git/index"), "").unwrap();
        let got = scan(&[d], 3, None);
        assert_eq!(got[0].name, "new");
        assert_eq!(got[1].name, "old");
    }

    #[test]
    fn ago_formats() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100_000_000);
        let at = |s: u64| now - Duration::from_secs(s);
        assert_eq!(ago(at(5), now), "just now");
        assert_eq!(ago(at(300), now), "5m ago");
        assert_eq!(ago(at(7200), now), "2h ago");
        assert_eq!(ago(at(3 * 86_400), now), "3d ago");
        assert_eq!(ago(at(30 * 86_400), now), "4w ago");
        assert_eq!(ago(at(800 * 86_400), now), "2y ago");
        assert_eq!(ago(now + Duration::from_secs(9), now), "just now");
    }
}
