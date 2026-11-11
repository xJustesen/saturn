//! Cursor motions as pure functions over the text. Positions may point one
//! past the end of a line where an operator needs an exclusive end; callers
//! clamp when they only move the cursor.

use super::{Pos, Text};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    Blank,
    Word,
    Punct,
}

pub fn class(c: char, big: bool) -> Class {
    if c.is_whitespace() {
        Class::Blank
    } else if big || c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

pub fn len(t: &Text, row: usize) -> usize {
    t.get(row).map_or(0, Vec::len)
}

pub fn first_nonblank(t: &Text, row: usize) -> usize {
    let line = &t[row];
    line.iter().position(|c| !c.is_whitespace()).unwrap_or(line.len().saturating_sub(1).min(line.len()))
}

/// Leading whitespace of a row.
pub fn indent(t: &Text, row: usize) -> usize {
    t[row].iter().take_while(|c| c.is_whitespace()).count()
}

pub fn is_blank(t: &Text, row: usize) -> bool {
    t[row].iter().all(|c| c.is_whitespace())
}

fn cls_at(t: &Text, p: Pos, big: bool) -> Class {
    t[p.row].get(p.col).map_or(Class::Blank, |c| class(*c, big))
}

/// Step to the next character position, crossing lines.
fn advance(t: &Text, p: &mut Pos) -> bool {
    if p.col + 1 < t[p.row].len() {
        p.col += 1;
        true
    } else if p.row + 1 < t.len() {
        p.row += 1;
        p.col = 0;
        true
    } else {
        false
    }
}

fn retreat(t: &Text, p: &mut Pos) -> bool {
    if p.col > 0 {
        p.col -= 1;
        true
    } else if p.row > 0 {
        p.row -= 1;
        p.col = t[p.row].len().saturating_sub(1);
        true
    } else {
        false
    }
}

/// `w` / `W`. At the end of the text the result is one past the last char.
pub fn word_fwd(t: &Text, mut p: Pos, big: bool) -> Pos {
    let last = t.len() - 1;
    if p.col < t[p.row].len() {
        let c0 = class(t[p.row][p.col], big);
        if c0 != Class::Blank {
            while p.col < t[p.row].len() && class(t[p.row][p.col], big) == c0 {
                p.col += 1;
            }
        }
    }
    loop {
        let line = &t[p.row];
        while p.col < line.len() && class(line[p.col], big) == Class::Blank {
            p.col += 1;
        }
        if p.col < line.len() {
            return p;
        }
        if p.row == last {
            return Pos { row: p.row, col: line.len() };
        }
        p.row += 1;
        p.col = 0;
        if t[p.row].is_empty() {
            return p;
        }
    }
}

/// `e` / `E`.
pub fn word_end(t: &Text, mut p: Pos, big: bool) -> Pos {
    if !advance(t, &mut p) {
        return p;
    }
    while cls_at(t, p, big) == Class::Blank {
        if !advance(t, &mut p) {
            return p;
        }
    }
    run_end(t, p, big)
}

/// Last position of the run of same-class characters at `p`.
pub fn run_end(t: &Text, mut p: Pos, big: bool) -> Pos {
    let c0 = cls_at(t, p, big);
    let line = &t[p.row];
    while p.col + 1 < line.len() && class(line[p.col + 1], big) == c0 {
        p.col += 1;
    }
    p
}

/// `b` / `B`.
pub fn word_back(t: &Text, mut p: Pos, big: bool) -> Pos {
    if !retreat(t, &mut p) {
        return p;
    }
    loop {
        if t[p.row].is_empty() {
            return p;
        }
        if cls_at(t, p, big) != Class::Blank {
            break;
        }
        if !retreat(t, &mut p) {
            return p;
        }
    }
    let c0 = cls_at(t, p, big);
    let line = &t[p.row];
    while p.col > 0 && class(line[p.col - 1], big) == c0 {
        p.col -= 1;
    }
    p
}

/// `ge` / `gE`.
pub fn word_end_back(t: &Text, mut p: Pos, big: bool) -> Pos {
    let row0 = p.row;
    let c0 = cls_at(t, p, big);
    if c0 != Class::Blank {
        loop {
            if !retreat(t, &mut p) {
                return Pos::default();
            }
            if p.row != row0 || cls_at(t, p, big) != c0 {
                break;
            }
        }
    } else if !retreat(t, &mut p) {
        return Pos::default();
    }
    loop {
        if t[p.row].is_empty() || cls_at(t, p, big) != Class::Blank {
            return p;
        }
        if !retreat(t, &mut p) {
            return p;
        }
    }
}

/// `}`: the next empty line, or one past the end of the text.
pub fn para_fwd(t: &Text, row: usize) -> Pos {
    let last = t.len() - 1;
    let mut r = row;
    while r < last && t[r].is_empty() {
        r += 1;
    }
    while r < last && !t[r].is_empty() {
        r += 1;
    }
    Pos { row: r, col: t[r].len() }
}

/// `{`: the previous empty line, or the start of the text.
pub fn para_back(t: &Text, row: usize) -> Pos {
    let mut r = row;
    while r > 0 && t[r].is_empty() {
        r -= 1;
    }
    while r > 0 && !t[r].is_empty() {
        r -= 1;
    }
    Pos { row: r, col: 0 }
}

pub fn closer(open: char) -> Option<char> {
    Some(match open {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        '<' => '>',
        _ => return None,
    })
}

pub fn opener(close: char) -> Option<char> {
    Some(match close {
        ')' => '(',
        ']' => '[',
        '}' => '{',
        '>' => '<',
        _ => return None,
    })
}

/// Matching close bracket for the open bracket at `p`.
pub fn match_fwd(t: &Text, p: Pos, open: char, close: char) -> Option<Pos> {
    let mut depth = 0usize;
    let mut q = p;
    loop {
        if let Some(&c) = t[q.row].get(q.col) {
            if c == open {
                depth += 1;
            } else if c == close {
                depth -= 1;
                if depth == 0 {
                    return Some(q);
                }
            }
        }
        if !advance(t, &mut q) {
            return None;
        }
    }
}

/// Matching open bracket for the close bracket at `p`.
pub fn match_back(t: &Text, p: Pos, open: char, close: char) -> Option<Pos> {
    let mut depth = 0usize;
    let mut q = p;
    loop {
        if let Some(&c) = t[q.row].get(q.col) {
            if c == close {
                depth += 1;
            } else if c == open {
                depth -= 1;
                if depth == 0 {
                    return Some(q);
                }
            }
        }
        if !retreat(t, &mut q) {
            return None;
        }
    }
}

/// `%`: jump from the first bracket at or after the cursor to its partner.
pub fn match_pair(t: &Text, p: Pos) -> Option<Pos> {
    let line = &t[p.row];
    let col = (p.col..line.len()).find(|&c| matches!(line[c], '(' | ')' | '[' | ']' | '{' | '}'))?;
    let at = Pos { row: p.row, col };
    let c = line[col];
    match closer(c) {
        Some(close) => match_fwd(t, at, c, close),
        None => match_back(t, at, opener(c)?, c),
    }
}

/// `[(` / `[{`: the unmatched open bracket before `p`.
pub fn unmatched_open(t: &Text, p: Pos, open: char, close: char) -> Option<Pos> {
    let mut depth = 0usize;
    let mut q = p;
    loop {
        if !retreat(t, &mut q) {
            return None;
        }
        match t[q.row].get(q.col) {
            Some(&c) if c == close => depth += 1,
            Some(&c) if c == open => {
                if depth == 0 {
                    return Some(q);
                }
                depth -= 1;
            }
            _ => {}
        }
    }
}

/// `])` / `]}`: the unmatched close bracket after `p`.
pub fn unmatched_close(t: &Text, p: Pos, open: char, close: char) -> Option<Pos> {
    let mut depth = 0usize;
    let mut q = p;
    loop {
        if !advance(t, &mut q) {
            return None;
        }
        match t[q.row].get(q.col) {
            Some(&c) if c == open => depth += 1,
            Some(&c) if c == close => {
                if depth == 0 {
                    return Some(q);
                }
                depth -= 1;
            }
            _ => {}
        }
    }
}

/// `f` `F` `t` `T` on one line. `repeat` is set for `;` and `,`, where a
/// till-motion must not stick on the character it already stopped before.
pub fn find_char(line: &[char], col: usize, ch: char, fwd: bool, till: bool, count: usize, repeat: bool) -> Option<usize> {
    let skip = usize::from(till && repeat);
    let mut left = count.max(1);
    if fwd {
        let mut i = col + 1 + skip;
        while i < line.len() {
            if line[i] == ch {
                left -= 1;
                if left == 0 {
                    return Some(if till { i - 1 } else { i });
                }
            }
            i += 1;
        }
    } else {
        let mut i = col.checked_sub(1 + skip)?;
        loop {
            if line[i] == ch {
                left -= 1;
                if left == 0 {
                    return Some(if till { i + 1 } else { i });
                }
            }
            if i == 0 {
                break;
            }
            i -= 1;
        }
    }
    None
}
