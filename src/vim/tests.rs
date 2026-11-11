//! Table-driven tests: `(text, keys) -> text`, with `|` marking the cursor
//! (it sits on the character after the bar).

use super::*;

fn load(s: &str) -> Vim {
    let at = s.find('|').expect("cursor marker");
    let before = &s[..at];
    let row = before.matches('\n').count();
    let col = before.rsplit('\n').next().unwrap().chars().count();
    let mut v = Vim::new(&s.replace('|', ""));
    v.cur = Pos { row, col };
    v
}

fn show(v: &Vim) -> String {
    let mut out = String::new();
    for (r, l) in v.text.iter().enumerate() {
        if r > 0 {
            out.push('\n');
        }
        for (c, ch) in l.iter().enumerate() {
            if (r, c) == (v.cur.row, v.cur.col) {
                out.push('|');
            }
            out.push(*ch);
        }
        if r == v.cur.row && v.cur.col >= l.len() {
            out.push('|');
        }
    }
    out
}

fn feed(v: &mut Vim, keys: &str) {
    for k in K::parse(keys) {
        v.key(k);
    }
}

fn run(text: &str, keys: &str) -> String {
    let mut v = load(text);
    feed(&mut v, keys);
    show(&v)
}

fn check(cases: &[(&str, &str, &str)]) {
    let mut failed = Vec::new();
    for (text, keys, want) in cases {
        // Registers and search are shared per thread: start each case clean.
        sh(|s| *s = Shared::default());
        let got = run(text, keys);
        if got != *want {
            failed.push(format!("\n  {text:?} + {keys:?}\n    want {want:?}\n    got  {got:?}"));
        }
    }
    assert!(failed.is_empty(), "{} failed:{}", failed.len(), failed.concat());
}

#[test]
fn motions() {
    check(&[
        ("|foo bar baz", "w", "foo |bar baz"),
        ("|foo bar baz", "2w", "foo bar |baz"),
        ("|foo bar baz", "3w", "foo bar ba|z"),
        ("|foo.bar baz", "w", "foo|.bar baz"),
        ("|foo.bar baz", "W", "foo.bar |baz"),
        ("foo |bar", "b", "|foo bar"),
        ("foo ba|r", "b", "foo |bar"),
        ("|foo bar", "e", "fo|o bar"),
        ("fo|o bar", "e", "foo ba|r"),
        ("foo |bar", "ge", "fo|o bar"),
        ("|foo\nbar", "w", "foo\n|bar"),
        ("|foo\n\nbar", "w", "foo\n|\nbar"),
        ("foo\n|bar", "b", "|foo\nbar"),
        ("  foo |bar", "0", "|  foo bar"),
        ("  foo |bar", "^", "  |foo bar"),
        ("|foo bar", "$", "foo ba|r"),
        ("|a\nb\nc", "G", "a\nb\n|c"),
        ("a\nb\n|c", "gg", "|a\nb\nc"),
        ("|a\nb\nc", "2G", "a\n|b\nc"),
        ("|a\nb\nc", "3gg", "a\nb\n|c"),
        ("|a\nb\nc\nd", "2j", "a\nb\n|c\nd"),
        ("a\nb\n|c", "5k", "|a\nb\nc"),
        // sticky column through a short line
        ("abc|d\nx\nabcd", "jj", "abcd\nx\nabc|d"),
        ("|abcd\nx\nabcdef", "$jj", "abcd\nx\nabcde|f"),
        ("|a b, c b", "fb", "a |b, c b"),
        ("|a b, c b", "2fb", "a b, c |b"),
        ("|a b, c b", "tb", "a| b, c b"),
        ("|a b, c b", "fb;", "a b, c |b"),
        ("|a b, c b", "fb;,", "a |b, c b"),
        ("|a b, c b", "tb;", "a b, c| b"),
        ("a b, c |b", "Fa", "|a b, c b"),
        ("a b, c |b", "Ta", "a| b, c b"),
        ("|f(a[1])", "%", "f(a[1]|)"),
        ("f(a[1]|)", "%", "f|(a[1])"),
        ("|a\nb\n\nc\nd\n\ne", "}", "a\nb\n|\nc\nd\n\ne"),
        ("a\nb\n\nc\n|d\n\ne", "{", "a\nb\n|\nc\nd\n\ne"),
        ("f(a, |b)", "[(", "f|(a, b)"),
        ("f(a, |b)", "])", "f(a, b|)"),
        ("|abcdef", "4|", "abc|def"),
        ("a\n  |b\nc", "-", "|a\n  b\nc"),
        ("|a\n  b\nc", "+", "a\n  |b\nc"),
        ("|a\nb", "ma" , "|a\nb"),
        ("|a\nb\nc", "majj'a", "|a\nb\nc"),
        ("a|b\nb\nc", "mxjj`x", "a|b\nb\nc"),
        ("|a\nb\nc", "G''", "|a\nb\nc"),
    ]);
}

