//! The kernel actor: launches the kernel process, owns the ZMQ connections,
//! serialises execution requests, and routes iopub traffic to cells.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use jupyter_protocol::{
    ConnectionInfo, ExecuteRequest, ExecutionState, InputReply, InterruptRequest, JupyterMessage,
    JupyterMessageContent, KernelInfoRequest, ReplyStatus as WireReplyStatus, ShutdownRequest,
    Stdio as WireStdio, Transport,
};
use jupyter_zmq_client::{
    ClientHeartbeatConnection, ClientIoPubConnection, DealerRecvConnection, DealerSendConnection,
    KernelspecDir, create_client_control_connection, create_client_heartbeat_connection,
    create_client_iopub_connection, create_client_shell_connection_with_identity,
    create_client_stdin_connection_with_identity, peek_ports_with_listeners,
    peer_identity_for_session, runtime_dir, wait_for_iopub_welcome,
};
use tokio::process::Child;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::time::timeout;

use super::{KernelCommand, KernelEvent, KernelStatus, ReplyStatus};
use crate::app::AppEvent;
use crate::notebook::{CellId, Output, StreamName};

const READY_TIMEOUT: Duration = Duration::from_secs(60);
const HEARTBEAT_EVERY: Duration = Duration::from_secs(3);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(10);
const HEARTBEAT_MISSES: u32 = 3;

enum Internal {
    IoPub(JupyterMessage),
    Shell(JupyterMessage),
    Stdin(JupyterMessage),
    Control(JupyterMessage),
    HeartbeatLost,
}

enum End {
    Restart,
    Shutdown,
    Died(String),
}

struct Session {
    tx: UnboundedSender<AppEvent>,
    session_id: String,
    shell_tx: DealerSendConnection,
    control_tx: DealerSendConnection,
    stdin_tx: DealerSendConnection,
    internal_rx: UnboundedReceiver<Internal>,
    child: Child,
    connection_path: PathBuf,
    queue: VecDeque<(CellId, String)>,
    in_flight: Option<(String, CellId)>,
    /// msg_id → cell for recently executed requests (iopub can trail the reply).
    msg_cells: HashMap<String, CellId>,
    msg_order: VecDeque<String>,
    /// Silent executions whose output must be dropped.
    silent: HashMap<String, ()>,
    pending_input: Option<JupyterMessage>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

fn emit(tx: &UnboundedSender<AppEvent>, ev: KernelEvent) {
    let _ = tx.send(AppEvent::Kernel(ev));
}

pub async fn run(
    spec: KernelspecDir,
    cwd: PathBuf,
    tx: UnboundedSender<AppEvent>,
    mut cmd_rx: UnboundedReceiver<KernelCommand>,
) {
    loop {
        emit(&tx, KernelEvent::Status(KernelStatus::Starting));
        let mut session = match start_session(&spec, &cwd, tx.clone()).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("kernel start failed: {e:#}");
                emit(&tx, KernelEvent::Died { reason: format!("{e:#}") });
                if !wait_for_restart(&mut cmd_rx).await {
                    return;
                }
                continue;
            }
        };
        let end = session.run(&mut cmd_rx).await;
        session.teardown().await;
        match end {
            End::Restart => continue,
            End::Shutdown => return,
            End::Died(reason) => {
                emit(&tx, KernelEvent::Died { reason });
                if !wait_for_restart(&mut cmd_rx).await {
                    return;
                }
            }
        }
    }
}

/// After the kernel died: wait for a Restart (true) or Shutdown/closed (false).
async fn wait_for_restart(cmd_rx: &mut UnboundedReceiver<KernelCommand>) -> bool {
    loop {
        match cmd_rx.recv().await {
            Some(KernelCommand::Restart) => return true,
            Some(KernelCommand::Shutdown) | None => return false,
            Some(_) => continue,
        }
    }
}

fn kernel_log_file(name: &str) -> Option<std::fs::File> {
    let dir = dirs::cache_dir()?.join("saturn");
    std::fs::create_dir_all(&dir).ok()?;
    let safe: String = name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(format!("kernel-{safe}.log")))
        .ok()
}

