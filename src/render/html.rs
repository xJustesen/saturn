//! Minimal HTML → text: a plain-text fallback (paragraphs, entities) and an
//! aligned table renderer that keeps per-cell colours (pandas `Styler`).

use std::collections::HashMap;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use unicode_width::UnicodeWidthStr;

use crate::ui::text::clean_text;

pub fn to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut chars = html.chars().peekable();
    let mut skip_until: Option<&str> = None;
    let mut pending_ws = false;
    while let Some(c) = chars.next() {
        if c == '<' {
            let mut tag = String::new();
            for t in chars.by_ref() {
                if t == '>' {
                    break;
                }
                tag.push(t);
            }
            let lower = tag.trim().to_ascii_lowercase();
            let name: String = lower
                .trim_start_matches('/')
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            let closing = lower.starts_with('/');
            if let Some(until) = skip_until {
                if closing && name == until {
                    skip_until = None;
                }
                continue;
            }
            match name.as_str() {
                "style" | "script" | "head" if !closing => skip_until = Some(if name == "style" { "style" } else if name == "script" { "script" } else { "head" }),
                "br" => out.push('\n'),
                "p" | "div" | "tr" | "li" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "pre" | "table" | "thead" | "tbody" | "ul" | "ol" | "blockquote" => {
                    if !out.is_empty() && !out.ends_with('\n') {
                        out.push('\n');
                    }
                    pending_ws = false;
                }
                "td" | "th" => {
                    if !closing && !out.ends_with('\n') && !out.is_empty() {
                        out.push_str("  ");
                    }
                    pending_ws = false;
                }
                _ => {}
            }
            continue;
        }
        if skip_until.is_some() {
            continue;
        }
        if c == '&' {
            if pending_ws && !out.is_empty() && !out.ends_with('\n') && !out.ends_with(' ') {
                out.push(' ');
            }
            pending_ws = false;
            let mut ent = String::new();
            while let Some(&n) = chars.peek() {
                if n == ';' || ent.len() > 8 {
                    chars.next();
                    break;
                }
                if !n.is_ascii_alphanumeric() && n != '#' {
                    break;
                }
                ent.push(n);
                chars.next();
            }
            let decoded = match ent.as_str() {
                "amp" => "&".to_string(),
                "lt" => "<".to_string(),
                "gt" => ">".to_string(),
                "quot" => "\"".to_string(),
                "apos" | "#39" => "'".to_string(),
                "nbsp" | "#160" => " ".to_string(),
                s if s.starts_with("#x") || s.starts_with("#X") => u32::from_str_radix(&s[2..], 16)
                    .ok()
                    .and_then(char::from_u32)
                    .map(|c| c.to_string())
                    .unwrap_or_default(),
                s if s.starts_with('#') => s[1..]
                    .parse::<u32>()
                    .ok()
                    .and_then(char::from_u32)
                    .map(|c| c.to_string())
                    .unwrap_or_default(),
                other => format!("&{other};"),
            };
            out.push_str(&decoded);
            continue;
        }
        if c.is_whitespace() {
            if c == '\n' && out.ends_with('\n') {
                continue;
            }
            pending_ws = true;
            continue;
        }
        if pending_ws && !out.is_empty() && !out.ends_with('\n') && !out.ends_with(' ') {
            out.push(' ');
        }
        pending_ws = false;
        out.push(c);
    }
    let trimmed = out.trim_end_matches('\n');
    // collapse runs of blank lines
    let mut result = String::with_capacity(trimmed.len());
    let mut blank = 0;
    for line in trimmed.lines() {
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        result.push_str(line.trim_end());
        result.push('\n');
    }
    result.trim_end_matches('\n').to_string()
}

/// One `<td>`/`<th>` of the first table.
struct RawCell {
    text: String,
    header: bool,
    style: Style,
    colspan: usize,
    rowspan: usize,
}

