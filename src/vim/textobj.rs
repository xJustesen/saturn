//! Text objects (`iw`, `a(`, `ip`, …) plus the mini.ai objects LazyVim adds:
//! `q` any quote, `b` any bracket, `a` argument, `u`/`U` function call,
//! `f`/`c` function/class definition, `i` indent scope, `g` whole cell,
//! `d` digits.

use super::motion::{Class, class, closer, indent, is_blank, match_fwd, unmatched_open};
use super::{Pos, RKind, Region, Text};

fn chars(a: Pos, b: Pos) -> Region {
    Region { a, b, kind: RKind::Char }
}

fn lines(a: usize, b: usize) -> Region {
    Region { a: Pos { row: a, col: 0 }, b: Pos { row: b, col: 0 }, kind: RKind::Line }
}

pub fn object(t: &Text, cur: Pos, key: char, around: bool, count: usize) -> Option<Region> {
    match key {
        'w' => word(t, cur, false, around, count),
        'W' => word(t, cur, true, around, count),
        '"' | '\'' | '`' => quote(t, cur, key, around),
        'q' => ['"', '\'', '`'].iter().filter_map(|q| quote(t, cur, *q, around)).min_by_key(|r| quote_rank(cur, r)),
        '(' | ')' => bracket(t, cur, '(', around),
        '[' | ']' => bracket(t, cur, '[', around),
        '{' | '}' | 'B' => bracket(t, cur, '{', around),
        '<' | '>' => bracket(t, cur, '<', around),
        'b' => any_bracket(t, cur, around),
        'a' => argument(t, cur, around),
        'u' => call(t, cur, around, true),
        'U' => call(t, cur, around, false),
        'p' => paragraph(t, cur, around),
        'i' => indent_scope(t, cur, around),
        'f' => definition(t, cur, around, &["def ", "async def "]),
        'c' => definition(t, cur, around, &["class "]),
        'g' => Some(lines(0, t.len() - 1)),
        'd' => digits(t, cur),
        _ => None,
    }
}

fn word(t: &Text, cur: Pos, big: bool, around: bool, count: usize) -> Option<Region> {
    let line = &t[cur.row];
    if line.is_empty() {
        return None;
    }
    let col = cur.col.min(line.len() - 1);
    let cls = |i: usize| class(line[i], big);
    let mut start = col;
    while start > 0 && cls(start - 1) == cls(col) {
        start -= 1;
    }
    let run = |from: usize| {
        let mut e = from;
        while e + 1 < line.len() && cls(e + 1) == cls(from) {
            e += 1;
        }
        e + 1
    };
    let mut end = run(col);
    if around {
        if cls(col) == Class::Blank {
            // blanks, then the word after them
            if end < line.len() {
                end = run(end);
            }
        } else if end < line.len() && cls(end) == Class::Blank {
            end = run(end);
        } else {
            while start > 0 && cls(start - 1) == Class::Blank {
                start -= 1;
            }
        }
    }
    // Each extra count takes one more run (inner) or word + blanks (around).
    for _ in 1..count.max(1) {
        if end >= line.len() {
            break;
        }
        end = run(end);
        if around && end < line.len() && cls(end) == Class::Blank {
            end = run(end);
        }
    }
    Some(chars(Pos { row: cur.row, col: start }, Pos { row: cur.row, col: end }))
}

/// Prefer the pair around the cursor, then the nearest one after it.
fn quote_rank(cur: Pos, r: &Region) -> (bool, usize) {
    let inside = r.a.col <= cur.col + 1 && cur.col < r.b.col + 1;
    (!inside, if inside { r.b.col - r.a.col } else { r.a.col })
}

fn quote(t: &Text, cur: Pos, q: char, around: bool) -> Option<Region> {
    let line = &t[cur.row];
    let mut marks = Vec::new();
    let mut i = 0;
    while i < line.len() {
        if line[i] == '\\' {
            i += 2;
            continue;
        }
        if line[i] == q {
            marks.push(i);
        }
        i += 1;
    }
    let pairs: Vec<(usize, usize)> = marks.as_chunks::<2>().0.iter().map(|c| (c[0], c[1])).collect();
    let (a, b) = pairs
        .iter()
        .find(|(a, b)| *a <= cur.col && cur.col <= *b)
        .or_else(|| pairs.iter().find(|(a, _)| *a > cur.col))
        .copied()?;
    let (s, e) = if around { (a, b + 1) } else { (a + 1, b) };
    Some(chars(Pos { row: cur.row, col: s }, Pos { row: cur.row, col: e }))
}

