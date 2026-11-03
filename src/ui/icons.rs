//! Nerd Font file-type icons for the files picker. Each code point was
//! checked against Nerd Fonts' `glyphnames.json` (3.5.1); the glyph names in
//! the comments are labels only.

use ratatui::style::Color;

use crate::theme::Ui;

const NOTEBOOK: char = '\u{f082e}'; // nf-md-notebook
const PYTHON: char = '\u{e606}'; // nf-seti-python
const RUST: char = '\u{e7a8}'; // nf-dev-rust
const JAVASCRIPT: char = '\u{e60c}'; // nf-seti-javascript
const TYPESCRIPT: char = '\u{e628}'; // nf-seti-typescript
const SHELL: char = '\u{e691}'; // nf-seti-shell
const DATABASE: char = '\u{e706}'; // nf-dev-database
const CODE: char = '\u{f1c9}'; // nf-fa-file_code_o
const CONFIG: char = '\u{e615}'; // nf-seti-config
const LOCK: char = '\u{f023}'; // nf-fa-lock
const MAKEFILE: char = '\u{e673}'; // nf-seti-makefile
const DOCKER: char = '\u{e7b0}'; // nf-dev-docker
const GIT: char = '\u{e702}'; // nf-dev-git
const TABLE: char = '\u{f0ce}'; // nf-fa-table
const ARCHIVE: char = '\u{f1c6}'; // nf-fa-file_archive_o
const MARKDOWN: char = '\u{e609}'; // nf-seti-markdown
const TEXT: char = '\u{f0f6}'; // nf-fa-file_text_o
const IMAGE: char = '\u{f1c5}'; // nf-fa-file_image_o
const PDF: char = '\u{f1c1}'; // nf-fa-file_pdf_o
const FILE: char = '\u{f016}'; // nf-fa-file_o

/// The icon and its colour for a file name: notebooks first, then whole
/// names (`README*` and `LICENSE*` as prefixes), then the lowercased
/// extension, then a plain file.
pub fn icon(name: &str, notebook: bool, ui: &Ui) -> (char, Color) {
    if notebook {
        return (NOTEBOOK, ui.focus);
    }
    match name {
        "Makefile" => return (MAKEFILE, ui.warning),
        "Dockerfile" => return (DOCKER, ui.warning),
        ".gitignore" | ".gitattributes" => return (GIT, ui.warning),
        _ if name.starts_with("README") => return (MARKDOWN, ui.muted),
        _ if name.starts_with("LICENSE") => return (TEXT, ui.muted),
        _ => {}
    }
    let ext = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => ext.to_ascii_lowercase(),
        _ => String::new(),
    };
    match ext.as_str() {
        "py" | "pyi" => (PYTHON, ui.info),
        "rs" => (RUST, ui.info),
        "js" => (JAVASCRIPT, ui.info),
        "ts" => (TYPESCRIPT, ui.info),
        "sh" | "zsh" | "bash" => (SHELL, ui.info),
        "sql" => (DATABASE, ui.info),
        "r" | "jl" | "html" | "css" => (CODE, ui.info),
        "toml" | "ini" | "cfg" | "conf" | "yaml" | "yml" | "json" => (CONFIG, ui.warning),
        "lock" => (LOCK, ui.warning),
        "csv" | "tsv" | "parquet" | "pkl" | "pickle" => (TABLE, ui.success),
        "bz2" | "gz" | "zip" | "xz" | "tar" => (ARCHIVE, ui.success),
        "md" => (MARKDOWN, ui.muted),
        "txt" | "log" => (TEXT, ui.muted),
        "png" | "jpg" | "jpeg" | "gif" | "svg" => (IMAGE, ui.muted),
        "pdf" => (PDF, ui.muted),
        _ => (FILE, ui.muted),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    const ALL: &[char] = &[
        NOTEBOOK, PYTHON, RUST, JAVASCRIPT, TYPESCRIPT, SHELL, DATABASE, CODE, CONFIG, LOCK,
        MAKEFILE, DOCKER, GIT, TABLE, ARCHIVE, MARKDOWN, TEXT, IMAGE, PDF, FILE,
    ];

    #[test]
    fn every_glyph_is_private_use() {
        for &c in ALL {
            let cp = c as u32;
            assert!((0xe000..=0xf8ff).contains(&cp) || cp >= 0xf0000, "U+{cp:X}");
        }
    }

    #[test]
    fn whole_names_win_over_extensions() {
        let ui = Theme::default().ui;
        assert_eq!(icon("README.md", false, &ui).0, MARKDOWN);
        assert_eq!(icon("LICENSE.md", false, &ui).0, TEXT);
        assert_eq!(icon("Makefile", false, &ui), (MAKEFILE, ui.warning));
        assert_eq!(icon("x.ipynb", true, &ui), (NOTEBOOK, ui.focus));
    }

    #[test]
    fn extensions_and_default() {
        let ui = Theme::default().ui;
        assert_eq!(icon("main.PY", false, &ui).0, PYTHON);
        assert_eq!(icon("data.parquet", false, &ui).0, TABLE);
        assert_eq!(icon("thing.xyz", false, &ui), (FILE, ui.muted));
        assert_eq!(icon("noext", false, &ui).0, FILE);
    }
}