/// Render the first `<table>` of `html` with aligned columns, bold headers and
/// the cells' background/foreground colours from `<style>` id rules or inline
/// `style` attributes. `None` when there is no table.
pub fn table(html: &str) -> Option<Text<'static>> {
    // ASCII lowercasing keeps byte offsets, so `lower` indexes `html`.
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<table")?;
    let end = lower[start..].find("</table").map_or(html.len(), |e| start + e);
    let css = css_rules(html, &lower);

    let mut rows: Vec<Vec<RawCell>> = Vec::new();
    let mut open: Option<(usize, RawCell)> = None;
    let finish = |open: &mut Option<(usize, RawCell)>, rows: &mut Vec<Vec<RawCell>>, at: usize| {
        if let Some((from, mut cell)) = open.take() {
            cell.text = clean_text(to_text(&html[from..at]).trim()).replace('\n', " ");
            if rows.is_empty() {
                rows.push(Vec::new());
            }
            rows.last_mut().unwrap().push(cell);
        }
    };
    let mut i = start;
    while let Some(off) = lower[i..end].find('<') {
        let p = i + off;
        let Some(close) = lower[p..end].find('>').map(|e| p + e) else { break };
        let tag = &html[p + 1..close];
        let tl = &lower[p + 1..close];
        let closing = tl.starts_with('/');
        let name: String = tl.trim_start_matches('/').chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
        match (name.as_str(), closing) {
            ("tr", false) => {
                finish(&mut open, &mut rows, p);
                rows.push(Vec::new());
            }
            ("tr", true) | ("td" | "th", true) => finish(&mut open, &mut rows, p),
            ("td" | "th", false) => {
                finish(&mut open, &mut rows, p);
                let mut style = attr(tag, "id").and_then(|id| css.get(&id).copied()).unwrap_or_default();
                if let Some(inline) = attr(tag, "style") {
                    style = style.patch(declarations(&inline));
                }
                let span = |a: &str| attr(tag, a).and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(1).clamp(1, 64);
                let cell = RawCell { text: String::new(), header: name == "th", style, colspan: span("colspan"), rowspan: span("rowspan") };
                open = Some((close + 1, cell));
            }
            _ => {}
        }
        i = close + 1;
    }
    finish(&mut open, &mut rows, end);

    // Lay the cells out on a grid, honouring colspan/rowspan.
    struct Slot {
        text: String,
        header: bool,
        style: Style,
    }
    let blank = || Slot { text: String::new(), header: false, style: Style::default() };
    let mut grid: Vec<Vec<Slot>> = Vec::new();
    let mut carry: Vec<usize> = Vec::new(); // rows a rowspan still covers, per column
    for row in rows {
        let mut out: Vec<Slot> = Vec::new();
        let mut cells = row.into_iter();
        loop {
            let col = out.len();
            if carry.get(col).is_some_and(|&n| n > 0) {
                carry[col] -= 1;
                out.push(blank());
                continue;
            }
            let Some(c) = cells.next() else { break };
            for k in 0..c.colspan {
                if carry.len() <= col + k {
                    carry.resize(col + k + 1, 0);
                }
                carry[col + k] = c.rowspan - 1;
                let text = if k == 0 { c.text.clone() } else { String::new() };
                out.push(Slot { text, header: c.header, style: c.style });
            }
        }
        // Columns past the last cell may still be covered by a rowspan.
        while carry.get(out.len()).is_some_and(|&n| n > 0) {
            let col = out.len();
            carry[col] -= 1;
            out.push(blank());
        }
        if out.iter().any(|s| !s.text.is_empty()) {
            grid.push(out);
        }
    }
    if grid.is_empty() {
        return None;
    }
    let ncols = grid.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![0usize; ncols];
    // Right-align columns whose data cells (not headers) are all numbers.
    let mut numeric: Vec<Option<bool>> = vec![None; ncols];
    for row in &grid {
        for (c, s) in row.iter().enumerate() {
            widths[c] = widths[c].max(s.text.width());
            if !s.header && !s.text.is_empty() {
                numeric[c] = Some(numeric[c].unwrap_or(true) && looks_numeric(&s.text));
            }
        }
    }
    let numeric: Vec<bool> = numeric.into_iter().map(|n| n.unwrap_or(false)).collect();
    let lines = grid
        .into_iter()
        .map(|row| {
            let mut spans = Vec::with_capacity(ncols);
            for (c, w) in widths.iter().enumerate() {
                let slot = row.get(c);
                let text = slot.map_or("", |s| s.text.as_str());
                let pad = w.saturating_sub(text.width());
                let body = if numeric[c] {
                    format!(" {}{text} ", " ".repeat(pad))
                } else {
                    format!(" {text}{} ", " ".repeat(pad))
                };
                let mut style = slot.map_or_else(Style::default, |s| s.style);
                if slot.is_some_and(|s| s.header) {
                    style = style.add_modifier(Modifier::BOLD);
                }
                spans.push(Span::styled(body, style));
            }
            // No trailing blanks without a background: they would only widen
            // the output.
            while spans.last().is_some_and(|s: &Span| s.style.bg.is_none() && s.content.trim().is_empty()) {
                spans.pop();
            }
            if let Some(last) = spans.last_mut()
                && last.style.bg.is_none()
            {
                last.content = last.content.trim_end().to_string().into();
            }
            Line::from(spans)
        })
        .collect::<Vec<_>>();
    Some(Text::from(lines))
}

