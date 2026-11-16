//! In-memory notebook model and conversion to/from nbformat v4.

pub mod io;
pub mod undo;

use std::path::PathBuf;

use jupyter_protocol::{ExecutionCount, Media};
use nbformat::v4;
pub use nbformat::v4::CellId;
use serde_json::{Map, Value};

use crate::editor::CellEditor;
use undo::{CellOp, UndoStack};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellKind {
    Code,
    Markdown,
    Raw,
}

impl CellKind {
    pub fn label(self) -> &'static str {
        match self {
            CellKind::Code => "code",
            CellKind::Markdown => "markdown",
            CellKind::Raw => "raw",
        }
    }
}

/// Runtime-only execution state of a code cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecState {
    #[default]
    Idle,
    Queued,
    Running,
    Done,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamName {
    Stdout,
    Stderr,
}

impl StreamName {
    pub fn as_str(self) -> &'static str {
        match self {
            StreamName::Stdout => "stdout",
            StreamName::Stderr => "stderr",
        }
    }
}

#[derive(Debug, Clone)]
pub enum Output {
    Stream {
        name: StreamName,
        text: String,
    },
    DisplayData {
        data: Media,
        metadata: Map<String, Value>,
        display_id: Option<String>,
    },
    ExecuteResult {
        execution_count: i32,
        data: Media,
        metadata: Map<String, Value>,
    },
    Error {
        ename: String,
        evalue: String,
        traceback: Vec<String>,
    },
}

impl Output {
    pub fn media(&self) -> Option<&Media> {
        match self {
            Output::DisplayData { data, .. } | Output::ExecuteResult { data, .. } => Some(data),
            _ => None,
        }
    }

    pub fn media_mut(&mut self) -> Option<&mut Media> {
        match self {
            Output::DisplayData { data, .. } | Output::ExecuteResult { data, .. } => Some(data),
            _ => None,
        }
    }
}

pub struct Cell {
    pub id: CellId,
    pub kind: CellKind,
    pub source: String,
    pub metadata: v4::CellMetadata,
    pub attachments: Option<Value>,
    pub execution_count: Option<i32>,
    pub outputs: Vec<Output>,
    pub exec: ExecState,
    pub editor: Option<CellEditor>,
    /// Outputs hidden (`o`).
    pub outputs_collapsed: bool,
    /// Long streams shown in full (`o` on a truncated output).
    pub outputs_expanded: bool,
    /// A `clear_output(wait=True)` is pending: clear before the next output.
    pub clear_pending: bool,
    /// An edit the assistant proposes, shown as a diff until it is settled.
    pub proposal: Option<crate::ui::diff::Proposal>,
    /// The inline prompt to the assistant, while it is open on this cell.
    pub prompt: Option<crate::ui::inline_prompt::InlinePrompt>,
}

impl Clone for Cell {
    fn clone(&self) -> Self {
        Cell {
            id: self.id.clone(),
            kind: self.kind,
            source: self.source.clone(),
            metadata: self.metadata.clone(),
            attachments: self.attachments.clone(),
            execution_count: self.execution_count,
            outputs: self.outputs.clone(),
            exec: ExecState::Idle,
            editor: None,
            outputs_collapsed: self.outputs_collapsed,
            outputs_expanded: self.outputs_expanded,
            clear_pending: false,
            proposal: None,
            prompt: None,
        }
    }
}

impl std::fmt::Debug for Cell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cell")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("source", &self.source)
            .field("outputs", &self.outputs.len())
            .finish()
    }
}

impl Cell {
    pub fn new(kind: CellKind, source: impl Into<String>) -> Self {
        Cell {
            id: CellId::from(uuid::Uuid::new_v4()),
            kind,
            source: source.into(),
            metadata: v4::CellMetadata::default(),
            attachments: None,
            execution_count: None,
            outputs: Vec::new(),
            exec: ExecState::Idle,
            editor: None,
            outputs_collapsed: false,
            outputs_expanded: false,
            clear_pending: false,
            proposal: None,
            prompt: None,
        }
    }

    /// A fresh copy with a new id (for paste).
    pub fn duplicate(&self) -> Self {
        let mut c = self.clone();
        c.id = CellId::from(uuid::Uuid::new_v4());
        c
    }

