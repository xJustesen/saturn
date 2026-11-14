//! Keeps the version badge in README.md in step with `version` in Cargo.toml.
//!
//! When they differ, this test rewrites the badge and then fails, so the stale
//! README never goes unnoticed: re-run the tests and commit the updated file.

use std::fs;
use std::path::Path;

const BADGE_PREFIX: &str = "https://img.shields.io/badge/version-";

#[test]
fn readme_version_badge_matches_cargo_toml() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md");
    let readme = fs::read_to_string(&path).expect("read README.md");

    let start = readme
        .find(BADGE_PREFIX)
        .expect("README.md has no version badge")
        + BADGE_PREFIX.len();
    // The badge path is `version-<version>-<colour>?<query>`.
    let path_len = readme[start..]
        .find('?')
        .expect("version badge has no query string");
    let (found, _colour) = readme[start..start + path_len]
        .rsplit_once('-')
        .expect("version badge has no colour segment");

    // shields.io reads a single dash as a separator, so a literal one is doubled.
    let expected = env!("CARGO_PKG_VERSION").replace('-', "--");
    if found == expected {
        return;
    }

    let mut updated = readme.clone();
    updated.replace_range(start..start + found.len(), &expected);
    fs::write(&path, updated).expect("write README.md");
    panic!(
        "README.md version badge said {found}, Cargo.toml says {expected}; \
         the badge has been updated, commit README.md"
    );
}
