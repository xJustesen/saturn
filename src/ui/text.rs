//! Text utilities: owning conversions, character wrapping that matches the
//! editor, and syntect highlighting for unfocused code cells.

use edtui::syntect::easy::HighlightLines;
use edtui::syntect::highlighting::{FontStyle, Style as SynStyle, Theme as SynTheme};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use unicode_width::UnicodeWidthChar;

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

/// Syntax-highlight `source` into owned lines (no wrapping).
pub fn highlight(source: &str, ext: &str, theme: &SynTheme, tab_width: usize) -> Text<'static> {
    let source = expand_tabs(source, tab_width);
    let ss = &*edtui::SYNTAX_SET;
    let Some(syntax) = ss.find_syntax_by_extension(ext) else {
        return Text::from(source.split('\n').map(|l| Line::from(l.to_string())).collect::<Vec<_>>());
    };
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
