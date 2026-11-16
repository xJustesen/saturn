//! The tools saturn gives the assistant for the open notebook: what they are
//! called, what they take, and the parts that only read the notebook. The
//! app runs them (`App::on_tool_call`), because most of them change it.

use serde_json::{Value, json};

use crate::ai::{MCP_SERVER, ToolSpec};
use crate::notebook::{Cell, CellKind, ExecState, Notebook, Output};
use crate::ui::text::{clean_text, cut};

pub const OUTLINE: &str = "notebook_outline";
pub const READ: &str = "read_cells";
pub const EDIT: &str = "propose_edit";
pub const INSERT: &str = "insert_cell";
pub const DELETE: &str = "delete_cell";
pub const RUN: &str = "run_cells";

fn spec(name: &str, description: &str, properties: Value, required: &[&str]) -> ToolSpec {
    ToolSpec {
        name: name.to_string(),
        description: description.to_string(),
        input_schema: json!({ "type": "object", "properties": properties, "required": required }),
    }
}

pub fn specs() -> Vec<ToolSpec> {
    let cell = json!({ "type": "integer", "minimum": 1, "description": "Cell number, counted from 1 as in notebook_outline." });
    let cells = json!({ "type": "array", "items": { "type": "integer", "minimum": 1 }, "minItems": 1, "description": "Cell numbers, counted from 1 as in notebook_outline." });
    vec![
        spec(
            OUTLINE,
            "List every cell of the open notebook: number, kind, execution state, first line and what its output holds. \
             Cell numbers change when cells are inserted or deleted, so call this again after such a change.",
            json!({}),
            &[],
        ),
        spec(
            READ,
            "Read cells of the open notebook in full: source, text outputs, errors, and figures as images.",
            json!({ "cells": cells }),
            &["cells"],
        ),
        spec(
            EDIT,
            "Propose a new source for one cell. The user sees a diff in the notebook and accepts or rejects it; \
             the result says which. Give the complete new source, not a patch.",
            json!({ "cell": cell, "source": { "type": "string", "description": "The complete new source of the cell." } }),
            &["cell", "source"],
        ),
        spec(
            INSERT,
            "Propose a new cell. The user sees it in the notebook and keeps or discards it; the result says which.",
            json!({
                "after": { "type": "integer", "minimum": 0, "description": "Number of the cell the new one goes after; 0 puts it first." },
                "kind": { "type": "string", "enum": ["code", "markdown"] },
                "source": { "type": "string" }
            }),
            &["after", "kind", "source"],
        ),
        spec(
            DELETE,
            "Propose deleting one cell. The user confirms or refuses; the result says which.",
            json!({ "cell": cell }),
            &["cell"],
        ),
        spec(
            RUN,
            "Run code cells in the notebook's kernel, in the order given, and return their outputs \
             (text, errors, figures as images). Running stops at the first error.",
            json!({ "cells": cells }),
            &["cells"],
        ),
    ]
}

/// The tools that run without a permission request: they only read, or the
/// user reviews their effect in the notebook. Running code still asks.
pub fn allowed() -> Vec<String> {
    [OUTLINE, READ, EDIT, INSERT, DELETE]
        .iter()
        .map(|t| format!("mcp__{MCP_SERVER}__{t}"))
        .collect()
}

/// The 0-based index for the cell number under `key`.
pub fn cell_index(args: &Value, key: &str, n: usize) -> Result<usize, String> {
    let v = args
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("`{key}` must be a cell number"))?;
    number_to_index(v, n)
}

fn number_to_index(v: u64, n: usize) -> Result<usize, String> {
    if v == 0 || v as usize > n {
        return Err(format!(
            "there is no cell {v}: the notebook has cells 1 to {n}"
        ));
    }
    Ok(v as usize - 1)
}

/// The 0-based indices for the cell numbers under `cells`.
pub fn cell_indices(args: &Value, n: usize) -> Result<Vec<usize>, String> {
    let list = args
        .get("cells")
        .and_then(Value::as_array)
        .filter(|l| !l.is_empty())
        .ok_or("`cells` must be a list of cell numbers")?;
    list.iter()
        .map(|v| {
            v.as_u64()
                .ok_or_else(|| "`cells` must be a list of cell numbers".to_string())
                .and_then(|v| number_to_index(v, n))
        })
        .collect()
}

pub fn text_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("`{key}` must be a string"))
}

