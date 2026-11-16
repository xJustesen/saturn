//! Smoke test of `AiHandle` against the real, logged-in `claude` CLI. It
//! spends a few subscription tokens, so it is ignored by default:
//!
//!     cargo test --test ai_real_cli -- --ignored --nocapture
//!
//! Run it after a CLI upgrade, together with `scripts/claude_wire_capture.py`,
//! to see whether the control protocol still holds.

use std::time::{Duration, Instant};

use saturn::ai::{
    AiCommand, AiConfig, AiEvent, AiHandle, Permission, ToolContent, ToolSpec, TurnOutcome,
};
use serde_json::json;
use tokio::sync::mpsc;

#[tokio::test]
#[ignore = "runs the real claude CLI and spends subscription tokens"]
async fn a_turn_with_a_saturn_tool() {
    let dir = std::env::temp_dir().join(format!("saturn-ai-real-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut config = AiConfig::new(dir.clone());
    config.model = Some("haiku".into());
    config.tools = vec![ToolSpec {
        name: "dummy".into(),
        description: "Echo the given text back.".into(),
        input_schema: json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}),
    }];

    let (tx, mut rx) = mpsc::unbounded_channel();
    let spawned = Instant::now();
    let handle = AiHandle::spawn_with(config, move |ev| {
        let _ = tx.send(ev);
    });
    let mut sent = None;
    let mut first_text = None;
    let mut called = false;
    let mut said = String::new();
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(120), rx.recv())
            .await
            .expect("an event")
            .expect("a client");
        match ev {
            AiEvent::Ready {
                model,
                session_id,
                version,
            } if sent.is_none() => {
                println!(
                    "ready after {:?}: version {version:?}, model {model:?}, session {session_id}",
                    spawned.elapsed()
                );
                handle.send(AiCommand::Send {
                    text: "Call the mcp__saturn__dummy tool with text \"ping\", then reply with exactly what it returned."
                        .into(),
                    images: Vec::new(),
                });
                sent = Some(Instant::now());
            }
            AiEvent::Ready { model, .. } => println!("model: {model:?}"),
            AiEvent::PermissionRequest(p) => {
                println!(
                    "permission request for {} after {:?}",
                    p.tool_name,
                    sent.unwrap().elapsed()
                );
                handle.send(AiCommand::PermissionReply {
                    id: p.id,
                    decision: Permission::allow(),
                });
            }
            AiEvent::ToolCall {
                id,
                name,
                arguments,
                ..
            } => {
                assert_eq!(name, "dummy");
                assert_eq!(arguments["text"], "ping");
                called = true;
                let content = vec![ToolContent::Text("pong from saturn".into())];
                handle.send(AiCommand::ToolReply {
                    id,
                    content,
                    is_error: false,
                });
            }
            AiEvent::TextDelta(text) => {
                if first_text.is_none() {
                    first_text = Some(sent.unwrap().elapsed());
                }
                said.push_str(&text);
            }
            AiEvent::TurnDone {
                outcome,
                usage,
                cost,
            } => {
                println!(
                    "turn done after {:?}: {outcome:?}, {usage:?}, ${cost:.4}",
                    sent.unwrap().elapsed()
                );
                assert_eq!(outcome, TurnOutcome::Completed);
                break;
            }
            AiEvent::NotFound { .. } | AiEvent::NotLoggedIn | AiEvent::Died { .. } => {
                panic!("{ev:?}")
            }
            other => println!("{other:?}"),
        }
    }
    println!("first text {first_text:?} after send: {said:?}");
    assert!(called, "the in-band tool was called");
    assert!(said.contains("pong from saturn"), "{said:?}");
    handle.send(AiCommand::Shutdown);
    tokio::time::timeout(Duration::from_secs(5), handle.wait())
        .await
        .expect("shutdown");
    let _ = std::fs::remove_dir_all(&dir);
}
