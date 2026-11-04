//! The start dashboard shown by `saturn` with no arguments: the Saturn logo,
//! a short menu and a footer. Holds the menu state, handles keys and draws;
//! the app runs the chosen item.

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use unicode_width::UnicodeWidthStr;

use crate::theme::{Theme, Ui, mix};
use crate::ui::dashboard_logo::{LOGO, MASK, WIDTH as LOGO_W};
use crate::ui::icons::dash_icon;
use crate::ui::text::cut;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DashItem {
    FindFiles,
    NewNotebook,
    Projects,
    Recent,
    Theme,
    Quit,
}

/// The menu, top to bottom: item, key, label.
pub const ITEMS: [(DashItem, char, &str); 6] = [
    (DashItem::FindFiles, 'f', "Find files"),
    (DashItem::NewNotebook, 'n', "New notebook"),
    (DashItem::Projects, 'p', "Projects"),
    (DashItem::Recent, 'r', "Recent notebooks"),
    (DashItem::Theme, 't', "Theme"),
    (DashItem::Quit, 'q', "Quit"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DashAction {
    None,
    Run(DashItem),
}

#[derive(Clone, Debug, Default)]
pub struct DashboardState {
    pub selected: usize,
}

impl DashboardState {
    fn select(&mut self, item: DashItem) -> DashAction {
        if let Some(i) = ITEMS.iter().position(|(it, ..)| *it == item) {
            self.selected = i;
        }
        DashAction::Run(item)
    }

    fn step(&mut self, down: bool) {
        let n = ITEMS.len();
        self.selected = if down { (self.selected + 1) % n } else { (self.selected + n - 1) % n };
    }

    /// Every key ends here while the dashboard is up, so none reaches the
    /// (hidden, empty) notebook behind it.
    pub fn handle_key(&mut self, key: KeyEvent) -> DashAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Down | KeyCode::Tab => self.step(true),
            KeyCode::Up | KeyCode::BackTab => self.step(false),
            KeyCode::Char('n') if ctrl => self.step(true),
            KeyCode::Char('p') if ctrl => self.step(false),
            KeyCode::Char('o') if ctrl => return self.select(DashItem::FindFiles),
            KeyCode::Char('q') if ctrl => return self.select(DashItem::Quit),
            _ if ctrl || alt => {}
            KeyCode::Char('j') => self.step(true),
            KeyCode::Char('k') => self.step(false),
            KeyCode::Char('g') | KeyCode::Home => self.selected = 0,
            KeyCode::Char('G') | KeyCode::End => self.selected = ITEMS.len() - 1,
            KeyCode::Enter => return DashAction::Run(ITEMS[self.selected].0),
            KeyCode::Char(c) => {
                if let Some((item, ..)) = ITEMS.iter().find(|(_, k, _)| *k == c) {
                    return self.select(*item);
                }
            }
            _ => {}
        }
        DashAction::None
    }
}

/// What the footer and icons need from the app.
pub struct DashInfo<'a> {
    pub version: &'a str,
    /// Kernelspecs found; `None` while discovery runs.
    pub kernels: Option<usize>,
    /// Startup time to the first frame.
    pub ready: Option<Duration>,
    pub icons: bool,
}

const MENU_W: u16 = 44;
const GAP: u16 = 2;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Tier {
    /// Logo, spaced menu, footer.
    Full,
    /// Spaced menu, footer.
    Spaced,
    /// One row per item, footer.
    Compact,
    /// One row per item, clipped around the selection.
    Tiny,
}

fn spaced_h() -> u16 {
    ITEMS.len() as u16 * 2 - 1
}

fn tier(area: Rect) -> Tier {
    let logo_h = LOGO.len() as u16;
    let full = logo_h + GAP + spaced_h() + GAP + 1;
    if area.height >= full && area.width as usize >= LOGO_W + 2 {
        Tier::Full
    } else if area.height > spaced_h() + GAP {
        Tier::Spaced
    } else if area.height >= ITEMS.len() as u16 + 2 {
        Tier::Compact
    } else {
        Tier::Tiny
    }
}

