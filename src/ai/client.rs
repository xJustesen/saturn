//! The task that owns the `claude` child process.
//!
//! `Session` is the protocol state machine and does no I/O: stdout lines and
//! host commands go in, `AiEvent`s and stdin lines come out. `run` wires it to
//! the process, in the shape of `run_sidecar` in `render/plotly.rs`.

use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};
use tokio::sync::mpsc::UnboundedReceiver;

use super::proto::{self, ContentBlock, ControlRequestBody, Delta, Incoming, StreamEvent};
use super::{
    AiCommand, AiConfig, AiEvent, MCP_SERVER, Permission, PermissionRequest, ToolSpec, TurnOutcome,
};

pub(crate) type Sink = Box<dyn Fn(AiEvent) + Send>;

/// How long the CLI gets to exit by itself once its stdin is closed.
const EXIT_GRACE: Duration = Duration::from_millis(500);
const AUTH_TIMEOUT: Duration = Duration::from_secs(15);
/// Stderr lines kept for the `Died` reason.
const STDERR_TAIL: usize = 5;

/// The program to run: `SATURN_CLAUDE_BIN` if set, else the configured one.
fn program(config: &AiConfig) -> OsString {
    std::env::var_os("SATURN_CLAUDE_BIN")
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| config.command.clone().into())
}

/// A `claude` command with saturn's environment rules applied.
fn command(config: &AiConfig) -> Command {
    let mut cmd = Command::new(program(config));
    cmd.current_dir(&config.cwd).kill_on_drop(true);
    // Saturn's few tools are loaded up front: looking each one up through
    // the CLI's tool search costs a model round trip on first use.
    cmd.env("ENABLE_TOOL_SEARCH", "false");
    if !config.use_api_key {
        // An API key in the environment always wins in `-p` mode; without
        // these the CLI uses the subscription login.
        cmd.env_remove("ANTHROPIC_API_KEY")
            .env_remove("ANTHROPIC_AUTH_TOKEN");
    }
    cmd
}

/// The arguments of the long-lived stream-json process.
fn cli_args(config: &AiConfig, session_id: &str) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--include-partial-messages",
        "--verbose",
        // Permission prompts come to us as `can_use_tool` control requests.
        "--permission-prompt-tool",
        "stdio",
    ]
    .map(String::from)
    .to_vec();
    let mut flag = |name: &str, value: &str| args.extend([name.to_string(), value.to_string()]);
    if !config.tools.is_empty() {
        // An "sdk" server has no process: the CLI talks to it through us.
        let mcp =
            serde_json::json!({"mcpServers": {MCP_SERVER: {"type": "sdk", "name": MCP_SERVER}}});
        flag("--mcp-config", &mcp.to_string());
    }
    if config.resume.is_some() {
        flag("--resume", session_id);
    } else {
        flag("--session-id", session_id);
    }
    if let Some(model) = &config.model {
        flag("--model", model);
    }
    if let Some(effort) = &config.effort {
        flag("--effort", effort);
    }
    if let Some(prompt) = &config.system_prompt {
        flag(if config.minimal { "--system-prompt" } else { "--append-system-prompt" }, prompt);
    }
    if config.minimal {
        // `--tools` names the built-in set only; saturn's own tools stay.
        flag("--tools", "");
    }
    if !config.allowed_tools.is_empty() {
        flag("--allowedTools", &config.allowed_tools.join(","));
    }
    if !config.disallowed_tools.is_empty() {
        flag("--disallowedTools", &config.disallowed_tools.join(","));
    }
    if !config.user_mcp {
        args.push("--strict-mcp-config".to_string());
    }
    if config.minimal {
        args.push("--disable-slash-commands".to_string());
    }
    args
}

/// Ask the CLI whether it is logged in. `None` when it cannot say (too old
/// for `auth status`, or it failed to run): the first turn will tell.
async fn logged_in(config: AiConfig) -> Option<bool> {
    let mut cmd = command(&config);
    cmd.args(["auth", "status", "--json"])
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    let out = tokio::time::timeout(AUTH_TIMEOUT, cmd.output())
        .await
        .ok()?
        .ok()?;
    let status: proto::AuthStatus = serde_json::from_slice(&out.stdout).ok()?;
    Some(status.logged_in)
}

/// A request the host sent and the CLI has yet to answer.
enum Sent {
    Initialize,
    Interrupt,
    SetModel(String),
}

