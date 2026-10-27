//! Cell-level undo/redo (insert, delete, move, kind change, replace).
//! In-cell text editing has its own undo inside the editor.

use super::{Cell, CellKind};

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum CellOp {
    Insert { idx: usize, cell: Cell },
    Delete { idx: usize, cell: Cell },
    Move { from: usize, to: usize },
    SetKind { idx: usize, from: CellKind, to: CellKind },
    Replace { idx: usize, before: Cell, after: Cell },
}

impl CellOp {
    pub fn invert(&self) -> CellOp {
        match self {
            CellOp::Insert { idx, cell } => CellOp::Delete { idx: *idx, cell: cell.clone() },
            CellOp::Delete { idx, cell } => CellOp::Insert { idx: *idx, cell: cell.clone() },
            CellOp::Move { from, to } => CellOp::Move { from: *to, to: *from },
            CellOp::SetKind { idx, from, to } => CellOp::SetKind { idx: *idx, from: *to, to: *from },
            CellOp::Replace { idx, before, after } => {
                CellOp::Replace { idx: *idx, before: after.clone(), after: before.clone() }
            }
        }
    }

    /// Index of the cell the user should be looking at after the op.
    pub fn focus(&self, len: usize) -> usize {
        let i = match self {
            CellOp::Insert { idx, .. } => *idx,
            CellOp::Delete { idx, .. } => *idx,
            CellOp::Move { to, .. } => *to,
            CellOp::SetKind { idx, .. } => *idx,
            CellOp::Replace { idx, .. } => *idx,
        };
        i.min(len.saturating_sub(1))
    }
}

pub fn apply(op: &CellOp, cells: &mut Vec<Cell>) {
    match op {
        CellOp::Insert { idx, cell } => {
            let idx = (*idx).min(cells.len());
            cells.insert(idx, cell.clone());
        }
        CellOp::Delete { idx, .. } => {
            if *idx < cells.len() {
                cells.remove(*idx);
            }
            if cells.is_empty() {
                cells.push(Cell::new(CellKind::Code, ""));
            }
        }
        CellOp::Move { from, to } => {
            if *from < cells.len() && *to < cells.len() {
                let c = cells.remove(*from);
                cells.insert(*to, c);
            }
        }
        CellOp::SetKind { idx, to, .. } => {
            if let Some(c) = cells.get_mut(*idx) {
                c.kind = *to;
                if *to != CellKind::Code {
                    c.outputs.clear();
                    c.execution_count = None;
                }
            }
        }
        CellOp::Replace { idx, after, .. } => {
            if let Some(c) = cells.get_mut(*idx) {
                *c = after.clone();
            }
        }
    }
}

#[derive(Default)]
pub struct UndoStack {
    undo: Vec<CellOp>,
    redo: Vec<CellOp>,
}

const CAP: usize = 200;

impl UndoStack {
    pub fn push(&mut self, op: CellOp) {
        self.undo.push(op);
        if self.undo.len() > CAP {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// Undo the last op; returns the index to focus.
    pub fn undo(&mut self, cells: &mut Vec<Cell>) -> Option<usize> {
        let op = self.undo.pop()?;
        let inv = op.invert();
        apply(&inv, cells);
        let focus = inv.focus(cells.len());
        self.redo.push(op);
        Some(focus)
    }

    pub fn redo(&mut self, cells: &mut Vec<Cell>) -> Option<usize> {
        let op = self.redo.pop()?;
        apply(&op, cells);
        let focus = op.focus(cells.len());
        self.undo.push(op);
        Some(focus)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
}