#[test]
fn operators_with_motions() {
    check(&[
        ("|foo bar baz", "dw", "|bar baz"),
        ("|foo bar baz", "d2w", "|baz"),
        ("|foo bar baz", "2dw", "|baz"),
        ("foo |bar", "dw", "foo| "),
        ("|foo\nbar", "dw", "|\nbar"),
        ("foo |bar\nbaz", "dw", "foo| \nbaz"),
        ("|foo bar baz", "de", "| bar baz"),
        ("foo |bar baz", "db", "|bar baz"),
        ("foo |bar baz", "d$", "foo| "),
        ("foo |bar baz", "D", "foo| "),
        ("foo |bar baz", "d0", "|bar baz"),
        ("|a\nb\nc\nd", "dd", "|b\nc\nd"),
        ("|a\nb\nc\nd", "2dd", "|c\nd"),
        ("|a\nb\nc\nd", "d2d", "|c\nd"),
        ("a\nb\nc\n|d", "dd", "a\nb\n|c"),
        ("|only", "dd", "|"),
        ("|a\nb\nc\nd", "dj", "|c\nd"),
        ("a\n|b\nc\nd", "dk", "|c\nd"),
        ("a\n|b\nc\nd", "dG", "|a"),
        ("a\nb\n|c\nd", "dgg", "|d"),
        ("|a b c d", "dfc", "| d"),
        ("|a b c d", "dtc", "|c d"),
        ("f(|a, b)", "d])", "f(|)"),
        ("x = |f(a) + 1", "d%", "x = | + 1"),
        ("|a\nb\n\nc", "d}", "|\nc"),
        ("|foo bar", "cwx<Esc>", "|x bar"),
        ("|foo bar", "c2wx<Esc>", "|x"),
        ("fo|o bar", "cwx<Esc>", "fo|x bar"),
        ("foo |bar baz", "Cx<Esc>", "foo |x"),
        ("  |foo\nbar", "ccx<Esc>", "  |x\nbar"),
        ("  |foo\nbar", "Sx<Esc>", "  |x\nbar"),
        ("|foo", "sx<Esc>", "|xoo"),
        ("|foo", "2sx<Esc>", "|xo"),
        ("|abc", "x", "|bc"),
        ("|abc", "2x", "|c"),
        ("ab|c", "x", "a|b"),
        ("ab|c", "X", "a|c"),
        ("|a\nb", ">>", "    |a\nb"),
        ("    |a\nb", "<lt><lt>", "|a\nb"),
        ("|a\nb\nc", ">j", "    |a\n    b\nc"),
        ("|a\nb\nc", "2>>", "    |a\n    b\nc"),
        ("|foo bar", "gUw", "|FOO bar"),
        ("|FOO bar", "guiw", "|foo bar"),
        ("|Foo bar", "g~~", "|fOO BAR"),
        ("|foo", "~", "F|oo"),
        ("|foo", "3~", "FO|O"),
        ("|a\nb\nc", "J", "a| b\nc"),
        ("|a\n  b\nc", "3J", "a b| c"),
        ("|a\nb", "gJ", "a|b"),
        ("|abc", "rx", "|xbc"),
        ("|abc", "2rx", "x|xc"),
        ("|abc", "4rx", "|abc"),
        ("|abc", "Rxy<Esc>", "x|yc"),
        ("x = |7", "<C-a>", "x = |8"),
        ("|x = 9", "<C-a>", "x = 1|0"),
        ("x = |10", "<C-x>", "x = |9"),
        ("x = |0", "<C-x>", "x = -|1"),
        ("x = |5", "10<C-a>", "x = 1|5"),
    ]);
}