/// The `open`…close pair around the cursor, or the next one on the line.
fn enclosing(t: &Text, cur: Pos, open: char) -> Option<(Pos, Pos)> {
    let close = closer(open)?;
    let here = t[cur.row].get(cur.col).copied();
    let start = if here == Some(open) {
        Some(cur)
    } else if here == Some(close) {
        super::motion::match_back(t, cur, open, close)
    } else {
        unmatched_open(t, cur, open, close)
    };
    let start = start.or_else(|| {
        let line = &t[cur.row];
        (cur.col..line.len()).find(|&c| line[c] == open).map(|col| Pos { row: cur.row, col })
    })?;
    let end = match_fwd(t, start, open, close)?;
    Some((start, end))
}

fn bracket_region(t: &Text, s: Pos, e: Pos, around: bool) -> Region {
    if around {
        return chars(s, Pos { row: e.row, col: e.col + 1 });
    }
    let a = Pos { row: s.row, col: s.col + 1 };
    // `(` ending its line and `)` starting its own: the lines between them.
    if e.row > s.row + 1 && a.col >= t[s.row].len() && t[e.row][..e.col].iter().all(|c| c.is_whitespace()) {
        return lines(s.row + 1, e.row - 1);
    }
    chars(a, e)
}

fn bracket(t: &Text, cur: Pos, open: char, around: bool) -> Option<Region> {
    let (s, e) = enclosing(t, cur, open)?;
    Some(bracket_region(t, s, e, around))
}

fn any_pair(t: &Text, cur: Pos) -> Option<(Pos, Pos)> {
    let found: Vec<(Pos, Pos)> = ['(', '[', '{'].iter().filter_map(|o| enclosing(t, cur, *o)).collect();
    // Innermost pair that really contains the cursor, else the nearest next.
    found
        .iter()
        .filter(|(s, e)| *s <= cur && cur <= *e)
        .max_by_key(|(s, _)| *s)
        .or_else(|| found.iter().min_by_key(|(s, _)| *s))
        .copied()
}

fn any_bracket(t: &Text, cur: Pos, around: bool) -> Option<Region> {
    let (s, e) = any_pair(t, cur)?;
    Some(bracket_region(t, s, e, around))
}

fn next_pos(t: &Text, p: Pos) -> Option<Pos> {
    if p.col + 1 < t[p.row].len() {
        Some(Pos { row: p.row, col: p.col + 1 })
    } else if p.row + 1 < t.len() {
        // Empty lines have no character; land on column 0 regardless.
        Some(Pos { row: p.row + 1, col: 0 })
    } else {
        None
    }
}

fn argument(t: &Text, cur: Pos, around: bool) -> Option<Region> {
    let (s, e) = any_pair(t, cur)?;
    // Split the inside on top-level commas.
    let mut seps = vec![s];
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut p = s;
    while let Some(n) = next_pos(t, p) {
        p = n;
        if p >= e {
            break;
        }
        let Some(&c) = t[p.row].get(p.col) else { continue };
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '"' | '\'' => quote = Some(c),
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                ',' if depth == 0 => seps.push(p),
                _ => {}
            },
        }
    }
    seps.push(e);
    let at = if cur <= s { 0 } else { seps.windows(2).position(|w| w[0] < cur && cur <= w[1]).unwrap_or(seps.len() - 2) };
    let (lo, hi) = (seps[at], seps[at + 1]);
    let is_ws = |p: Pos| t[p.row].get(p.col).is_none_or(|c| c.is_whitespace());
    // Trim whitespace on both sides of the argument.
    let mut a = next_pos(t, lo)?;
    while a < hi && is_ws(a) {
        a = next_pos(t, a)?;
    }
    let mut b = hi; // exclusive
    loop {
        let prev = if b.col > 0 {
            Pos { row: b.row, col: b.col - 1 }
        } else if b.row > a.row {
            Pos { row: b.row - 1, col: t[b.row - 1].len() }
        } else {
            break;
        };
        if prev < a || !(prev.col >= t[prev.row].len() || is_ws(prev)) {
            break;
        }
        b = prev;
    }
    if a >= b {
        return None;
    }
    if !around {
        return Some(chars(a, b));
    }
    if hi != e {
        // Take the trailing comma and the blanks after it.
        let mut end = Pos { row: hi.row, col: hi.col + 1 };
        while end.col < t[end.row].len() && t[end.row][end.col].is_whitespace() {
            end.col += 1;
        }
        Some(chars(a, end))
    } else if lo != s {
        Some(chars(lo, b))
    } else {
        Some(chars(a, b))
    }
}

