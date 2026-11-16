//! Drives `AiHandle` against `scripts/fake_claude.py`, which replays the
//! recordings of the real CLI in `tests/fixtures/ai/`. Nothing here touches
//! the real `claude`, a login or the network.

use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::Duration;

use saturn::ai::{
    AiCommand, AiConfig, AiEvent, AiHandle, Image, Permission, PermissionRequest, ToolContent,
    ToolSpec, TurnOutcome,
};
use saturn::app::AppEvent;
use serde_json::{Value, json};
use tokio::sync::mpsc::{self, UnboundedReceiver};

const TIMEOUT: Duration = Duration::from_secs(20);

fn repo() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Point saturn at the fake, and plant the credentials it must not pass on.
fn fake_env() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: every test calls this before it starts a thread or a
        // process, and `Once` holds the others back until the first is done.
        unsafe {
            std::env::set_var("SATURN_CLAUDE_BIN", repo().join("scripts/fake_claude.py"));
            std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-not-a-real-key");
            std::env::set_var("ANTHROPIC_AUTH_TOKEN", "not-a-real-token");
        }
    });
}

/// One `AiHandle` on a fake CLI, in a working directory of its own.
struct Fake {
    dir: PathBuf,
    handle: Option<AiHandle>,
    rx: UnboundedReceiver<AppEvent>,
}