#[test]
fn text_objects() {
    check(&[
        ("foo b|ar baz", "diw", "foo | baz"),
        ("foo b|ar baz", "daw", "foo |baz"),
        ("foo ba|r", "daw", "fo|o"),
        ("foo b|ar baz", "ciwx<Esc>", "foo |x baz"),
        ("foo b|ar.x baz", "diW", "foo | baz"),
        ("f(a, |b)", "di(", "f(|)"),
        ("f(a, |b)", "dib", "f(|)"),
        ("f(a, |b)", "da(", "|f"),
        ("f(a, |b)", "ci)x<Esc>", "f(|x)"),
        ("x = [1, |2]", "di[", "x = [|]"),
        ("d = {'a': |1}", "di{", "d = {|}"),
        ("|x = f(1)", "di(", "x = f(|)"),
        ("x = \"he|llo\" + y", "di\"", "x = \"|\" + y"),
        ("x = \"he|llo\" + y", "da\"", "x = | + y"),
        ("|x = 'hello'", "ci'y<Esc>", "x = '|y'"),
        ("x = 'he|llo'", "diq", "x = '|'"),
        ("f(a, |bb, c)", "dia", "f(a, |, c)"),
        ("f(a, |bb, c)", "daa", "f(a, |c)"),
        ("f(a, bb, |c)", "daa", "f(a, bb|)"),
        ("f(|a, bb, c)", "daa", "f(|bb, c)"),
        ("f(g(1, 2), |h(3))", "dia", "f(g(1, 2), |)"),
        ("x = foo.b|ar(1, 2) + 3", "dau", "x = | + 3"),
        ("x = foo.bar(1, |2) + 3", "dau", "x = | + 3"),
        ("x = foo.bar(1, |2) + 3", "daU", "x = foo.| + 3"),
        ("x = foo.bar(1, |2) + 3", "diu", "x = foo.bar(|) + 3"),
        ("a\n\nb\n|c\n\nd", "dip", "a\n\n|\nd"),
        ("a\n\nb\n|c\n\nd", "dap", "a\n\n|d"),
        ("if x:\n    a\n    |b\nc", "dii", "if x:\n|c"),
        ("if x:\n    a\n    |b\nc", "dai", "|c"),
        ("x = 1\ndef f():\n    a = 1\n    |return a\ny = 2", "daf", "x = 1\n|y = 2"),
        ("x = 1\ndef f():\n    a = 1\n    |return a\ny = 2", "dif", "x = 1\ndef f():\n|y = 2"),
        ("@dec\ndef f():\n    |pass\ny = 2", "daf", "|y = 2"),
        ("class A:\n    def f(self):\n        |pass\nz", "dac", "|z"),
        ("a\n|b\nc", "yigGp", "a\nb\nc\n|a\nb\nc"),
        ("|x = abc123", "did", "x = ab|c"),
        ("f(\n    a,\n    |b,\n)", "di(", "f(\n|)"),
        ("f(\n    a,\n    |b,\n)", "ci(x<Esc>", "f(\n    |x\n)"),
        ("f(\n    a,\n    |b)", "di(", "f(|)"),
        ("foo |bar baz qux", "d2aw", "foo |qux"),
    ]);
}

