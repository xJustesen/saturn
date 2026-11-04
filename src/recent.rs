//! The recent-notebooks list behind the dashboard's "Recent notebooks":
//! `state_root()/recent.json`, newest first, at most [`CAP`] entries.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const CAP: usize = 50;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub path: PathBuf,
    /// Seconds since the Unix epoch.
    pub opened: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recent {
    #[serde(default = "version")]
    pub version: u32,
    #[serde(default)]
    pub notebooks: Vec<Entry>,
}

fn version() -> u32 {
    1
}

impl Default for Recent {
    fn default() -> Self {
        Recent {
            version: version(),
            notebooks: Vec::new(),
        }
    }
}

/// Where the list lives, or `None` without a home folder.
pub fn default_path() -> Option<PathBuf> {
    crate::config::state_root().map(|d| d.join("recent.json"))
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Recent {
    /// Read the list. A missing or invalid file gives an empty list; entries
    /// whose file is gone are dropped.
    pub fn load(path: &Path) -> Recent {
        let mut r = match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str::<Recent>(&s).unwrap_or_else(|e| {
                tracing::warn!("invalid recent list {}: {e}", path.display());
                Recent::default()
            }),
            Err(_) => Recent::default(),
        };
        r.version = 1;
        r.notebooks.retain(|e| e.path.is_file());
        r.notebooks.truncate(CAP);
        r
    }

    /// Put `nb` first (canonicalised), dropping any older copy.
    pub fn touch(&mut self, nb: &Path, now: u64) {
        let path = std::fs::canonicalize(nb).unwrap_or_else(|_| nb.to_path_buf());
        self.notebooks.retain(|e| e.path != path);
        self.notebooks.insert(0, Entry { path, opened: now });
        self.notebooks.truncate(CAP);
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        }
        let tmp = path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(&tmp, text).with_context(|| format!("cannot write {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("cannot replace {}", path.display()))?;
        Ok(())
    }

    /// Re-read the file (another saturn may have written it), touch `nb` and
    /// write it back. Returns the updated list.
    pub fn remember(path: &Path, nb: &Path) -> Result<Recent> {
        let mut r = Recent::load(path);
        r.touch(nb, now());
        r.save(path)?;
        Ok(r)
    }

    pub fn paths(&self) -> impl Iterator<Item = &Path> {
        self.notebooks.iter().map(|e| e.path.as_path())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("saturn-recent-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::canonicalize(&d).unwrap()
    }

    fn file(d: &Path, name: &str) -> PathBuf {
        let p = d.join(name);
        std::fs::write(&p, "{}").unwrap();
        p
    }

    #[test]
    fn touch_moves_to_front_without_duplicates() {
        let d = scratch("touch");
        let (a, b) = (file(&d, "a.ipynb"), file(&d, "b.ipynb"));
        let mut r = Recent::default();
        r.touch(&a, 1);
        r.touch(&b, 2);
        r.touch(&a, 3);
        let got: Vec<_> = r.paths().collect();
        assert_eq!(got, [a.as_path(), b.as_path()]);
        assert_eq!(r.notebooks[0].opened, 3);
    }

    #[test]
    fn list_is_capped() {
        let d = scratch("cap");
        let mut r = Recent::default();
        for i in 0..CAP + 5 {
            r.touch(&file(&d, &format!("{i}.ipynb")), i as u64);
        }
        assert_eq!(r.notebooks.len(), CAP);
        assert!(r.notebooks[0].path.ends_with(format!("{}.ipynb", CAP + 4)));
    }

    #[test]
    fn save_load_round_trip_and_missing_files_dropped() {
        let d = scratch("round");
        let store = d.join("state/sub/recent.json");
        let (a, b) = (file(&d, "a.ipynb"), file(&d, "b.ipynb"));
        let mut r = Recent::default();
        r.touch(&a, 1);
        r.touch(&b, 2);
        r.save(&store).unwrap();
        assert_eq!(Recent::load(&store), r);
        std::fs::remove_file(&b).unwrap();
        let got: Vec<_> = Recent::load(&store).paths().map(Path::to_path_buf).collect();
        assert_eq!(got, [a]);
    }

    #[test]
    fn invalid_or_missing_file_is_empty() {
        let d = scratch("bad");
        assert!(Recent::load(&d.join("nope.json")).notebooks.is_empty());
        std::fs::write(d.join("bad.json"), "not json").unwrap();
        assert!(Recent::load(&d.join("bad.json")).notebooks.is_empty());
    }

    #[test]
    fn remember_rereads_the_file() {
        let d = scratch("remember");
        let store = d.join("recent.json");
        let (a, b) = (file(&d, "a.ipynb"), file(&d, "b.ipynb"));
        Recent::remember(&store, &a).unwrap();
        let r = Recent::remember(&store, &b).unwrap();
        let got: Vec<_> = r.paths().collect();
        assert_eq!(got, [b.as_path(), a.as_path()]);
    }
}