    /// Current text: the live editor if one exists, else the stored source.
    pub fn text(&self) -> String {
        match &self.editor {
            Some(ed) => ed.text(),
            None => self.source.clone(),
        }
    }

    /// Copy the editor contents back into `source`. Returns true if changed.
    pub fn sync_from_editor(&mut self) -> bool {
        if let Some(ed) = &self.editor {
            let t = ed.text();
            if t != self.source {
                self.source = t;
                return true;
            }
        }
        false
    }

    pub fn set_source(&mut self, s: String) {
        self.source = s;
        self.editor = None;
    }

    /// Append stream text, merging with a preceding stream of the same name.
    pub fn push_stream(&mut self, name: StreamName, text: &str) {
        if let Some(Output::Stream { name: last, text: buf }) = self.outputs.last_mut()
            && *last == name
        {
            buf.push_str(text);
            return;
        }
        self.outputs.push(Output::Stream { name, text: text.to_string() });
    }

    fn from_v4(cell: v4::Cell) -> Self {
        match cell {
            v4::Cell::Code { id, metadata, execution_count, source, outputs } => Cell {
                id,
                kind: CellKind::Code,
                source: source.concat(),
                metadata,
                attachments: None,
                execution_count,
                outputs: outputs.into_iter().map(Output::from_v4).collect(),
                exec: ExecState::Idle,
                editor: None,
                outputs_collapsed: false,
                outputs_expanded: false,
                clear_pending: false,
            proposal: None,
            prompt: None,
            },
            v4::Cell::Markdown { id, metadata, source, attachments } => Cell {
                id,
                kind: CellKind::Markdown,
                source: source.concat(),
                metadata,
                attachments,
                execution_count: None,
                outputs: Vec::new(),
                exec: ExecState::Idle,
                editor: None,
                outputs_collapsed: false,
                outputs_expanded: false,
                clear_pending: false,
            proposal: None,
            prompt: None,
            },
            v4::Cell::Raw { id, metadata, source } => Cell {
                id,
                kind: CellKind::Raw,
                source: source.concat(),
                metadata,
                attachments: None,
                execution_count: None,
                outputs: Vec::new(),
                exec: ExecState::Idle,
                editor: None,
                outputs_collapsed: false,
                outputs_expanded: false,
                clear_pending: false,
            proposal: None,
            prompt: None,
            },
        }
    }

    fn to_v4(&self) -> v4::Cell {
        let source = split_lines(&self.text());
        match self.kind {
            CellKind::Code => v4::Cell::Code {
                id: self.id.clone(),
                metadata: self.metadata.clone(),
                execution_count: self.execution_count,
                source,
                outputs: self.outputs.iter().map(Output::to_v4).collect(),
            },
            CellKind::Markdown => v4::Cell::Markdown {
                id: self.id.clone(),
                metadata: self.metadata.clone(),
                source,
                attachments: self.attachments.clone(),
            },
            CellKind::Raw => v4::Cell::Raw {
                id: self.id.clone(),
                metadata: self.metadata.clone(),
                source,
            },
        }
    }
}

/// Split text into nbformat "multiline string" chunks (each keeps its newline).
pub fn split_lines(s: &str) -> Vec<String> {
    s.split_inclusive('\n').map(str::to_string).collect()
}

impl Output {
    fn from_v4(o: v4::Output) -> Self {
        match o {
            v4::Output::Stream { name, text } => Output::Stream {
                name: if name == "stderr" { StreamName::Stderr } else { StreamName::Stdout },
                text: text.0,
            },
            v4::Output::DisplayData(d) => Output::DisplayData {
                data: d.data,
                metadata: d.metadata,
                display_id: None,
            },
            v4::Output::ExecuteResult(r) => Output::ExecuteResult {
                execution_count: r.execution_count.value() as i32,
                data: r.data,
                metadata: r.metadata,
            },
            v4::Output::Error(e) => Output::Error {
                ename: e.ename,
                evalue: e.evalue,
                traceback: e.traceback,
            },
        }
    }