#[test]
fn registers_and_paste() {
    check(&[
        ("|a\nb", "yyp", "a\n|a\nb"),
        ("|a\nb", "yyP", "|a\na\nb"),
        ("|a\nb", "yy2p", "a\n|a\na\nb"),
        ("|foo bar", "yiwP", "fo|ofoo bar"),
        ("|foo bar", "yiw$p", "foo barfo|o"),
        ("|foo bar", "dwwP", "bafoo| r"),
        ("|a\nb\nc", "ddp", "b\n|a\nc"),
        ("|ab", "xp", "b|a"),
        ("|foo bar", "\"ayiww\"byiw\"aP", "foo fo|obar"),
        ("|a\nb", "\"ayyj\"Ayy\"ap", "a\nb\n|a\nb"),
        // yank survives a later delete in "0
        ("|foo bar", "yiwwdiw\"0P", "foofo|o "),
        // the black hole keeps the unnamed register
        ("|foo bar", "yiww\"_diwP", "foofo|o "),
        ("|foo bar", "Y$p", "foo barfoo ba|r"),
        ("|foo bar", "yiwwviwp", "foo fo|o"),
        ("|a\nb\nc", "yyjVp", "a\n|a\nc"),
        ("|a b", "ywgp", "aa | b"),
        ("|x", "ifoo <Esc>byiwo<C-r>\"<Esc>", "foo x\nfo|o"),
    ]);
}

#[test]
fn visual() {
    check(&[
        ("|foo bar", "ved", "| bar"),
        ("|foo bar", "vey$p", "foo barfo|o"),
        ("foo |bar baz", "viwd", "foo | baz"),
        ("|a\nb\nc", "Vjd", "|c"),
        ("|a\nb\nc", "Vjy", "|a\nb\nc"),
        ("a\n|b\nc", "Vkd", "|c"),
        ("|foo bar", "vwhU", "|FOO bar"),
        ("|foo bar", "veu", "|foo bar"),
        ("|foo", "v$~", "|FOO"),
        ("ab|cd", "vhhd", "|d"),
        ("|abcd", "vllohd", "|d"),
        ("|a\nb\nc", "VjJ", "a| b\nc"),
        ("|a\nb\nc", "Vj>", "    a\n|    b\nc"),
        // `>` keeps the selection, so it can be repeated
        ("|a\nb\nc", "Vj>><Esc>", "        a\n|        b\nc"),
        ("|abc", "vlrx", "|xxc"),
        ("|foo bar", "vecx<Esc>", "|x bar"),
        ("f(a, |b)", "vi(d", "f(|)"),
        ("a\n|b\nc", "vipd", "|"),
        ("|a\nb", "vjd", "|"),
        ("|abc\nabc\nabc", "<C-v>jjld", "|c\nc\nc"),
        ("|abc\nabc\nabc", "<C-v>jjIx<Esc>", "|xabc\nxabc\nxabc"),
        ("|abc\nabc\nabc", "<C-v>jjAx<Esc>", "a|xbc\naxbc\naxbc"),
        ("|abc\nabc\nabc", "<C-v>jjcx<Esc>", "|xbc\nxbc\nxbc"),
        ("a|bc\nabc", "<C-v>jlyP", "a|bcbc\nabcbc"),
        ("|abc\nabc", "<C-v>jrx", "|xbc\nxbc"),
        ("|foo bar", "viw<Esc>wgvd", "| bar"),
        ("|a\nb\nc", "Vj<A-j>", "c\na\n|b"),
        ("|x = foo(1)", "vauy$p", "x = foo(1)foo(1|)"),
    ]);
}