fn looks_numeric(s: &str) -> bool {
    let t = s.trim().trim_end_matches('%').replace([',', '_'], "");
    let t = t.strip_prefix('\u{2212}').map_or(t.clone(), |r| format!("-{r}"));
    t.parse::<f64>().is_ok() || matches!(t.to_ascii_lowercase().as_str(), "nan" | "inf" | "-inf" | "<na>" | "nat" | "none")
}

/// The value of attribute `name` in a tag's inside (`td id="x" class=y`).
fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut from = 0;
    while let Some(off) = lower[from..].find(name) {
        let p = from + off;
        from = p + name.len();
        let before_ok = p == 0 || bytes[p - 1].is_ascii_whitespace();
        let rest = lower[from..].trim_start();
        if !before_ok || !rest.starts_with('=') {
            continue;
        }
        let vstart = tag.len() - rest.len() + 1;
        let v = tag[vstart..].trim_start();
        return Some(match v.chars().next() {
            Some(q @ ('"' | '\'')) => v[1..].split(q).next().unwrap_or("").to_string(),
            _ => v.split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("").to_string(),
        });
    }
    None
}

/// `#id { … }` rules from the `<style>` blocks, as styles keyed by id.
fn css_rules(html: &str, lower: &str) -> HashMap<String, Style> {
    let mut map: HashMap<String, Style> = HashMap::new();
    let mut i = 0;
    while let Some(off) = lower[i..].find("<style") {
        let Some(open) = lower[i + off..].find('>').map(|e| i + off + e + 1) else { break };
        let close = lower[open..].find("</style").map_or(html.len(), |e| open + e);
        for rule in html[open..close].split('}') {
            let Some((sel, decl)) = rule.split_once('{') else { continue };
            let style = declarations(decl);
            if style == Style::default() {
                continue;
            }
            for s in sel.split(',') {
                if let Some(id) = s.trim().strip_prefix('#')
                    && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                {
                    let e = map.entry(id.to_string()).or_default();
                    *e = e.patch(style);
                }
            }
        }
        i = close;
    }
    map
}

/// The colours of a CSS declaration list (`background-color: #fff; color: red`).
fn declarations(decl: &str) -> Style {
    let mut style = Style::default();
    for d in decl.split(';') {
        let Some((prop, val)) = d.split_once(':') else { continue };
        let val = val.trim().trim_end_matches("!important").trim();
        match prop.trim().to_ascii_lowercase().as_str() {
            "background-color" | "background" => {
                if let Some(c) = css_color(val) {
                    style = style.bg(c);
                }
            }
            "color" => {
                if let Some(c) = css_color(val) {
                    style = style.fg(c);
                }
            }
            "font-weight" if val == "bold" || val.parse::<u32>().is_ok_and(|w| w >= 600) => {
                style = style.add_modifier(Modifier::BOLD);
            }
            _ => {}
        }
    }
    style
}