/// The protocol state of one CLI process.
pub(crate) struct Session {
    emit: Sink,
    tools: Vec<ToolSpec>,
    next_id: u64,
    sent: HashMap<String, Sent>,
    /// Open permission requests: request id → the input the tool asked with.
    permissions: HashMap<String, Value>,
    /// Open in-band tool calls: request id → JSON-RPC id.
    tool_calls: HashMap<String, Value>,
    session_id: String,
    model: Option<String>,
    version: Option<String>,
    /// The CLI answered the handshake (or started a turn).
    alive: bool,
    /// The login check came back without a "no".
    authed: bool,
    ready: bool,
    /// The CLI said it has no login; it exits right after.
    pub(crate) logged_out: bool,
}

impl Session {
    pub(crate) fn new(config: &AiConfig, session_id: String, emit: Sink) -> Self {
        Session {
            emit,
            tools: config.tools.clone(),
            next_id: 0,
            sent: HashMap::new(),
            permissions: HashMap::new(),
            tool_calls: HashMap::new(),
            session_id,
            model: None,
            version: None,
            alive: false,
            authed: false,
            ready: false,
            logged_out: false,
        }
    }

    fn request(&mut self, kind: Sent, body: Value) -> Value {
        self.next_id += 1;
        let id = format!("saturn_{}", self.next_id);
        self.sent.insert(id.clone(), kind);
        proto::control_request(&id, body)
    }

    /// The first line to write: the handshake.
    pub(crate) fn start(&mut self) -> Value {
        self.request(Sent::Initialize, proto::initialize())
    }

    /// The login check passed (or could not tell).
    pub(crate) fn auth_checked(&mut self) {
        self.authed = true;
        self.announce(false);
    }

    /// Send `Ready` once both the handshake and the login check are through,
    /// and again when `changed` says the model or session moved.
    fn announce(&mut self, changed: bool) {
        if !(self.alive && self.authed) || (self.ready && !changed) {
            return;
        }
        self.ready = true;
        (self.emit)(AiEvent::Ready {
            model: self.model.clone(),
            session_id: self.session_id.clone(),
            version: self.version.clone(),
        });
    }

    fn set_version(&mut self, version: Option<&str>) {
        let Some(version) = version else { return };
        if self.version.as_deref() != Some(version) {
            if !super::version_supported(version) {
                tracing::warn!(
                    "claude {version} is older than the tested {}",
                    super::MIN_CLI_VERSION
                );
            }
            self.version = Some(version.to_string());
        }
    }

    /// A host command; returns the lines to write to the CLI.
    pub(crate) fn on_command(&mut self, cmd: AiCommand) -> Vec<Value> {
        match cmd {
            AiCommand::Send { text, images } => vec![proto::user_message(&text, &images)],
            AiCommand::Interrupt => vec![self.request(Sent::Interrupt, proto::interrupt())],
            AiCommand::SetModel(model) => {
                let body = proto::set_model(&model);
                vec![self.request(Sent::SetModel(model), body)]
            }
            AiCommand::PermissionReply { id, decision } => {
                let Some(asked) = self.permissions.remove(&id) else {
                    tracing::warn!("ai: no open permission request {id}");
                    return Vec::new();
                };
                let response = match decision {
                    Permission::Allow {
                        updated_input,
                        remember,
                    } => proto::permission_allow(updated_input.unwrap_or(asked), &remember),
                    Permission::Deny { message } => proto::permission_deny(&message),
                };
                vec![proto::control_success(&id, response)]
            }
            AiCommand::ToolReply {
                id,
                content,
                is_error,
            } => {
                let Some(rpc_id) = self.tool_calls.remove(&id) else {
                    tracing::warn!("ai: no open tool call {id}");
                    return Vec::new();
                };
                let rpc = proto::rpc_result(&rpc_id, proto::mcp_tool_result(&content, is_error));
                vec![proto::control_success(&id, proto::mcp_response(rpc))]
            }
            // Handled by `run`: it ends the process.
            AiCommand::Shutdown => Vec::new(),
        }
    }

