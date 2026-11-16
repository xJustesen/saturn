//! Per-cell text editor: the vim engine (`crate::vim`) owns the text and the
//! keys, edtui draws it. Also the wrapped-row maths the layout needs (edtui
//! keeps its viewport private).

use crossterm::event::KeyEvent;
use edtui::{EditorMode, EditorState, Highlight, Index2, Lines};
use ratatui::style::Style;
use unicode_width::UnicodeWidthChar;

use crate::vim::{self, K, Mode, Request, Vim};

pub struct CellEditor {
    /// What edtui draws: a mirror of `vim`, refreshed after every key.
    pub state: EditorState,
    pub vim: Vim,
}

struct SystemClipboard(arboard::Clipboard);

impl vim::Clip for SystemClipboard {
    fn get(&mut self) -> Option<String> {
        self.0.get_text().ok()
    }
    fn set(&mut self, text: &str) {
        let _ = self.0.set_text(text.to_string());
    }
}

/// Yank, delete and paste through the system clipboard, as with nvim's
/// `clipboard=unnamedplus`. Without a clipboard the registers stay in memory.
pub fn install_system_clipboard() {
    if let Ok(c) = arboard::Clipboard::new() {
        vim::set_clipboard(Box::new(SystemClipboard(c)));
    }
}

/// Line comment leader for `gc`, by syntect file extension.
pub fn comment_leader(ext: &str) -> &'static str {
    match ext {
        "rs" | "js" | "ts" | "c" | "cpp" | "go" | "scala" => "//",
        "sql" | "hs" => "--",
        "ml" => "(*",
        _ => "#",
    }
}

fn idx(p: vim::Pos) -> Index2 {
    Index2::new(p.row, p.col)
}

impl CellEditor {
    pub fn new(source: &str) -> Self {
        let mut ed = CellEditor { state: EditorState::new(Lines::from(source)), vim: Vim::new(source) };
        ed.sync();
        ed
    }

    /// Mirror the engine's text, cursor, mode and selection into edtui.
    fn sync(&mut self) {
        let v = &self.vim;
        self.state.lines = Lines::new(v.text.clone());
        self.state.cursor = idx(v.cur);
        self.state.mode = match v.mode {
            Mode::Insert | Mode::Replace => EditorMode::Insert,
            Mode::Normal => EditorMode::Normal,
            _ => EditorMode::Visual,
        };
        // edtui's selection type is private: selections are drawn as
        // highlights instead (see `decorate`).
        self.state.selection = None;
    }

    /// Per-frame highlights: search matches and the visual selection.
    pub fn decorate(&mut self, selection: Style, search: Style) {
        let v = &self.vim;
        let mut hl = Vec::new();
        for (p, n) in v.search_matches() {
            hl.push(Highlight::new(idx(p), Index2::new(p.row, p.col + n - 1), search));
        }
        if let Some(r) = v.selection() {
            for (row, start, end) in v.spans(&r) {
                if end > start {
                    hl.push(Highlight::new(Index2::new(row, start), Index2::new(row, end - 1), selection));
                }
            }
        }
        self.state.highlights = hl;
    }

    pub fn text(&self) -> String {
        self.vim.text()
    }

    /// The text of the visual selection, if there is one.
    pub fn selected_text(&self) -> Option<String> {
        let v = &self.vim;
        let region = v.selection()?;
        let rows: Vec<String> = v
            .spans(&region)
            .into_iter()
            .filter_map(|(row, start, end)| {
                let line = v.text.get(row)?;
                Some(line[start.min(line.len())..end.min(line.len())].iter().collect())
            })
            .collect();
        Some(rows.join("\n"))
    }

    pub fn key(&mut self, key: KeyEvent) {
        if let Some(k) = K::from_event(&key) {
            self.vim.key(k);
            self.sync();
        }
    }

    pub fn paste(&mut self, text: String) {
        self.vim.paste(&text);
        self.sync();
    }

    pub fn mode(&self) -> Mode {
        self.vim.mode
    }

    /// Normal mode with no command half-typed: `Esc` leaves the cell.
    pub fn at_rest(&self) -> bool {
        self.vim.at_rest()
    }

    pub fn enter_insert(&mut self) {
        self.vim.enter_insert();
        self.sync();
    }

    pub fn enter_normal(&mut self) {
        self.vim.enter_normal();
        self.sync();
    }

    pub fn take_requests(&mut self) -> Vec<Request> {
        self.vim.take_requests()
    }

    pub fn cursor_to_end(&mut self) {
        let row = self.vim.text.len() - 1;
        self.vim.cur = vim::Pos { row, col: self.vim.text[row].len() };
        self.sync();
    }

    pub fn cursor_to_start(&mut self) {
        self.vim.cur = vim::Pos::default();
        self.sync();
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
