//! Notebook-wide text search (`/`, `n`, `N`).
//!
//! Matches are found in the rendered rows of every cell's source, so what is
//! highlighted is exactly what is on screen: markdown after rendering, code
//! and raw text after tab expansion and wrapping. The pattern is literal
//! text with vim's smart case: all lowercase matches case-insensitively, any
//! uppercase letter makes it case-sensitive.

use ratatui::text::Line;
use unicode_width::UnicodeWidthChar;

/// One occurrence of the pattern, in screen geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    pub cell: usize,
    /// Visual row inside the cell's source rows.
    pub row: u16,
    /// Column (cells from the content's left edge) and width in cells.
    pub x: u16,
    pub w: u16,
}

/// The `/` prompt while it is open.
pub struct SearchInput {
    pub text: String,
    /// Focus and scroll when `/` was pressed; restored by Esc (and while the
    /// text has no match).
    pub from_focus: usize,
    pub from_scroll: usize,
    /// The pattern and highlight state before `/`, restored by Esc.
    pub prev_query: String,
    pub prev_hl: bool,
}

#[derive(Default)]
pub struct SearchState {
    /// The pattern `n`/`N` repeat; survives Esc like vim's search register.
    pub query: String,
    /// Sorted by (cell, row, x).
    pub matches: Vec<Match>,
    /// Index into `matches` of the one the cursor is on.
    pub current: Option<usize>,
    /// Highlights shown? Esc hides them, `n`/`N`/`/` show them again.
    pub hl: bool,
    pub input: Option<SearchInput>,
    /// Layout generation `matches` was computed for; `None` forces a recompute.
    pub generation: Option<u64>,
}

impl SearchState {
    pub fn active(&self) -> bool {
        self.hl && !self.query.is_empty()
    }

    pub fn current_match(&self) -> Option<Match> {
        self.current.and_then(|k| self.matches.get(k).copied())
    }

    /// Matches of cell `cell`, as a range into `matches`.
    pub fn range_of(&self, cell: usize) -> std::ops::Range<usize> {
        let lo = self.matches.partition_point(|m| m.cell < cell);
        let hi = self.matches.partition_point(|m| m.cell <= cell);
        lo..hi
    }
}

/// Vim's smart case: a pattern with an uppercase letter is case-sensitive.
pub fn case_sensitive(query: &str) -> bool {
    query.chars().any(char::is_uppercase)
}

fn fold(c: char, sensitive: bool) -> char {
    if sensitive { c } else { c.to_lowercase().next().unwrap_or(c) }
}

/// Prepare a pattern for [`find_in_line`].
pub fn pattern(query: &str) -> Vec<char> {
    let sensitive = case_sensitive(query);
    query.chars().map(|c| fold(c, sensitive)).collect()
}

/// Non-overlapping occurrences of `pat` (from [`pattern`]) in one rendered
/// row, as `(x, width)` in cells; wide characters count for their width.
pub fn find_in_line(line: &Line<'_>, pat: &[char], sensitive: bool) -> Vec<(u16, u16)> {
    if pat.is_empty() {
        return Vec::new();
    }
    let mut chars: Vec<char> = Vec::new();
    let mut xs: Vec<u16> = Vec::new();
    let mut ws: Vec<u16> = Vec::new();
    let mut x: u16 = 0;
    for span in &line.spans {
        for ch in span.content.chars() {
            let w = UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
            chars.push(fold(ch, sensitive));
            xs.push(x);
            ws.push(w);
            x = x.saturating_add(w);
        }
    }
    let mut out = Vec::new();
    let n = pat.len();
    let mut i = 0;
    while i + n <= chars.len() {
        if chars[i..i + n] == *pat {
            let last = i + n - 1;
            let w = xs[last] + ws[last] - xs[i];
            if w > 0 {
                out.push((xs[i], w));
            }
            i += n;
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::text::Span;

    fn find(text: &str, q: &str) -> Vec<(u16, u16)> {
        let line = Line::from(vec![Span::raw(&text[..3]), Span::raw(&text[3..])]);
        find_in_line(&line, &pattern(q), case_sensitive(q))
    }

    #[test]
    fn smart_case_and_spans() {
        assert_eq!(find("import numpy as np; Import", "import"), vec![(0, 6), (20, 6)]);
        assert_eq!(find("import numpy as np; Import", "Import"), vec![(20, 6)]);
        assert_eq!(find("aaaa", "aa"), vec![(0, 2), (2, 2)]);
        assert!(find("abc", "").is_empty());
    }

    #[test]
    fn wide_chars_take_two_cells() {
        assert_eq!(find("日本語 abc", "abc"), vec![(7, 3)]);
        assert_eq!(find("日本語 abc", "本"), vec![(2, 2)]);
    }
}