    /// A line of the CLI's stdout; returns the lines to write back.
    pub(crate) fn on_line(&mut self, line: &str) -> Vec<Value> {
        let msg = match proto::parse(line) {
            Ok(msg) => msg,
            Err(e) => {
                let head: String = line.chars().take(200).collect();
                tracing::warn!("ai: bad line {head:?}: {e}");
                return Vec::new();
            }
        };
        match msg {
            Incoming::System(sys) if sys.is_init() => {
                self.set_version(sys.claude_code_version.as_deref());
                let mut changed = false;
                if let Some(id) = sys.session_id
                    && id != self.session_id
                {
                    self.session_id = id;
                    changed = true;
                }
                if sys.model.is_some() && sys.model != self.model {
                    self.model = sys.model;
                    changed = true;
                }
                // A turn started, so the CLI is up even if it never answered
                // the handshake.
                self.alive = true;
                self.announce(changed);
            }
            Incoming::StreamEvent(ev) => {
                // A subagent's text is not part of the conversation.
                if ev.parent_tool_use_id.is_none()
                    && let StreamEvent::ContentBlockDelta {
                        delta: Delta::TextDelta { text },
                        ..
                    } = ev.event
                    && !text.is_empty()
                {
                    (self.emit)(AiEvent::TextDelta(text));
                }
            }
            Incoming::Assistant(msg) => {
                // Text was already streamed; a tool call arrives here whole.
                for block in msg.message.content {
                    if let ContentBlock::ToolUse { id, name, input } = block {
                        (self.emit)(AiEvent::ToolStart {
                            id,
                            name,
                            input,
                            parent: msg.parent_tool_use_id.clone(),
                        });
                    }
                }
            }
            Incoming::User(msg) => {
                for block in msg.message.content {
                    if let ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } = block
                    {
                        (self.emit)(AiEvent::ToolResult {
                            id: tool_use_id,
                            is_error,
                            text: ContentBlock::result_text(&content),
                            images: ContentBlock::result_images(&content),
                        });
                    }
                }
            }
            Incoming::Result(res) => self.on_result(res),
            Incoming::ControlRequest(req) => return self.on_request(req.request_id, req.request),
            Incoming::ControlResponse(msg) => self.on_response(msg.response),
            Incoming::ControlCancelRequest { request_id } => {
                let open = self.permissions.remove(&request_id).is_some();
                if self.tool_calls.remove(&request_id).is_some() || open {
                    (self.emit)(AiEvent::RequestCancelled { id: request_id });
                }
            }
            Incoming::System(_) | Incoming::Unknown => {}
        }
        Vec::new()
    }

    fn on_result(&mut self, res: proto::ResultMsg) {
        let text = res.result.as_deref().unwrap_or_default();
        let reason = res.terminal_reason.as_deref().unwrap_or_default();
        if res.is_error && reason == "api_error" && text.contains("/login") {
            self.logged_out = true;
            (self.emit)(AiEvent::NotLoggedIn);
            return;
        }
        let outcome = if !res.is_error {
            TurnOutcome::Completed
        } else if reason.starts_with("aborted") {
            TurnOutcome::Interrupted
        } else if !text.is_empty() {
            TurnOutcome::Error(text.to_string())
        } else if !res.errors.is_empty() {
            TurnOutcome::Error(res.errors.join("; "))
        } else {
            TurnOutcome::Error(format!(
                "turn failed ({})",
                if reason.is_empty() {
                    &res.subtype
                } else {
                    reason
                }
            ))
        };
        (self.emit)(AiEvent::TurnDone {
            outcome,
            usage: res.usage,
            cost: res.total_cost_usd,
        });
    }

    /// The CLI asks the host something.
    fn on_request(&mut self, request_id: String, request: ControlRequestBody) -> Vec<Value> {
        match request {
            ControlRequestBody::CanUseTool {
                tool_name,
                display_name,
                description,
                input,
                permission_suggestions,
                blocked_path,
                tool_use_id,
            } => {
                self.permissions.insert(request_id.clone(), input.clone());
                (self.emit)(AiEvent::PermissionRequest(PermissionRequest {
                    id: request_id,
                    tool_name,
                    title: display_name,
                    description,
                    input,
                    tool_use_id,
                    suggestions: permission_suggestions,
                    blocked_path,
                }));
                Vec::new()
            }
            ControlRequestBody::McpMessage {
                server_name,
                message,
            } => match self.on_mcp(&request_id, &server_name, &message) {
                Some(rpc) => vec![proto::control_success(
                    &request_id,
                    proto::mcp_response(rpc),
                )],
                None => Vec::new(),
            },
            // Answer, so the CLI is not left waiting on something we do not know.
            ControlRequestBody::Other => {
                vec![proto::control_error(&request_id, "not supported by saturn")]
            }
        }
    }

    /// A JSON-RPC message for the in-band MCP server. `None` for a
    /// `tools/call`: the host answers that one later.
    fn on_mcp(&mut self, request_id: &str, server: &str, rpc: &Value) -> Option<Value> {
        let method = rpc["method"].as_str().unwrap_or_default();
        let Some(id) = rpc.get("id").filter(|id| !id.is_null()) else {
            return Some(proto::rpc_ack()); // a notification
        };
        Some(match method {
            "initialize" => {
                proto::rpc_result(id, proto::mcp_initialize_result(server, &rpc["params"]))
            }
            "tools/list" => proto::rpc_result(id, proto::mcp_tools_list(&self.tools)),
            "ping" => proto::rpc_result(id, serde_json::json!({})),
            "tools/call" => {
                let params = &rpc["params"];
                self.tool_calls.insert(request_id.to_string(), id.clone());
                (self.emit)(AiEvent::ToolCall {
                    id: request_id.to_string(),
                    name: params["name"].as_str().unwrap_or_default().to_string(),
                    arguments: params
                        .get("arguments")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({})),
                    tool_use_id: params["_meta"]["claudecode/toolUseId"]
                        .as_str()
                        .map(String::from),
                });
                return None;
            }
            _ => proto::rpc_error(id, -32601, &format!("method not found: {method}")),
        })
    }

    /// The CLI answered one of our requests.
    fn on_response(&mut self, res: proto::ControlResponse) {
        let Some(sent) = self.sent.remove(&res.request_id) else {
            return;
        };
        if !res.is_success() {
            tracing::warn!(
                "ai: control request {} failed: {:?}",
                res.request_id,
                res.error
            );
        }
        match sent {
            Sent::Initialize => {
                self.set_version(res.response["claude_code_version"].as_str());
                // Even a refused handshake shows the CLI is up and reading.
                self.alive = true;
                self.announce(false);
            }
            Sent::SetModel(asked) if res.is_success() => {
                let model = res.response["model"]
                    .as_str()
                    .map(String::from)
                    .unwrap_or(asked);
                if self.model.as_deref() != Some(&model) {
                    self.model = Some(model);
                    self.announce(true);
                }
            }
            Sent::SetModel(_) | Sent::Interrupt => {}
        }
    }
}

