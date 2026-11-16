//! The built-in assistant's backend: saturn drives the user's own installed,
//! logged-in `claude` CLI as a long-lived child process and speaks its
//! stream-json protocol on stdin/stdout, so the assistant runs on the Claude
//! subscription with no API key. The UI only sees `AiCommand`/`AiEvent`.
//!
//! Saturn's own tools are served in-band: the CLI sends MCP JSON-RPC as
//! control requests on stdout and the client answers on stdin. `initialize`
//! and `tools/list` are answered here from `AiConfig::tools`; a `tools/call`
//! becomes `AiEvent::ToolCall` for the host to answer with
//! `AiCommand::ToolReply`. Permission prompts for the CLI's own tools arrive
//! the same way as `AiEvent::PermissionRequest`.

pub mod client;
pub mod context;
pub mod proto;
pub mod tools;

use std::path::PathBuf;

use serde_json::Value;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::task::JoinHandle;

use crate::app::AppEvent;

pub use proto::Usage;

/// The CLI release the control protocol was recorded and verified against.
/// Older releases are untested; `version_supported` says which side a given
/// version is on.
pub const MIN_CLI_VERSION: &str = "2.1.314";

/// The name of saturn's in-band MCP server. The model sees its tools as
/// `mcp__saturn__<tool>`.
pub const MCP_SERVER: &str = "saturn";

/// Whether a `claude --version` string (`2.1.314`, `2.1.314 (Claude Code)`)
/// is at least `MIN_CLI_VERSION`. An unreadable version counts as too old.
pub fn version_supported(version: &str) -> bool {
    fn parts(v: &str) -> Option<Vec<u32>> {
        v.split_whitespace()
            .next()?
            .split('.')
            .map(|p| p.parse().ok())
            .collect()
    }
    match (parts(version), parts(MIN_CLI_VERSION)) {
        (Some(have), Some(min)) => have >= min,
        _ => false,
    }
}

/// An image for the model, base64-encoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// `image/png`, `image/jpeg`, …
    pub media_type: String,
    pub data: String,
}

impl Image {
    pub fn new(media_type: &str, bytes: &[u8]) -> Self {
        let data = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes);
        Image {
            media_type: media_type.to_string(),
            data,
        }
    }

    pub fn png(bytes: &[u8]) -> Self {
        Image::new("image/png", bytes)
    }
}

/// A tool saturn offers the model through the in-band MCP server.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema of the arguments.
    pub input_schema: Value,
}

/// One part of a tool's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolContent {
    Text(String),
    Image(Image),
}

/// How to start the CLI.
#[derive(Debug, Clone)]
pub struct AiConfig {
    /// The program to run. `SATURN_CLAUDE_BIN` overrides it (tests point it
    /// at `scripts/fake_claude.py`).
    pub command: String,
    /// Working directory: the notebook's project root, so the project's
    /// `CLAUDE.md`, skills and settings apply.
    pub cwd: PathBuf,
    /// `--model`; `None` is the account's default.
    pub model: Option<String>,
    /// `--effort`.
    pub effort: Option<String>,
    /// Continue this session (`--resume`) instead of starting a new one.
    pub resume: Option<String>,
    /// `--append-system-prompt`.
    pub system_prompt: Option<String>,
    /// `--allowedTools`: these run without a permission request.
    pub allowed_tools: Vec<String>,
    /// `--disallowedTools`.
    pub disallowed_tools: Vec<String>,
    /// Also load the user's own MCP servers. Off by default: start is faster
    /// without them.
    pub user_mcp: bool,
    /// Keep `ANTHROPIC_API_KEY`/`ANTHROPIC_AUTH_TOKEN` in the child's
    /// environment. Off by default, because a key always wins over the
    /// subscription login.
    pub use_api_key: bool,
    /// The tools of the in-band MCP server.
    pub tools: Vec<ToolSpec>,
    /// A lean process for quick, single-purpose requests: `system_prompt`
    /// replaces the CLI's own instead of adding to it, and none of the
    /// CLI's built-in tools, skills or commands are offered. It starts
    /// faster and carries a fraction of the context.
    pub minimal: bool,
}

impl AiConfig {
    pub fn new(cwd: PathBuf) -> Self {
        AiConfig {
            command: "claude".to_string(),
            cwd,
            model: None,
            effort: None,
            resume: None,
            system_prompt: None,
            allowed_tools: Vec::new(),
            disallowed_tools: Vec::new(),
            user_mcp: false,
            use_api_key: false,
            tools: Vec::new(),
            minimal: false,
        }
    }
}

/// The answer to an `AiEvent::PermissionRequest`.
#[derive(Debug, Clone)]
pub enum Permission {
    Allow {
        /// Run the tool with this input instead of the one it asked with.
        updated_input: Option<Value>,
        /// Entries of the request's `suggestions` to apply ("always allow").
        /// Set an entry's `destination` to `session` to keep it out of the
        /// project's settings files.
        remember: Vec<Value>,
    },
    /// Refuse; the model is shown `message`.
    Deny { message: String },
}

impl Permission {
    pub fn allow() -> Self {
        Permission::Allow {
            updated_input: None,
            remember: Vec::new(),
        }
    }

    pub fn deny(message: &str) -> Self {
        Permission::Deny {
            message: message.to_string(),
        }
    }
}

