//! Text utilities: owning conversions, character wrapping that matches the
//! editor, syntect highlighting for unfocused code cells, cleaning of text
//! read from files, and width-aware cutting.

use std::borrow::Cow;

use edtui::syntect::easy::HighlightLines;
use edtui::syntect::highlighting::{FontStyle, Style as SynStyle, Theme as SynTheme};
use edtui::syntect::parsing::SyntaxReference;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::editor::char_width;
use crate::theme::from_syn;

pub fn owned_text(t: Text<'_>) -> Text<'static> {
    let style = t.style;
    let alignment = t.alignment;
    let mut out = Text::from(t.lines.into_iter().map(owned_line).collect::<Vec<_>>());
    out.style = style;
    out.alignment = alignment;
    out
}

pub fn owned_line(l: Line<'_>) -> Line<'static> {
    let style = l.style;
    let alignment = l.alignment;
    let mut out = Line::from(
        l.spans
            .into_iter()
            .map(|s| Span::styled(s.content.into_owned(), s.style))
            .collect::<Vec<_>>(),
    );
    out.style = style;
    out.alignment = alignment;
    out
}

/// Expand tabs so widths are stable.
pub fn expand_tabs(s: &str, tab_width: usize) -> String {
    if !s.contains('\t') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut col = 0;
    for ch in s.chars() {
        if ch == '\t' {
            let n = tab_width - (col % tab_width.max(1));
            out.extend(std::iter::repeat_n(' ', n));
            col += n;
        } else if ch == '\n' {
            out.push(ch);
            col = 0;
        } else {
            out.push(ch);
            col += UnicodeWidthChar::width(ch).unwrap_or(0);
        }
    }
    out
}

/// Greedy character wrap of one styled line (same algorithm as edtui).
pub fn wrap_line(line: &Line<'static>, max_width: usize, tab_width: usize) -> Vec<Line<'static>> {
    if max_width == 0 {
        return vec![line.clone()];
    }
    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut cur: Vec<Span<'static>> = Vec::new();
    let mut cur_w = 0usize;
    for span in &line.spans {
        let mut buf = String::new();
        for ch in span.content.chars() {
            let cw = char_width(ch, tab_width);
            if cur_w + cw > max_width {
                if !buf.is_empty() {
                    cur.push(Span::styled(std::mem::take(&mut buf), span.style));
                }
                let mut l = Line::from(std::mem::take(&mut cur));
                l.style = line.style;
                rows.push(l);
                cur_w = 0;
            }
            buf.push(ch);
            cur_w += cw;
        }
        if !buf.is_empty() {
            cur.push(Span::styled(buf, span.style));
        }
    }
    let mut l = Line::from(cur);
    l.style = line.style;
    rows.push(l);
    rows
}

pub fn wrap_text(text: Text<'static>, max_width: usize, tab_width: usize, wrap: bool) -> Text<'static> {
    if !wrap {
        return text;
    }
    let style = text.style;
    let mut lines = Vec::with_capacity(text.lines.len());
    for l in &text.lines {
        lines.extend(wrap_line(l, max_width, tab_width));
    }
    let mut out = Text::from(lines);
    out.style = style;
    out
}

fn syn_to_style(s: SynStyle) -> Style {
    let mut st = Style::default().fg(from_syn(s.foreground));
    if s.background.a != 0 {
        st = st.bg(from_syn(s.background));
    }
    if s.font_style.contains(FontStyle::BOLD) {
        st = st.add_modifier(Modifier::BOLD);
    }
    if s.font_style.contains(FontStyle::ITALIC) {
        st = st.add_modifier(Modifier::ITALIC);
    }
    if s.font_style.contains(FontStyle::UNDERLINE) {
        st = st.add_modifier(Modifier::UNDERLINED);
    }
    st
}

/// Syntax-highlight `source` into owned lines (no wrapping), picking the
/// syntax by file extension. Without a syntax for `ext`, the lines are plain.
pub fn highlight(source: &str, ext: &str, theme: &SynTheme, tab_width: usize) -> Text<'static> {
    match crate::syntax::by_extension(ext) {
        Some(syntax) => highlight_with(source, syntax, theme, tab_width),
        None => {
            let source = expand_tabs(source, tab_width);
            Text::from(source.split('\n').map(|l| Line::from(l.to_string())).collect::<Vec<_>>())
        }
    }
}

/// Syntax-highlight `source` with `syntax` into owned lines (no wrapping).
pub fn highlight_with(source: &str, syntax: &SyntaxReference, theme: &SynTheme, tab_width: usize) -> Text<'static> {
    let source = expand_tabs(source, tab_width);
    let ss = &*crate::syntax::SYNTAX_SET;
    let mut h = HighlightLines::new(syntax, theme);
    let mut lines = Vec::new();
    // One output line per `split('\n')` segment, exactly like `text_rows`.
    for raw in source.split('\n') {
        let line = format!("{raw}\n");
        let spans: Vec<Span<'static>> = match h.highlight_line(&line, ss) {
            Ok(ranges) => ranges
                .into_iter()
                .map(|(st, s)| Span::styled(s.trim_end_matches(['\n', '\r']).to_string(), syn_to_style(st)))
                .collect(),
            Err(_) => vec![Span::raw(line.trim_end_matches(['\n', '\r']).to_string())],
        };
        lines.push(Line::from(spans));
    }
    if lines.is_empty() {
        lines.push(Line::default());
    }
    Text::from(lines)
}

pub fn plain(source: &str, style: Style) -> Text<'static> {
    let mut t = Text::from(source.split('\n').map(|l| Line::from(l.to_string())).collect::<Vec<_>>());
    if t.lines.is_empty() {
        t.lines.push(Line::default());
    }
    t.style = style;
    t
}