#[test]
fn insert_mode() {
    check(&[
        ("|abc", "ix<Esc>", "|xabc"),
        ("|abc", "ax<Esc>", "a|xbc"),
        ("  a|bc", "Ix<Esc>", "  |xabc"),
        ("|abc", "Ax<Esc>", "abc|x"),
        ("|abc", "ox<Esc>", "abc\n|x"),
        ("|abc", "Ox<Esc>", "|x\nabc"),
        ("  |abc", "ox<Esc>", "  abc\n  |x"),
        ("|if x:", "oy<Esc>", "if x:\n    |y"),
        ("|abc", "3ix<Esc>", "xx|xabc"),
        ("|abc", "A<CR>x<Esc>", "abc\n|x"),
        ("if x|", "a:<CR>y<Esc>", "if x:\n    |y"),
        ("    ab|c", "i<CR><Esc>", "    ab\n   | c"),
        ("ab|c", "i<BS><Esc>", "|ac"),
        ("a\n|b", "i<BS><Esc>", "|ab"),
        ("        |x", "i<BS><Esc>", "   | x"),
        ("foo bar|", "a<C-w><Esc>", "foo| "),
        ("  foo bar|", "a<C-u><Esc>", " | "),
        ("|x", "i<Tab>y<Esc>", "    |yx"),
        ("a|x", "i<Tab>y<Esc>", "a   |yx"),
        ("|x", "a<C-t><Esc>", "    |x"),
        // auto-pairs
        ("|", "if(<Esc>", "f|()"),
        ("|", "if(x)<Esc>", "f(x|)"),
        ("|", "if(<BS><Esc>", "|f"),
        ("|", "ix = \"a<Esc>", "x = \"|a\""),
        ("|", "ix = \"a\"<Esc>", "x = \"a|\""),
        ("|", "idon't<Esc>", "don'|t"),
        ("|x", "i(<Esc>", "|(x"),
        ("|", "id = {<CR>1<Esc>", "d = {\n    |1\n}"),
        ("|", "i[(<Esc>", "[|()]"),
        ("|a\nb", "i<Del><Esc>", "|\nb"),
        ("a|b", "i<Left>x<Right>y<Esc>", "xa|yb"),
        ("|a\nb", "A<A-j><Esc>", "b\n|a"),
    ]);
}

#[test]
fn undo_redo_dot() {
    check(&[
        ("|foo bar", "dwu", "|foo bar"),
        ("|foo bar", "dwu<C-r>", "|bar"),
        ("|foo bar baz", "dwdwuu", "|foo bar baz"),
        ("|foo bar baz", "dwdw2u", "|foo bar baz"),
        ("|abc", "ixyz<Esc>u", "|abc"),
        ("|foo bar baz", "dw.", "|baz"),
        ("|a\nb\nc\nd", "dd..", "|d"),
        ("|a b c", "cwx<Esc>w.", "x |x c"),
        ("|a\nb\nc", "Ax<Esc>j.j.", "ax\nbx\nc|x"),
        ("|a\nb\nc", "ix<Esc>j.", "xa\n|xb\nc"),
        ("|a\nb\nc", ">>j.", "    a\n    |b\nc"),
        ("|f(a) g(b)", "ci(x<Esc>fb.", "f(x) g(|x)"),
        ("|a\nb\nc\nd", "ddjp", "b\nc\n|a\nd"),
        ("|a\nb\nc", "ddu.", "|b\nc"),
        ("|abc", "xu.", "|bc"),
        ("|a b c d", "dwu..", "|c d"),
        ("|a\nb\nc\nd", "qaddjq@a", "b\n|d"),
        ("|a\nb\nc\nd\ne", "qaA!<Esc>jq2@a", "a!\nb!\nc!\n|d\ne"),
        ("|a\nb\nc\nd", "qaxjq@a@@", "\n\n\n|d"),
        ("|a\nb", "ddu", "|a\nb"),
    ]);
}

