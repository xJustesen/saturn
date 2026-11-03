//! The app's single syntax set: two-face's (the `bat` tool's syntaxes), used by
//! the cell view, the cell editor and the files picker preview.

use std::sync::{Arc, LazyLock};

use edtui::syntect::parsing::{SyntaxReference, SyntaxSet};

pub static SYNTAX_SET: LazyLock<Arc<SyntaxSet>> =
    LazyLock::new(|| Arc::new(two_face::syntax::extra_newlines()));

/// The syntax for a file extension (without the dot), if the set has one.
pub fn by_extension(ext: &str) -> Option<&'static SyntaxReference> {
    SYNTAX_SET.find_syntax_by_extension(ext)
}

/// The syntax for a file: by extension, then by whole file name (`Makefile`,
/// `Dockerfile`), then by its first line (a shebang), then plain text. The
/// caller supplies `first_line`; this never reads the file.
pub fn for_file(name: &str, first_line: Option<&str>) -> &'static SyntaxReference {
    let ss = &**SYNTAX_SET;
    std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .and_then(|e| ss.find_syntax_by_extension(e))
        .or_else(|| ss.find_syntax_by_extension(name))
        .or_else(|| first_line.and_then(|l| ss.find_syntax_by_first_line(l)))
        .unwrap_or_else(|| ss.find_syntax_plain_text())
}