async fn start_session(spec: &KernelspecDir, cwd: &Path, tx: UnboundedSender<AppEvent>) -> Result<Session> {
    let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let (ports, listeners) = peek_ports_with_listeners(ip, 5).await.context("allocating ports")?;
    let connection_info = ConnectionInfo {
        ip: ip.to_string(),
        transport: Transport::TCP,
        shell_port: ports[0],
        iopub_port: ports[1],
        stdin_port: ports[2],
        control_port: ports[3],
        hb_port: ports[4],
        key: uuid::Uuid::new_v4().to_string(),
        signature_scheme: "hmac-sha256".to_string(),
        kernel_name: Some(spec.kernel_name.clone()),
    };
    let runtime = runtime_dir();
    tokio::fs::create_dir_all(&runtime).await.ok();
    let connection_path = runtime.join(format!(
        "saturn-{}-{}.json",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    tokio::fs::write(&connection_path, serde_json::to_vec_pretty(&connection_info)?)
        .await
        .with_context(|| format!("writing {}", connection_path.display()))?;

    let log = kernel_log_file(&spec.kernel_name);
    let (stdout, stderr) = match log {
        Some(f) => {
            let f2 = f.try_clone().ok();
            (Some(Stdio::from(f)), f2.map(Stdio::from))
        }
        None => (None, None),
    };
    let mut command = spec
        .clone()
        .command(&connection_path, stderr, stdout)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    command.current_dir(cwd).kill_on_drop(true);
    tracing::info!(kernel = %spec.kernel_name, argv = ?spec.kernelspec.argv, "launching kernel");
    let child = command.spawn().with_context(|| format!("spawning kernel {}", spec.kernel_name))?;
    // The kernel binds the ports we reserved; release them now.
    drop(listeners);

    let session_id = uuid::Uuid::new_v4().to_string();
    let mut iopub: ClientIoPubConnection =
        create_client_iopub_connection(&connection_info, "", &session_id).await.map_err(|e| anyhow::anyhow!("{e}"))?;
    let identity = peer_identity_for_session(&session_id).map_err(|e| anyhow::anyhow!("{e}"))?;
    let shell = create_client_shell_connection_with_identity(&connection_info, &session_id, identity.clone())
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let stdin = create_client_stdin_connection_with_identity(&connection_info, &session_id, identity)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let control = create_client_control_connection(&connection_info, &session_id)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let heartbeat: ClientHeartbeatConnection =
        create_client_heartbeat_connection(&connection_info).await.map_err(|e| anyhow::anyhow!("{e}"))?;

    let _ = wait_for_iopub_welcome(&mut iopub, Duration::from_millis(300)).await;

    let (mut shell_tx, mut shell_rx) = shell.split();
    let (stdin_tx, stdin_rx) = stdin.split();
    let (control_tx, control_rx) = control.split();

    // Readiness: poke kernel_info until the kernel answers.
    let start = tokio::time::Instant::now();
    let mut ready = None;
    while start.elapsed() < READY_TIMEOUT {
        let req: JupyterMessage = KernelInfoRequest {}.into();
        let req = req.with_session(&session_id);
        if let Err(e) = shell_tx.send(req).await {
            tracing::debug!("kernel_info send failed (not up yet?): {e}");
        }
        match timeout(Duration::from_millis(700), shell_rx.read()).await {
            Ok(Ok(msg)) => {
                if let JupyterMessageContent::KernelInfoReply(reply) = msg.content {
                    ready = Some(reply);
                    break;
                }
            }
            Ok(Err(e)) => tracing::debug!("shell read error during startup: {e}"),
            Err(_) => {}
        }
    }
    let Some(info) = ready else {
        anyhow::bail!("kernel did not answer kernel_info within {}s", READY_TIMEOUT.as_secs());
    };

    let (internal_tx, internal_rx) = mpsc::unbounded_channel();
    let mut tasks = Vec::new();
    {
        let itx = internal_tx.clone();
        tasks.push(tokio::spawn(async move {
            loop {
                match iopub.read().await {
                    Ok(m) => {
                        if itx.send(Internal::IoPub(m)).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::debug!("iopub read error: {e}");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            }
        }));
    }
    tasks.push(spawn_reader(shell_rx, internal_tx.clone(), Internal::Shell));
    tasks.push(spawn_reader(stdin_rx, internal_tx.clone(), Internal::Stdin));
    tasks.push(spawn_reader(control_rx, internal_tx.clone(), Internal::Control));
    {
        let itx = internal_tx.clone();
        let mut hb = heartbeat;
        tasks.push(tokio::spawn(async move {
            let mut misses = 0u32;
            loop {
                tokio::time::sleep(HEARTBEAT_EVERY).await;
                match timeout(HEARTBEAT_TIMEOUT, hb.single_heartbeat()).await {
                    Ok(Ok(())) => misses = 0,
                    _ => {
                        misses += 1;
                        if misses >= HEARTBEAT_MISSES {
                            let _ = itx.send(Internal::HeartbeatLost);
                            break;
                        }
                    }
                }
            }
        }));
    }

    emit(
        &tx,
        KernelEvent::Ready {
            implementation: info.implementation.clone(),
            language: info.language_info.name.clone(),
            version: info.language_info.version.clone(),
        },
    );
    emit(&tx, KernelEvent::Status(KernelStatus::Idle));

    Ok(Session {
        tx,
        session_id,
        shell_tx,
        control_tx,
        stdin_tx,
        internal_rx,
        child,
        connection_path,
        queue: VecDeque::new(),
        in_flight: None,
        msg_cells: HashMap::new(),
        msg_order: VecDeque::new(),
        silent: HashMap::new(),
        pending_input: None,
        tasks,
    })
}

fn spawn_reader(
    mut rx: DealerRecvConnection,
    itx: UnboundedSender<Internal>,
    wrap: fn(JupyterMessage) -> Internal,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match rx.read().await {
                Ok(m) => {
                    if itx.send(wrap(m)).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    tracing::debug!("socket read error: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
    })
}

impl Session {
    fn remember(&mut self, msg_id: String, cell: CellId) {
        self.msg_cells.insert(msg_id.clone(), cell);
        self.msg_order.push_back(msg_id);
        while self.msg_order.len() > 64 {
            if let Some(old) = self.msg_order.pop_front() {
                self.msg_cells.remove(&old);
            }
        }
    }

    fn cell_for(&self, msg: &JupyterMessage) -> Option<CellId> {
        let parent = msg.parent_header.as_ref()?;
        self.msg_cells.get(&parent.msg_id).cloned()
    }

    fn is_silent(&self, msg: &JupyterMessage) -> bool {
        msg.parent_header
            .as_ref()
            .map(|p| self.silent.contains_key(&p.msg_id))
            .unwrap_or(false)
    }

    async fn dispatch_next(&mut self) {
        if self.in_flight.is_some() {
            return;
        }
        let Some((cell_id, code)) = self.queue.pop_front() else {
            return;
        };
        let req = ExecuteRequest {
            code,
            silent: false,
            store_history: true,
            user_expressions: None,
            allow_stdin: true,
            stop_on_error: true,
        };
        let msg: JupyterMessage = req.into();
        let msg = msg.with_session(&self.session_id);
        let msg_id = msg.header.msg_id.clone();
        match self.shell_tx.send(msg).await {
            Ok(()) => {
                self.remember(msg_id.clone(), cell_id.clone());
                self.in_flight = Some((msg_id, cell_id.clone()));
                emit(&self.tx, KernelEvent::ExecuteStarted { cell_id });
                self.emit_queue();
            }
            Err(e) => {
                emit(&self.tx, KernelEvent::Log(format!("execute send failed: {e}")));
                emit(
                    &self.tx,
                    KernelEvent::ExecuteDone { cell_id, status: ReplyStatus::Aborted, execution_count: None },
                );
            }
        }
    }

    fn emit_queue(&self) {
        emit(
            &self.tx,
            KernelEvent::QueueChanged { queued: self.queue.iter().map(|(c, _)| c.clone()).collect() },
        );
    }

    async fn run(&mut self, cmd_rx: &mut UnboundedReceiver<KernelCommand>) -> End {
        // The child is polled with try_wait on a timer so that it can still be
        // killed from here on shutdown.
        let mut poll = tokio::time::interval(Duration::from_millis(500));
        loop {
            tokio::select! {
                cmd = cmd_rx.recv() => {
                    let Some(cmd) = cmd else { return End::Shutdown };
                    match cmd {
                        KernelCommand::Execute { cell_id, code } => {
                            self.queue.push_back((cell_id, code));
                            self.emit_queue();
                            self.dispatch_next().await;
                        }
                        KernelCommand::ExecuteSilent { code } => {
                            let req = ExecuteRequest {
                                code,
                                silent: true,
                                store_history: false,
                                user_expressions: None,
                                allow_stdin: false,
                                stop_on_error: false,
                            };
                            let msg: JupyterMessage = req.into();
                            let msg = msg.with_session(&self.session_id);
                            self.silent.insert(msg.header.msg_id.clone(), ());
                            if let Err(e) = self.shell_tx.send(msg).await {
                                tracing::warn!("silent execute failed: {e}");
                            }
                        }
                        KernelCommand::Interrupt => {
                            self.queue.clear();
                            self.emit_queue();
                            let msg: JupyterMessage = InterruptRequest {}.into();
                            let msg = msg.with_session(&self.session_id);
                            if let Err(e) = self.control_tx.send(msg).await {
                                emit(&self.tx, KernelEvent::Log(format!("interrupt failed: {e}")));
                            }
                        }
                        KernelCommand::Restart => {
                            emit(&self.tx, KernelEvent::Status(KernelStatus::Restarting));
                            self.request_shutdown(true).await;
                            return End::Restart;
                        }
                        KernelCommand::Shutdown => {
                            self.request_shutdown(false).await;
                            return End::Shutdown;
                        }
                        KernelCommand::InputReply { value } => {
                            if let Some(req) = self.pending_input.take() {
                                let reply = InputReply { value, status: WireReplyStatus::Ok, error: None };
                                let msg = JupyterMessage::new(reply, Some(&req)).with_session(&self.session_id);
                                if let Err(e) = self.stdin_tx.send(msg).await {
                                    emit(&self.tx, KernelEvent::Log(format!("input reply failed: {e}")));
                                }
                            }
                        }
                    }
                }
                internal = self.internal_rx.recv() => {
                    let Some(internal) = internal else { return End::Died("kernel connections closed".into()) };
                    match internal {
                        Internal::IoPub(msg) => self.on_iopub(msg),
                        Internal::Shell(msg) => self.on_shell(msg).await,
                        Internal::Stdin(msg) => self.on_stdin(msg),
                        Internal::Control(msg) => tracing::debug!("control: {}", msg.message_type()),
                        Internal::HeartbeatLost => return End::Died("kernel stopped responding to heartbeats".into()),
                    }
                }
                _ = poll.tick() => {
                    if let Ok(Some(status)) = self.child.try_wait() {
                        return End::Died(format!("kernel process exited ({status})"));
                    }
                }
            }
        }
    }

    async fn request_shutdown(&mut self, restart: bool) {
        let msg: JupyterMessage = ShutdownRequest { restart }.into();
        let msg = msg.with_session(&self.session_id);
        if self.control_tx.send(msg).await.is_ok() {
            // Give the kernel a moment to exit cleanly.
            let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
            while tokio::time::Instant::now() < deadline {
                if let Ok(Some(_)) = self.child.try_wait() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }

    async fn teardown(&mut self) {
        for t in self.tasks.drain(..) {
            t.abort();
        }
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.start_kill();
            let _ = timeout(Duration::from_secs(2), self.child.wait()).await;
        }
        let _ = tokio::fs::remove_file(&self.connection_path).await;
    }

    fn on_iopub(&mut self, msg: JupyterMessage) {
        if self.is_silent(&msg) {
            if let JupyterMessageContent::Status(s) = &msg.content {
                self.forward_status(s.execution_state.clone());
            }
            return;
        }
        let cell = self.cell_for(&msg);
        match msg.content {
            JupyterMessageContent::Status(s) => self.forward_status(s.execution_state),
            JupyterMessageContent::ExecuteInput(ei) => {
                if let Some(cell_id) = cell {
                    emit(
                        &self.tx,
                        KernelEvent::ExecuteInput { cell_id, execution_count: ei.execution_count.value() as i32 },
                    );
                }
            }
            JupyterMessageContent::StreamContent(s) => {
                if let Some(cell_id) = cell {
                    let name = match s.name {
                        WireStdio::Stdout => StreamName::Stdout,
                        WireStdio::Stderr => StreamName::Stderr,
                    };
                    emit(&self.tx, KernelEvent::Output { cell_id, output: Output::Stream { name, text: s.text } });
                }
            }
            JupyterMessageContent::DisplayData(d) => {
                if let Some(cell_id) = cell {
                    let display_id = d.transient.and_then(|t| t.display_id);
                    emit(
                        &self.tx,
                        KernelEvent::Output {
                            cell_id,
                            output: Output::DisplayData { data: d.data, metadata: d.metadata, display_id },
                        },
                    );
                }
            }
            JupyterMessageContent::UpdateDisplayData(u) => {
                if let Some(display_id) = u.transient.display_id {
                    emit(&self.tx, KernelEvent::UpdateDisplay { display_id, data: u.data, metadata: u.metadata });
                }
            }
            JupyterMessageContent::ExecuteResult(r) => {
                if let Some(cell_id) = cell {
                    emit(
                        &self.tx,
                        KernelEvent::Output {
                            cell_id,
                            output: Output::ExecuteResult {
                                execution_count: r.execution_count.value() as i32,
                                data: r.data,
                                metadata: r.metadata,
                            },
                        },
                    );
                }
            }
            JupyterMessageContent::ErrorOutput(e) => {
                if let Some(cell_id) = cell {
                    emit(
                        &self.tx,
                        KernelEvent::Output {
                            cell_id,
                            output: Output::Error { ename: e.ename, evalue: e.evalue, traceback: e.traceback },
                        },
                    );
                }
            }
            JupyterMessageContent::ClearOutput(c) => {
                if let Some(cell_id) = cell {
                    emit(&self.tx, KernelEvent::ClearOutput { cell_id, wait: c.wait });
                }
            }
            other => tracing::trace!("iopub {}", other.message_type()),
        }
    }

    fn forward_status(&self, state: ExecutionState) {
        let status = match state {
            ExecutionState::Busy => KernelStatus::Busy,
            ExecutionState::Idle => KernelStatus::Idle,
            ExecutionState::Starting => KernelStatus::Starting,
            ExecutionState::Restarting | ExecutionState::AutoRestarting => KernelStatus::Restarting,
            ExecutionState::Dead | ExecutionState::Terminating => KernelStatus::Dead,
            _ => return,
        };
        emit(&self.tx, KernelEvent::Status(status));
    }

    async fn on_shell(&mut self, msg: JupyterMessage) {
        if let JupyterMessageContent::ExecuteReply(reply) = &msg.content {
            let parent_id = msg.parent_header.as_ref().map(|p| p.msg_id.clone());
            if self.silent.remove(parent_id.as_deref().unwrap_or("")).is_some() {
                return;
            }
            let Some((msg_id, cell_id)) = self.in_flight.clone() else { return };
            if parent_id.as_deref() != Some(msg_id.as_str()) {
                return;
            }
            self.in_flight = None;
            let status = match reply.status {
                WireReplyStatus::Ok => ReplyStatus::Ok,
                WireReplyStatus::Error => ReplyStatus::Error,
                WireReplyStatus::Aborted => ReplyStatus::Aborted,
            };
            let count = Some(reply.execution_count.value() as i32).filter(|c| *c > 0);
            if status != ReplyStatus::Ok {
                // Jupyter semantics: an error stops the run queue.
                self.queue.clear();
                self.emit_queue();
            }
            emit(&self.tx, KernelEvent::ExecuteDone { cell_id, status, execution_count: count });
            self.dispatch_next().await;
        } else {
            tracing::trace!("shell {}", msg.message_type());
        }
    }

    fn on_stdin(&mut self, msg: JupyterMessage) {
        if let JupyterMessageContent::InputRequest(req) = &msg.content {
            let cell_id = self.cell_for(&msg);
            emit(
                &self.tx,
                KernelEvent::InputRequest { cell_id, prompt: req.prompt.clone(), password: req.password },
            );
            self.pending_input = Some(msg);
        }
    }
}