#[derive(Debug)]
pub enum AiCommand {
    /// A user turn. Sent while a turn is running, it is queued by the CLI.
    Send {
        text: String,
        images: Vec<Image>,
    },
    /// Stop the running turn; a `TurnDone` with `TurnOutcome::Interrupted`
    /// follows.
    Interrupt,
    /// Answer the `PermissionRequest` with this `id`.
    PermissionReply {
        id: String,
        decision: Permission,
    },
    /// Answer the `ToolCall` with this `id`.
    ToolReply {
        id: String,
        content: Vec<ToolContent>,
        is_error: bool,
    },
    /// Switch model for the following turns; a new `Ready` confirms it.
    SetModel(String),
    Shutdown,
}

/// The CLI asks whether one of its tools may run.
#[derive(Debug, Clone)]
pub struct PermissionRequest {
    /// Pass back in `AiCommand::PermissionReply`.
    pub id: String,
    /// `Bash`, `Edit`, `mcp__saturn__run_cells`, …
    pub tool_name: String,
    /// The tool's name for people, when the CLI gives one.
    pub title: Option<String>,
    pub description: Option<String>,
    pub input: Value,
    /// The `ToolStart` this request belongs to.
    pub tool_use_id: Option<String>,
    /// What the CLI offers for "always allow"; see `Permission::Allow`.
    pub suggestions: Vec<Value>,
    pub blocked_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnOutcome {
    Completed,
    Interrupted,
    Error(String),
}

#[derive(Debug, Clone)]
pub enum AiEvent {
    /// The CLI is up, logged in and has answered the handshake. Sent again
    /// whenever the model or the session changes: the model is only known
    /// (`Some`) from the first turn on, or after `SetModel`.
    Ready {
        model: Option<String>,
        session_id: String,
        version: Option<String>,
    },
    /// The next piece of the assistant's answer.
    TextDelta(String),
    /// The model calls a tool; `parent` is set inside a subagent.
    ToolStart {
        id: String,
        name: String,
        input: Value,
        parent: Option<String>,
    },
    /// A tool finished. `text` is its textual result, `images` how many
    /// images came with it.
    ToolResult {
        id: String,
        is_error: bool,
        text: String,
        images: usize,
    },
    PermissionRequest(PermissionRequest),
    /// The model calls one of saturn's own tools (`name` without the
    /// `mcp__saturn__` prefix); answer with `AiCommand::ToolReply`.
    ToolCall {
        id: String,
        name: String,
        arguments: Value,
        tool_use_id: Option<String>,
    },
    /// The CLI withdrew a `PermissionRequest` or `ToolCall`, for example
    /// because the turn was interrupted: no reply is wanted any more.
    RequestCancelled {
        id: String,
    },
    /// The turn is over. `cost` is the session's list-price total so far in
    /// USD (a subscription is not billed for it).
    TurnDone {
        outcome: TurnOutcome,
        usage: Usage,
        cost: f64,
    },
    /// The `claude` program is not installed (or not on `PATH`).
    NotFound {
        command: String,
    },
    /// The CLI has no login: the user must run `claude` and `/login`.
    NotLoggedIn,
    /// The process ended on its own.
    Died {
        reason: String,
    },
}

/// A running (or finished) assistant process. After `NotFound`,
/// `NotLoggedIn` or `Died` the task is over; spawn a new handle to retry.
pub struct AiHandle {
    cmd_tx: UnboundedSender<AiCommand>,
    task: JoinHandle<()>,
}

impl AiHandle {
    /// Start the CLI; its events arrive as `AppEvent::Ai`. Spawning spends no
    /// tokens, so it can be done ahead of the first prompt.
    pub fn spawn(config: AiConfig, tx: UnboundedSender<AppEvent>) -> Self {
        Self::spawn_with(config, move |ev| {
            let _ = tx.send(AppEvent::Ai(ev));
        })
    }

    /// Like `spawn`, with the events going to `sink`.
    pub fn spawn_with(config: AiConfig, sink: impl Fn(AiEvent) + Send + 'static) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(client::run(config, cmd_rx, Box::new(sink)));
        AiHandle { cmd_tx, task }
    }

    pub fn send(&self, cmd: AiCommand) {
        if self.cmd_tx.send(cmd).is_err() {
            tracing::warn!("ai client is gone");
        }
    }

    pub fn is_alive(&self) -> bool {
        !self.task.is_finished()
    }

    /// Wait for the client task to finish (after a Shutdown command).
    pub async fn wait(self) {
        let _ = self.task.await;
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    use serde_json::Value;

    /// One line of a recording: who wrote it, and what.
    pub struct Row {
        /// The CLI wrote it (stdout); otherwise the host did (stdin).
        pub from_cli: bool,
        pub msg: Value,
    }

    pub const ALL: [&str; 7] = [
        "basic",
        "permission",
        "always",
        "mcp",
        "interrupt",
        "interrupt_pending",
        "loggedout",
    ];

    /// A recording from `tests/fixtures/ai/`, without the capture script's own
    /// end-of-stream markers.
    pub fn load(name: &str) -> Vec<Row> {
        let path = format!(
            "{}/tests/fixtures/ai/{name}.jsonl",
            env!("CARGO_MANIFEST_DIR")
        );
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
        text.lines()
            .map(|line| {
                serde_json::from_str::<Value>(line).unwrap_or_else(|e| panic!("{path}: {e}"))
            })
            .filter(|row| row["msg"].get("_eof").is_none() && row["msg"].get("_exit").is_none())
            .map(|row| Row {
                from_cli: row["dir"] == "out",
                msg: row["msg"].clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_check() {
        assert!(version_supported("2.1.314"));
        assert!(version_supported("2.1.314 (Claude Code)"));
        assert!(version_supported("2.2.0"));
        assert!(version_supported("3.0"));
        assert!(!version_supported("2.1.313"));
        assert!(!version_supported("2.0.999"));
        assert!(!version_supported(""));
        assert!(!version_supported("nightly"));
    }
}
