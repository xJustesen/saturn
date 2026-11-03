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