/// The colour for one logo cell.
fn logo_style(class: char, ch: char, ui: &Ui) -> Style {
    let bg = ui.background.or(ui.float_bg).unwrap_or(Color::Rgb(0, 0, 0));
    let faded = |pct| match mix(ui.focus, bg, pct) {
        Some(c) => Style::default().fg(c),
        None => Style::default().fg(ui.focus).add_modifier(Modifier::DIM),
    };
    match class {
        'p' => Style::default().fg(ui.warning),
        'B' => Style::default().fg(ui.focus),
        'A' => faded(35),
        'C' => faded(60),
        's' if ch == '✦' => Style::default().fg(ui.mode_visual),
        _ => Style::default().fg(ui.muted),
    }
}

fn draw_logo(buf: &mut Buffer, area: Rect, top: u16, theme: &Theme) {
    let x0 = area.x + (area.width - LOGO_W as u16) / 2;
    for (dy, (row, mask)) in LOGO.iter().zip(MASK.iter()).enumerate() {
        let y = top + dy as u16;
        for (dx, (ch, class)) in row.chars().zip(mask.chars()).enumerate() {
            if ch == ' ' {
                continue;
            }
            let mut tmp = [0u8; 4];
            buf.set_string(x0 + dx as u16, y, ch.encode_utf8(&mut tmp), logo_style(class, ch, &theme.ui));
        }
    }
}

fn footer_text(info: &DashInfo) -> String {
    let mut parts = vec![format!("saturn {}", info.version)];
    parts.push(match info.kernels {
        None => "finding kernels…".to_string(),
        Some(1) => "1 kernel".to_string(),
        Some(n) => format!("{n} kernels"),
    });
    if let Some(d) = info.ready {
        parts.push(format!("ready in {} ms", d.as_millis()));
    }
    parts.join(" · ")
}