// ----- cleaning ----------------------------------------------------------------

const ESC: char = '\u{1b}';

/// A control character that may not reach the terminal: C0 (but tab and
/// newline are handled by the callers), DEL, C1 and the bidi controls.
fn is_unsafe(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// Clean file contents for display: `\r\n` becomes `\n`, a lone `\r` and every
/// other control character (C0 but tab and newline, DEL, C1, bidi controls)
/// is removed, and escape sequences are removed whole. An unterminated
/// sequence is removed up to the end of its line.
pub fn clean_text(s: &str) -> Cow<'_, str> {
    if !s.chars().any(|c| c != '\t' && c != '\n' && is_unsafe(c)) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\t' | '\n' => out.push(c),
            ESC => match it.peek().copied() {
                // CSI: up to and including the final byte.
                Some('[') => {
                    it.next();
                    while let Some(&n) = it.peek() {
                        if n == '\n' {
                            break;
                        }
                        it.next();
                        if ('\u{40}'..='\u{7e}').contains(&n) {
                            break;
                        }
                    }
                }
                // OSC, DCS, SOS, PM, APC: up to and including BEL or ESC \.
                Some(']' | 'P' | 'X' | '^' | '_') => {
                    it.next();
                    while let Some(&n) = it.peek() {
                        if n == '\n' {
                            break;
                        }
                        it.next();
                        if n == '\u{7}' {
                            break;
                        }
                        if n == ESC && it.peek() == Some(&'\\') {
                            it.next();
                            break;
                        }
                    }
                }
                Some('\n') | None => {}
                Some(_) => {
                    it.next();
                }
            },
            c if is_unsafe(c) => {}
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// Clean a file name for display: every control character (C0 including tab,
/// `\r` and `\n`, DEL, C1, bidi controls) becomes one `?`, so the result has
/// the same characters at the same positions otherwise.
pub fn clean_name(s: &str) -> Cow<'_, str> {
    if !s.chars().any(is_unsafe) {
        return Cow::Borrowed(s);
    }
    Cow::Owned(s.chars().map(|c| if is_unsafe(c) { '?' } else { c }).collect())
}

// ----- cutting -----------------------------------------------------------------

fn char_cols(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(0)
}

