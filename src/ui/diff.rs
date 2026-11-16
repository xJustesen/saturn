//! A proposed edit to a cell's source, shown in place as a unified diff
//! until the user accepts or rejects it.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::Theme;
use crate::ui::text::{expand_tabs, highlight, wrap_line};

/// Columns taken by the `+`/`-` sign and the space after it.
const SIGN_W: u16 = 2;
/// Above this many line pairs the diff is not aligned: everything old is
/// removed and everything new is added.
const MAX_PAIRS: usize = 4_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tag {
    Same,
    Del,
    Add,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub tag: Tag,
    pub text: String,
}

/// Line diff of `old` against `new` (longest common subsequence), removals
/// before additions inside a changed run.
pub fn diff_lines(old: &str, new: &str) -> Vec<DiffLine> {
    let a: Vec<&str> = old.split('\n').collect();
    let b: Vec<&str> = new.split('\n').collect();
    let line = |tag, text: &str| DiffLine {
        tag,
        text: text.to_string(),
    };
    if a.len().saturating_mul(b.len()) > MAX_PAIRS {
        return a
            .iter()
            .map(|t| line(Tag::Del, t))
            .chain(b.iter().map(|t| line(Tag::Add, t)))
            .collect();
    }
    // lcs[i][j]: length of the common subsequence of a[i..] and b[j..].
    let (n, m) = (a.len(), b.len());
    let mut lcs = vec![0u32; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[at(i, j)] = if a[i] == b[j] {
                lcs[at(i + 1, j + 1)] + 1
            } else {
                lcs[at(i + 1, j)].max(lcs[at(i, j + 1)])
            };
        }
    }
    let mut out = Vec::with_capacity(n.max(m));
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            out.push(line(Tag::Same, a[i]));
            i += 1;
            j += 1;
        } else if lcs[at(i + 1, j)] >= lcs[at(i, j + 1)] {
            out.push(line(Tag::Del, a[i]));
            i += 1;
        } else {
            out.push(line(Tag::Add, b[j]));
            j += 1;
        }
    }
    out.extend(a[i..].iter().map(|t| line(Tag::Del, t)));
    out.extend(b[j..].iter().map(|t| line(Tag::Add, t)));
    out
}

/// An edit the assistant proposes for one cell.
#[derive(Debug, Clone)]
pub struct Proposal {
    /// Names the proposal to whoever made it.
    pub id: u64,
    /// The source the cell gets when the proposal is accepted.
    pub after: String,
    /// The source the diff was taken against.
    base: String,
    lines: Vec<DiffLine>,
}

impl Proposal {
    pub fn new(id: u64, base: &str, after: String) -> Self {
        let lines = diff_lines(base, &after);
        Proposal {
            id,
            after,
            base: base.to_string(),
            lines,
        }
    }

    /// Take the diff again when the cell's source changed under the proposal.
    pub fn rebase(&mut self, base: &str) {
        if self.base != base {
            self.base = base.to_string();
            self.lines = diff_lines(base, &self.after);
        }
    }

    pub fn lines(&self) -> &[DiffLine] {
        &self.lines
    }

    /// Lines added and removed.
    pub fn changed(&self) -> (usize, usize) {
        let count = |t| self.lines.iter().filter(|l| l.tag == t).count();
        (count(Tag::Add), count(Tag::Del))
    }

    /// The diff as wrapped rows `cw` columns wide (sign column included);
    /// the flag marks the first row of a source line.
    fn wrapped(
        &self,
        cw: u16,
        ext: &str,
        theme: &Theme,
        tab: usize,
    ) -> Vec<(Tag, Line<'static>, bool)> {
        let w = cw.saturating_sub(SIGN_W).max(1) as usize;
        // Kept and added lines are the new source in order, so one pass
        // highlights them with the right state across lines.
        let mut new = highlight(&self.after, ext, &theme.syntax, tab)
            .lines
            .into_iter();
        let mut rows = Vec::new();
        for l in &self.lines {
            let line = match l.tag {
                Tag::Del => Line::from(Span::styled(expand_tabs(&l.text, tab), theme.muted())),
                Tag::Same | Tag::Add => new.next().unwrap_or_default(),
            };
            let parts = wrap_line(&line, w, tab);
            if parts.is_empty() {
                rows.push((l.tag, Line::default(), true));
            }
            rows.extend(
                parts
                    .into_iter()
                    .enumerate()
                    .map(|(k, p)| (l.tag, p, k == 0)),
            );
        }
        rows
    }

    /// Rows the proposal takes in a cell `cw` columns wide: the diff and
    /// the row of keys under it.
    pub fn rows(&self, cw: u16, ext: &str, theme: &Theme, tab: usize) -> u16 {
        (self.wrapped(cw, ext, theme, tab).len() + 1).min(u16::MAX as usize) as u16
    }
}