fn css_color(v: &str) -> Option<Color> {
    let v = v.trim().to_ascii_lowercase();
    if let Some(hex) = v.strip_prefix('#') {
        let hex = match hex.len() {
            3 | 4 => hex.chars().take(3).flat_map(|c| [c, c]).collect::<String>(),
            6 | 8 => hex[..6].to_string(),
            _ => return None,
        };
        let n = u32::from_str_radix(&hex, 16).ok()?;
        return Some(Color::Rgb((n >> 16) as u8, (n >> 8) as u8, n as u8));
    }
    if let Some(args) = v.strip_prefix("rgba(").or_else(|| v.strip_prefix("rgb(")) {
        let mut it = args.trim_end_matches(')').split(',').map(|x| x.trim().parse::<f32>().ok());
        let (r, g, b) = (it.next()??, it.next()??, it.next()??);
        return Some(Color::Rgb(r as u8, g as u8, b as u8));
    }
    match v.as_str() {
        "black" => Some(Color::Rgb(0, 0, 0)),
        "white" => Some(Color::Rgb(255, 255, 255)),
        "red" => Some(Color::Rgb(255, 0, 0)),
        "green" => Some(Color::Rgb(0, 128, 0)),
        "blue" => Some(Color::Rgb(0, 0, 255)),
        "yellow" => Some(Color::Rgb(255, 255, 0)),
        "orange" => Some(Color::Rgb(255, 165, 0)),
        "gray" | "grey" => Some(Color::Rgb(128, 128, 128)),
        "lightgreen" => Some(Color::Rgb(144, 238, 144)),
        "pink" => Some(Color::Rgb(255, 192, 203)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::to_text;

    #[test]
    fn strips_tags_and_entities() {
        assert_eq!(to_text("<p>a &amp; b</p><p>c</p>"), "a & b\nc");
        assert_eq!(to_text("<table><tr><th>x</th><th>y</th></tr><tr><td>1</td><td>2</td></tr></table>"), "x  y\n1  2");
        assert_eq!(to_text("<style>p{}</style><div>hi<br>there</div>"), "hi\nthere");
    }

    fn rows(t: &ratatui::text::Text) -> Vec<String> {
        t.lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect()
    }

    #[test]
    fn styler_table_keeps_colours_and_alignment() {
        let html = r#"<style type="text/css">
#T_a_row0_col1, #T_a_row1_col0 { background-color: #efb299; color: #000000; }
</style>
<table id="T_a"><thead>
<tr><th class="blank">&nbsp;</th><th>name</th><th>IC</th></tr>
<tr><th>feature</th><th>&nbsp;</th><th>&nbsp;</th></tr>
<tr><th>&nbsp;</th><th>&nbsp;</th><th>&nbsp;</th></tr>
</thead><tbody>
<tr><th>alpha</th><td id="T_a_row0_col0">+</td><td id="T_a_row0_col1">0.046</td></tr>
<tr><th>b</th><td id="T_a_row1_col0">long text</td><td id="T_a_row1_col1">-12.5</td></tr>
</tbody></table>"#;
        let t = super::table(html).unwrap();
        assert_eq!(
            rows(&t),
            vec![
                "          name          IC",
                " feature",
                " alpha    +          0.046 ",
                " b        long text  -12.5",
            ]
        );
        let peach = ratatui::style::Color::Rgb(0xef, 0xb2, 0x99);
        assert_eq!(t.lines[2].spans[2].style.bg, Some(peach));
        assert_eq!(t.lines[3].spans[1].style.bg, Some(peach));
        assert_eq!(t.lines[3].spans[2].style.bg, None);
    }

    #[test]
    fn table_spans() {
        let html = "<table><tr><th colspan=2>ab</th><th>c</th></tr><tr><th rowspan=2>x</th><td>1</td><td>2</td></tr><tr><td>3</td><td>4</td></tr></table>";
        assert_eq!(rows(&super::table(html).unwrap()), vec![" ab     c", " x   1  2", "     3  4"]);
    }
}