/// Keep the start of `s` within `max` columns. A cut string ends in `…` and
/// is padded to exactly `max` columns when a wide character leaves a gap.
pub fn cut(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = char_cols(ch);
        if w + cw + 1 > max {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out.extend(std::iter::repeat_n(' ', max - w - 1));
    out
}

/// Keep the end of `s` within `max` columns. A cut string starts with `…` and
/// is padded to exactly `max` columns when a wide character leaves a gap.
pub fn cut_left(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut tail = Vec::new();
    let mut w = 0;
    for ch in s.chars().rev() {
        let cw = char_cols(ch);
        if w + cw + 1 > max {
            break;
        }
        tail.push(ch);
        w += cw;
    }
    let mut out = String::from('…');
    out.extend(std::iter::repeat_n(' ', max - w - 1));
    out.extend(tail.into_iter().rev());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_text_removes_escape_sequences() {
        assert_eq!(clean_text("a\x1b[31mred\x1b[0m b"), "ared b");
        assert_eq!(clean_text("\x1b]8;;https://x.io\x1b\\link\x1b]8;;\x1b\\!"), "link!");
        assert_eq!(clean_text("\x1b]0;title\x07after"), "after");
        assert_eq!(clean_text("x\x1b[12;3\nnext"), "x\nnext");
        assert_eq!(clean_text("x\x1b]0;title\nnext"), "x\nnext");
        assert_eq!(clean_text("\x1bcreset"), "reset");
    }

    #[test]
    fn clean_text_handles_controls() {
        assert_eq!(clean_text("a\rb"), "ab");
        assert_eq!(clean_text("a\r\nb\r\n"), "a\nb\n");
        assert_eq!(clean_text("a\tb"), "a\tb");
        assert!(matches!(clean_text("a\tb\nc"), Cow::Borrowed(_)));
        assert_eq!(clean_text("a\x7fb"), "ab");
        assert_eq!(clean_text("a\u{202e}b\u{2066}c"), "abc");
        assert_eq!(clean_text("a\u{85}b"), "ab");
    }

    #[test]
    fn clean_name_replaces_one_for_one() {
        let input = "a\tb\rc\nd\x01e\x7ff\u{9b}g\x1bh\u{202e}i";
        let out = clean_name(input);
        assert_eq!(out, "a?b?c?d?e?f?g?h?i");
        assert_eq!(out.chars().count(), input.chars().count());
        assert!(matches!(clean_name("src/main.rs"), Cow::Borrowed(_)));
    }

    fn snapshot(t: &Text<'_>) -> String {
        t.lines
            .iter()
            .map(|l| l.spans.iter().map(|s| format!("{:?}={:?}", s.content, s.style)).collect::<Vec<_>>().join(" "))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Taken from `highlight` before `highlight_with` was split out of it.
    const PY_SNAPSHOT: &str = "\
\"def\"=Style::new().fg(Color::Rgb(102, 217, 239)).bg(Color::Rgb(39, 40, 34)).italic() \
\" \"=Style::new().fg(Color::Rgb(248, 248, 242)).bg(Color::Rgb(39, 40, 34)) \
\"f\"=Style::new().fg(Color::Rgb(166, 226, 46)).bg(Color::Rgb(39, 40, 34)) \
\"(\"=Style::new().fg(Color::Rgb(248, 248, 242)).bg(Color::Rgb(39, 40, 34)) \
\"x\"=Style::new().fg(Color::Rgb(253, 151, 31)).bg(Color::Rgb(39, 40, 34)).italic() \
\")\"=Style::new().fg(Color::Rgb(248, 248, 242)).bg(Color::Rgb(39, 40, 34)) \
\":\"=Style::new().fg(Color::Rgb(248, 248, 242)).bg(Color::Rgb(39, 40, 34)) \
\"\"=Style::new().fg(Color::Rgb(248, 248, 242)).bg(Color::Rgb(39, 40, 34))
\"    \"=Style::new().fg(Color::Rgb(248, 248, 242)).bg(Color::Rgb(39, 40, 34)) \
\"return\"=Style::new().fg(Color::Rgb(249, 38, 114)).bg(Color::Rgb(39, 40, 34)) \
\" \"=Style::new().fg(Color::Rgb(248, 248, 242)).bg(Color::Rgb(39, 40, 34)) \
\"x\"=Style::new().fg(Color::Rgb(248, 248, 242)).bg(Color::Rgb(39, 40, 34)) \
\"\"=Style::new().fg(Color::Rgb(248, 248, 242)).bg(Color::Rgb(39, 40, 34))
\"\"=Style::new().fg(Color::Rgb(248, 248, 242)).bg(Color::Rgb(39, 40, 34))";

    #[test]
    fn highlight_snapshot_is_unchanged() {
        let t = crate::theme::Theme::load("monokai").unwrap();
        let text = highlight("def f(x):\n    return x\n", "py", &t.syntax, 4);
        assert_eq!(snapshot(&text), PY_SNAPSHOT);
    }

    #[test]
    fn cut_keeps_width() {
        assert_eq!(cut("abcdef", 4), "abc…");
        assert_eq!(cut("abc", 4), "abc");
        assert_eq!(cut("日本語テキスト", 5), "日本…");
    }

    #[test]
    fn cut_left_keeps_the_end() {
        assert_eq!(cut_left("abc/def.py", 8), "…/def.py");
        assert_eq!(cut_left("abc", 8), "abc");
        assert_eq!(cut_left("日本語", 4).width(), 4);
        assert_eq!(cut("日本語", 4).width(), 4);
    }
}
