//! Per-cell text editor: a thin wrapper around edtui (vim-modal) plus the
//! wrapped-row maths the layout needs (edtui keeps its viewport private).

use crossterm::event::KeyEvent;
use edtui::{EditorEventHandler, EditorMode, EditorState, Lines};
use unicode_width::UnicodeWidthChar;

pub struct CellEditor {
    pub state: EditorState,
    handler: EditorEventHandler,
}

impl CellEditor {
    pub fn new(source: &str) -> Self {
        let mut state = EditorState::new(Lines::from(source));
        state.mode = EditorMode::Normal;
        CellEditor { state, handler: EditorEventHandler::default() }
    }

    pub fn text(&self) -> String {
        self.state.lines.flatten(&Some('\n')).into_iter().collect()
    }

    pub fn key(&mut self, key: KeyEvent) {
        self.handler.on_key_event(key, &mut self.state);
    }

    pub fn paste(&mut self, text: String) {
        self.handler.on_paste_event(text, &mut self.state);
    }

    pub fn mode(&self) -> EditorMode {
        self.state.mode
    }

    pub fn set_mode(&mut self, mode: EditorMode) {
        self.state.mode = mode;
    }

    pub fn cursor_to_end(&mut self) {
        let rows = self.state.lines.len();
        if rows > 0 {
            let row = rows - 1;
            let col = self.state.lines.len_col(row).unwrap_or(0);
            self.state.cursor = edtui::Index2::new(row, col);
        }
    }

    pub fn cursor_to_start(&mut self) {
        self.state.cursor = edtui::Index2::new(0, 0);
    }

    pub fn line_count(&self) -> usize {
        self.state.lines.len().max(1)
    }

    /// Byte offset of the cursor in `text()` (for splitting cells).
    pub fn cursor_offset(&self) -> usize {
        let cur = self.state.cursor;
        let mut off = 0usize;
        for (i, row) in self.state.lines.iter_row().enumerate() {
            if i == cur.row {
                off += row.iter().take(cur.col).map(|c| c.len_utf8()).sum::<usize>();
                return off;
            }
            off += row.iter().map(|c| c.len_utf8()).sum::<usize>() + 1;
        }
        off
    }

    /// Number of visual rows the editor needs at `width` columns when
    /// wrapping (plus a cursor slack cell on the cursor line).
    pub fn rows(&self, width: usize, tab_width: usize, wrap: bool) -> usize {
        if !wrap {
            return self.line_count();
        }
        let cur = self.state.cursor.row;
        let mut total = 0;
        for (i, row) in self.state.lines.iter_row().enumerate() {
            total += if i == cur {
                wrapped_rows(row.iter().copied().chain(std::iter::once(' ')), width, tab_width)
            } else {
                wrapped_rows(row.iter().copied(), width, tab_width)
            };
        }
        total.max(1)
    }

    /// Visual row (0-based) of the cursor inside the editor at `width`.
    pub fn cursor_visual_row(&self, width: usize, tab_width: usize, wrap: bool) -> usize {
        let cur = self.state.cursor;
        if !wrap {
            return cur.row;
        }
        let mut row_acc = 0;
        for (i, row) in self.state.lines.iter_row().enumerate() {
            if i == cur.row {
                let upto = row.iter().take(cur.col).copied().chain(std::iter::once(' '));
                return row_acc + wrapped_rows(upto, width, tab_width) - 1;
            }
            row_acc += wrapped_rows(row.iter().copied(), width, tab_width);
        }
        row_acc
    }
}

pub fn char_width(ch: char, tab_width: usize) -> usize {
    if ch == '\t' { tab_width } else { UnicodeWidthChar::width(ch).unwrap_or(0) }
}

/// Port of edtui's greedy character wrapping: rows needed for one line.
pub fn wrapped_rows(chars: impl Iterator<Item = char>, max_width: usize, tab_width: usize) -> usize {
    if max_width == 0 {
        return 1;
    }
    let mut rows = 1;
    let mut w = 0;
    for ch in chars {
        let cw = char_width(ch, tab_width);
        if w + cw > max_width {
            rows += 1;
            w = 0;
        }
        w += cw;
    }
    rows
}

/// Rows needed for a whole text (same algorithm as the editor).
pub fn text_rows(text: &str, max_width: usize, tab_width: usize, wrap: bool) -> usize {
    let mut n = 0;
    for line in text.split('\n') {
        n += if wrap { wrapped_rows(line.chars(), max_width, tab_width) } else { 1 };
    }
    n.max(1)
}

/// Map a Jupyter language name to a syntect file extension.
pub fn language_extension(lang: &str) -> &'static str {
    match lang.to_ascii_lowercase().as_str() {
        "python" | "python3" | "py" => "py",
        "rust" => "rs",
        "julia" => "jl",
        "r" => "r",
        "javascript" | "js" | "node" => "js",
        "typescript" | "ts" => "ts",
        "bash" | "sh" | "shell" => "sh",
        "c" => "c",
        "cpp" | "c++" => "cpp",
        "go" => "go",
        "scala" => "scala",
        "sql" => "sql",
        "ruby" => "rb",
        "haskell" => "hs",
        "ocaml" => "ml",
        "markdown" => "md",
        _ => "txt",
    }
}