impl Fake {
    /// Start a client whose fake CLI plays `session` (JSONL text).
    fn start(name: &str, session: Option<&str>, setup: impl FnOnce(&mut AiConfig, &Path)) -> Self {
        fake_env();
        let dir = std::env::temp_dir().join(format!("saturn-ai-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(session) = session {
            std::fs::write(dir.join("fake_claude_session.jsonl"), session).unwrap();
        }
        let mut config = AiConfig::new(dir.clone());
        setup(&mut config, &dir);
        let (tx, rx) = mpsc::unbounded_channel();
        let handle = AiHandle::spawn(config, tx);
        Fake {
            dir,
            handle: Some(handle),
            rx,
        }
    }

    /// Start a client on a recording from `tests/fixtures/ai/`.
    fn replay(fixture: &str, setup: impl FnOnce(&mut AiConfig, &Path)) -> Self {
        let path = repo()
            .join("tests/fixtures/ai")
            .join(format!("{fixture}.jsonl"));
        let session =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        Self::start(fixture, Some(&session), setup)
    }

    fn send(&self, cmd: AiCommand) {
        self.handle.as_ref().unwrap().send(cmd);
    }

    fn say(&self, text: &str) {
        self.send(AiCommand::Send {
            text: text.to_string(),
            images: Vec::new(),
        });
    }

    async fn next(&mut self) -> AiEvent {
        match tokio::time::timeout(TIMEOUT, self.rx.recv()).await {
            Ok(Some(AppEvent::Ai(ev))) => ev,
            Ok(Some(_)) => panic!("an event that is not from the assistant"),
            Ok(None) => panic!("the client hung up"),
            Err(_) => panic!("no event within {TIMEOUT:?}"),
        }
    }

    /// Events up to and including the first one `stop` accepts. A dead
    /// process fails the test, with the fake's complaint.
    async fn until(&mut self, stop: impl Fn(&AiEvent) -> bool) -> Vec<AiEvent> {
        let mut seen = Vec::new();
        loop {
            let ev = self.next().await;
            let done = stop(&ev);
            if let AiEvent::Died { reason } = &ev
                && !done
            {
                panic!("the fake CLI died: {reason}\nafter {seen:?}");
            }
            seen.push(ev);
            if done {
                return seen;
            }
        }
    }

    async fn turn(&mut self) -> Vec<AiEvent> {
        self.until(|e| matches!(e, AiEvent::TurnDone { .. })).await
    }

    async fn permission(&mut self) -> PermissionRequest {
        match self
            .until(|e| matches!(e, AiEvent::PermissionRequest(_)))
            .await
            .pop()
        {
            Some(AiEvent::PermissionRequest(p)) => p,
            _ => unreachable!(),
        }
    }

    /// Shut down and return what the fake recorded: its arguments and
    /// environment, and every line it read on stdin.
    async fn finish(mut self) -> (Value, Vec<Value>) {
        let handle = self.handle.take().unwrap();
        handle.send(AiCommand::Shutdown);
        tokio::time::timeout(TIMEOUT, handle.wait())
            .await
            .expect("the client task ends after Shutdown");
        let argv = std::fs::read_to_string(self.dir.join("fake_claude_argv.json")).unwrap();
        let stdin = std::fs::read_to_string(self.dir.join("fake_claude_stdin.jsonl")).unwrap();
        (
            serde_json::from_str(&argv).unwrap(),
            stdin
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect(),
        )
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
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

fn outcome(events: &[AiEvent]) -> &TurnOutcome {
    match events.last() {
        Some(AiEvent::TurnDone { outcome, .. }) => outcome,
        other => panic!("not the end of a turn: {other:?}"),
    }
}

fn dummy_tool() -> ToolSpec {
    ToolSpec {
        name: "dummy".into(),
        description: "Echo the given text back, together with a small test image.".into(),
        input_schema: json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}),
    }
}

fn with_dummy_tool(config: &mut AiConfig, _: &Path) {
    config.tools = vec![dummy_tool()];
}

#[tokio::test]
async fn streamed_turns_with_an_image() {
    let mut fake = Fake::replay("basic", |config, _| {
        with_dummy_tool(config, Path::new(""));
        config.model = Some("haiku".into());
        config.system_prompt = Some("You are inside saturn.".into());
    });

    // Up before any prompt: the handshake and the login check are enough.
    let ready = fake.next().await;
    let AiEvent::Ready {
        model: None,
        session_id,
        version,
    } = &ready
    else {
        panic!("first event: {ready:?}")
    };
    assert_eq!(version.as_deref(), Some("2.1.314"));
    let first_session = session_id.clone();

    fake.say("Reply with exactly: hello there");
    let turn = fake.turn().await;
    // The turn's init line names the model.
    assert!(
        matches!(&turn[0], AiEvent::Ready { model: Some(m), .. } if m == "claude-haiku-5-5"),
        "{turn:?}"
    );
    assert_eq!(text(&turn), "hello there");
    assert_eq!(outcome(&turn), &TurnOutcome::Completed);
    let AiEvent::TurnDone { usage, cost, .. } = turn.last().unwrap() else {
        unreachable!()
    };
    assert!(usage.output_tokens > 0 && *cost > 0.0);

    // A second turn in the same process.
    fake.say("Now reply with exactly: bye now");
    let turn = fake.turn().await;
    assert_eq!(text(&turn), "bye now");

    // A turn with an image block.
    let image = Image::png(b"not really a png");
    fake.send(AiCommand::Send {
        text: "What colour is this image?".into(),
        images: vec![image.clone()],
    });
    let turn = fake.turn().await;
    assert_eq!(text(&turn), "Red");

    let (recorded, stdin) = fake.finish().await;
    // The handshake goes first, then the MCP server answers from the config.
    assert_eq!(stdin[0]["request"]["subtype"], "initialize");
    let tools = stdin
        .iter()
        .find_map(|l| l.pointer("/response/response/mcp_response/result/tools"))
        .unwrap();
    assert_eq!(tools[0]["name"], "dummy");
    assert_eq!(tools[0]["inputSchema"]["required"], json!(["text"]));
    let users: Vec<&Value> = stdin.iter().filter(|l| l["type"] == "user").collect();
    assert_eq!(users.len(), 3);
    assert_eq!(
        users[0]["message"]["content"],
        json!([{"type": "text", "text": "Reply with exactly: hello there"}])
    );
    assert_eq!(
        users[2]["message"]["content"][1],
        json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": image.data}})
    );

    // The command line, the working directory and the scrubbed environment.
    let argv: Vec<&str> = recorded["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    let value = |flag: &str| argv.iter().position(|a| *a == flag).map(|i| argv[i + 1]);
    assert_eq!(argv[0], "-p");
    assert_eq!(value("--input-format"), Some("stream-json"));
    assert_eq!(value("--output-format"), Some("stream-json"));
    assert_eq!(value("--permission-prompt-tool"), Some("stdio"));
    assert_eq!(value("--session-id"), Some(first_session.as_str()));
    assert_eq!(value("--model"), Some("haiku"));
    assert_eq!(
        value("--append-system-prompt"),
        Some("You are inside saturn.")
    );
    assert!(value("--mcp-config").unwrap().contains(r#""type":"sdk""#));
    for flag in [
        "--include-partial-messages",
        "--verbose",
        "--strict-mcp-config",
    ] {
        assert!(argv.contains(&flag), "{flag} missing from {argv:?}");
    }
    assert_eq!(
        recorded["env"],
        json!({"ANTHROPIC_API_KEY": false, "ANTHROPIC_AUTH_TOKEN": false})
    );
    assert!(recorded["cwd"].as_str().unwrap().contains("saturn-ai-"));
}

#[tokio::test]
async fn permission_request_allowed_then_denied() {
    let mut fake = Fake::replay("permission", with_dummy_tool);
    fake.until(|e| matches!(e, AiEvent::Ready { .. })).await;

    fake.say("Use the Bash tool to run `touch saturn_allow.txt`.");
    let asked = fake.permission().await;
    assert_eq!(asked.tool_name, "Bash");
    assert_eq!(asked.input["command"], "touch saturn_allow.txt");
    fake.send(AiCommand::PermissionReply {
        id: asked.id.clone(),
        decision: Permission::allow(),
    });
    let turn = fake.turn().await;
    assert!(turn.iter().any(
        |e| matches!(e, AiEvent::ToolResult { id, is_error: false, .. }
        if Some(id) == asked.tool_use_id.as_ref())
    ));
    assert_eq!(text(&turn), "done");

    fake.say("Use the Bash tool to run `touch saturn_deny.txt`.");
    let denied = fake.permission().await;
    fake.send(AiCommand::PermissionReply {
        id: denied.id.clone(),
        decision: Permission::deny("Not this one."),
    });
    let turn = fake.turn().await;
    assert!(
        turn.iter()
            .any(|e| matches!(e, AiEvent::ToolResult { is_error: true, .. }))
    );
    assert_eq!(text(&turn), "refused");

    let (_, stdin) = fake.finish().await;
    let reply = |id: &str| {
        stdin
            .iter()
            .find(|l| l["response"]["request_id"] == id)
            .unwrap()["response"]["response"]
            .clone()
    };
    // Allowing sends the tool's own input back.
    assert_eq!(
        reply(&asked.id),
        json!({"behavior": "allow", "updatedInput": asked.input})
    );
    assert_eq!(
        reply(&denied.id),
        json!({"behavior": "deny", "message": "Not this one."})
    );
}

#[tokio::test]
async fn in_band_tool_call() {
    let mut fake = Fake::replay("mcp", with_dummy_tool);
    fake.until(|e| matches!(e, AiEvent::Ready { .. })).await;

    fake.say("Call the mcp__saturn__dummy tool with text \"ping\".");
    // Saturn's own tool asks for permission first, like any other.
    let asked = fake.permission().await;
    assert_eq!(asked.tool_name, "mcp__saturn__dummy");
    fake.send(AiCommand::PermissionReply {
        id: asked.id,
        decision: Permission::allow(),
    });

    let call = fake
        .until(|e| matches!(e, AiEvent::ToolCall { .. }))
        .await
        .pop()
        .unwrap();
    let AiEvent::ToolCall {
        id,
        name,
        arguments,
        tool_use_id,
    } = call
    else {
        unreachable!()
    };
    assert_eq!(name, "dummy");
    assert_eq!(arguments, json!({"text": "ping"}));
    assert_eq!(tool_use_id, asked.tool_use_id);
    let image = Image::png(b"plot");
    fake.send(AiCommand::ToolReply {
        id: id.clone(),
        content: vec![
            ToolContent::Text("dummy says: ping".into()),
            ToolContent::Image(image.clone()),
        ],
        is_error: false,
    });

    let turn = fake.turn().await;
    assert!(turn.iter().any(
        |e| matches!(e, AiEvent::ToolResult { is_error: false, images: 1, text, .. }
        if text == "dummy says: ping")
    ));
    let said = text(&turn);
    assert!(
        said.starts_with("dummy says: ping") && said.ends_with("Blue"),
        "{said:?}"
    );

    let (_, stdin) = fake.finish().await;
    let reply = stdin
        .iter()
        .find(|l| l["response"]["request_id"] == id.as_str())
        .unwrap();
    assert_eq!(
        reply["response"]["response"]["mcp_response"],
        json!({"jsonrpc": "2.0", "id": 2, "result": {"isError": false, "content": [
            {"type": "text", "text": "dummy says: ping"},
            {"type": "image", "data": image.data, "mimeType": "image/png"},
        ]}})
    );
}

#[tokio::test]
async fn interrupt_then_switch_model() {
    let mut fake = Fake::replay("interrupt", with_dummy_tool);
    fake.until(|e| matches!(e, AiEvent::Ready { .. })).await;

    fake.say("Count from 1 to 300, one number per line.");
    let mut seen = String::new();
    while !seen.contains("\n9") {
        if let AiEvent::TextDelta(t) = fake.next().await {
            seen.push_str(&t);
        }
    }
    fake.send(AiCommand::Interrupt);
    let turn = fake.turn().await;
    assert_eq!(outcome(&turn), &TurnOutcome::Interrupted);

    // The process is still there, and takes a model switch.
    fake.send(AiCommand::SetModel("sonnet".into()));
    let ready = fake
        .until(|e| matches!(e, AiEvent::Ready { .. }))
        .await
        .pop()
        .unwrap();
    assert!(
        matches!(&ready, AiEvent::Ready { model: Some(m), .. } if m == "claude-sonnet-5-5"),
        "{ready:?}"
    );
    fake.say("Reply with exactly: still here");
    let turn = fake.turn().await;
    assert_eq!(text(&turn), "still here");
    assert_eq!(outcome(&turn), &TurnOutcome::Completed);

    let (_, stdin) = fake.finish().await;
    let subtypes: Vec<&str> = stdin
        .iter()
        .filter_map(|l| l["request"]["subtype"].as_str())
        .collect();
    assert_eq!(subtypes, ["initialize", "interrupt", "set_model"]);
}

#[tokio::test]
async fn not_logged_in() {
    let mut fake = Fake::start("logged-out", None, |_, dir| {
        std::fs::write(dir.join("fake_claude_logged_out"), "").unwrap();
    });
    // The login check says no: no Ready, and the client gives up.
    assert!(matches!(fake.next().await, AiEvent::NotLoggedIn));
    let handle = fake.handle.take().unwrap();
    tokio::time::timeout(TIMEOUT, handle.wait())
        .await
        .expect("the client task ends by itself");
}

#[tokio::test]
async fn a_dying_process_is_reported() {
    // A session that expects a user message first: the handshake breaks it.
    let mut fake = Fake::start(
        "dies",
        Some(r#"{"dir":"in","expect":{"type":"user"}}"#),
        |_, _| {},
    );
    let last = fake
        .until(|e| matches!(e, AiEvent::Died { .. }))
        .await
        .pop()
        .unwrap();
    let AiEvent::Died { reason } = last else {
        unreachable!()
    };
    // The exit status and the process's last words are in the reason.
    assert!(reason.contains("exit status: 2"), "{reason}");
    assert!(reason.contains("fake_claude: step 1"), "{reason}");
    assert!(
        !fake.handle.as_ref().unwrap().is_alive() || {
            tokio::time::sleep(Duration::from_millis(200)).await;
            !fake.handle.as_ref().unwrap().is_alive()
        }
    );
}