fn output_note(cell: &Cell) -> String {
    use jupyter_protocol::MediaType as M;
    let mut notes: Vec<String> = Vec::new();
    let mut push = |s: String| {
        if !notes.contains(&s) {
            notes.push(s);
        }
    };
    for o in &cell.outputs {
        match o {
            Output::Stream { .. } => push("text".into()),
            Output::Error { ename, .. } => push(format!("error {ename}")),
            Output::DisplayData { data, .. } | Output::ExecuteResult { data, .. } => {
                let figure = data
                    .content
                    .iter()
                    .any(|t| matches!(t, M::Png(_) | M::Jpeg(_) | M::Gif(_) | M::Plotly(_)));
                push(if figure {
                    "figure".into()
                } else {
                    "value".into()
                });
            }
        }
    }
    notes.join(", ")
}

/// One line per cell, for `notebook_outline`.
pub fn outline(nb: &Notebook, focused: usize) -> String {
    let mut out = format!(
        "{} ({}), {} cells. The user is on cell {}.\n",
        nb.title(),
        nb.language().unwrap_or("unknown language"),
        nb.cells.len(),
        focused + 1
    );
    for (i, cell) in nb.cells.iter().enumerate() {
        let kind = match cell.kind {
            CellKind::Code => "code",
            CellKind::Markdown => "markdown",
            CellKind::Raw => "raw",
        };
        let state = match (cell.kind, cell.exec, cell.execution_count) {
            (CellKind::Code, ExecState::Running, _) => " running".to_string(),
            (CellKind::Code, ExecState::Queued, _) => " queued".to_string(),
            (CellKind::Code, ExecState::Error, _) => " failed".to_string(),
            (CellKind::Code, _, Some(n)) => format!(" run [{n}]"),
            (CellKind::Code, _, None) => " not run".to_string(),
            _ => String::new(),
        };
        let source = cell.text();
        let source = clean_text(&source);
        let mut lines = source.lines();
        let first = cut(lines.next().unwrap_or("").trim(), 80);
        let more = lines.count();
        out.push_str(&format!("{}. {kind}{state}: {first}", i + 1));
        if more > 0 {
            out.push_str(&format!(" (+{more} lines)"));
        }
        let note = output_note(cell);
        if !note.is_empty() {
            out.push_str(&format!(" -> {note}"));
        }
        if cell.proposal.is_some() {
            out.push_str(" [a proposed change is waiting for the user]");
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notebook::StreamName;

    #[test]
    fn every_tool_has_an_object_schema_and_readers_need_no_permission() {
        let specs = specs();
        assert_eq!(
            specs.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            [OUTLINE, READ, EDIT, INSERT, DELETE, RUN]
        );
        assert!(specs.iter().all(|s| s.input_schema["type"] == "object"));
        let allowed = allowed();
        assert!(allowed.contains(&"mcp__saturn__read_cells".to_string()));
        assert!(!allowed.contains(&"mcp__saturn__run_cells".to_string()));
    }

    #[test]
    fn cell_numbers_count_from_one_and_are_checked() {
        assert_eq!(cell_index(&json!({"cell": 2}), "cell", 3), Ok(1));
        assert!(
            cell_index(&json!({"cell": 0}), "cell", 3)
                .unwrap_err()
                .contains("cells 1 to 3")
        );
        assert!(cell_index(&json!({"cell": 4}), "cell", 3).is_err());
        assert!(cell_index(&json!({"cell": "2"}), "cell", 3).is_err());
        assert_eq!(cell_indices(&json!({"cells": [3, 1]}), 3), Ok(vec![2, 0]));
        assert!(cell_indices(&json!({"cells": []}), 3).is_err());
        assert!(cell_indices(&json!({"cells": [1, 9]}), 3).is_err());
    }

    #[test]
    fn the_outline_has_one_line_per_cell() {
        let mut nb = Notebook::empty();
        let mut code = Cell::new(
            CellKind::Code,
            "import pandas as pd\ndf = pd.DataFrame()\ndf",
        );
        code.execution_count = Some(4);
        code.outputs.push(Output::Stream {
            name: StreamName::Stdout,
            text: "x".into(),
        });
        code.outputs.push(Output::Error {
            ename: "KeyError".into(),
            evalue: "a".into(),
            traceback: Vec::new(),
        });
        nb.cells = vec![
            Cell::new(CellKind::Markdown, "# Title"),
            code,
            Cell::new(CellKind::Code, ""),
        ];
        let text = outline(&nb, 1);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[0],
            "Untitled.ipynb (unknown language), 3 cells. The user is on cell 2."
        );
        assert_eq!(lines[1], "1. markdown: # Title");
        assert_eq!(
            lines[2],
            "2. code run [4]: import pandas as pd (+2 lines) -> text, error KeyError"
        );
        assert_eq!(lines[3], "3. code not run: ");
    }
}