#[test]
fn search_and_ex() {
    check(&[
        ("|foo bar foo baz", "/foo<CR>", "foo bar |foo baz"),
        ("|foo bar foo baz", "/foo<CR>n", "|foo bar foo baz"),
        ("|a foo\nb foo\nc foo", "/foo<CR>nN", "a |foo\nb foo\nc foo"),
        ("a foo\nb |foo", "?foo<CR>", "a |foo\nb foo"),
        ("|foo bar foo baz", "*", "foo bar |foo baz"),
        ("foo bar |foo baz", "#", "|foo bar foo baz"),
        ("|foo food foo", "*", "foo food |foo"),
        ("|a b c b", "d/b<CR>", "|b c b"),
        ("|Foo foo", "/foo<CR>", "Foo |foo"),
        ("|x Foo foo", "/Foo<CR>n", "x |Foo foo"),
        ("|a1 b22", "/\\d+<CR>", "a|1 b22"),
        ("|a\nb\nc", ":3<CR>", "a\nb\n|c"),
        ("|a a\nb a", ":s/a/x/<CR>", "|x a\nb a"),
        ("|a a\nb a", ":s/a/x/g<CR>", "|x x\nb a"),
        ("|a a\nb a", ":%s/a/x/g<CR>", "x x\n|b x"),
        ("|a1\nb2", ":%s/(\\w)(\\d)/\\2\\1/<CR>", "1a\n|2b"),
        ("|a\nb\nc", "Vj:s/$/!/<CR>", "a!\n|b!\nc"),
        ("|a/b", ":s#/#-#<CR>", "|a-b"),
        ("|foo", "/zzz<CR>", "|foo"),
        ("|foo bar", "/bar<Esc>", "|foo bar"),
    ]);
}

#[test]
fn lazyvim_extras() {
    check(&[
        ("|x = 1\ny = 2", "gcc", "|# x = 1\ny = 2"),
        ("|# x = 1\ny = 2", "gcc", "|x = 1\ny = 2"),
        ("|x = 1\ny = 2", "gcj", "|# x = 1\n# y = 2"),
        ("|x = 1\ny = 2", "Vjgc", "|# x = 1\n# y = 2"),
        ("if x:\n    |a\n    b", "gcii", "if x:\n    |# a\n    # b"),
        ("|if x:\n    a", "gcip", "|# if x:\n#     a"),
        ("|x", "gcoy<Esc>", "x\n# |y"),
        ("|a\nb\nc", "<A-j>", "b\n|a\nc"),
        ("a\nb\n|c", "<A-k>", "a\n|c\nb"),
        ("|a\nb", "2gcc", "|# a\n# b"),
    ]);
}

#[test]
fn requests() {
    let mut v = load("|a");
    feed(&mut v, ":w<CR>");
    assert_eq!(v.take_requests(), vec![Request::Save]);
    feed(&mut v, ":wq<CR>");
    assert_eq!(v.take_requests(), vec![Request::Save, Request::Leave]);
    feed(&mut v, "ZZ");
    assert_eq!(v.take_requests(), vec![Request::Save, Request::Leave]);
    feed(&mut v, ":bogus<CR>");
    assert!(matches!(v.take_requests().as_slice(), [Request::Message(_)]));
}

#[test]
fn rest_and_pending() {
    let mut v = load("|foo bar");
    assert!(v.at_rest());
    feed(&mut v, "2d");
    assert!(!v.at_rest());
    assert_eq!(v.pending(), "2d");
    feed(&mut v, "<Esc>");
    assert!(v.at_rest());
    assert_eq!(v.pending(), "");
    feed(&mut v, "v");
    assert!(!v.at_rest());
    assert_eq!(v.mode, Mode::Visual);
    feed(&mut v, "<Esc>");
    assert!(v.at_rest());
    feed(&mut v, "/ba");
    assert_eq!(v.cmdline().as_deref(), Some("/ba"));
    feed(&mut v, "<BS><BS><BS>");
    assert!(v.at_rest());
    feed(&mut v, "qa");
    assert_eq!(v.recording(), Some('a'));
    feed(&mut v, "q");
    assert_eq!(v.recording(), None);
}