async fn write_line(stdin: &mut ChildStdin, msg: &Value) -> std::io::Result<()> {
    let mut line = msg.to_string();
    line.push('\n');
    stdin.write_all(line.as_bytes()).await?;
    stdin.flush().await
}

pub(crate) async fn run(config: AiConfig, mut cmd_rx: UnboundedReceiver<AiCommand>, emit: Sink) {
    let session_id = config
        .resume
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let mut cmd = command(&config);
    cmd.args(cli_args(&config, &session_id))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            emit(AiEvent::NotFound {
                command: program(&config).to_string_lossy().into_owned(),
            });
            return;
        }
        Err(e) => {
            emit(AiEvent::Died {
                reason: format!("cannot start claude: {e}"),
            });
            return;
        }
    };
    let mut stdin = child.stdin.take().expect("piped stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("piped stdout")).lines();
    let mut stderr = BufReader::new(child.stderr.take().expect("piped stderr")).lines();
    let mut stderr_open = true;
    let mut stderr_tail: VecDeque<String> = VecDeque::new();
    // The login check runs beside the start-up instead of in front of it.
    let mut auth = tokio::spawn(logged_in(config.clone()));
    let mut auth_pending = true;

    let mut session = Session::new(&config, session_id, emit);
    let mut outgoing = vec![session.start()];
    // Why the loop ended: `None` is a shutdown we chose, `Some` a failure.
    let failure: Option<String> = 'life: loop {
        for msg in outgoing.drain(..) {
            if let Err(e) = write_line(&mut stdin, &msg).await {
                break 'life Some(format!("claude stdin closed: {e}"));
            }
        }
        tokio::select! {
            cmd = cmd_rx.recv() => match cmd {
                None | Some(AiCommand::Shutdown) => break None,
                Some(cmd) => outgoing = session.on_command(cmd),
            },
            line = stdout.next_line() => match line {
                Ok(Some(line)) => {
                    outgoing = session.on_line(&line);
                    if session.logged_out {
                        break None;
                    }
                }
                Ok(None) => break Some("claude exited".to_string()),
                Err(e) => break Some(format!("claude stdout: {e}")),
            },
            line = stderr.next_line(), if stderr_open => match line {
                Ok(Some(line)) => {
                    tracing::debug!("claude: {line}");
                    if stderr_tail.len() == STDERR_TAIL {
                        stderr_tail.pop_front();
                    }
                    stderr_tail.push_back(line);
                }
                _ => stderr_open = false,
            },
            res = &mut auth, if auth_pending => {
                auth_pending = false;
                if let Ok(Some(false)) = res {
                    session.logged_out = true;
                    (session.emit)(AiEvent::NotLoggedIn);
                    break None;
                }
                session.auth_checked();
            }
        }
    };
    auth.abort();

    // Closing stdin is how the CLI is asked to leave.
    drop(stdin);
    let Some(mut reason) = failure else {
        if tokio::time::timeout(EXIT_GRACE, child.wait())
            .await
            .is_err()
        {
            let _ = child.kill().await;
        }
        return;
    };
    let status = match tokio::time::timeout(EXIT_GRACE, child.wait()).await {
        Ok(Ok(status)) => Some(status),
        _ => {
            let _ = child.kill().await;
            None
        }
    };
    // Whatever it said last usually explains why it left.
    while stderr_open
        && let Ok(Ok(Some(line))) = tokio::time::timeout(EXIT_GRACE, stderr.next_line()).await
    {
        tracing::debug!("claude: {line}");
        if stderr_tail.len() == STDERR_TAIL {
            stderr_tail.pop_front();
        }
        stderr_tail.push_back(line);
    }
    if let Some(status) = status {
        reason = format!("{reason} ({status})");
    }
    if !stderr_tail.is_empty() {
        reason = format!("{reason}: {}", Vec::from(stderr_tail).join(" | "));
    }
    tracing::warn!("ai: {reason}");
    (session.emit)(AiEvent::Died { reason });
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::json;

    use super::super::fixtures::{self, Row};
    use super::super::{Image, ToolContent};
    use super::*;

    /// The one tool `scripts/claude_wire_capture.py` serves.
    fn dummy_tool() -> ToolSpec {
        ToolSpec {
            name: "dummy".into(),
            description: "Echo the given text back, together with a small test image.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {"text": {"type": "string"}},
                "required": ["text"],
            }),
        }
    }

    fn session() -> (Session, Arc<Mutex<Vec<AiEvent>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let mut config = AiConfig::new(".".into());
        config.tools = vec![dummy_tool()];
        let session = Session::new(
            &config,
            "local-session".into(),
            Box::new(move |ev| sink.lock().unwrap().push(ev)),
        );
        (session, events)
    }

    /// The command that makes the session write the recorded host line, or
    /// `None` when the session should have written it on its own.
    fn command_for(msg: &Value) -> Option<AiCommand> {
        match msg["type"].as_str()? {
            "user" => {
                let content = msg["message"]["content"].as_array()?;
                let text = content
                    .iter()
                    .find_map(|b| b["text"].as_str())
                    .unwrap_or_default()
                    .to_string();
                let images = content
                    .iter()
                    .filter(|b| b["type"] == "image")
                    .map(|b| Image {
                        media_type: b["source"]["media_type"].as_str().unwrap().into(),
                        data: b["source"]["data"].as_str().unwrap().into(),
                    })
                    .collect();
                Some(AiCommand::Send { text, images })
            }
            "control_request" => match msg["request"]["subtype"].as_str()? {
                "interrupt" => Some(AiCommand::Interrupt),
                "set_model" => Some(AiCommand::SetModel(
                    msg["request"]["model"].as_str()?.into(),
                )),
                _ => None,
            },
            "control_response" => {
                let id = msg["response"]["request_id"].as_str()?.to_string();
                let response = &msg["response"]["response"];
                match response["behavior"].as_str() {
                    Some("allow") => {
                        let remember = response["updatedPermissions"]
                            .as_array()
                            .cloned()
                            .unwrap_or_default();
                        return Some(AiCommand::PermissionReply {
                            id,
                            decision: Permission::Allow {
                                updated_input: None,
                                remember,
                            },
                        });
                    }
                    Some(_) => {
                        let decision = Permission::deny(response["message"].as_str()?);
                        return Some(AiCommand::PermissionReply { id, decision });
                    }
                    None => {}
                }
                let result = &response["mcp_response"]["result"];
                let content = result["content"].as_array()?;
                let content = content
                    .iter()
                    .map(|c| match c["type"].as_str().unwrap() {
                        "text" => ToolContent::Text(c["text"].as_str().unwrap().into()),
                        _ => ToolContent::Image(Image {
                            media_type: c["mimeType"].as_str().unwrap().into(),
                            data: c["data"].as_str().unwrap().into(),
                        }),
                    })
                    .collect();
                Some(AiCommand::ToolReply {
                    id,
                    content,
                    is_error: result["isError"].as_bool()?,
                })
            }
            _ => None,
        }
    }

    /// The capture script's MCP server calls itself version 0.0.0.
    fn same_line(ours: &Value, recorded: &Value) -> bool {
        let mut ours = ours.clone();
        if let Some(v) =
            ours.pointer_mut("/response/response/mcp_response/result/serverInfo/version")
        {
            *v = json!("0.0.0");
        }
        &ours == recorded
    }

    /// Run a recording through a `Session`: the CLI's lines go in, and every
    /// line the host wrote in the recording must come out, in order and equal.
    fn replay(name: &str) -> Vec<AiEvent> {
        let (mut session, events) = session();
        session.auth_checked();
        let mut written: VecDeque<Value> = VecDeque::from([session.start()]);
        for (n, Row { from_cli, msg }) in fixtures::load(name).into_iter().enumerate() {
            if from_cli {
                written.extend(session.on_line(&msg.to_string()));
                continue;
            }
            if let Some(cmd) = command_for(&msg) {
                written.extend(session.on_command(cmd));
            }
            let ours = written
                .pop_front()
                .unwrap_or_else(|| panic!("{name}:{n}: nothing written for {msg}"));
            assert!(
                same_line(&ours, &msg),
                "{name}:{n}:\n  wrote    {ours}\n  recorded {msg}"
            );
        }
        assert!(
            written.is_empty(),
            "{name}: wrote more than the recording: {written:?}"
        );
        let events = events.lock().unwrap();
        events.clone()
    }

    fn text(events: &[AiEvent]) -> String {
        events
            .iter()
            .filter_map(|e| {
                if let AiEvent::TextDelta(t) = e {
                    Some(t.as_str())
                } else {
                    None
                }
            })
            .collect()
    }

    fn outcomes(events: &[AiEvent]) -> Vec<TurnOutcome> {
        events
            .iter()
            .filter_map(|e| {
                if let AiEvent::TurnDone { outcome, .. } = e {
                    Some(outcome.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    #[test]
    fn every_recorded_line_parses() {
        for name in fixtures::ALL {
            let mut unknown = std::collections::BTreeSet::new();
            for row in fixtures::load(name).iter().filter(|r| r.from_cli) {
                match proto::parse(&row.msg.to_string()) {
                    Ok(Incoming::Unknown) => {
                        unknown.insert(row.msg["type"].as_str().unwrap_or_default().to_string());
                    }
                    Ok(_) => {}
                    Err(e) => panic!("{name}: {e}: {}", row.msg),
                }
            }
            // The only type the client has no use for.
            assert!(
                unknown.iter().all(|t| t == "rate_limit_event"),
                "{name}: unknown types {unknown:?}"
            );
        }
    }

    #[test]
    fn basic_turns_stream_text() {
        let events = replay("basic");
        // Ready on the handshake (model not known yet), again once the first
        // turn reports the model and the CLI's own session id.
        let AiEvent::Ready {
            model: None,
            session_id,
            version: Some(version),
        } = &events[0]
        else {
            panic!("first event is {:?}", events[0]);
        };
        assert_eq!(session_id, "local-session");
        assert_eq!(version, "2.1.314");
        let AiEvent::Ready {
            model: Some(model),
            session_id,
            ..
        } = &events[1]
        else {
            panic!("second event is {:?}", events[1]);
        };
        assert_eq!(model, "claude-haiku-5-5");
        assert_ne!(session_id, "local-session");
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, AiEvent::Ready { .. }))
                .count(),
            2
        );
        // Three turns; the last one answers a question about an image.
        assert_eq!(text(&events), "hello therebye nowRed");
        assert_eq!(outcomes(&events), vec![TurnOutcome::Completed; 3]);
        let costs: Vec<f64> = events
            .iter()
            .filter_map(|e| {
                if let AiEvent::TurnDone { cost, .. } = e {
                    Some(*cost)
                } else {
                    None
                }
            })
            .collect();
        assert!(
            costs[0] > 0.0 && costs[0] < costs[1] && costs[1] < costs[2],
            "cost accumulates: {costs:?}"
        );
        let Some(AiEvent::TurnDone { usage, .. }) = events.last() else {
            panic!("no final TurnDone")
        };
        assert!(usage.output_tokens > 0 && usage.context_tokens > 1000);
    }

    #[test]
    fn permission_allow_and_deny() {
        let events = replay("permission");
        let asked: Vec<&PermissionRequest> = events
            .iter()
            .filter_map(|e| {
                if let AiEvent::PermissionRequest(p) = e {
                    Some(p)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(asked.len(), 2);
        assert_eq!(asked[0].tool_name, "Bash");
        assert_eq!(asked[0].title.as_deref(), Some("Bash"));
        assert_eq!(asked[0].input["command"], "touch saturn_allow.txt");
        assert!(asked[0].suggestions.iter().any(|s| s["type"] == "addRules"));
        assert_eq!(
            asked[0].blocked_path.as_deref(),
            Some("/Users/user/project/saturn_allow.txt")
        );
        // The request names the tool call it is about.
        let starts: Vec<(&String, &String)> = events
            .iter()
            .filter_map(|e| {
                if let AiEvent::ToolStart { id, name, .. } = e {
                    Some((id, name))
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(starts.len(), 2);
        assert_eq!(starts[0].1, "Bash");
        assert_eq!(asked[0].tool_use_id.as_ref(), Some(starts[0].0));
        let results: Vec<(bool, &String)> = events
            .iter()
            .filter_map(|e| {
                if let AiEvent::ToolResult { is_error, text, .. } = e {
                    Some((*is_error, text))
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(results.len(), 2);
        assert!(!results[0].0);
        assert_eq!(
            results[1],
            (true, &"The user declined this command.".to_string())
        );
        assert_eq!(text(&events), "donerefused");
    }

    #[test]
    fn always_allow_asks_once() {
        let events = replay("always");
        // Two identical tool calls, one prompt: the first answer carried a rule.
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, AiEvent::ToolStart { .. }))
                .count(),
            2
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, AiEvent::PermissionRequest(_)))
                .count(),
            1
        );
    }

    #[test]
    fn in_band_tool_call() {
        let events = replay("mcp");
        let calls: Vec<&AiEvent> = events
            .iter()
            .filter(|e| matches!(e, AiEvent::ToolCall { .. }))
            .collect();
        let [
            AiEvent::ToolCall {
                name,
                arguments,
                tool_use_id: Some(tool_use_id),
                ..
            },
        ] = calls[..]
        else {
            panic!("tool calls: {calls:?}");
        };
        assert_eq!(name, "dummy");
        assert_eq!(arguments, &json!({"text": "ping"}));
        // The model sees the tool under its MCP name, and its result has the image.
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AiEvent::ToolStart { id, name, .. }
            if id == tool_use_id && name == "mcp__saturn__dummy"))
        );
        assert!(events.iter().any(
            |e| matches!(e, AiEvent::ToolResult { id, is_error: false, text, images: 1 }
            if id == tool_use_id && text == "dummy says: ping")
        ));
        // Saturn's own tools ask for permission like any other.
        assert!(events.iter().any(
            |e| matches!(e, AiEvent::PermissionRequest(p) if p.tool_name == "mcp__saturn__dummy")
        ));
        let said = text(&events);
        assert!(
            said.starts_with("dummy says: ping") && said.ends_with("Blue"),
            "{said:?}"
        );
    }

    #[test]
    fn interrupt_ends_the_turn_and_the_process_lives_on() {
        let events = replay("interrupt");
        assert_eq!(
            outcomes(&events),
            vec![TurnOutcome::Interrupted, TurnOutcome::Completed]
        );
        assert!(text(&events).starts_with("1\n2\n3"));
        assert!(text(&events).ends_with("still here"));
        // set_model is confirmed with a fresh Ready.
        let models: Vec<Option<&str>> = events
            .iter()
            .filter_map(|e| {
                if let AiEvent::Ready { model, .. } = e {
                    Some(model.as_deref())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(
            models,
            vec![None, Some("claude-haiku-5-5"), Some("claude-sonnet-5-5")]
        );
    }

    #[test]
    fn interrupt_withdraws_an_open_permission_request() {
        let events = replay("interrupt_pending");
        let asked = events.iter().find_map(|e| {
            if let AiEvent::PermissionRequest(p) = e {
                Some(&p.id)
            } else {
                None
            }
        });
        let cancelled = events.iter().find_map(|e| {
            if let AiEvent::RequestCancelled { id } = e {
                Some(id)
            } else {
                None
            }
        });
        assert!(asked.is_some());
        assert_eq!(asked, cancelled);
        assert_eq!(
            outcomes(&events),
            vec![TurnOutcome::Interrupted, TurnOutcome::Completed]
        );
    }

    #[test]
    fn no_login_is_reported_instead_of_a_turn() {
        let events = replay("loggedout");
        assert!(matches!(events.last(), Some(AiEvent::NotLoggedIn)));
        assert!(outcomes(&events).is_empty());
    }

    #[test]
    fn ready_waits_for_the_login_check() {
        let (mut session, events) = session();
        let first = session.start();
        assert_eq!(first["request"]["subtype"], "initialize");
        let reply = proto::control_success("saturn_1", json!({"claude_code_version": "2.1.314"}));
        assert!(session.on_line(&reply.to_string()).is_empty());
        assert!(
            events.lock().unwrap().is_empty(),
            "no Ready before the login check"
        );
        session.auth_checked();
        assert!(matches!(
            events.lock().unwrap()[..],
            [AiEvent::Ready { model: None, .. }]
        ));
    }

    #[test]
    fn stray_replies_and_unknown_requests() {
        let (mut session, events) = session();
        // Replies to requests that are not open write nothing.
        assert!(
            session
                .on_command(AiCommand::PermissionReply {
                    id: "x".into(),
                    decision: Permission::allow()
                })
                .is_empty()
        );
        assert!(
            session
                .on_command(AiCommand::ToolReply {
                    id: "x".into(),
                    content: vec![],
                    is_error: false
                })
                .is_empty()
        );
        // An unknown control request is refused, not left hanging.
        let out = session.on_line(
            r#"{"type":"control_request","request_id":"r1","request":{"subtype":"novelty"}}"#,
        );
        assert_eq!(out[0]["response"]["subtype"], "error");
        assert_eq!(out[0]["response"]["request_id"], "r1");
        // An unknown MCP method gets a JSON-RPC error; a ping gets a result.
        let mcp = |rpc: Value| {
            json!({"type": "control_request", "request_id": "r2",
                "request": {"subtype": "mcp_message", "server_name": "saturn", "message": rpc}})
            .to_string()
        };
        let out = session.on_line(&mcp(
            json!({"jsonrpc": "2.0", "id": 7, "method": "resources/list"}),
        ));
        assert_eq!(
            out[0]["response"]["response"]["mcp_response"]["error"]["code"],
            -32601
        );
        let out = session.on_line(&mcp(json!({"jsonrpc": "2.0", "id": 8, "method": "ping"})));
        assert_eq!(
            out[0]["response"]["response"]["mcp_response"],
            json!({"jsonrpc": "2.0", "id": 8, "result": {}})
        );
        // A subagent's text stays out of the transcript.
        let delta = json!({"type": "stream_event", "parent_tool_use_id": "toolu_1",
            "event": {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "inner"}}});
        session.on_line(&delta.to_string());
        session.on_line("garbage");
        assert!(events.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_missing_program_is_not_found() {
        if std::env::var_os("SATURN_CLAUDE_BIN").is_some() {
            return; // the override would replace the missing program
        }
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let mut config = AiConfig::new(".".into());
        config.command = "/nonexistent/saturn-test-claude".into();
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        run(
            config,
            rx,
            Box::new(move |ev| sink.lock().unwrap().push(ev)),
        )
        .await;
        let events = events.lock().unwrap();
        assert!(
            matches!(&events[..], [AiEvent::NotFound { command }] if command == "/nonexistent/saturn-test-claude"),
            "{events:?}"
        );
    }

    #[test]
    fn cli_arguments() {
        let mut config = AiConfig::new(".".into());
        let args = cli_args(&config, "sid");
        assert!(
            args.windows(2)
                .any(|w| w == ["--permission-prompt-tool", "stdio"])
        );
        assert!(args.windows(2).any(|w| w == ["--session-id", "sid"]));
        assert!(args.contains(&"--strict-mcp-config".to_string()));
        // No tools, no in-band server.
        assert!(!args.contains(&"--mcp-config".to_string()));

        config.tools = vec![dummy_tool()];
        config.resume = Some("old".into());
        config.model = Some("haiku".into());
        config.effort = Some("low".into());
        config.system_prompt = Some("brief".into());
        config.allowed_tools = vec!["mcp__saturn__read_cells".into(), "Read".into()];
        config.disallowed_tools = vec!["NotebookEdit".into()];
        config.user_mcp = true;
        let args = cli_args(&config, "old");
        let value = |flag: &str| {
            args.iter()
                .position(|a| a == flag)
                .map(|i| args[i + 1].as_str())
        };
        assert_eq!(
            serde_json::from_str::<Value>(value("--mcp-config").unwrap()).unwrap(),
            json!({"mcpServers": {"saturn": {"type": "sdk", "name": "saturn"}}})
        );
        assert_eq!(value("--resume"), Some("old"));
        assert_eq!(value("--session-id"), None);
        assert_eq!(value("--model"), Some("haiku"));
        assert_eq!(value("--effort"), Some("low"));
        assert_eq!(value("--append-system-prompt"), Some("brief"));
        assert_eq!(
            value("--allowedTools"),
            Some("mcp__saturn__read_cells,Read")
        );
        assert_eq!(value("--disallowedTools"), Some("NotebookEdit"));
        assert!(!args.contains(&"--strict-mcp-config".to_string()));
    }
}
