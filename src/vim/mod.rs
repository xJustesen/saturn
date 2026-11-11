//! A vim engine for cell editing: the composable grammar
//! `[count]["reg]operator[count](motion | text object)`, visual modes,
//! registers, marks, search, dot-repeat, macros and undo.
//!
//! The engine owns the text and the cursor and knows nothing about drawing;
//! `editor.rs` mirrors its state into the edtui widget that renders it.
//! Defaults follow LazyVim where they differ from plain vim (system
//! clipboard as the unnamed register, `gc` comments, auto-pairs, mini.ai
//! text objects, `<`/`>` keeping the selection, `Alt+j/k` moving lines).

pub mod motion;
pub mod textobj;

#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use regex::{Regex, RegexBuilder};

use motion as mv;

pub type Text = Vec<Vec<char>>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub row: usize,
    pub col: usize,
}

fn pos(row: usize, col: usize) -> Pos {
    Pos { row, col }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RKind {
    Char,
    Line,
    Block,
}

/// A stretch of text. `Char`: from `a` up to but excluding `b`. `Line`: rows
/// `a.row..=b.row`. `Block`: those rows, columns `a.col..=b.col`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub a: Pos,
    pub b: Pos,
    pub kind: RKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum K {
    Char(char),
    Ctrl(char),
    Alt(char),
    Esc,
    Enter,
    Backspace,
    Delete,
    Tab,
    BackTab,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
}

impl K {
    pub fn from_event(ev: &KeyEvent) -> Option<K> {
        if ev.kind == KeyEventKind::Release {
            return None;
        }
        let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
        let alt = ev.modifiers.contains(KeyModifiers::ALT);
        Some(match ev.code {
            KeyCode::Char('[') if ctrl => K::Esc,
            KeyCode::Char(c) if ctrl => K::Ctrl(c.to_ascii_lowercase()),
            KeyCode::Char(c) if alt => K::Alt(c),
            KeyCode::Char(c) if ev.modifiers.contains(KeyModifiers::SHIFT) && c.is_ascii_lowercase() => {
                K::Char(c.to_ascii_uppercase())
            }
            KeyCode::Char(c) => K::Char(c),
            KeyCode::Esc => K::Esc,
            KeyCode::Enter => K::Enter,
            KeyCode::Backspace if alt => K::Ctrl('w'),
            KeyCode::Backspace => K::Backspace,
            KeyCode::Delete => K::Delete,
            KeyCode::Tab => K::Tab,
            KeyCode::BackTab => K::BackTab,
            KeyCode::Left => K::Left,
            KeyCode::Right => K::Right,
            KeyCode::Up => K::Up,
            KeyCode::Down => K::Down,
            KeyCode::Home => K::Home,
            KeyCode::End => K::End,
            KeyCode::PageUp => K::PageUp,
            KeyCode::PageDown => K::PageDown,
            _ => return None,
        })
    }

    /// Parse vim key notation: `d2w<Esc><C-r><CR><lt>`.
    pub fn parse(s: &str) -> Vec<K> {
        let mut out = Vec::new();
        let mut rest = s;
        while let Some(c) = rest.chars().next() {
            if c == '<'
                && let Some(end) = rest.find('>')
                && let Some(k) = Self::named(&rest[1..end])
            {
                out.push(k);
                rest = &rest[end + 1..];
                continue;
            }
            out.push(K::Char(c));
            rest = &rest[c.len_utf8()..];
        }
        out
    }

    fn named(name: &str) -> Option<K> {
        Some(match name {
            "Esc" => K::Esc,
            "CR" => K::Enter,
            "BS" => K::Backspace,
            "Del" => K::Delete,
            "Tab" => K::Tab,
            "S-Tab" => K::BackTab,
            "Left" => K::Left,
            "Right" => K::Right,
            "Up" => K::Up,
            "Down" => K::Down,
            "Home" => K::Home,
            "End" => K::End,
            "PageUp" => K::PageUp,
            "PageDown" => K::PageDown,
            "lt" => K::Char('<'),
            "Space" => K::Char(' '),
            _ => {
                let mut cs = name.chars();
                match (cs.next()?, cs.next()?, cs.next()?, cs.next()) {
                    ('C', '-', c, None) => K::Ctrl(c),
                    ('A', '-', c, None) => K::Alt(c),
                    _ => return None,
                }
            }
        })
    }

    fn label(self) -> String {
        match self {
            K::Char(c) => c.to_string(),
            K::Ctrl(c) => format!("^{}", c.to_ascii_uppercase()),
            _ => String::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert,
    Replace,
    Visual,
    VisualLine,
    VisualBlock,
}

impl Mode {
    pub fn is_visual(self) -> bool {
        matches!(self, Mode::Visual | Mode::VisualLine | Mode::VisualBlock)
    }
    pub fn is_insert(self) -> bool {
        matches!(self, Mode::Insert | Mode::Replace)
    }
}

/// Something the editor asks the notebook to do (`:w`, `:q`, `ZZ`, errors).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Save,
    Leave,
    Message(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Delete,
    Change,
    Yank,
    Indent,
    Dedent,
    Lower,
    Upper,
    Tilde,
    Comment,
}

impl Op {
    /// The key that, typed again, applies the operator to whole lines.
    fn double(self) -> char {
        match self {
            Op::Delete => 'd',
            Op::Change | Op::Comment => 'c',
            Op::Yank => 'y',
            Op::Indent => '>',
            Op::Dedent => '<',
            Op::Lower => 'u',
            Op::Upper => 'U',
            Op::Tilde => '~',
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Wait {
    #[default]
    None,
    Reg,
    G,
    Z,
    ZZ,
    Find {
        fwd: bool,
        till: bool,
    },
    Replace,
    Obj {
        around: bool,
    },
    Mark,
    Jump {
        exact: bool,
    },
    Bracket {
        fwd: bool,
    },
    MacroRec,
    MacroPlay,
}

#[derive(Default)]
struct Pending {
    count: Option<usize>,
    op_count: Option<usize>,
    reg: Option<char>,
    op: Option<Op>,
    wait: Wait,
    keys: String,
}

impl Pending {
    fn is_empty(&self) -> bool {
        self.count.is_none() && self.op_count.is_none() && self.reg.is_none() && self.op.is_none() && self.wait == Wait::None
    }
    fn clear(&mut self) {
        *self = Pending::default();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MKind {
    Excl,
    Incl,
    Line,
}

struct Motion {
    to: Pos,
    kind: MKind,
    /// Vertical motion: keep the remembered column.
    sticky: bool,
    /// Long jump: remember where we came from for `''`.
    jump: bool,
}

enum MRes {
    No,
    Fail,
    Ok(Motion),
}

fn excl(to: Pos) -> MRes {
    MRes::Ok(Motion { to, kind: MKind::Excl, sticky: false, jump: false })
}
fn incl(to: Pos) -> MRes {
    MRes::Ok(Motion { to, kind: MKind::Incl, sticky: false, jump: false })
}
fn line(to: Pos) -> MRes {
    MRes::Ok(Motion { to, kind: MKind::Line, sticky: false, jump: false })
}
fn jump(m: MRes) -> MRes {
    match m {
        MRes::Ok(mut m) => {
            m.jump = true;
            MRes::Ok(m)
        }
        other => other,
    }
}

struct Snap {
    text: Text,
    cur: Pos,
}

struct Cmdline {
    kind: char,
    text: String,
}

struct BlockIns {
    row: usize,
    rows: std::ops::RangeInclusive<usize>,
    col: usize,
    base_len: usize,
    pad: bool,
}

#[derive(Default)]
struct InsertSession {
    count: usize,
    keys: Vec<K>,
    block: Option<BlockIns>,
    reg_wait: bool,
}

// ----- state shared by every cell ------------------------------------------

pub trait Clip {
    fn get(&mut self) -> Option<String>;
    fn set(&mut self, text: &str);
}

struct Search {
    pat: String,
    re: Regex,
}

#[derive(Default)]
struct Shared {
    regs: HashMap<char, (String, RKind)>,
    clip: Option<Box<dyn Clip>>,
    /// What we last put on the system clipboard, to recover its kind.
    clip_sent: Option<(String, String, RKind)>,
    search: Option<Search>,
    hl: bool,
    find: Option<(char, bool, bool)>,
    dot: Option<Vec<K>>,
    macros: HashMap<char, Vec<K>>,
    last_macro: Option<char>,
}

thread_local! {
    static SHARED: RefCell<Shared> = RefCell::new(Shared::default());
}

fn sh<R>(f: impl FnOnce(&mut Shared) -> R) -> R {
    SHARED.with(|s| f(&mut s.borrow_mut()))
}

/// Use the system clipboard as the unnamed register (`clipboard=unnamedplus`).
pub fn set_clipboard(clip: Box<dyn Clip>) {
    sh(|s| s.clip = Some(clip));
}

/// Stop highlighting search matches (`:noh`).
pub fn clear_search_highlight() {
    sh(|s| s.hl = false);
}

impl Shared {
    fn unnamed(&mut self) -> Option<(String, RKind)> {
        if let Some(t) = self.clip.as_mut().and_then(|c| c.get()) {
            if let Some((sent, text, kind)) = &self.clip_sent
                && *sent == t
            {
                return Some((text.clone(), *kind));
            }
            return Some(match t.strip_suffix('\n') {
                Some(body) => (body.to_string(), RKind::Line),
                None => (t, RKind::Char),
            });
        }
        self.regs.get(&'"').cloned()
    }

    fn get(&mut self, name: Option<char>) -> Option<(String, RKind)> {
        match name {
            None | Some('"' | '+' | '*') => self.unnamed(),
            Some(c) => self.regs.get(&c.to_ascii_lowercase()).cloned(),
        }
    }

    fn set(&mut self, name: Option<char>, text: String, kind: RKind, delete: bool) {
        match name {
            Some('_') => return,
            Some(c) if c.is_ascii_uppercase() => {
                let c = c.to_ascii_lowercase();
                let merged = match self.regs.remove(&c) {
                    Some((old, ok)) if ok == RKind::Line || kind == RKind::Line => (format!("{old}\n{text}"), RKind::Line),
                    Some((old, _)) => (old + &text, kind),
                    None => (text, kind),
                };
                self.regs.insert(c, merged.clone());
                self.regs.insert('"', merged);
                return;
            }
            Some(c) if !matches!(c, '"' | '+' | '*') => {
                self.regs.insert(c, (text.clone(), kind));
                self.regs.insert('"', (text, kind));
                return;
            }
            _ => {}
        }
        if !delete {
            self.regs.insert('0', (text.clone(), kind));
        } else if kind == RKind::Line || text.contains('\n') {
            for n in (1..9u8).rev() {
                if let Some(v) = self.regs.remove(&((b'0' + n) as char)) {
                    self.regs.insert((b'0' + n + 1) as char, v);
                }
            }
            self.regs.insert('1', (text.clone(), kind));
        } else {
            self.regs.insert('-', (text.clone(), kind));
        }
        self.regs.insert('"', (text.clone(), kind));
        if let Some(clip) = self.clip.as_mut() {
            let sent = if kind == RKind::Line { format!("{text}\n") } else { text.clone() };
            clip.set(&sent);
            self.clip_sent = Some((sent, text, kind));
        }
    }
}

fn compile(pat: &str) -> Option<Regex> {
    // Smartcase, and vim's word boundaries.
    let ic = !pat.chars().any(char::is_uppercase);
    let p = pat.replace("\\<", "\\b").replace("\\>", "\\b");
    RegexBuilder::new(&p)
        .case_insensitive(ic)
        .build()
        .or_else(|_| RegexBuilder::new(&regex::escape(pat)).case_insensitive(ic).build())
        .ok()
}

fn set_search(pat: &str) {
    if let Some(re) = compile(pat) {
        sh(|s| {
            s.search = Some(Search { pat: pat.to_string(), re });
            s.hl = true;
        });
    }
}

// ----- the editor -----------------------------------------------------------

pub struct Vim {
    pub text: Text,
    pub cur: Pos,
    pub mode: Mode,
    /// Columns per indent level (`shiftwidth`).
    pub shiftwidth: usize,
    /// Line comment leader for `gc`.
    pub comment: String,
    pub autopairs: bool,
    /// Rows moved by `Ctrl+D` / `Ctrl+U`.
    pub page: usize,
    want_col: Option<usize>,
    anchor: Pos,
    pend: Pending,
    cmd: Option<Cmdline>,
    undo: Vec<Snap>,
    redo: Vec<Snap>,
    snap: Option<Snap>,
    /// The running command is undo/redo itself: don't record it as a change.
    hist_cmd: bool,
    marks: HashMap<char, Pos>,
    last_visual: Option<(Mode, Pos, Pos)>,
    rec: Vec<K>,
    replaying: bool,
    depth: usize,
    ins: InsertSession,
    macro_rec: Option<(char, Vec<K>)>,
    requests: Vec<Request>,
}

fn ordered(a: Pos, b: Pos) -> (Pos, Pos) {
    if a <= b { (a, b) } else { (b, a) }
}

fn is_ws(c: &char) -> bool {
    c.is_whitespace()
}

impl Vim {
    pub fn new(src: &str) -> Self {
        let text: Text = src.split('\n').map(|l| l.chars().collect()).collect();
        Vim {
            text,
            cur: Pos::default(),
            mode: Mode::Normal,
            shiftwidth: 4,
            comment: "#".into(),
            autopairs: true,
            page: 10,
            want_col: None,
            anchor: Pos::default(),
            pend: Pending::default(),
            cmd: None,
            undo: Vec::new(),
            redo: Vec::new(),
            snap: None,
            hist_cmd: false,
            marks: HashMap::new(),
            last_visual: None,
            rec: Vec::new(),
            replaying: false,
            depth: 0,
            ins: InsertSession::default(),
            macro_rec: None,
            requests: Vec::new(),
        }
    }

    pub fn text(&self) -> String {
        let mut s = String::new();
        for (i, l) in self.text.iter().enumerate() {
            if i > 0 {
                s.push('\n');
            }
            s.extend(l.iter());
        }
        s
    }

    /// Normal mode with nothing half-typed: `Esc` now leaves the cell.
    pub fn at_rest(&self) -> bool {
        self.is_idle()
    }

    /// Keys of the command being typed (`2d`, `"ay`, `ci`).
    pub fn pending(&self) -> &str {
        &self.pend.keys
    }

    /// The `/`, `?` or `:` prompt, including its leading character.
    pub fn cmdline(&self) -> Option<String> {
        self.cmd.as_ref().map(|c| format!("{}{}", c.kind, c.text))
    }

    pub fn recording(&self) -> Option<char> {
        self.macro_rec.as_ref().map(|(c, _)| *c)
    }

    pub fn take_requests(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.requests)
    }

    /// The visual selection, if any.
    pub fn selection(&self) -> Option<Region> {
        self.mode.is_visual().then(|| self.visual_region())
    }

    /// Search matches to highlight: start and length in characters.
    pub fn search_matches(&self) -> Vec<(Pos, usize)> {
        if !sh(|s| s.hl) {
            return Vec::new();
        }
        self.all_matches()
    }

    pub fn key(&mut self, k: K) {
        if let Some((_, keys)) = &mut self.macro_rec {
            keys.push(k);
        }
        self.feed(k);
    }

    /// Bracketed paste: literal text, no auto-pairs or auto-indent.
    pub fn paste(&mut self, s: &str) {
        let s = s.replace("\r\n", "\n").replace('\r', "\n");
        if let Some(cmd) = &mut self.cmd {
            cmd.text.push_str(s.lines().next().unwrap_or(""));
            return;
        }
        if self.mode.is_insert() {
            self.cur = self.insert_at(self.cur, &s);
            return;
        }
        if self.mode.is_visual() {
            self.exit_visual();
        }
        self.pend.clear();
        self.snap = Some(Snap { text: self.text.clone(), cur: self.cur });
        self.cur = self.insert_at(self.cur, &s);
        self.clamp();
        self.hist_cmd = false;
        self.rec.clear();
        self.replaying = true;
        self.finish();
        self.replaying = false;
    }

    /// Enter insert mode from outside (the notebook's `i`).
    pub fn enter_insert(&mut self) {
        self.enter_normal();
        self.feed(K::Char('i'));
    }

    /// Drop whatever is in progress and return to normal mode.
    pub fn enter_normal(&mut self) {
        self.cmd = None;
        self.pend.clear();
        if self.mode.is_insert() {
            self.leave_insert();
        } else if self.mode.is_visual() {
            self.exit_visual();
        }
        self.clamp();
        self.finish();
    }

    fn is_idle(&self) -> bool {
        self.mode == Mode::Normal && self.pend.is_empty() && self.cmd.is_none()
    }

    fn feed(&mut self, k: K) {
        if self.is_idle() {
            self.snap = Some(Snap { text: self.text.clone(), cur: self.cur });
            self.hist_cmd = false;
            if !self.replaying {
                self.rec.clear();
            }
        }
        if !self.replaying {
            self.rec.push(k);
        }
        if self.cmd.is_some() {
            self.cmdline_key(k);
        } else {
            match self.mode {
                Mode::Insert | Mode::Replace => {
                    if k != K::Esc {
                        self.ins.keys.push(k);
                    }
                    self.insert_key(k);
                }
                _ => self.command(k),
            }
        }
        self.clamp();
        if self.is_idle() {
            self.finish();
        }
    }

    /// A command ended in normal mode: record it for undo and `.`.
    fn finish(&mut self) {
        if let Some(s) = self.snap.take()
            && !self.hist_cmd
            && s.text != self.text
        {
            self.undo.push(s);
            self.redo.clear();
            if !self.replaying {
                let keys = std::mem::take(&mut self.rec);
                sh(|s| s.dot = Some(keys));
            }
        }
    }

    fn replay(&mut self, keys: &[K]) {
        if self.depth > 50 {
            return;
        }
        self.depth += 1;
        let was = std::mem::replace(&mut self.replaying, true);
        for k in keys {
            self.feed(*k);
        }
        self.replaying = was;
        self.depth -= 1;
    }

    fn len(&self, row: usize) -> usize {
        self.text[row].len()
    }

    fn last_row(&self) -> usize {
        self.text.len() - 1
    }

    fn clamp(&mut self) {
        if self.text.is_empty() {
            self.text.push(Vec::new());
        }
        self.cur.row = self.cur.row.min(self.last_row());
        let len = self.len(self.cur.row);
        let max = if self.mode.is_insert() { len } else { len.saturating_sub(1) };
        self.cur.col = self.cur.col.min(max);
    }

    fn msg(&mut self, m: impl Into<String>) {
        self.requests.push(Request::Message(m.into()));
    }

    fn count(&self) -> usize {
        self.pend.count.unwrap_or(1).saturating_mul(self.pend.op_count.unwrap_or(1)).min(100_000)
    }

    fn has_count(&self) -> bool {
        self.pend.count.is_some() || self.pend.op_count.is_some()
    }

    // ----- normal / visual / operator-pending keys ---------------------------

    fn command(&mut self, k: K) {
        let had = !self.pend.is_empty();
        self.pend.keys.push_str(&k.label());
        let wait = std::mem::take(&mut self.pend.wait);
        if wait != Wait::None {
            return self.after_wait(wait, k);
        }
        let visual = self.mode.is_visual();
        let op = self.pend.op;
        if let K::Char(c) = k
            && let Some(d) = c.to_digit(10)
        {
            let slot = if op.is_some() { &mut self.pend.op_count } else { &mut self.pend.count };
            if d != 0 || slot.is_some() {
                *slot = Some(slot.unwrap_or(0).saturating_mul(10).saturating_add(d as usize).min(100_000));
                return;
            }
        }
        match k {
            K::Esc => {
                if !had && visual {
                    self.exit_visual();
                }
                self.pend.clear();
            }
            K::Char('"') => self.pend.wait = Wait::Reg,
            K::Char('g') => self.pend.wait = Wait::G,
            K::Char('z') => self.pend.wait = Wait::Z,
            K::Char(c @ ('f' | 'F' | 't' | 'T')) => {
                self.pend.wait = Wait::Find { fwd: c.is_ascii_lowercase(), till: matches!(c, 't' | 'T') }
            }
            K::Char('\'') => self.pend.wait = Wait::Jump { exact: false },
            K::Char('`') => self.pend.wait = Wait::Jump { exact: true },
            K::Char('[') => self.pend.wait = Wait::Bracket { fwd: false },
            K::Char(']') => self.pend.wait = Wait::Bracket { fwd: true },
            K::Char(c @ ('i' | 'a')) if op.is_some() || visual => self.pend.wait = Wait::Obj { around: c == 'a' },
            K::Char(c @ ('/' | '?')) => self.cmd = Some(Cmdline { kind: c, text: String::new() }),
            K::Char(':') if op.is_none() => {
                let text = if visual {
                    self.exit_visual();
                    "'<,'>".to_string()
                } else {
                    String::new()
                };
                self.cmd = Some(Cmdline { kind: ':', text });
            }
            K::Char(c) if op.is_some_and(|o| o.double() == c) => {
                let last = (self.cur.row + self.count() - 1).min(self.last_row());
                let r = Region { a: pos(self.cur.row, 0), b: pos(last, 0), kind: RKind::Line };
                self.apply_op(op.unwrap(), r);
            }
            K::Char(c @ ('o' | 'O')) if op == Some(Op::Comment) => {
                self.pend.clear();
                self.open_line(c == 'o');
                let lead: String = format!("{} ", self.comment);
                self.cur = self.insert_at(self.cur, &lead);
            }
            _ => match self.motion(k) {
                MRes::Ok(m) => self.do_motion(m),
                MRes::Fail => self.pend.clear(),
                MRes::No => {
                    let new_op = match k {
                        K::Char('d') => Some(Op::Delete),
                        K::Char('c') => Some(Op::Change),
                        K::Char('y') => Some(Op::Yank),
                        K::Char('>') => Some(Op::Indent),
                        K::Char('<') => Some(Op::Dedent),
                        _ => None,
                    };
                    if visual {
                        self.visual_cmd(k);
                    } else if op.is_some() {
                        self.pend.clear();
                    } else if let Some(o) = new_op {
                        self.pend.op = Some(o);
                    } else {
                        self.normal_cmd(k);
                    }
                }
            },
        }
    }

    fn after_wait(&mut self, w: Wait, k: K) {
        let K::Char(c) = k else {
            self.pend.clear();
            return;
        };
        match w {
            Wait::None => {}
            Wait::Reg => self.pend.reg = Some(c),
            Wait::G => self.g_cmd(c),
            Wait::Z => self.pend.clear(),
            Wait::ZZ => {
                if c == 'Z' {
                    self.requests.push(Request::Save);
                }
                if matches!(c, 'Z' | 'Q') {
                    self.requests.push(Request::Leave);
                }
                self.pend.clear();
            }
            Wait::Find { fwd, till } => {
                sh(|s| s.find = Some((c, fwd, till)));
                match self.find(c, fwd, till, false) {
                    Some(m) => self.do_motion(m),
                    None => self.pend.clear(),
                }
            }
            Wait::Replace => self.replace_chars(c),
            Wait::Obj { around } => self.text_object(c, around),
            Wait::Mark => {
                self.marks.insert(c, self.cur);
                self.pend.clear();
            }
            Wait::Jump { exact } => {
                let key = if c == '`' { '\'' } else { c };
                match self.marks.get(&key).copied() {
                    Some(p) => {
                        let row = p.row.min(self.last_row());
                        let m = if exact {
                            Motion { to: pos(row, p.col), kind: MKind::Excl, sticky: false, jump: true }
                        } else {
                            Motion { to: pos(row, mv::first_nonblank(&self.text, row)), kind: MKind::Line, sticky: false, jump: true }
                        };
                        self.do_motion(m);
                    }
                    None => {
                        self.msg("E20: Mark not set");
                        self.pend.clear();
                    }
                }
            }
            Wait::Bracket { fwd } => {
                let pair = match c {
                    '(' | ')' => Some(('(', ')')),
                    '{' | '}' => Some(('{', '}')),
                    _ => None,
                };
                let to = pair.and_then(|(o, cl)| {
                    if fwd { mv::unmatched_close(&self.text, self.cur, o, cl) } else { mv::unmatched_open(&self.text, self.cur, o, cl) }
                });
                match to {
                    Some(p) => self.do_motion(Motion { to: p, kind: MKind::Excl, sticky: false, jump: true }),
                    None => self.pend.clear(),
                }
            }
            Wait::MacroRec => {
                self.pend.clear();
                if c.is_ascii_alphanumeric() {
                    self.macro_rec = Some((c.to_ascii_lowercase(), Vec::new()));
                }
            }
            Wait::MacroPlay => {
                let n = self.count();
                self.pend.clear();
                let name = if c == '@' { sh(|s| s.last_macro) } else { Some(c.to_ascii_lowercase()) };
                if let Some(name) = name
                    && let Some(keys) = sh(|s| s.macros.get(&name).cloned())
                {
                    sh(|s| s.last_macro = Some(name));
                    for _ in 0..n {
                        self.replay(&keys);
                    }
                }
            }
        }
    }

    fn g_cmd(&mut self, c: char) {
        let n = self.count();
        let cur = self.cur;
        let m = match c {
            'g' => {
                let row = if self.has_count() { (n - 1).min(self.last_row()) } else { 0 };
                jump(line(pos(row, mv::first_nonblank(&self.text, row))))
            }
            'e' | 'E' => {
                let mut p = cur;
                for _ in 0..n {
                    p = mv::word_end_back(&self.text, p, c == 'E');
                }
                incl(p)
            }
            '_' => {
                let row = (cur.row + n - 1).min(self.last_row());
                let l = &self.text[row];
                incl(pos(row, l.iter().rposition(|c| !c.is_whitespace()).unwrap_or(0)))
            }
            'j' => self.motion(K::Char('j')),
            'k' => self.motion(K::Char('k')),
            '0' => self.motion(K::Home),
            '^' => self.motion(K::Char('^')),
            '$' => self.motion(K::Char('$')),
            'u' => return self.operator(Op::Lower),
            'U' => return self.operator(Op::Upper),
            '~' => return self.operator(Op::Tilde),
            'c' => return self.operator(Op::Comment),
            'v' => {
                self.pend.clear();
                if let Some((mode, a, b)) = self.last_visual {
                    self.mode = mode;
                    self.anchor = pos(a.row.min(self.last_row()), a.col);
                    self.cur = b;
                }
                return;
            }
            'i' => {
                self.pend.clear();
                if let Some(p) = self.marks.get(&'^').copied() {
                    self.cur = p;
                }
                return self.begin_insert(1);
            }
            'J' => {
                self.pend.clear();
                if self.mode.is_visual() {
                    let r = self.visual_region();
                    self.exit_visual();
                    return self.join(r.a.row, (r.b.row - r.a.row).max(1), false);
                }
                return self.join(cur.row, n.max(2) - 1, false);
            }
            'p' | 'P' if !self.mode.is_visual() => return self.put(c == 'p', n, true),
            _ => MRes::Fail,
        };
        match m {
            MRes::Ok(m) => self.do_motion(m),
            _ => self.pend.clear(),
        }
    }

    /// An operator key that isn't doubled: wait for a motion, or act on the
    /// selection in visual mode.
    fn operator(&mut self, op: Op) {
        if self.mode.is_visual() {
            let r = self.visual_region();
            self.exit_visual();
            self.apply_op(op, r);
        } else if self.pend.op.is_some() {
            self.pend.clear();
        } else {
            self.pend.op = Some(op);
        }
    }

    fn vertical(&mut self, down: bool, n: usize) -> MRes {
        let cur = self.cur;
        let row = if down { (cur.row + n).min(self.last_row()) } else { cur.row.saturating_sub(n) };
        if row == cur.row && self.pend.op.is_some() {
            return MRes::Fail;
        }
        let col = self.want_col.unwrap_or(cur.col);
        self.want_col = Some(col);
        let to = pos(row, col.min(self.len(row).saturating_sub(1)));
        MRes::Ok(Motion { to, kind: MKind::Line, sticky: true, jump: false })
    }

    fn find(&self, ch: char, fwd: bool, till: bool, repeat: bool) -> Option<Motion> {
        let col = mv::find_char(&self.text[self.cur.row], self.cur.col, ch, fwd, till, self.count(), repeat)?;
        let kind = if fwd { MKind::Incl } else { MKind::Excl };
        Some(Motion { to: pos(self.cur.row, col), kind, sticky: false, jump: false })
    }

    fn motion(&mut self, k: K) -> MRes {
        let n = self.count();
        let has = self.has_count();
        let cur = self.cur;
        let op = self.pend.op;
        let last = self.last_row();
        let len = self.len(cur.row);
        let t = &self.text;
        let fnb = |row: usize| pos(row, mv::first_nonblank(t, row));
        match k {
            K::Char('h') | K::Left | K::Backspace => excl(pos(cur.row, cur.col.saturating_sub(n))),
            K::Char('l' | ' ') | K::Right => {
                let max = if op.is_some() { len } else { len.saturating_sub(1) };
                excl(pos(cur.row, (cur.col + n).min(max)))
            }
            K::Char('j') | K::Down | K::Ctrl('n' | 'j') => self.vertical(true, n),
            K::Char('k') | K::Up => self.vertical(false, n),
            K::Char('+') | K::Enter => line(fnb((cur.row + n).min(last))),
            K::Char('-') => line(fnb(cur.row.saturating_sub(n))),
            K::Char('_') => line(fnb((cur.row + n - 1).min(last))),
            K::Char('0') | K::Home => excl(pos(cur.row, 0)),
            K::Char('^') => excl(fnb(cur.row)),
            K::Char('|') => excl(pos(cur.row, (n - 1).min(len.saturating_sub(1)))),
            K::Char('$') | K::End => {
                let row = (cur.row + n - 1).min(last);
                let to = pos(row, self.len(row).saturating_sub(1));
                self.want_col = Some(usize::MAX);
                MRes::Ok(Motion { to, kind: MKind::Incl, sticky: true, jump: false })
            }
            K::Char(c @ ('w' | 'W')) => {
                let big = c == 'W';
                let mut p = cur;
                let on_word = t[cur.row].get(cur.col).is_some_and(|c| !c.is_whitespace());
                if op == Some(Op::Change) && on_word {
                    // `cw` changes to the end of the word, like `ce`.
                    p = mv::run_end(t, p, big);
                    for _ in 1..n {
                        p = mv::word_end(t, p, big);
                    }
                    return incl(p);
                }
                for _ in 0..n {
                    p = mv::word_fwd(t, p, big);
                }
                if op.is_some() && p.row > cur.row && p.col <= mv::indent(t, p.row) {
                    // An operator stops at the end of the last word's line.
                    p = pos(p.row - 1, self.len(p.row - 1));
                    if p < cur {
                        p = pos(cur.row, len);
                    }
                }
                excl(p)
            }
            K::Char(c @ ('b' | 'B')) => {
                let mut p = cur;
                for _ in 0..n {
                    p = mv::word_back(t, p, c == 'B');
                }
                excl(p)
            }
            K::Char(c @ ('e' | 'E')) => {
                let mut p = cur;
                for _ in 0..n {
                    p = mv::word_end(t, p, c == 'E');
                }
                incl(p)
            }
            K::Char('G') => {
                let row = if has { (n - 1).min(last) } else { last };
                jump(line(fnb(row)))
            }
            K::Char('}') => {
                let mut p = cur;
                for _ in 0..n {
                    p = mv::para_fwd(t, p.row);
                }
                jump(excl(p))
            }
            K::Char('{') => {
                let mut p = cur;
                for _ in 0..n {
                    p = mv::para_back(t, p.row);
                }
                jump(excl(p))
            }
            K::Char('%') => match mv::match_pair(t, cur) {
                Some(p) => jump(incl(p)),
                None => MRes::Fail,
            },
            K::Char(c @ (';' | ',')) => {
                let Some((ch, fwd, till)) = sh(|s| s.find) else { return MRes::Fail };
                let fwd = if c == ';' { fwd } else { !fwd };
                self.find(ch, fwd, till, true).map_or(MRes::Fail, MRes::Ok)
            }
            K::Char('H') => line(fnb((n - 1).min(last))),
            K::Char('M') => line(fnb(last / 2)),
            K::Char('L') => line(fnb(last.saturating_sub(n - 1))),
            K::Char(c @ ('n' | 'N')) => {
                // LazyVim: `n` always searches forward, `N` backward.
                match self.search_from(cur, c == 'n', n) {
                    Some(p) => jump(excl(p)),
                    None => MRes::Fail,
                }
            }
            K::Char(c @ ('*' | '#')) if !self.mode.is_visual() => {
                let line = &t[cur.row];
                let is_kw = |c: &char| c.is_alphanumeric() || *c == '_';
                let Some(at) = (cur.col..line.len()).find(|&i| is_kw(&line[i])) else { return MRes::Fail };
                let mut s = at;
                while s > 0 && is_kw(&line[s - 1]) {
                    s -= 1;
                }
                let word: String = line[s..].iter().take_while(|c| is_kw(c)).collect();
                set_search(&format!("\\<{}\\>", regex::escape(&word)));
                match self.search_from(pos(cur.row, s), c == '*', n) {
                    Some(p) => jump(excl(p)),
                    None => MRes::Fail,
                }
            }
            K::Ctrl('d') | K::PageDown | K::Ctrl('f') if op.is_none() => {
                let rows = if k == K::Ctrl('d') { self.page } else { self.page * 2 };
                self.vertical(true, rows.max(1))
            }
            K::Ctrl('u') | K::PageUp | K::Ctrl('b') if op.is_none() => {
                let rows = if k == K::Ctrl('u') { self.page } else { self.page * 2 };
                self.vertical(false, rows.max(1))
            }
            _ => MRes::No,
        }
    }

    fn do_motion(&mut self, m: Motion) {
        if let Some(op) = self.pend.op {
            let r = self.motion_region(&m);
            self.apply_op(op, r);
            return;
        }
        if m.jump {
            self.marks.insert('\'', self.cur);
        }
        if !m.sticky {
            self.want_col = None;
        }
        self.cur = m.to;
        self.pend.clear();
    }

    fn motion_region(&self, m: &Motion) -> Region {
        let (a, mut b) = ordered(self.cur, m.to);
        match m.kind {
            MKind::Line => Region { a: pos(a.row, 0), b: pos(b.row, 0), kind: RKind::Line },
            MKind::Incl => {
                b.col = (b.col + 1).min(self.len(b.row));
                Region { a, b, kind: RKind::Char }
            }
            MKind::Excl => {
                if a.row != b.row && b.col == 0 {
                    // Vim's rule: an exclusive motion ending in column 0
                    // stops at the end of the line before, and covers whole
                    // lines when it also starts in the indent.
                    if a.col <= mv::indent(&self.text, a.row) {
                        return Region { a: pos(a.row, 0), b: pos(b.row - 1, 0), kind: RKind::Line };
                    }
                    b = pos(b.row - 1, self.len(b.row - 1));
                }
                b.col = b.col.min(self.len(b.row));
                Region { a, b, kind: RKind::Char }
            }
        }
    }

    fn text_object(&mut self, c: char, around: bool) {
        let Some(r) = textobj::object(&self.text, self.cur, c, around, self.count()) else {
            self.pend.clear();
            return;
        };
        if self.mode.is_visual() {
            self.pend.clear();
            if r.kind == RKind::Line {
                self.mode = Mode::VisualLine;
                self.anchor = r.a;
                self.cur = pos(r.b.row, 0);
            } else {
                if r.a == r.b {
                    return;
                }
                self.anchor = r.a;
                self.cur = if r.b.col > 0 { pos(r.b.row, r.b.col - 1) } else { pos(r.b.row - 1, self.len(r.b.row - 1).saturating_sub(1)) };
            }
            return;
        }
        match self.pend.op {
            Some(op) => self.apply_op(op, r),
            None => self.pend.clear(),
        }
    }

    // ----- commands that are not motions -------------------------------------

    fn normal_cmd(&mut self, k: K) {
        let n = self.count();
        let cur = self.cur;
        let len = self.len(cur.row);
        let chars = |a: usize, b: usize| Region { a: pos(cur.row, a), b: pos(cur.row, b), kind: RKind::Char };
        match k {
            K::Char('i') => self.begin_insert(n),
            K::Char('a') => {
                if len > 0 {
                    self.cur.col += 1;
                }
                self.begin_insert(n);
            }
            K::Char('I') => {
                self.cur.col = mv::indent(&self.text, cur.row);
                self.begin_insert(n);
            }
            K::Char('A') => {
                self.cur.col = len;
                self.begin_insert(n);
            }
            K::Char('o') => self.open_line(true),
            K::Char('O') => self.open_line(false),
            K::Char('x') | K::Delete => self.apply_op(Op::Delete, chars(cur.col, (cur.col + n).min(len))),
            K::Char('X') => self.apply_op(Op::Delete, chars(cur.col.saturating_sub(n), cur.col)),
            K::Char('D') => self.apply_op(Op::Delete, chars(cur.col, len)),
            K::Char('C') => self.apply_op(Op::Change, chars(cur.col, len)),
            K::Char('s') => self.apply_op(Op::Change, chars(cur.col, (cur.col + n).min(len))),
            K::Char('S') => {
                let last = (cur.row + n - 1).min(self.last_row());
                self.apply_op(Op::Change, Region { a: pos(cur.row, 0), b: pos(last, 0), kind: RKind::Line });
            }
            K::Char('Y') => self.apply_op(Op::Yank, chars(cur.col, len)),
            K::Char('p') => self.put(true, n, false),
            K::Char('P') => self.put(false, n, false),
            K::Char('r') => self.pend.wait = Wait::Replace,
            K::Char('R') => {
                self.begin_insert(1);
                self.mode = Mode::Replace;
            }
            K::Char('~') => {
                let end = (cur.col + n).min(len);
                for c in &mut self.text[cur.row][cur.col..end] {
                    *c = toggle_case(*c);
                }
                self.cur.col = end;
                self.pend.clear();
            }
            K::Char('J') => {
                self.pend.clear();
                self.join(cur.row, n.max(2) - 1, true);
            }
            K::Char('u') => {
                self.pend.clear();
                self.hist_cmd = true;
                for _ in 0..n {
                    let Some(s) = self.undo.pop() else {
                        self.msg("Already at oldest change");
                        break;
                    };
                    self.redo.push(Snap { text: std::mem::replace(&mut self.text, s.text), cur: self.cur });
                    self.cur = s.cur;
                }
            }
            K::Ctrl('r') => {
                self.pend.clear();
                self.hist_cmd = true;
                for _ in 0..n {
                    let Some(s) = self.redo.pop() else {
                        self.msg("Already at newest change");
                        break;
                    };
                    self.undo.push(Snap { text: std::mem::replace(&mut self.text, s.text), cur: self.cur });
                    self.cur = s.cur;
                }
            }
            K::Char('.') => {
                self.pend.clear();
                if let Some(keys) = sh(|s| s.dot.clone()) {
                    self.replay(&keys);
                }
            }
            K::Char('v') => self.enter_visual(Mode::Visual),
            K::Char('V') => self.enter_visual(Mode::VisualLine),
            K::Ctrl('v') => self.enter_visual(Mode::VisualBlock),
            K::Char('m') => self.pend.wait = Wait::Mark,
            K::Char('q') => {
                self.pend.clear();
                match self.macro_rec.take() {
                    Some((name, mut keys)) => {
                        keys.pop(); // this `q`
                        sh(|s| s.macros.insert(name, keys));
                    }
                    None => self.pend.wait = Wait::MacroRec,
                }
            }
            K::Char('@') => self.pend.wait = Wait::MacroPlay,
            K::Char('Z') => self.pend.wait = Wait::ZZ,
            K::Ctrl('a') => self.increment(n as i64),
            K::Ctrl('x') => self.increment(-(n as i64)),
            K::Alt('j') => self.move_lines(cur.row, cur.row, true),
            K::Alt('k') => self.move_lines(cur.row, cur.row, false),
            _ => self.pend.clear(),
        }
    }

    fn enter_visual(&mut self, mode: Mode) {
        self.pend.clear();
        self.anchor = self.cur;
        self.mode = mode;
    }

    fn exit_visual(&mut self) {
        if !self.mode.is_visual() {
            return;
        }
        self.last_visual = Some((self.mode, self.anchor, self.cur));
        let (a, b) = ordered(self.anchor, self.cur);
        self.marks.insert('<', a);
        self.marks.insert('>', b);
        self.mode = Mode::Normal;
    }

    fn visual_region(&self) -> Region {
        let (a, b) = ordered(self.anchor, self.cur);
        match self.mode {
            Mode::VisualLine => Region { a: pos(a.row, 0), b: pos(b.row, 0), kind: RKind::Line },
            Mode::VisualBlock => {
                let (l, r) = (self.anchor.col.min(self.cur.col), self.anchor.col.max(self.cur.col));
                Region { a: pos(a.row, l), b: pos(b.row, r), kind: RKind::Block }
            }
            _ => Region { a, b: pos(b.row, (b.col + 1).min(self.len(b.row))), kind: RKind::Char },
        }
    }

    fn visual_cmd(&mut self, k: K) {
        let r = self.visual_region();
        let rows = Region { a: pos(r.a.row, 0), b: pos(r.b.row, 0), kind: RKind::Line };
        let n = self.count();
        let mode = self.mode;
        let act = |v: &mut Vim, op: Op, r: Region| {
            v.exit_visual();
            v.apply_op(op, r);
        };
        match k {
            K::Char('v') => self.switch_visual(Mode::Visual),
            K::Char('V') => self.switch_visual(Mode::VisualLine),
            K::Ctrl('v') => self.switch_visual(Mode::VisualBlock),
            K::Char('o' | 'O') => {
                std::mem::swap(&mut self.anchor, &mut self.cur);
                self.pend.clear();
            }
            K::Char('d' | 'x') | K::Delete => act(self, Op::Delete, r),
            K::Char('X' | 'D') => act(self, Op::Delete, rows),
            K::Char('y') => act(self, Op::Yank, r),
            K::Char('Y') => act(self, Op::Yank, rows),
            K::Char('c' | 's') => act(self, Op::Change, r),
            K::Char('C' | 'S' | 'R') => act(self, Op::Change, rows),
            K::Char('~') => act(self, Op::Tilde, r),
            K::Char('u') => act(self, Op::Lower, r),
            K::Char('U') => act(self, Op::Upper, r),
            K::Char(c @ ('>' | '<')) => {
                // LazyVim keeps the selection after shifting.
                let (anchor, cur) = (self.anchor, self.cur);
                self.shift(r.a.row, r.b.row, c == '>', n);
                self.pend.clear();
                self.mode = mode;
                self.anchor = anchor;
                self.cur = cur;
            }
            K::Char('J') => {
                self.exit_visual();
                self.pend.clear();
                self.join(r.a.row, (r.b.row - r.a.row).max(1), true);
            }
            K::Char(c @ ('p' | 'P')) => {
                let reg = self.pend.reg;
                let Some((text, kind)) = sh(|s| s.get(reg)) else {
                    self.pend.clear();
                    return;
                };
                self.exit_visual();
                // `P` keeps the register; `p` swaps it with the selection.
                self.pend.reg = if c == 'P' { Some('_') } else { None };
                self.apply_op(Op::Delete, r);
                match (r.kind, kind) {
                    (RKind::Line, _) => {
                        let new: Vec<Vec<char>> = text.split('\n').map(|l| l.chars().collect()).collect();
                        let at = r.a.row.min(self.text.len());
                        // Deleting every line leaves one empty line behind.
                        if self.text.len() == 1 && self.text[0].is_empty() {
                            self.text = new;
                        } else {
                            self.text.splice(at..at, new);
                        }
                        self.cur = pos(at, 0);
                    }
                    (_, RKind::Line) => {
                        let end = self.insert_at(r.a, &format!("\n{text}\n"));
                        self.cur = pos(end.row.saturating_sub(1).max(r.a.row + 1), 0);
                    }
                    _ => {
                        let end = self.insert_at(r.a, &text);
                        self.cur = pos(end.row, end.col.saturating_sub(1));
                    }
                }
            }
            K::Char('r') => self.pend.wait = Wait::Replace,
            K::Char(c @ ('I' | 'A')) => {
                self.exit_visual();
                self.pend.clear();
                let append = c == 'A';
                if r.kind == RKind::Block {
                    let col = if append { r.b.col + 1 } else { r.a.col };
                    let line = &mut self.text[r.a.row];
                    if line.len() < col {
                        line.resize(col, ' ');
                    }
                    self.cur = pos(r.a.row, col);
                    self.begin_insert(1);
                    self.ins.block = Some(BlockIns {
                        row: r.a.row,
                        rows: r.a.row + 1..=r.b.row,
                        col,
                        base_len: self.len(r.a.row),
                        pad: append,
                    });
                } else {
                    self.cur = match (append, r.kind) {
                        (false, RKind::Line) => pos(r.a.row, mv::indent(&self.text, r.a.row)),
                        (false, _) => r.a,
                        (true, RKind::Line) => pos(r.b.row, self.len(r.b.row)),
                        (true, _) => r.b,
                    };
                    self.begin_insert(1);
                }
            }
            K::Char(c @ ('*' | '#')) => {
                let text = self.yank_text(&r);
                self.exit_visual();
                self.pend.clear();
                set_search(&regex::escape(text.lines().next().unwrap_or("")));
                if let Some(p) = self.search_from(r.a, c == '*', 1) {
                    self.marks.insert('\'', self.cur);
                    self.cur = p;
                }
            }
            K::Char('m') => self.pend.wait = Wait::Mark,
            K::Alt('j') => self.move_lines(r.a.row, r.b.row, true),
            K::Alt('k') => self.move_lines(r.a.row, r.b.row, false),
            _ => self.pend.clear(),
        }
    }

    fn switch_visual(&mut self, mode: Mode) {
        self.pend.clear();
        if self.mode == mode {
            self.exit_visual();
        } else {
            self.mode = mode;
        }
    }

    // ----- text primitives ---------------------------------------------------

    /// Per-row column spans `[start, end)` covered by a region.
    pub fn spans(&self, r: &Region) -> Vec<(usize, usize, usize)> {
        (r.a.row..=r.b.row.min(self.last_row()))
            .map(|row| {
                let len = self.len(row);
                match r.kind {
                    RKind::Line => (row, 0, len),
                    RKind::Block => (row, r.a.col.min(len), (r.b.col + 1).min(len)),
                    RKind::Char => {
                        let s = if row == r.a.row { r.a.col.min(len) } else { 0 };
                        let e = if row == r.b.row { r.b.col.min(len) } else { len };
                        (row, s, e.max(s))
                    }
                }
            })
            .collect()
    }

    fn yank_text(&self, r: &Region) -> String {
        let parts: Vec<String> = self.spans(r).iter().map(|&(row, s, e)| self.text[row][s..e].iter().collect()).collect();
        parts.join("\n")
    }

    fn remove(&mut self, r: &Region) {
        match r.kind {
            RKind::Line => {
                self.text.drain(r.a.row..=r.b.row.min(self.last_row()));
                if self.text.is_empty() {
                    self.text.push(Vec::new());
                }
            }
            RKind::Block => {
                for (row, s, e) in self.spans(r) {
                    self.text[row].drain(s..e);
                }
            }
            RKind::Char => {
                let b_col = r.b.col.min(self.len(r.b.row));
                let tail: Vec<char> = self.text[r.b.row][b_col..].to_vec();
                self.text[r.a.row].truncate(r.a.col);
                self.text[r.a.row].extend(tail);
                if r.b.row > r.a.row {
                    self.text.drain(r.a.row + 1..=r.b.row);
                }
            }
        }
    }

    /// Insert literal text; returns the position just after it.
    fn insert_at(&mut self, p: Pos, s: &str) -> Pos {
        let col = p.col.min(self.len(p.row));
        let tail = self.text[p.row].split_off(col);
        let mut row = p.row;
        for (i, part) in s.split('\n').enumerate() {
            if i > 0 {
                row += 1;
                self.text.insert(row, Vec::new());
            }
            self.text[row].extend(part.chars());
        }
        let end = pos(row, self.len(row));
        self.text[row].extend(tail);
        end
    }

    // ----- operators ---------------------------------------------------------

    fn apply_op(&mut self, op: Op, r: Region) {
        let reg = self.pend.reg;
        let n = self.count();
        self.pend.clear();
        self.want_col = None;
        let empty = r.kind == RKind::Char && r.a == r.b;
        if empty && op != Op::Change {
            return;
        }
        self.marks.insert('[', r.a);
        self.marks.insert(']', r.b);
        match op {
            Op::Yank => {
                let text = self.yank_text(&r);
                sh(|s| s.set(reg, text, r.kind, false));
                self.cur = match r.kind {
                    RKind::Line => pos(r.a.row, if r.a.row == self.cur.row { self.cur.col } else { self.cur.col.min(self.len(r.a.row)) }),
                    _ => r.a,
                };
            }
            Op::Delete => {
                let text = self.yank_text(&r);
                sh(|s| s.set(reg, text, r.kind, true));
                self.remove(&r);
                self.cur = match r.kind {
                    RKind::Line => {
                        let row = r.a.row.min(self.last_row());
                        pos(row, mv::first_nonblank(&self.text, row))
                    }
                    _ => r.a,
                };
            }
            Op::Change => {
                if !empty {
                    let text = self.yank_text(&r);
                    sh(|s| s.set(reg, text, r.kind, true));
                }
                match r.kind {
                    RKind::Line => {
                        // Keep one line, with the indent of the first.
                        let ind = mv::indent(&self.text, r.a.row);
                        self.text[r.a.row].truncate(ind);
                        if r.b.row > r.a.row {
                            self.text.drain(r.a.row + 1..=r.b.row.min(self.last_row()));
                        }
                        self.cur = pos(r.a.row, ind);
                        self.begin_insert(1);
                    }
                    RKind::Char => {
                        self.remove(&r);
                        self.cur = r.a;
                        self.begin_insert(1);
                    }
                    RKind::Block => {
                        self.remove(&r);
                        self.cur = r.a;
                        self.begin_insert(1);
                        self.ins.block = Some(BlockIns {
                            row: r.a.row,
                            rows: r.a.row + 1..=r.b.row,
                            col: r.a.col,
                            base_len: self.len(r.a.row),
                            pad: false,
                        });
                    }
                }
            }
            Op::Indent | Op::Dedent => {
                // A count before a doubled operator picks lines, not levels.
                let levels = if self.mode.is_visual() { n } else { 1 };
                self.shift(r.a.row, r.b.row, op == Op::Indent, levels);
            }
            Op::Lower | Op::Upper | Op::Tilde => {
                for (row, s, e) in self.spans(&r) {
                    for c in &mut self.text[row][s..e] {
                        *c = match op {
                            Op::Lower => c.to_lowercase().next().unwrap_or(*c),
                            Op::Upper => c.to_uppercase().next().unwrap_or(*c),
                            _ => toggle_case(*c),
                        };
                    }
                }
                self.cur = if r.kind == RKind::Line { pos(r.a.row, self.cur.col) } else { r.a };
            }
            Op::Comment => {
                self.toggle_comment(r.a.row, r.b.row);
                self.cur.row = r.a.row;
            }
        }
    }

    fn shift(&mut self, a: usize, b: usize, right: bool, levels: usize) {
        let sw = self.shiftwidth.max(1) * levels.max(1);
        for row in a..=b.min(self.last_row()) {
            let line = &mut self.text[row];
            if right {
                if !line.is_empty() {
                    line.splice(0..0, std::iter::repeat_n(' ', sw));
                }
            } else {
                let mut cols = 0;
                let mut cut = 0;
                while cut < line.len() && cols < sw && line[cut].is_whitespace() {
                    cols += if line[cut] == '\t' { self.shiftwidth.max(1) } else { 1 };
                    cut += 1;
                }
                line.drain(..cut);
            }
        }
        self.cur = pos(a, mv::first_nonblank(&self.text, a));
    }

    fn toggle_comment(&mut self, a: usize, b: usize) {
        let lead: Vec<char> = self.comment.chars().collect();
        let rows: Vec<usize> = (a..=b.min(self.last_row())).filter(|r| !mv::is_blank(&self.text, *r)).collect();
        if rows.is_empty() || lead.is_empty() {
            return;
        }
        let commented = |t: &Text, r: usize| t[r][mv::indent(t, r)..].starts_with(&lead);
        if rows.iter().all(|r| commented(&self.text, *r)) {
            for r in rows {
                let at = mv::indent(&self.text, r);
                let line = &mut self.text[r];
                let mut end = at + lead.len();
                if line.get(end) == Some(&' ') {
                    end += 1;
                }
                line.drain(at..end);
            }
        } else {
            let at = rows.iter().map(|r| mv::indent(&self.text, *r)).min().unwrap_or(0);
            for r in rows {
                let mut ins = lead.clone();
                ins.push(' ');
                self.text[r].splice(at..at, ins);
            }
        }
    }

    fn join(&mut self, row: usize, times: usize, spaces: bool) {
        for _ in 0..times {
            if row + 1 >= self.text.len() {
                break;
            }
            let mut next = self.text.remove(row + 1);
            let line = &mut self.text[row];
            if spaces {
                let lead = next.iter().take_while(|c| c.is_whitespace()).count();
                next.drain(..lead);
                while line.last().is_some_and(is_ws) {
                    line.pop();
                }
                let at = line.len();
                if !line.is_empty() && !next.is_empty() && next[0] != ')' {
                    line.push(' ');
                }
                self.cur = pos(row, at);
            } else {
                self.cur = pos(row, line.len());
            }
            line.extend(next);
        }
    }

    fn move_lines(&mut self, a: usize, b: usize, down: bool) {
        self.pend.clear();
        if down {
            if b + 1 >= self.text.len() {
                return;
            }
            let l = self.text.remove(b + 1);
            self.text.insert(a, l);
            self.cur.row += 1;
            self.anchor.row += 1;
        } else {
            if a == 0 {
                return;
            }
            let l = self.text.remove(a - 1);
            self.text.insert(b, l);
            self.cur.row -= 1;
            self.anchor.row = self.anchor.row.saturating_sub(1);
        }
    }

    fn replace_chars(&mut self, c: char) {
        let n = self.count();
        self.pend.clear();
        if self.mode.is_visual() {
            let r = self.visual_region();
            self.exit_visual();
            for (row, s, e) in self.spans(&r) {
                for x in &mut self.text[row][s..e] {
                    *x = c;
                }
            }
            self.cur = r.a;
            return;
        }
        let cur = self.cur;
        if cur.col + n > self.len(cur.row) {
            return;
        }
        for x in &mut self.text[cur.row][cur.col..cur.col + n] {
            *x = c;
        }
        self.cur.col = cur.col + n - 1;
    }

    fn increment(&mut self, by: i64) {
        self.pend.clear();
        let cur = self.cur;
        let line = &self.text[cur.row];
        // The number under the cursor, else the next one on the line.
        let mut at = cur.col.min(line.len().saturating_sub(1));
        while at > 0 && line.get(at).is_some_and(char::is_ascii_digit) && line[at - 1].is_ascii_digit() {
            at -= 1;
        }
        let Some(s) = (at..line.len()).find(|&i| line[i].is_ascii_digit()) else { return };
        let mut e = s;
        while e < line.len() && line[e].is_ascii_digit() {
            e += 1;
        }
        let neg = s > 0 && line[s - 1] == '-';
        let digits: String = line[s..e].iter().collect();
        let Ok(v) = digits.parse::<i64>() else { return };
        let v = if neg { -v } else { v }.saturating_add(by);
        let start = if neg { s - 1 } else { s };
        let new: Vec<char> = v.to_string().chars().collect();
        let end = start + new.len();
        self.text[cur.row].splice(start..e, new);
        self.cur.col = end - 1;
    }

    fn put(&mut self, after: bool, n: usize, cursor_after: bool) {
        let reg = self.pend.reg;
        self.pend.clear();
        let Some((text, kind)) = sh(|s| s.get(reg)) else { return };
        let cur = self.cur;
        match kind {
            RKind::Line => {
                let one: Vec<Vec<char>> = text.split('\n').map(|l| l.chars().collect()).collect();
                let cnt = one.len() * n;
                let at = if after { cur.row + 1 } else { cur.row };
                let rows = one.iter().cloned().cycle().take(cnt);
                self.text.splice(at..at, rows);
                self.cur = if cursor_after {
                    pos((at + cnt).min(self.last_row()), 0)
                } else {
                    pos(at, mv::first_nonblank(&self.text, at))
                };
            }
            RKind::Char => {
                let s = text.repeat(n);
                let col = if after && self.len(cur.row) > 0 { cur.col + 1 } else { cur.col };
                let start = pos(cur.row, col);
                let end = self.insert_at(start, &s);
                self.cur = if cursor_after {
                    end
                } else if s.contains('\n') {
                    start
                } else {
                    pos(end.row, end.col.saturating_sub(1))
                };
                if cursor_after && self.cur.col >= self.len(self.cur.row) && self.cur.row < self.last_row() {
                    self.cur = pos(self.cur.row + 1, 0);
                }
            }
            RKind::Block => {
                let col = if after && self.len(cur.row) > 0 { cur.col + 1 } else { cur.col };
                for (i, part) in text.split('\n').enumerate() {
                    let row = cur.row + i;
                    if row >= self.text.len() {
                        self.text.push(Vec::new());
                    }
                    let line = &mut self.text[row];
                    if line.len() < col {
                        line.resize(col, ' ');
                    }
                    let chunk: Vec<char> = part.repeat(n).chars().collect();
                    line.splice(col..col, chunk);
                }
                self.cur = pos(cur.row, col);
            }
        }
    }

    // ----- insert mode ---------------------------------------------------------

    fn begin_insert(&mut self, count: usize) {
        self.pend.clear();
        self.mode = Mode::Insert;
        self.ins = InsertSession { count: count.max(1), ..Default::default() };
    }

    fn open_line(&mut self, below: bool) {
        self.pend.clear();
        let row = self.cur.row;
        let mut ind: Vec<char> = self.text[row][..mv::indent(&self.text, row)].to_vec();
        if below && self.opens_block(&self.text[row]) {
            ind.extend(std::iter::repeat_n(' ', self.shiftwidth));
        }
        let at = if below { row + 1 } else { row };
        let col = ind.len();
        self.text.insert(at, ind);
        self.cur = pos(at, col);
        self.begin_insert(1);
    }

    /// The text before the cursor ends in `:` or an open bracket: the next
    /// line is indented one level deeper.
    fn opens_block(&self, before: &[char]) -> bool {
        matches!(before.iter().rev().find(|c| !c.is_whitespace()), Some(':' | '(' | '[' | '{'))
    }

    fn leave_insert(&mut self) {
        let ins = std::mem::take(&mut self.ins);
        for _ in 1..ins.count {
            for k in &ins.keys {
                self.insert_key(*k);
            }
        }
        if let Some(b) = ins.block
            && self.cur.row == b.row
            && self.len(b.row) > b.base_len
        {
            let added: Vec<char> = self.text[b.row][b.col..b.col + self.len(b.row) - b.base_len].to_vec();
            for r in b.rows {
                let Some(line) = self.text.get_mut(r) else { break };
                if line.len() < b.col {
                    if !b.pad {
                        continue;
                    }
                    line.resize(b.col, ' ');
                }
                line.splice(b.col..b.col, added.iter().copied());
            }
            self.cur = pos(b.row, b.col + 1);
        }
        self.marks.insert('^', self.cur);
        self.cur.col = self.cur.col.saturating_sub(1);
        self.mode = Mode::Normal;
    }

    fn insert_key(&mut self, k: K) {
        if std::mem::take(&mut self.ins.reg_wait) {
            if let K::Char(c) = k
                && let Some((text, _)) = sh(|s| s.get(Some(c)))
            {
                self.cur = self.insert_at(self.cur, &text);
            }
            return;
        }
        self.clamp();
        let cur = self.cur;
        let len = self.len(cur.row);
        let replace = self.mode == Mode::Replace;
        match k {
            K::Esc => self.leave_insert(),
            K::Char(c) if replace => {
                if cur.col < len {
                    self.text[cur.row][cur.col] = c;
                } else {
                    self.text[cur.row].push(c);
                }
                self.cur.col += 1;
            }
            K::Char(c) => self.type_char(c),
            K::Enter => self.newline(),
            K::Backspace | K::Ctrl('h') if replace => {
                self.cur.col = cur.col.saturating_sub(1);
            }
            K::Backspace | K::Ctrl('h') => self.backspace(),
            K::Delete => {
                if cur.col < len {
                    self.text[cur.row].remove(cur.col);
                } else if cur.row < self.last_row() {
                    let next = self.text.remove(cur.row + 1);
                    self.text[cur.row].extend(next);
                }
            }
            K::Tab => {
                let sw = self.shiftwidth.max(1);
                let n = sw - cur.col % sw;
                self.text[cur.row].splice(cur.col..cur.col, std::iter::repeat_n(' ', n));
                self.cur.col += n;
            }
            K::BackTab | K::Ctrl('d') => self.shift_in_insert(false),
            K::Ctrl('t') => self.shift_in_insert(true),
            K::Left => self.cur.col = cur.col.saturating_sub(1),
            K::Right => self.cur.col = (cur.col + 1).min(len),
            K::Up => self.cur = pos(cur.row.saturating_sub(1), cur.col),
            K::Down => self.cur = pos((cur.row + 1).min(self.last_row()), cur.col),
            K::Home => self.cur.col = 0,
            K::End => self.cur.col = len,
            K::Ctrl('w') => {
                if cur.col == 0 {
                    self.backspace();
                } else {
                    let mut to = mv::word_back(&self.text, cur, false);
                    if to.row != cur.row {
                        to = pos(cur.row, 0);
                    }
                    self.text[cur.row].drain(to.col..cur.col);
                    self.cur = to;
                }
            }
            K::Ctrl('u') => {
                let ind = mv::indent(&self.text, cur.row).min(cur.col);
                let from = if cur.col > ind { ind } else { 0 };
                self.text[cur.row].drain(from..cur.col);
                self.cur.col = from;
            }
            K::Ctrl('r') => self.ins.reg_wait = true,
            K::Alt('j') => self.move_lines(cur.row, cur.row, true),
            K::Alt('k') => self.move_lines(cur.row, cur.row, false),
            _ => {}
        }
    }

    fn shift_in_insert(&mut self, right: bool) {
        let cur = self.cur;
        let before = self.len(cur.row);
        self.shift(cur.row, cur.row, right, 1);
        let after = self.len(cur.row);
        self.cur = pos(cur.row, (cur.col + after).saturating_sub(before));
    }

    fn type_char(&mut self, c: char) {
        let cur = self.cur;
        let line = &self.text[cur.row];
        let next = line.get(cur.col).copied();
        let prev = cur.col.checked_sub(1).and_then(|i| line.get(i)).copied();
        if self.autopairs {
            // mini.pairs as LazyVim configures it.
            let blocked_next = next.is_some_and(|n| n.is_alphanumeric() || matches!(n, '_' | '%' | '\'' | '[' | '"' | '.' | '`' | '$'));
            let close = match c {
                '(' => Some(')'),
                '[' => Some(']'),
                '{' => Some('}'),
                _ => None,
            };
            if matches!(c, ')' | ']' | '}') && next == Some(c) {
                self.cur.col += 1;
                return;
            }
            if let Some(close) = close
                && prev != Some('\\')
                && !blocked_next
            {
                // Skip when the line already has a closer waiting for this.
                let opens = line.iter().filter(|x| **x == c).count();
                let closes = line.iter().filter(|x| **x == close).count();
                if !(next == Some(close) && closes > opens) {
                    self.text[cur.row].splice(cur.col..cur.col, [c, close]);
                    self.cur.col += 1;
                    return;
                }
            }
            if matches!(c, '"' | '\'' | '`') {
                if next == Some(c) {
                    self.cur.col += 1;
                    return;
                }
                if !prev.is_some_and(|p| p.is_alphabetic() || p == '\\') && !blocked_next {
                    self.text[cur.row].splice(cur.col..cur.col, [c, c]);
                    self.cur.col += 1;
                    return;
                }
            }
        }
        self.text[cur.row].insert(cur.col, c);
        self.cur.col += 1;
    }

    fn newline(&mut self) {
        let cur = self.cur;
        let tail = self.text[cur.row].split_off(cur.col);
        let before = &self.text[cur.row];
        let mut ind: Vec<char> = before.iter().take_while(|c| c.is_whitespace()).copied().collect();
        let base = ind.clone();
        let deeper = self.opens_block(before);
        if deeper {
            ind.extend(std::iter::repeat_n(' ', self.shiftwidth));
        }
        let col = ind.len();
        // `{|}` + Enter puts the closer on its own line.
        let opened = before.iter().rev().find(|c| !c.is_whitespace()).copied();
        let split_pair = self.autopairs && deeper && opened.and_then(mv::closer).is_some_and(|cl| tail.first() == Some(&cl));
        if split_pair {
            let mut closing = base;
            closing.extend(tail);
            self.text.insert(cur.row + 1, ind);
            self.text.insert(cur.row + 2, closing);
        } else {
            let lead = tail.iter().take_while(|c| c.is_whitespace()).count();
            ind.extend(tail.into_iter().skip(lead));
            self.text.insert(cur.row + 1, ind);
        }
        self.cur = pos(cur.row + 1, col);
    }

    fn backspace(&mut self) {
        let cur = self.cur;
        if cur.col == 0 {
            if cur.row > 0 {
                let line = self.text.remove(cur.row);
                let col = self.len(cur.row - 1);
                self.text[cur.row - 1].extend(line);
                self.cur = pos(cur.row - 1, col);
            }
            return;
        }
        let line = &mut self.text[cur.row];
        let prev = line[cur.col - 1];
        let next = line.get(cur.col).copied();
        let pair = matches!((prev, next), ('(', Some(')')) | ('[', Some(']')) | ('{', Some('}')) | ('"', Some('"')) | ('\'', Some('\'')) | ('`', Some('`')));
        if self.autopairs && pair {
            line.drain(cur.col - 1..=cur.col);
            self.cur.col -= 1;
            return;
        }
        // In the indent, step back one level (smarttab).
        let sw = self.shiftwidth.max(1);
        let n = if line[..cur.col].iter().all(|c| *c == ' ') { (cur.col - 1) % sw + 1 } else { 1 };
        line.drain(cur.col - n..cur.col);
        self.cur.col -= n;
    }

    // ----- search and the command line ----------------------------------------

    fn all_matches(&self) -> Vec<(Pos, usize)> {
        let Some(re) = sh(|s| s.search.as_ref().map(|x| x.re.clone())) else { return Vec::new() };
        let mut out = Vec::new();
        for (row, l) in self.text.iter().enumerate() {
            let s: String = l.iter().collect();
            for m in re.find_iter(&s) {
                if m.is_empty() {
                    continue;
                }
                out.push((pos(row, s[..m.start()].chars().count()), m.as_str().chars().count()));
            }
        }
        out
    }

    fn search_from(&mut self, from: Pos, fwd: bool, n: usize) -> Option<Pos> {
        let Some(pat) = sh(|s| s.search.as_ref().map(|x| x.pat.clone())) else {
            self.msg("E35: No previous regular expression");
            return None;
        };
        sh(|s| s.hl = true);
        let ms: Vec<Pos> = self.all_matches().into_iter().map(|m| m.0).collect();
        if ms.is_empty() {
            self.msg(format!("E486: Pattern not found: {pat}"));
            return None;
        }
        let mut p = from;
        for _ in 0..n.max(1) {
            p = if fwd {
                ms.iter().find(|m| **m > p).or(ms.first()).copied()?
            } else {
                ms.iter().rev().find(|m| **m < p).or(ms.last()).copied()?
            };
        }
        Some(p)
    }

    fn cmdline_key(&mut self, k: K) {
        let Some(cmd) = self.cmd.as_mut() else { return };
        match k {
            K::Esc => {
                self.cmd = None;
                self.pend.clear();
            }
            K::Enter => {
                if let Some(c) = self.cmd.take() {
                    self.run_cmd(c);
                }
            }
            K::Backspace | K::Ctrl('h') => {
                if cmd.text.pop().is_none() {
                    self.cmd = None;
                    self.pend.clear();
                }
            }
            K::Ctrl('u') => cmd.text.clear(),
            K::Ctrl('w') => {
                let t = cmd.text.trim_end_matches(|c: char| !c.is_alphanumeric());
                let t = t.trim_end_matches(char::is_alphanumeric);
                cmd.text.truncate(t.len());
            }
            K::Char(c) => cmd.text.push(c),
            _ => {}
        }
    }

    fn run_cmd(&mut self, c: Cmdline) {
        if c.kind == ':' {
            self.pend.clear();
            return self.ex(c.text.trim());
        }
        if !c.text.is_empty() {
            set_search(&c.text);
        }
        let n = self.count();
        match self.search_from(self.cur, c.kind == '/', n) {
            Some(p) => self.do_motion(Motion { to: p, kind: MKind::Excl, sticky: false, jump: true }),
            None => self.pend.clear(),
        }
    }

    fn ex(&mut self, s: &str) {
        let last = self.last_row();
        let line_no = |v: &str| v.parse::<usize>().ok().map(|n| n.saturating_sub(1).min(last));
        // Range: `%`, `'<,'>`, `N`, `N,M`.
        let (range, rest) = if let Some(r) = s.strip_prefix('%') {
            (Some((0, last)), r)
        } else if let Some(r) = s.strip_prefix("'<,'>") {
            let a = self.marks.get(&'<').map_or(self.cur.row, |p| p.row);
            let b = self.marks.get(&'>').map_or(self.cur.row, |p| p.row);
            (Some((a.min(last), b.min(last))), r)
        } else {
            let digits = s.len() - s.trim_start_matches(|c: char| c.is_ascii_digit()).len();
            match line_no(&s[..digits]) {
                Some(a) => {
                    let r = &s[digits..];
                    let more = r.strip_prefix(',').map(|r| (r.len() - r.trim_start_matches(|c: char| c.is_ascii_digit()).len(), r));
                    match more {
                        Some((d, r)) if d > 0 => (Some((a, line_no(&r[..d]).unwrap_or(a))), &r[d..]),
                        _ => (Some((a, a)), r),
                    }
                }
                None => (None, s),
            }
        };
        let rest = rest.trim();
        match rest {
            "" => {
                if let Some((_, b)) = range {
                    self.marks.insert('\'', self.cur);
                    self.cur = pos(b, mv::first_nonblank(&self.text, b));
                }
            }
            "$" => self.cur = pos(last, mv::first_nonblank(&self.text, last)),
            "w" | "w!" | "write" | "up" | "update" => self.requests.push(Request::Save),
            "q" | "q!" | "quit" | "clo" | "close" => self.requests.push(Request::Leave),
            "wq" | "wq!" | "x" | "xit" => {
                self.requests.push(Request::Save);
                self.requests.push(Request::Leave);
            }
            "noh" | "nohl" | "nohlsearch" => clear_search_highlight(),
            _ if rest.starts_with('s') && rest[1..].chars().next().is_some_and(|c| !c.is_alphanumeric() && c != ' ') => {
                let (a, b) = range.unwrap_or((self.cur.row, self.cur.row));
                self.substitute(&rest[1..], a.min(b), a.max(b));
            }
            _ => self.msg(format!("E492: Not an editor command: {s}")),
        }
    }

    /// `:s/pat/rep/flags` over rows `a..=b`; `spec` starts at the delimiter.
    fn substitute(&mut self, spec: &str, a: usize, b: usize) {
        let mut cs = spec.chars();
        let Some(delim) = cs.next() else { return };
        let mut parts = vec![String::new()];
        let mut esc = false;
        for c in cs {
            if esc {
                if c != delim {
                    parts.last_mut().unwrap().push('\\');
                }
                parts.last_mut().unwrap().push(c);
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == delim && parts.len() < 3 {
                parts.push(String::new());
            } else {
                parts.last_mut().unwrap().push(c);
            }
        }
        let pat = if parts[0].is_empty() { sh(|s| s.search.as_ref().map(|x| x.pat.clone())).unwrap_or_default() } else { parts[0].clone() };
        let flags = parts.get(2).cloned().unwrap_or_default();
        let Some(re) = compile(&if flags.contains('i') { format!("(?i){pat}") } else { pat.clone() }) else { return };
        // Vim's `\1` and `&` to the regex crate's `${1}` and `${0}`.
        let mut rep = String::new();
        let mut it = parts.get(1).map(String::as_str).unwrap_or("").chars().peekable();
        while let Some(c) = it.next() {
            match c {
                '\\' => match it.next() {
                    Some(d) if d.is_ascii_digit() => rep.push_str(&format!("${{{d}}}")),
                    Some('n') => rep.push('\n'),
                    Some('t') => rep.push('\t'),
                    Some(o) => rep.push(o),
                    None => {}
                },
                '&' => rep.push_str("${0}"),
                '$' => rep.push_str("$$"),
                c => rep.push(c),
            }
        }
        let all = flags.contains('g');
        let mut count = 0usize;
        let mut last_row = None;
        let mut out: Text = Vec::new();
        for (row, l) in self.text.iter().enumerate() {
            if row < a || row > b {
                out.push(l.clone());
                continue;
            }
            let s: String = l.iter().collect();
            let hits = re.find_iter(&s).count();
            if hits == 0 {
                out.push(l.clone());
                continue;
            }
            count += if all { hits } else { 1 };
            let new = if all { re.replace_all(&s, rep.as_str()) } else { re.replace(&s, rep.as_str()) };
            last_row = Some(out.len());
            out.extend(new.split('\n').map(|p| p.chars().collect::<Vec<char>>()));
        }
        if count == 0 {
            self.msg(format!("E486: Pattern not found: {pat}"));
            return;
        }
        self.text = out;
        set_search(&pat);
        if let Some(r) = last_row {
            let r = r.min(self.last_row());
            self.cur = pos(r, mv::first_nonblank(&self.text, r));
        }
        if count > 1 {
            self.msg(format!("{count} substitutions"));
        }
    }
}

fn toggle_case(c: char) -> char {
    if c.is_uppercase() {
        c.to_lowercase().next().unwrap_or(c)
    } else {
        c.to_uppercase().next().unwrap_or(c)
    }
}