    fn to_v4(&self) -> v4::Output {
        match self {
            Output::Stream { name, text } => v4::Output::Stream {
                name: name.as_str().to_string(),
                text: v4::MultilineString(text.clone()),
            },
            Output::DisplayData { data, metadata, .. } => v4::Output::DisplayData(v4::DisplayData {
                data: data.clone(),
                metadata: metadata.clone(),
            }),
            Output::ExecuteResult { execution_count, data, metadata } => {
                v4::Output::ExecuteResult(v4::ExecuteResult {
                    execution_count: ExecutionCount::new((*execution_count).max(0) as usize),
                    data: data.clone(),
                    metadata: metadata.clone(),
                })
            }
            Output::Error { ename, evalue, traceback } => v4::Output::Error(v4::ErrorOutput {
                ename: ename.clone(),
                evalue: evalue.clone(),
                traceback: traceback.clone(),
            }),
        }
    }
}

pub struct Notebook {
    pub path: Option<PathBuf>,
    pub metadata: v4::Metadata,
    pub cells: Vec<Cell>,
    pub dirty: bool,
    pub undo: UndoStack,
}

impl Notebook {
    pub fn empty() -> Self {
        Notebook {
            path: None,
            metadata: v4::Metadata::default(),
            cells: vec![Cell::new(CellKind::Code, "")],
            dirty: false,
            undo: UndoStack::default(),
        }
    }

    pub fn from_v4(nb: v4::Notebook, path: Option<PathBuf>) -> Self {
        let mut cells: Vec<Cell> = nb.cells.into_iter().map(Cell::from_v4).collect();
        if cells.is_empty() {
            cells.push(Cell::new(CellKind::Code, ""));
        }
        Notebook {
            path,
            metadata: nb.metadata,
            cells,
            dirty: false,
            undo: UndoStack::default(),
        }
    }

    pub fn to_v4(&self) -> v4::Notebook {
        v4::Notebook {
            metadata: self.metadata.clone(),
            nbformat: 4,
            nbformat_minor: 5,
            cells: self.cells.iter().map(Cell::to_v4).collect(),
        }
    }

    pub fn title(&self) -> String {
        match &self.path {
            Some(p) => p
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.display().to_string()),
            None => "Untitled.ipynb".to_string(),
        }
    }

    pub fn kernelspec_name(&self) -> Option<&str> {
        self.metadata.kernelspec.as_ref().map(|k| k.name.as_str())
    }

    pub fn language(&self) -> Option<&str> {
        self.metadata
            .language_info
            .as_ref()
            .map(|l| l.name.as_str())
            .or_else(|| self.metadata.kernelspec.as_ref().and_then(|k| k.language.as_deref()))
    }

    /// Any output carrying a plotly figure (which the sidecar may have to render).
    pub fn has_plotly_outputs(&self) -> bool {
        self.cells.iter().flat_map(|c| &c.outputs).filter_map(Output::media).any(|m| {
            m.content.iter().any(|t| matches!(t, jupyter_protocol::MediaType::Plotly(_)))
        })
    }

    /// Any code cell that looks like it produces plotly figures.
    pub fn mentions_plotly(&self) -> bool {
        self.cells.iter().any(|c| c.kind == CellKind::Code && c.source.contains("plotly"))
    }

    pub fn set_kernelspec(&mut self, name: &str, display_name: &str, language: &str) {
        let changed = self.kernelspec_name() != Some(name);
        self.metadata.kernelspec = Some(v4::KernelSpec {
            display_name: display_name.to_string(),
            name: name.to_string(),
            language: Some(language.to_string()),
            additional: Default::default(),
        });
        if changed {
            self.dirty = true;
        }
    }

    pub fn set_language_info(&mut self, name: &str, version: &str) {
        let info = v4::LanguageInfo {
            name: name.to_string(),
            version: Some(version.to_string()),
            codemirror_mode: None,
            additional: Default::default(),
        };
        match &mut self.metadata.language_info {
            Some(existing) if existing.name == name => {
                existing.version = Some(version.to_string());
            }
            _ => {
                self.metadata.language_info = Some(info);
                self.dirty = true;
            }
        }
    }

    pub fn index_of(&self, id: &CellId) -> Option<usize> {
        self.cells.iter().position(|c| &c.id == id)
    }

    pub fn cell_by_id_mut(&mut self, id: &CellId) -> Option<&mut Cell> {
        self.cells.iter_mut().find(|c| &c.id == id)
    }

    /// Apply a cell operation, record it for undo, and mark dirty.
    pub fn apply(&mut self, op: CellOp) {
        undo::apply(&op, &mut self.cells);
        self.undo.push(op);
        self.dirty = true;
    }
}