fn call(t: &Text, cur: Pos, around: bool, dotted: bool) -> Option<Region> {
    let name_char = |c: char| c.is_alphanumeric() || c == '_' || (dotted && c == '.');
    let line = &t[cur.row];
    // On the name itself: use the call that follows it.
    let mut from = cur;
    if line.get(cur.col).is_some_and(|c| name_char(*c)) {
        let mut e = cur.col;
        while e < line.len() && name_char(line[e]) {
            e += 1;
        }
        if line.get(e) == Some(&'(') {
            from = Pos { row: cur.row, col: e };
        }
    }
    loop {
        let (s, e) = enclosing(t, from, '(')?;
        let l = &t[s.row];
        let mut n = s.col;
        while n > 0 && name_char(l[n - 1]) {
            n -= 1;
        }
        if n < s.col {
            return Some(if around {
                chars(Pos { row: s.row, col: n }, Pos { row: e.row, col: e.col + 1 })
            } else {
                bracket_region(t, s, e, false)
            });
        }
        // A bare parenthesis: try the pair around it.
        if s.col == 0 && s.row == 0 {
            return None;
        }
        let outer = unmatched_open(t, s, '(', ')')?;
        from = outer;
    }
}

fn paragraph(t: &Text, cur: Pos, around: bool) -> Option<Region> {
    let empty = t[cur.row].is_empty();
    let same = |r: usize| t[r].is_empty() == empty;
    let mut a = cur.row;
    while a > 0 && same(a - 1) {
        a -= 1;
    }
    let mut b = cur.row;
    while b + 1 < t.len() && same(b + 1) {
        b += 1;
    }
    if around {
        let b0 = b;
        while b + 1 < t.len() && !same(b + 1) {
            b += 1;
        }
        if b == b0 {
            while a > 0 && !same(a - 1) {
                a -= 1;
            }
        }
    }
    Some(lines(a, b))
}

/// Indent of the row, or of the nearest non-blank row for a blank one.
fn scope_indent(t: &Text, row: usize) -> Option<usize> {
    if !is_blank(t, row) {
        return Some(indent(t, row));
    }
    let below = (row..t.len()).find(|r| !is_blank(t, *r)).map(|r| indent(t, r));
    let above = (0..row).rev().find(|r| !is_blank(t, *r)).map(|r| indent(t, r));
    match (above, below) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

/// Rows around `row` indented at least `ind` (blank rows in between count).
fn scope_rows(t: &Text, row: usize, ind: usize) -> (usize, usize) {
    let inside = |r: usize| is_blank(t, r) || indent(t, r) >= ind;
    let mut a = row;
    while a > 0 && inside(a - 1) {
        a -= 1;
    }
    let mut b = row;
    while b + 1 < t.len() && inside(b + 1) {
        b += 1;
    }
    while a < b && is_blank(t, a) {
        a += 1;
    }
    while b > a && is_blank(t, b) {
        b -= 1;
    }
    (a, b)
}

fn indent_scope(t: &Text, cur: Pos, around: bool) -> Option<Region> {
    let ind = scope_indent(t, cur.row)?;
    let (mut a, b) = scope_rows(t, cur.row, ind);
    if around && a > 0 && ind > 0 {
        a -= 1;
    }
    Some(lines(a, b))
}

/// Python `def` / `class` found by indentation: the header line at or above
/// the cursor whose body contains it.
fn definition(t: &Text, cur: Pos, around: bool, heads: &[&str]) -> Option<Region> {
    let is_head = |r: usize| {
        let s: String = t[r].iter().skip(indent(t, r)).collect();
        heads.iter().any(|h| s.starts_with(h))
    };
    let mut limit = scope_indent(t, cur.row)? + 1;
    let mut r = cur.row;
    let head = loop {
        if !is_blank(t, r) && indent(t, r) < limit {
            if is_head(r) {
                // The body must reach down to the cursor row.
                break r;
            }
            limit = indent(t, r);
        }
        if r == 0 {
            return None;
        }
        r -= 1;
    };
    let ind = indent(t, head);
    let mut end = head;
    for row in head + 1..t.len() {
        if is_blank(t, row) {
            continue;
        }
        if indent(t, row) <= ind {
            break;
        }
        end = row;
    }
    if end < cur.row {
        return None;
    }
    if !around {
        return (end > head).then(|| lines(head + 1, end));
    }
    // Decorators belong to the definition.
    let mut start = head;
    while start > 0 && indent(t, start - 1) == ind && t[start - 1].get(ind) == Some(&'@') {
        start -= 1;
    }
    Some(lines(start, end))
}

fn digits(t: &Text, cur: Pos) -> Option<Region> {
    let line = &t[cur.row];
    let at = (cur.col..line.len()).find(|&c| line[c].is_ascii_digit())?;
    let mut s = at;
    while s > 0 && line[s - 1].is_ascii_digit() {
        s -= 1;
    }
    let mut e = at;
    while e < line.len() && line[e].is_ascii_digit() {
        e += 1;
    }
    Some(chars(Pos { row: cur.row, col: s }, Pos { row: cur.row, col: e }))
}