pub fn render(buf: &mut Buffer, area: Rect, st: &DashboardState, info: &DashInfo, theme: &Theme) {
    if area.width < 4 || area.height == 0 {
        return;
    }
    let ui = &theme.ui;
    let t = tier(area);
    let spaced = matches!(t, Tier::Full | Tier::Spaced);
    let menu_h = if spaced { spaced_h() } else { ITEMS.len() as u16 };
    let logo_h = if t == Tier::Full { LOGO.len() as u16 + GAP } else { 0 };
    let footer_gap = if spaced { GAP } else { 1 };
    let footer_h = if t == Tier::Tiny { 0 } else { footer_gap + 1 };
    let total = logo_h + menu_h + footer_h;
    let mut y = area.y + area.height.saturating_sub(total) * 2 / 5;

    if t == Tier::Full {
        draw_logo(buf, area, y, theme);
        y += logo_h;
    }

    // Menu rows: which items, and on which line each sits.
    let rows: Vec<(usize, u16)> = if t == Tier::Tiny {
        let fit = area.height as usize;
        let first = st.selected.saturating_sub(fit.saturating_sub(1));
        (first..ITEMS.len()).take(fit).enumerate().map(|(k, i)| (i, area.y + k as u16)).collect()
    } else {
        let step = if spaced { 2 } else { 1 };
        (0..ITEMS.len()).map(|i| (i, y + (i as u16) * step)).collect()
    };
    let mw = MENU_W.min(area.width.saturating_sub(2)).max(8);
    let mx = area.x + (area.width - mw) / 2;
    for (i, ry) in rows {
        let (item, key, label) = ITEMS[i];
        let selected = i == st.selected;
        let row_bg = if selected { Style::default().bg(ui.selection) } else { Style::default() };
        if selected {
            buf.set_style(Rect::new(mx, ry, mw, 1), row_bg);
        }
        let mut tmp = [0u8; 4];
        let icon = dash_icon(item, info.icons).encode_utf8(&mut tmp).to_string();
        buf.set_string(mx + 1, ry, icon, row_bg.fg(ui.focus));
        let label_st = if selected {
            row_bg.fg(ui.focus).add_modifier(Modifier::BOLD)
        } else {
            row_bg.fg(ui.foreground)
        };
        let label_w = mw.saturating_sub(8) as usize;
        buf.set_string(mx + 4, ry, cut(label, label_w), label_st);
        buf.set_string(mx + mw - 2, ry, key.to_string(), row_bg.fg(ui.mode_visual));
    }

    if t != Tier::Tiny {
        let fy = y + menu_h + footer_gap;
        if fy < area.y + area.height {
            let text = footer_text(info);
            let max = area.width.saturating_sub(4) as usize;
            let text = cut(&text, max);
            let w = text.width() as u16 + 2;
            let fx = area.x + area.width.saturating_sub(w) / 2;
            buf.set_string(fx, fy, "◍", Style::default().fg(ui.warning));
            buf.set_string(fx + 2, fy, text, theme.muted());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn info(icons: bool) -> DashInfo<'static> {
        DashInfo {
            version: "0.1.0",
            kernels: Some(2),
            ready: Some(Duration::from_millis(38)),
            icons,
        }
    }

    fn draw(w: u16, h: u16, st: &DashboardState, icons: bool) -> (Buffer, Theme) {
        let theme = Theme::load("tokyo-night").unwrap();
        let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
        let a = buf.area;
        render(&mut buf, a, st, &info(icons), &theme);
        (buf, theme)
    }

    fn lines(buf: &Buffer) -> Vec<String> {
        (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }

    fn row_of(buf: &Buffer, needle: &str) -> Option<usize> {
        lines(buf).iter().position(|l| l.contains(needle))
    }

    fn has_braille(buf: &Buffer) -> bool {
        lines(buf).iter().any(|l| l.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c)))
    }

    #[test]
    fn logo_table_is_consistent() {
        assert_eq!(LOGO.len(), MASK.len());
        for (row, mask) in LOGO.iter().zip(MASK.iter()) {
            assert_eq!(row.chars().count(), LOGO_W, "{row}");
            assert_eq!(mask.chars().count(), LOGO_W, "{mask}");
            assert_eq!(row.width(), LOGO_W, "{row}");
            for (c, m) in row.chars().zip(mask.chars()) {
                assert!(" pABCs".contains(m), "class {m:?}");
                assert_eq!(c == ' ', m == ' ', "{c:?} vs {m:?}");
                assert_ne!(c, '\u{2800}');
            }
        }
    }

    #[test]
    fn keys_move_and_wrap() {
        let mut st = DashboardState::default();
        assert_eq!(st.handle_key(key(KeyCode::Char('k'))), DashAction::None);
        assert_eq!(st.selected, ITEMS.len() - 1);
        st.handle_key(key(KeyCode::Down));
        assert_eq!(st.selected, 0);
        st.handle_key(key(KeyCode::Char('j')));
        st.handle_key(ctrl('n'));
        assert_eq!(st.selected, 2);
        st.handle_key(ctrl('p'));
        assert_eq!(st.selected, 1);
        st.handle_key(key(KeyCode::Char('G')));
        assert_eq!(st.selected, 5);
        st.handle_key(key(KeyCode::Char('g')));
        assert_eq!(st.selected, 0);
        assert_eq!(st.handle_key(key(KeyCode::Enter)), DashAction::Run(DashItem::FindFiles));
    }

    #[test]
    fn letters_run_their_item() {
        let mut st = DashboardState::default();
        for (i, (item, k, _)) in ITEMS.iter().enumerate() {
            assert_eq!(st.handle_key(key(KeyCode::Char(*k))), DashAction::Run(*item));
            assert_eq!(st.selected, i);
        }
        assert_eq!(st.handle_key(ctrl('o')), DashAction::Run(DashItem::FindFiles));
        assert_eq!(st.handle_key(ctrl('q')), DashAction::Run(DashItem::Quit));
        assert_eq!(st.handle_key(ctrl('s')), DashAction::None);
        assert_eq!(st.handle_key(key(KeyCode::Char('x'))), DashAction::None);
        let alt_f = KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT);
        assert_eq!(st.handle_key(alt_f), DashAction::None);
    }

    #[test]
    fn full_layout_shows_logo_menu_and_footer() {
        let st = DashboardState::default();
        let (buf, theme) = draw(80, 40, &st, true);
        assert!(has_braille(&buf));
        let rows: Vec<usize> = ITEMS.iter().map(|(_, _, l)| row_of(&buf, l).unwrap()).collect();
        for w in rows.windows(2) {
            assert_eq!(w[1], w[0] + 2, "one blank row between items");
        }
        // Logo above the menu.
        let logo_row = lines(&buf).iter().position(|l| l.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c))).unwrap();
        assert!(logo_row < rows[0]);
        // Selected row background; key hints in the accent colour.
        let y = rows[0] as u16;
        let x = lines(&buf)[rows[0]].find("Find files").unwrap();
        let x = lines(&buf)[rows[0]][..x].chars().count() as u16;
        assert_eq!(buf[(x, y)].bg, theme.ui.selection);
        assert_eq!(buf[(x, y)].fg, theme.ui.focus);
        let line = &lines(&buf)[rows[1]];
        let kx = line.char_indices().filter(|(_, c)| *c == 'n').map(|(i, _)| line[..i].chars().count()).next_back().unwrap();
        assert_eq!(buf[(kx as u16, rows[1] as u16)].fg, theme.ui.mode_visual);
        let foot = row_of(&buf, "saturn 0.1.0").unwrap();
        assert!(foot > rows[5]);
        assert!(lines(&buf)[foot].contains("2 kernels · ready in 38 ms"));
    }

    #[test]
    fn logo_colours_follow_the_theme() {
        let st = DashboardState::default();
        let (buf, theme) = draw(80, 40, &st, true);
        let ui = &theme.ui;
        let top = row_of(&buf, "·").unwrap();
        let x0 = (80 - LOGO_W as u16) / 2;
        let mut seen = std::collections::HashSet::new();
        for (dy, (row, mask)) in LOGO.iter().zip(MASK.iter()).enumerate() {
            for (dx, (c, m)) in row.chars().zip(mask.chars()).enumerate() {
                if c == ' ' {
                    continue;
                }
                let cell = &buf[(x0 + dx as u16, top as u16 + dy as u16)];
                assert_eq!(cell.symbol(), c.to_string());
                match m {
                    'p' => assert_eq!(cell.fg, ui.warning),
                    'B' => assert_eq!(cell.fg, ui.focus),
                    'A' | 'C' => assert_ne!(cell.fg, ui.focus),
                    's' if c == '✦' => assert_eq!(cell.fg, ui.mode_visual),
                    _ => assert_eq!(cell.fg, ui.muted),
                }
                seen.insert(m);
            }
        }
        assert_eq!(seen.len(), 5, "{seen:?}");
    }

    #[test]
    fn smaller_tiers() {
        let st = DashboardState::default();
        let (buf, _) = draw(80, 20, &st, true);
        assert!(!has_braille(&buf));
        let a = row_of(&buf, "Find files").unwrap();
        assert_eq!(row_of(&buf, "New notebook").unwrap(), a + 2);
        assert!(row_of(&buf, "saturn 0.1.0").is_some());

        let (buf, _) = draw(80, 9, &st, true);
        let a = row_of(&buf, "Find files").unwrap();
        assert_eq!(row_of(&buf, "Quit").unwrap(), a + 5);
        assert!(row_of(&buf, "saturn 0.1.0").is_some());

        let st = DashboardState { selected: 5 };
        let (buf, _) = draw(80, 4, &st, true);
        assert!(row_of(&buf, "Quit").is_some());
        assert!(row_of(&buf, "saturn").is_none());

        // Too narrow for the logo, any height.
        let (buf, _) = draw(30, 40, &DashboardState::default(), true);
        assert!(!has_braille(&buf));
        assert!(row_of(&buf, "Recent notebooks").is_some());

        let mut buf = Buffer::empty(Rect::new(0, 0, 0, 0));
        let a = buf.area;
        render(&mut buf, a, &st, &info(true), &Theme::load("tokyo-night").unwrap());
        let mut buf = Buffer::empty(Rect::new(0, 0, 9, 1));
        let a = buf.area;
        render(&mut buf, a, &st, &info(true), &Theme::load("tokyo-night").unwrap());
    }

    #[test]
    fn icons_follow_the_setting() {
        let st = DashboardState::default();
        let (buf, _) = draw(80, 40, &st, true);
        let row = &lines(&buf)[row_of(&buf, "Find files").unwrap()];
        assert!(row.contains('\u{f002}'));
        let (buf, _) = draw(80, 40, &st, false);
        let row = &lines(&buf)[row_of(&buf, "Find files").unwrap()];
        assert!(row.contains('»'));
    }

    #[test]
    fn footer_while_kernels_load() {
        let i = DashInfo { version: "1.2.3", kernels: None, ready: None, icons: false };
        assert_eq!(footer_text(&i), "saturn 1.2.3 · finding kernels…");
        let i = DashInfo { kernels: Some(1), ..i };
        assert_eq!(footer_text(&i), "saturn 1.2.3 · 1 kernel");
    }
}
