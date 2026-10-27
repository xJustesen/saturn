//! Key bindings: global chords (active in every mode) and the notebook
//! command-mode table, including multi-key sequences such as `dd` and `gg`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::commands::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

const fn k(code: KeyCode) -> Key {
    Key { code, mods: KeyModifiers::NONE }
}
const fn ch(c: char) -> Key {
    Key { code: KeyCode::Char(c), mods: KeyModifiers::NONE }
}
const fn ctrl(c: char) -> Key {
    Key { code: KeyCode::Char(c), mods: KeyModifiers::CONTROL }
}
const fn alt(code: KeyCode) -> Key {
    Key { code, mods: KeyModifiers::ALT }
}
const fn shift(code: KeyCode) -> Key {
    Key { code, mods: KeyModifiers::SHIFT }
}

/// Normalise a crossterm key event into our comparable form: shifted
/// characters already carry their case, so drop SHIFT for them; lowercase
/// control characters.
pub fn normalize(ev: &KeyEvent) -> Key {
    let mut mods = ev.modifiers & (KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT);
    let code = match ev.code {
        KeyCode::Char(c) => {
            mods.remove(KeyModifiers::SHIFT);
            if mods.contains(KeyModifiers::CONTROL) {
                KeyCode::Char(c.to_ascii_lowercase())
            } else {
                KeyCode::Char(c)
            }
        }
        other => other,
    };
    Key { code, mods }
}

/// Bindings that work in every mode, including while editing a cell.
const GLOBAL: &[(Key, Command)] = &[
    (ctrl('s'), Command::Save),
    (shift(KeyCode::Enter), Command::RunAndAdvance),
    (Key { code: KeyCode::Enter, mods: KeyModifiers::CONTROL }, Command::RunCell),
    (alt(KeyCode::Enter), Command::RunAndInsert),
    (ctrl('p'), Command::CommandPalette),
    (ctrl('o'), Command::OpenFile),
    (ctrl('q'), Command::Quit),
    (ctrl('c'), Command::InterruptKernel),
    (Key { code: KeyCode::Char('_'), mods: KeyModifiers::CONTROL }, Command::SplitCell),
    (Key { code: KeyCode::Char('-'), mods: KeyModifiers::CONTROL.union(KeyModifiers::SHIFT) }, Command::SplitCell),
];

/// Notebook command-mode bindings. Sequences are matched as prefixes.
const COMMAND: &[(&[Key], Command)] = &[
    (&[ch('j')], Command::FocusNext),
    (&[k(KeyCode::Down)], Command::FocusNext),
    (&[ch('k')], Command::FocusPrev),
    (&[k(KeyCode::Up)], Command::FocusPrev),
    (&[ch('g'), ch('g')], Command::FocusFirst),
    (&[ch('G')], Command::FocusLast),
    (&[k(KeyCode::Home)], Command::FocusFirst),
    (&[k(KeyCode::End)], Command::FocusLast),
    (&[ctrl('d')], Command::HalfPageDown),
    (&[ctrl('u')], Command::HalfPageUp),
    (&[k(KeyCode::PageDown)], Command::HalfPageDown),
    (&[k(KeyCode::PageUp)], Command::HalfPageUp),
    (&[ctrl('e')], Command::ScrollDown),
    (&[ctrl('y')], Command::ScrollUp),
    (&[k(KeyCode::Enter)], Command::EditCell),
    (&[ch('i')], Command::EditCellInsert),
    (&[ch('a')], Command::InsertAbove),
    (&[ch('b')], Command::InsertBelow),
    (&[ch('d'), ch('d')], Command::DeleteCell),
    (&[k(KeyCode::Delete)], Command::DeleteCell),
    (&[ch('x')], Command::CutCell),
    (&[ch('c')], Command::CopyCell),
    (&[ch('v')], Command::PasteBelow),
    (&[ch('V')], Command::PasteAbove),
    (&[ctrl('k')], Command::MoveCellUp),
    (&[ctrl('j')], Command::MoveCellDown),
    (&[ch('m')], Command::ToMarkdown),
    (&[ch('y')], Command::ToCode),
    (&[ch('r')], Command::ToRaw),
    (&[ch('M')], Command::MergeCellBelow),
    (&[ch('u')], Command::UndoCellOp),
    (&[ctrl('r')], Command::RedoCellOp),
    (&[ch('o')], Command::ToggleOutputs),
    (&[ch('O')], Command::ClearOutputs),
    (&[ch('0'), ch('0')], Command::RestartKernel),
    (&[ch('I')], Command::InterruptKernel),
    (&[ch('K')], Command::SelectKernel),
    (&[ch('?')], Command::Help),
    (&[ch('q')], Command::Quit),
    (&[k(KeyCode::Esc)], Command::Escape),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    Command(Command),
    /// The key starts a multi-key sequence; wait for more input.
    Pending,
    Unbound,
}

pub fn global(ev: &KeyEvent) -> Option<Command> {
    let key = normalize(ev);
    GLOBAL.iter().find(|(k, _)| *k == key).map(|(_, c)| *c)
}

pub fn command_mode(pending: &[Key], ev: &KeyEvent) -> KeyAction {
    let key = normalize(ev);
    let mut seq: Vec<Key> = pending.to_vec();
    seq.push(key);
    let mut is_prefix = false;
    for (keys, cmd) in COMMAND {
        if *keys == seq.as_slice() {
            return KeyAction::Command(*cmd);
        }
        if keys.len() > seq.len() && keys[..seq.len()] == seq[..] {
            is_prefix = true;
        }
    }
    if is_prefix { KeyAction::Pending } else { KeyAction::Unbound }
}

/// Human readable label for a pending key sequence (status bar).
pub fn describe(keys: &[Key]) -> String {
    keys.iter()
        .map(|k| match k.code {
            KeyCode::Char(c) => c.to_string(),
            other => format!("{other:?}"),
        })
        .collect::<Vec<_>>()
        .join("")
}