#[test]
fn external_mode_changes_and_paste() {
    let mut v = load("|abc");
    v.enter_insert();
    feed(&mut v, "x");
    v.enter_normal();
    assert_eq!(show(&v), "|xabc");
    feed(&mut v, "u");
    assert_eq!(show(&v), "|abc");
    // A bracketed paste is literal: no auto-pairs, no auto-indent.
    v.enter_insert();
    v.paste("f(\n  x");
    v.enter_normal();
    assert_eq!(v.text(), "f(\n  xabc");
    feed(&mut v, "u");
    assert_eq!(v.text(), "abc");
}

struct FakeClip(std::rc::Rc<RefCell<String>>);
impl Clip for FakeClip {
    fn get(&mut self) -> Option<String> {
        Some(self.0.borrow().clone())
    }
    fn set(&mut self, text: &str) {
        *self.0.borrow_mut() = text.to_string();
    }
}

#[test]
fn system_clipboard_is_the_unnamed_register() {
    sh(|s| *s = Shared::default());
    let board = std::rc::Rc::new(RefCell::new(String::new()));
    set_clipboard(Box::new(FakeClip(board.clone())));
    let mut v = load("|foo bar\nx");
    feed(&mut v, "yiw");
    assert_eq!(*board.borrow(), "foo");
    feed(&mut v, "yy");
    assert_eq!(*board.borrow(), "foo bar\n");
    feed(&mut v, "p");
    assert_eq!(v.text(), "foo bar\nfoo bar\nx");
    // Text copied elsewhere: charwise unless it ends in a newline.
    *board.borrow_mut() = "ext".to_string();
    feed(&mut v, "P");
    assert_eq!(v.text(), "foo bar\nextfoo bar\nx");
    *board.borrow_mut() = "line\n".to_string();
    feed(&mut v, "p");
    assert_eq!(v.text(), "foo bar\nextfoo bar\nline\nx");
    // A second cell sees the same registers.
    let mut w = load("|");
    feed(&mut w, "p");
    assert_eq!(w.text(), "\nline");
}

#[test]
fn search_highlights() {
    sh(|s| *s = Shared::default());
    let mut v = load("|ab ab\nxab");
    assert!(v.search_matches().is_empty());
    feed(&mut v, "/ab<CR>");
    let m = v.search_matches();
    assert_eq!(m, vec![(pos(0, 0), 2), (pos(0, 3), 2), (pos(1, 1), 2)]);
    feed(&mut v, ":noh<CR>");
    assert!(v.search_matches().is_empty());
    feed(&mut v, "n");
    assert_eq!(v.search_matches().len(), 3);
}

#[test]
fn key_notation() {
    assert_eq!(K::parse("d<Esc><C-r><lt>x"), vec![K::Char('d'), K::Esc, K::Ctrl('r'), K::Char('<'), K::Char('x')]);
    assert_eq!(K::parse("a<b"), vec![K::Char('a'), K::Char('<'), K::Char('b')]);
}

/// Random key storms must never panic or break the buffer invariants.
#[test]
fn fuzz_never_panics() {
    let keys = K::parse(
        "hjklwbeWBE0^$Gggfx;,%{}dcy><~xXpPuJrRsSoOiIaAvV.nN*#\"amq@12345 ()[]'`/:tT_-+|gUzZ\
         <Esc><Esc><Esc><CR><BS><Del><Tab><C-v><C-r><C-a><C-x><C-d><C-u><C-w><A-j><A-k><Left><Right><Up><Down><Home><End>",
    );
    let texts = ["", "x", "def f(a, b):\n    return (a + b) * 2\n\nprint(f(1, 2))", "  \n\n\tx = {'a': [1, 2]}\n", "é ü\n\n\n(\n"];
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    for round in 0..2000 {
        sh(|s| *s = Shared::default());
        let mut v = Vim::new(texts[round % texts.len()]);
        for _ in 0..300 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            v.key(keys[(seed % keys.len() as u64) as usize]);
            assert!(!v.text.is_empty());
            assert!(v.cur.row < v.text.len());
            assert!(v.cur.col <= v.text[v.cur.row].len());
            let _ = v.selection().map(|r| v.spans(&r));
            let _ = v.search_matches();
        }
    }
}