fn hint(theme: &Theme) -> Line<'static> {
    let ui = &theme.ui;
    let key = |s: &'static str, st: Style| Span::styled(s, st.add_modifier(Modifier::BOLD));
    let muted = |s: &'static str| Span::styled(s, theme.muted());
    Line::from(vec![
        key("± claude", Style::default().fg(ui.warning)),
        muted("   "),
        key("Tab", Style::default().fg(ui.focus)),
        muted(" accept   "),
        key("X", theme.error()),
        muted(" reject   "),
        key("⏎", Style::default().fg(ui.focus)),
        muted(" edit first"),
    ])
}

/// Draw the proposal at column `x`, `cw` columns wide, its first row at
/// `clip.y + top`; rows outside `clip` are skipped.
#[allow(clippy::too_many_arguments)]
pub fn render(
    p: &Proposal,
    x: u16,
    cw: u16,
    clip: Rect,
    top: i32,
    ext: &str,
    theme: &Theme,
    tab: usize,
    buf: &mut Buffer,
) {
    let ui = &theme.ui;
    let rows = p.wrapped(cw, ext, theme, tab);
    let visible = |i: usize| -> Option<u16> {
        let dy = top + i as i32;
        (dy >= 0 && dy < clip.height as i32).then(|| clip.y + dy as u16)
    };
    for (i, (tag, line, first)) in rows.iter().enumerate() {
        let Some(y) = visible(i) else { continue };
        let (sign, sign_st, bg) = match tag {
            Tag::Same => (" ", Style::default(), None),
            Tag::Del => ("-", theme.error(), ui.diff_del_bg),
            Tag::Add => ("+", Style::default().fg(ui.edit), ui.diff_add_bg),
        };
        // The sign goes on the first row of a wrapped line only.
        if *first {
            buf.set_string(x, y, sign, sign_st.add_modifier(Modifier::BOLD));
        }
        buf.set_line(x + SIGN_W, y, line, cw.saturating_sub(SIGN_W));
        if let Some(bg) = bg {
            buf.set_style(Rect::new(x, y, cw, 1), Style::default().bg(bg));
        }
    }
    if let Some(y) = visible(rows.len()) {
        buf.set_line(x, y, &hint(theme), cw);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(old: &str, new: &str) -> String {
        diff_lines(old, new)
            .iter()
            .map(|l| {
                let sign = match l.tag {
                    Tag::Same => ' ',
                    Tag::Del => '-',
                    Tag::Add => '+',
                };
                format!("{sign}{}", l.text)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn replaced_line_is_removed_then_added() {
        assert_eq!(tags("a\nb\nc", "a\nB\nc"), " a\n-b\n+B\n c");
    }

    #[test]
    fn inserted_and_deleted_lines() {
        assert_eq!(tags("a\nc", "a\nb\nc"), " a\n+b\n c");
        assert_eq!(tags("a\nb\nc", "a\nc"), " a\n-b\n c");
        assert_eq!(tags("x", "y\nz"), "-x\n+y\n+z");
        assert_eq!(tags("same", "same"), " same");
    }

    #[test]
    fn rebase_takes_the_diff_against_the_new_source() {
        let mut p = Proposal::new(1, "a", "a\nb".into());
        assert_eq!(p.changed(), (1, 0));
        p.rebase("a\nb");
        assert_eq!(p.changed(), (0, 0));
        p.rebase("b");
        assert_eq!(p.changed(), (1, 0));
    }

    #[test]
    fn draws_signs_rows_and_the_keys() {
        let theme = Theme::default();
        let p = Proposal::new(1, "x = 1\ny = 2", "x = 1\ny = 3\nz = 4".into());
        assert_eq!(p.rows(30, "py", &theme, 4), 5);
        let area = Rect::new(0, 0, 60, 6);
        let mut buf = Buffer::empty(area);
        render(&p, 2, 58, area, 0, "py", &theme, 4, &mut buf);
        let s: Vec<String> = (0..6)
            .map(|y| {
                (0..60)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect();
        assert_eq!(s[0], "    x = 1");
        assert_eq!(s[1], "  - y = 2");
        assert_eq!(s[2], "  + y = 3");
        assert_eq!(s[3], "  + z = 4");
        assert_eq!(s[4], "  ± claude   Tab accept   X reject   ⏎ edit first");
    }

    #[test]
    fn rows_above_the_clip_are_skipped() {
        let theme = Theme::default();
        let p = Proposal::new(1, "a", "b".into());
        let area = Rect::new(0, 0, 30, 2);
        let mut buf = Buffer::empty(area);
        render(&p, 0, 30, area, -1, "py", &theme, 4, &mut buf);
        let row0: String = (0..30).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert!(row0.starts_with("+ b"), "{row0}");
    }
}
