//! Minimal HTML → plain text fallback (tables, paragraphs, entities).

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

#[cfg(test)]
mod tests {
    use super::to_text;

    #[test]
    fn strips_tags_and_entities() {
        assert_eq!(to_text("<p>a &amp; b</p><p>c</p>"), "a & b\nc");
        assert_eq!(to_text("<table><tr><th>x</th><th>y</th></tr><tr><td>1</td><td>2</td></tr></table>"), "x  y\n1  2");
        assert_eq!(to_text("<style>p{}</style><div>hi<br>there</div>"), "hi\nthere");
    }
}
