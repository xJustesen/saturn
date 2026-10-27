//! Plotly figures are rendered to PNG by a sidecar Python process
//! (`uv run --no-project --with plotly --with kaleido`), lazily started on first use.
//!
//! The sidecar must not see the notebook's Python project: run from a project
//! directory (or with a virtualenv active), uv would layer kaleido on top of the
//! project's environment, picking up its plotly pin (kaleido 1.x needs plotly >= 6.1.1)
//! and locking/syncing the project as a side effect. So it runs with `--no-project`,
//! from saturn's cache directory, with any active environment hidden.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::app::AppEvent;

const SCRIPT: &str = include_str!("../../assets/plotly_sidecar.py");

pub enum PlotlyState {
    Pending,
    Ready(Arc<Vec<u8>>),
    Failed(String),
}

struct Request {
    hash: u64,
    figure: Value,
    width: u32,
    height: u32,
}

#[derive(Deserialize)]
struct Response {
    id: u64,
    #[serde(default)]
    png_base64: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

pub struct PlotlySidecar {
    tx: UnboundedSender<AppEvent>,
    req_tx: Option<mpsc::UnboundedSender<Request>>,
    cache: HashMap<u64, Arc<Vec<u8>>>,
    pending: HashSet<u64>,
    failed: HashMap<u64, String>,
    scale: f64,
    pub fatal: Option<String>,
    pub enabled: bool,
}

impl PlotlySidecar {
    pub fn new(tx: UnboundedSender<AppEvent>, scale: f64) -> Self {
        PlotlySidecar {
            tx,
            req_tx: None,
            cache: HashMap::new(),
            pending: HashSet::new(),
            failed: HashMap::new(),
            scale,
            fatal: None,
            enabled: true,
        }
    }

    pub fn request(&mut self, hash: u64, figure: &Value, width: u32, height: u32) -> PlotlyState {
        if let Some(png) = self.cache.get(&hash) {
            return PlotlyState::Ready(png.clone());
        }
        if let Some(e) = self.failed.get(&hash) {
            return PlotlyState::Failed(e.clone());
        }
        if let Some(e) = &self.fatal {
            return PlotlyState::Failed(e.clone());
        }
        if !self.enabled {
            return PlotlyState::Failed("sidecar disabled".into());
        }
        if self.pending.contains(&hash) {
            return PlotlyState::Pending;
        }
        if self.req_tx.is_none() {
            self.start();
        }
        if let Some(req_tx) = &self.req_tx {
            self.pending.insert(hash);
            let _ = req_tx.send(Request { hash, figure: figure.clone(), width, height });
            PlotlyState::Pending
        } else {
            PlotlyState::Failed(self.fatal.clone().unwrap_or_else(|| "sidecar unavailable".into()))
        }
    }

    pub fn complete(&mut self, hash: u64, result: Result<Vec<u8>, String>) {
        self.pending.remove(&hash);
        match result {
            Ok(png) => {
                self.cache.insert(hash, Arc::new(png));
            }
            Err(e) => {
                self.failed.insert(hash, e);
            }
        }
    }

    pub fn fail_all(&mut self, reason: String) {
        for h in self.pending.drain() {
            self.failed.insert(h, reason.clone());
        }
        self.fatal = Some(reason);
        self.req_tx = None;
    }

    fn script_path() -> PathBuf {
        let dir = dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("saturn");
        let _ = std::fs::create_dir_all(&dir);
        dir.join("plotly_sidecar.py")
    }

    fn start(&mut self) {
        let path = Self::script_path();
        let stale = std::fs::read_to_string(&path).map(|s| s != SCRIPT).unwrap_or(true);
        if stale && let Err(e) = std::fs::write(&path, SCRIPT) {
            self.fatal = Some(format!("cannot write sidecar script: {e}"));
            return;
        }
        let (req_tx, req_rx) = mpsc::unbounded_channel::<Request>();
        self.req_tx = Some(req_tx);
        let tx = self.tx.clone();
        let scale = self.scale;
        tokio::spawn(run_sidecar(path, req_rx, tx, scale));
    }
}

async fn run_sidecar(
    script: PathBuf,
    mut req_rx: mpsc::UnboundedReceiver<Request>,
    tx: UnboundedSender<AppEvent>,
    scale: f64,
) {
    let mut cmd = tokio::process::Command::new("uv");
    cmd.args(["run", "--quiet", "--no-project", "--with", "plotly", "--with", "kaleido", "python"])
        .arg(&script)
        .env_remove("VIRTUAL_ENV")
        .env_remove("CONDA_PREFIX")
        .env_remove("UV_PYTHON")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    if let Some(dir) = script.parent() {
        cmd.current_dir(dir);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(AppEvent::PlotlyFatal(format!("cannot start uv: {e}")));
            return;
        }
    };
    let mut stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let mut lines = BufReader::new(stdout).lines();
    let mut in_flight: HashMap<u64, u64> = HashMap::new();
    let mut next_id = 1u64;
    loop {
        tokio::select! {
            req = req_rx.recv() => {
                let Some(req) = req else { break };
                let id = next_id;
                next_id += 1;
                in_flight.insert(id, req.hash);
                let line = serde_json::json!({
                    "id": id,
                    "figure": req.figure,
                    "width": req.width,
                    "height": req.height,
                    "scale": scale,
                })
                .to_string();
                if stdin.write_all(line.as_bytes()).await.is_err() || stdin.write_all(b"\n").await.is_err() {
                    let _ = tx.send(AppEvent::PlotlyFatal("sidecar stdin closed".into()));
                    break;
                }
                let _ = stdin.flush().await;
            }
            line = tokio::time::timeout(Duration::from_secs(120), lines.next_line()) => {
                match line {
                    Ok(Ok(Some(line))) => {
                        match serde_json::from_str::<Response>(&line) {
                            Ok(resp) => {
                                let Some(hash) = in_flight.remove(&resp.id) else { continue };
                                let result = match (resp.png_base64, resp.error) {
                                    (Some(b64), _) => base64::Engine::decode(
                                        &base64::engine::general_purpose::STANDARD,
                                        b64.as_bytes(),
                                    )
                                    .map_err(|e| e.to_string()),
                                    (None, Some(e)) => Err(e),
                                    (None, None) => Err("empty sidecar response".into()),
                                };
                                let _ = tx.send(AppEvent::PlotlyDone { hash, result });
                            }
                            Err(e) => tracing::warn!("sidecar: bad line {line:?}: {e}"),
                        }
                    }
                    Ok(Ok(None)) | Ok(Err(_)) => {
                        let _ = tx.send(AppEvent::PlotlyFatal("sidecar exited (is Chrome installed? see log)".into()));
                        break;
                    }
                    Err(_) => {
                        if in_flight.is_empty() {
                            continue;
                        }
                        let _ = tx.send(AppEvent::PlotlyFatal("sidecar timed out".into()));
                        break;
                    }
                }
            }
        }
    }
    let _ = child.kill().await;
}

impl PlotlySidecar {
    pub fn cached(&self, hash: u64) -> Option<Arc<Vec<u8>>> {
        self.cache.get(&hash).cloned()
    }
}
