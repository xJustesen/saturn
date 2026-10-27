//! Kernel management: one actor task per running kernel, talking the Jupyter
//! wire protocol over ZeroMQ. The UI only sees `KernelCommand`/`KernelEvent`.

pub mod actor;
pub mod startup;

use std::path::PathBuf;

use jupyter_protocol::Media;
pub use jupyter_zmq_client::KernelspecDir;
use serde_json::{Map, Value};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::task::JoinHandle;

use crate::app::AppEvent;
use crate::notebook::{CellId, Output};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelStatus {
    Starting,
    Idle,
    Busy,
    Restarting,
    Dead,
}

impl KernelStatus {
    pub fn label(self) -> &'static str {
        match self {
            KernelStatus::Starting => "starting",
            KernelStatus::Idle => "idle",
            KernelStatus::Busy => "busy",
            KernelStatus::Restarting => "restarting",
            KernelStatus::Dead => "dead",
        }
    }
}

#[derive(Debug)]
pub enum KernelCommand {
    Execute { cell_id: CellId, code: String },
    /// Run code without a cell (startup hooks); outputs are discarded.
    ExecuteSilent { code: String },
    Interrupt,
    Restart,
    Shutdown,
    InputReply { value: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyStatus {
    Ok,
    Error,
    Aborted,
}

#[derive(Debug)]
pub enum KernelEvent {
    Status(KernelStatus),
    Ready { implementation: String, language: String, version: String },
    ExecuteStarted { cell_id: CellId },
    ExecuteInput { cell_id: CellId, execution_count: i32 },
    Output { cell_id: CellId, output: Output },
    UpdateDisplay { display_id: String, data: Media, metadata: Map<String, Value> },
    ClearOutput { cell_id: CellId, wait: bool },
    ExecuteDone { cell_id: CellId, status: ReplyStatus, execution_count: Option<i32> },
    InputRequest { cell_id: Option<CellId>, prompt: String, password: bool },
    QueueChanged { queued: Vec<CellId> },
    Died { reason: String },
    Log(String),
}

pub struct KernelHandle {
    pub spec: KernelspecDir,
    pub status: KernelStatus,
    pub language: String,
    cmd_tx: UnboundedSender<KernelCommand>,
    task: JoinHandle<()>,
}

impl KernelHandle {
    pub fn launch(spec: KernelspecDir, cwd: PathBuf, tx: UnboundedSender<AppEvent>) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let language = spec.kernelspec.language.clone();
        let task = tokio::spawn(actor::run(spec.clone(), cwd, tx, cmd_rx));
        KernelHandle { spec, status: KernelStatus::Starting, language, cmd_tx, task }
    }

    pub fn name(&self) -> &str {
        &self.spec.kernel_name
    }

    pub fn display_name(&self) -> &str {
        &self.spec.kernelspec.display_name
    }

    pub fn send(&self, cmd: KernelCommand) {
        if self.cmd_tx.send(cmd).is_err() {
            tracing::warn!("kernel actor is gone");
        }
    }

    pub fn is_alive(&self) -> bool {
        !self.task.is_finished() && self.status != KernelStatus::Dead
    }

    pub fn abort(&self) {
        self.task.abort();
    }
}

impl KernelHandle {
    /// Wait for the actor task to finish (after a Shutdown command).
    pub async fn wait(self) {
        let _ = self.task.await;
    }
}

/// Discover kernelspecs: the standard Jupyter data dirs plus kernels that
/// `uv tool install` placed next to tools such as jupyterlab.
pub async fn discover_kernelspecs() -> Vec<KernelspecDir> {
    let mut specs = jupyter_zmq_client::list_kernelspecs().await;
    let mut extra_dirs: Vec<PathBuf> = Vec::new();
    // uv keeps tools under $UV_TOOL_DIR, else $XDG_DATA_HOME/uv/tools, else
    // ~/.local/share/uv/tools (also on macOS).
    let mut tool_dirs: Vec<PathBuf> = Vec::new();
    if let Ok(d) = std::env::var("UV_TOOL_DIR") {
        tool_dirs.push(PathBuf::from(d));
    }
    if let Ok(d) = std::env::var("XDG_DATA_HOME") {
        tool_dirs.push(PathBuf::from(d).join("uv").join("tools"));
    }
    if let Some(home) = dirs::home_dir() {
        tool_dirs.push(home.join(".local").join("share").join("uv").join("tools"));
    }
    if let Some(data) = dirs::data_dir() {
        tool_dirs.push(data.join("uv").join("tools"));
    }
    for tools in tool_dirs {
        if let Ok(rd) = std::fs::read_dir(&tools) {
            for e in rd.flatten() {
                extra_dirs.push(e.path().join("share").join("jupyter"));
            }
        }
    }
    if let Ok(venv) = std::env::var("VIRTUAL_ENV") {
        extra_dirs.push(PathBuf::from(venv).join("share").join("jupyter"));
    }
    if let Ok(extra) = std::env::var("SATURN_KERNEL_DIRS") {
        extra_dirs.extend(extra.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
    }
    for dir in extra_dirs {
        if !dir.join("kernels").is_dir() {
            continue;
        }
        for mut spec in jupyter_zmq_client::read_kernelspec_jsons(&dir).await {
            // The native ipykernel spec says `python`, meaning "the
            // interpreter of the environment this spec lives in".
            if let Some(argv0) = spec.kernelspec.argv.first_mut()
                && (argv0 == "python" || argv0 == "python3")
                && let Some(prefix) = dir.parent().and_then(|share| share.parent())
            {
                let candidate = prefix.join("bin").join(argv0.as_str());
                if candidate.is_file() {
                    *argv0 = candidate.display().to_string();
                }
            }
            if !specs.iter().any(|s| s.kernel_name == spec.kernel_name) {
                specs.push(spec);
            }
        }
    }
    specs.sort_by(|a, b| a.kernelspec.display_name.cmp(&b.kernelspec.display_name));
    specs
}
